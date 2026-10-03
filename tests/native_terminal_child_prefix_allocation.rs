use rdx::{
    native_cfg,
    native_dex::{self, DexClass, DexSymbols},
    native_dominators, native_ir, native_java,
};
use std::{fs, process::Command, sync::Arc};
#[path = "../src/native_java/terminal_child_loop.rs"]
mod plan;
fn fixture() -> DexClass {
    let mut c = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    c.methods.retain(|m| m.name.as_ref() == "answer");
    c.symbols = Arc::new(DexSymbols {
        strings: vec!["touch".into(), "<init>".into()],
        types: vec!["Lsample/Effects;".into(), "Lsample/Marker;".into()],
        protos: vec![
            ("I".into(), vec!["I".into(), "I".into()]),
            ("V".into(), vec![]),
        ],
        methods: vec![(0, 0, 0), (1, 1, 1)],
        ..Default::default()
    });
    let m = &mut c.methods[0];
    m.name = "allocatedPrefix".into();
    m.access_flags = 9;
    m.parameters = vec!["I".into(), "I".into()];
    m.return_type = "I".into();
    let code = m.code.as_mut().unwrap();
    code.registers = 6;
    code.ins = 2;
    code.outs = 2;
    code.tries = 0;
    code.try_regions.clear();
    code.instructions = vec![
        0x0022, 1, 0x1070, 1, 0, 0x0012, 0x0212, 0x4035, 0x0010, 0x00d8, 0x0100, 0x0112, 0x5135,
        0xfffb, 0x2071, 0, 0x0010, 0x030a, 0x0290, 0x0302, 0x01d8, 0x0101, 0xf628, 0x020f,
    ];
    c
}
#[test]
fn prefix_pairs_are_closed_before_exact_parent_frames() {
    let c = fixture();
    let code = c.methods[0].code.as_ref().unwrap();
    let p = plan::select(code).unwrap();
    assert_eq!(p.prefix_allocation_pairs, vec![(0, 2, 0)]);
    assert!(plan::validate(code, &p));
    let mut changed = p.clone();
    changed.original_words[3] = 0;
    assert!(!plan::validate(code, &changed));
    let mut changed = p.clone();
    changed.prefix_allocation_pairs.clear();
    assert!(!plan::validate(code, &changed));
}
#[test]
fn interrupted_wrong_receiver_body_or_constructor_interior_entry_decline() {
    for (pc, w) in [(2, 0x1071), (4, 1), (2, 0x0000), (5, 0x0122)] {
        let mut c = fixture();
        let code = c.methods[0].code.as_mut().unwrap();
        code.instructions[pc] = w;
        assert!(plan::select(code).is_none(), "{pc} {w:#x}");
    }
    let mut c = fixture();
    let code = c.methods[0].code.as_mut().unwrap();
    code.instructions.splice(0..0, [0x0238, 4]);
    assert!(plan::select(code).is_none());
}
#[test]
fn actual_renderer_prefix_allocator_guard_is_not_relaxed() {
    let mut c = fixture();
    c.methods[0].code.as_mut().unwrap().instructions[3] = 0;
    assert!(native_java::render_method("sample.Prefix", &c, &c.methods[0]).is_err());
}
fn oracle(outer: i64, inner: i64, ctor_fail: i64, touch_fail: i64, error: bool) -> String {
    let c = fixture();
    let words = &c.methods[0].code.as_ref().unwrap().instructions;
    let mut r = [0i64; 6];
    r[4] = outer;
    r[5] = inner;
    let mut pc = 0;
    let mut pending = 0;
    let mut trace = Vec::new();
    let mut calls = 0;
    for _ in 0..10000 {
        let w = words[pc];
        let op = w as u8;
        let dst = (w >> 8) as usize;
        match op {
            0x22 => {
                r[dst] = 1;
                pc += 2;
            }
            0x70 => {
                trace.push("C".into());
                if ctor_fail != 0 {
                    return format!(
                        "throw:{}|{}",
                        if error { "Error" } else { "RuntimeException" },
                        trace.join(",")
                    );
                }
                pc += 3;
            }
            0x12 => {
                r[(w as usize >> 8) & 15] = ((w as i16) >> 12) as i64;
                pc += 1;
            }
            0x35 => {
                pc = if r[(w as usize >> 8) & 15] >= r[(w as usize >> 12) & 15] {
                    (pc as isize + words[pc + 1] as i16 as isize) as usize
                } else {
                    pc + 2
                };
            }
            0xd8 => {
                let a = words[pc + 1];
                r[dst] = r[a as u8 as usize] + (a as i16 >> 8) as i64;
                pc += 2;
            }
            0x71 => {
                let arg = words[pc + 2];
                let a = r[(arg & 15) as usize];
                let b = r[((arg >> 4) & 15) as usize];
                trace.push(format!("{a}:{b}"));
                calls += 1;
                if calls == touch_fail {
                    return format!(
                        "throw:{}|{}",
                        if error { "Error" } else { "RuntimeException" },
                        trace.join(",")
                    );
                }
                pending = a * 100 + b;
                pc += 3;
            }
            0x0a => {
                r[dst] = pending;
                pc += 1;
            }
            0x90 => {
                let a = words[pc + 1];
                r[dst] = r[a as u8 as usize] + r[(a >> 8) as usize];
                pc += 2;
            }
            0x28 => pc = (pc as isize + (w as i16 >> 8) as isize) as usize,
            0x0f => return format!("return:{}|{}", r[dst], trace.join(",")),
            _ => panic!("{op:#x} {pc}"),
        }
    }
    panic!("budget")
}
#[test]
fn original_allocation_constructor_and_nested_loop_order_is_instruction_oracled() {
    assert_eq!(oracle(1, 1, 0, 0, false), "return:100|C,1:0");
    assert_eq!(oracle(0, 4, 0, 0, false), "return:0|C");
    assert_eq!(oracle(3, 0, 1, 0, true), "throw:Error|C");
    assert_eq!(
        oracle(2, 2, 0, 2, false),
        "throw:RuntimeException|C,1:0,1:1"
    );
}
#[test]
#[ignore = "requires candidate integrated emitter and JDK"]
fn unchanged_emitted_prefix_constructor_order_and_faults_match_original_dex() {
    let c = fixture();
    let source = native_java::render_method("sample.Prefix", &c, &c.methods[0])
        .unwrap()
        .source;
    let root = std::env::temp_dir().join(format!("rdx-child-prefix-{}", std::process::id()));
    fs::create_dir_all(root.join("sample")).unwrap();
    let mut checks = String::new();
    let mut count = 0;
    for outer in -1..=5 {
        for inner in -1..=5 {
            for ctor_fail in 0..=1 {
                for touch_fail in [0, 1, 3] {
                    for error in [false, true] {
                        let expected = oracle(outer, inner, ctor_fail, touch_fail, error);
                        checks.push_str(&format!("Effects.trace.clear();Effects.calls=0;Effects.ctorFail={ctor_fail};Effects.touchFail={touch_fail};Effects.error={error};actual=\"\";try{{actual=\"return:\"+allocatedPrefix({outer},{inner});}}catch(Throwable t){{actual=\"throw:\"+t.getClass().getSimpleName();}}actual+=\"|\"+String.join(\",\",Effects.trace);if(!actual.equals(\"{expected}\"))throw new AssertionError(actual);\n"));
                        count += 1;
                    }
                }
            }
        }
    }
    let java = format!(
        "package sample;public class Prefix{{{source}public static void main(String[] args){{String actual;{checks}System.out.println(\"{count} cases\");}}}}"
    );
    fs::write(root.join("sample/Prefix.java"), java).unwrap();
    fs::write(root.join("sample/Effects.java"),"package sample;public class Effects{static java.util.List<String> trace=new java.util.ArrayList<>();static int calls,ctorFail,touchFail;static boolean error;static void fail(){if(error)throw new Error();throw new RuntimeException();}public static int touch(int a,int b){trace.add(a+\":\"+b);if(++calls==touchFail)fail();return a*100+b;}}").unwrap();
    fs::write(root.join("sample/Marker.java"),"package sample;public class Marker{public Marker(){Effects.trace.add(\"C\");if(Effects.ctorFail!=0)Effects.fail();}}").unwrap();
    for command in [
        (
            "javac",
            vec![
                "sample/Prefix.java",
                "sample/Effects.java",
                "sample/Marker.java",
            ],
        ),
        ("java", vec!["-Xverify:all", "sample.Prefix"]),
    ] {
        let r = Command::new(command.0)
            .args(command.1)
            .current_dir(&root)
            .output()
            .unwrap();
        assert!(
            r.status.success(),
            "{} {}",
            String::from_utf8_lossy(&r.stdout),
            String::from_utf8_lossy(&r.stderr)
        );
    }
}
