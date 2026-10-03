use rdx::{
    native_cfg,
    native_dex::{self, DexClass, DexSymbols, DexTryRegion},
    native_ir, native_java, native_ssa,
};
use std::{fs, process::Command, sync::Arc};
#[path = "../src/native_java/shared_terminal_exit.rs"]
mod shared_terminal_exit;
fn fixture(kind: usize) -> DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class.methods.retain(|m| m.name.as_ref() == "answer");
    class.descriptor = "Lsample/TerminalTail;".into();
    let ret = ["Z", "I", "J", "Ljava/lang/Object;"][kind];
    class.symbols = Arc::new(DexSymbols {
        strings: vec![
            match kind {
                2 => "touchWide",
                3 => "touchReference",
                _ => "touch",
            }
            .into(),
        ],
        types: vec!["Lsample/Effects;".into()],
        protos: vec![(ret.into(), vec!["I".into()])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    let method = &mut class.methods[0];
    method.declaring_type = class.descriptor.clone();
    method.name = ["prefix", "scalar", "wide", "reference"][kind].into();
    method.access_flags = 9;
    method.return_type = ret.into();
    method.parameters = match kind {
        0 => vec!["[B".into(), "[B".into()],
        2 => vec!["J".into(), "I".into(), "I".into()],
        3 => vec!["I".into(), "I".into(), "Ljava/lang/Object;".into()],
        _ => vec!["I".into(), "I".into(), "I".into()],
    };
    let code = method.code.as_mut().unwrap();
    code.tries = 0;
    code.try_regions.clear();
    code.outs = 1;
    if kind == 0 {
        code.registers = 6;
        code.ins = 2;
        code.instructions = vec![
            0x12, 0x539, 0x3, 0x1028, 0x4121, 0x5221, 0x2135, 0x3, 0xb28, 0x101, 0x5221, 0x2135,
            0xc, 0x248, 0x104, 0x348, 0x105, 0x3232, 0x3, 0xf, 0x1d8, 0x101, 0xf428, 0x1412, 0x40f,
        ];
    } else {
        let wide = kind == 2;
        code.registers = if wide { 7 } else { 5 };
        code.ins = if wide { 4 } else { 3 };
        code.instructions = vec![
            0x0012,
            if wide {
                0x3104
            } else if kind == 3 {
                0x4107
            } else {
                0x4101
            },
            if wide { 0x0639 } else { 0x0339 },
            11,
            if wide { 0x5035 } else { 0x2035 },
            13,
            0x1071,
            0,
            0,
            if wide {
                0x010b
            } else if kind == 3 {
                0x010c
            } else {
                0x010a
            },
            0x003c,
            3,
            0x0228,
            if wide {
                0x0110
            } else if kind == 3 {
                0x0111
            } else {
                0x010f
            },
            0x00d8,
            0x0100,
            0xf428,
            if wide {
                0x0110
            } else if kind == 3 {
                0x0111
            } else {
                0x010f
            },
        ];
    }
    class
}
#[test]
fn exact_original_edges_pure_terminal_values_and_cached_metadata_are_proved() {
    for kind in 0..4 {
        let class = fixture(kind);
        let code = class.methods[0].code.as_ref().unwrap();
        let (from, target, range) = if kind == 0 {
            (3, 19, 10..23)
        } else {
            (2, 13, 4..17)
        };
        let plan = shared_terminal_exit::prove(code, from, target, range).unwrap();
        assert!(shared_terminal_exit::validate(code, &plan));
        assert_eq!(plan.instructions, vec![target]);
        assert_eq!(plan.live_in_registers, vec![if kind == 0 { 0 } else { 1 }]);
        let mut p = plan.clone();
        p.raw_words[0] ^= 0x100;
        assert!(!shared_terminal_exit::validate(code, &p));
        let mut p = plan.clone();
        p.instructions.clear();
        assert!(!shared_terminal_exit::validate(code, &p));
        let mut p = plan.clone();
        p.incoming_edges.pop();
        assert!(!shared_terminal_exit::validate(code, &p));
        let mut p = plan.clone();
        p.registers += 1;
        assert!(!shared_terminal_exit::validate(code, &p));
        let mut p = plan.clone();
        p.target += 1;
        assert!(!shared_terminal_exit::validate(code, &p));
    }
}
#[test]
fn outside_copy_and_phi_cannot_hide_an_undefined_terminal_input() {
    let mut class = fixture(1);
    let code = class.methods[0].code.as_mut().unwrap();
    code.instructions[0] = 0;
    code.instructions[1] = 0x0101; // v1 copies genuinely undefined v0.
    let analysis = rdx::native_method::MethodAnalysis::build(&class, &class.methods[0]).unwrap();
    let types = analysis.infer_types().unwrap();
    let read = analysis
        .ssa()
        .instructions
        .iter()
        .find(|instruction| instruction.pc == 13)
        .unwrap()
        .reads
        .iter()
        .find(|read| read.register == 1)
        .unwrap()
        .words[0];
    assert!(
        types.values[read]
            .assignment
            .contains(&rdx::native_types::AssignmentBound::Undefined)
    );
    assert!(
        shared_terminal_exit::prove(class.methods[0].code.as_ref().unwrap(), 2, 13, 4..17)
            .is_none()
    );
    assert!(native_java::render_method("sample.TerminalTail", &class, &class.methods[0]).is_err());
}

#[test]
fn cycles_effects_protected_paths_undefined_values_and_result_state_decline() {
    for word in [
        0x0028, 0xff28, 0x001a, 0x001c, 0x001d, 0x0022, 0x0038, 0x0062, 0x0071, 0x010a, 0x010b,
        0x010c, 0x0027,
    ] {
        let mut c = fixture(1);
        let code = c.methods[0].code.as_mut().unwrap();
        code.instructions[13] = word;
        assert!(
            shared_terminal_exit::prove(code, 2, 13, 4..17).is_none(),
            "{word:#x}"
        );
    }
    let mut c = fixture(1);
    let code = c.methods[0].code.as_mut().unwrap();
    code.tries = 1;
    assert!(shared_terminal_exit::prove(code, 2, 13, 4..17).is_none());
    code.tries = 0;
    code.try_regions = vec![DexTryRegion {
        start: 13,
        end: 14,
        catches: vec![(None, 13)].into(),
    }];
    assert!(shared_terminal_exit::prove(code, 2, 13, 4..17).is_none());
    let mut c = fixture(1);
    let code = c.methods[0].code.as_mut().unwrap();
    code.instructions[1] = 0;
    assert!(shared_terminal_exit::prove(code, 2, 13, 4..17).is_none());
    let c = fixture(1);
    let code = c.methods[0].code.as_ref().unwrap();
    assert!(shared_terminal_exit::prove(code, 3, 13, 4..17).is_none());
    assert!(shared_terminal_exit::prove(code, 2, 14, 4..17).is_none());
}
#[test]
fn total_terminal_path_budget_is_32_nodes() {
    for n in [31, 32, 33] {
        let mut c = fixture(1);
        let code = c.methods[0].code.as_mut().unwrap();
        code.instructions = vec![0x0039, 3, 0x000e];
        code.instructions.extend(std::iter::repeat_n(0x0112, n - 1));
        code.instructions.push(0x010f);
        assert_eq!(
            shared_terminal_exit::prove(code, 0, 3, 2..code.instructions.len()).is_some(),
            n <= 32
        );
    }
}
#[test]
fn terminal_copy_preserves_current_scalar_wide_reference_and_real_array_prefix() {
    for kind in 0..4 {
        let c = fixture(kind);
        let s = native_java::render_method("sample.TerminalTail", &c, &c.methods[0])
            .unwrap()
            .source;
        assert_eq!(s.matches("while (true)").count(), 1, "{s}");
        assert!(!s.contains("switch ("), "{s}");
    }
}

// An instruction-driven oracle executes original DEX registers/targets. Array
// null and length behavior follows DEX instruction order, not Java source text.
fn prefix_oracle(a: Option<&[i8]>, b: Option<&[i8]>) -> Result<bool, &'static str> {
    let class = fixture(0);
    let words = &class.methods[0].code.as_ref().unwrap().instructions;
    let mut r = [0i64; 6];
    r[4] = i64::from(a.is_some());
    r[5] = i64::from(b.is_some());
    let mut pc = 0;
    for _ in 0..1000 {
        let w = words[pc];
        let op = w as u8;
        let dst = (w >> 8) as usize;
        match op {
            0x12 => {
                let v = ((w as i16) >> 12) as i64;
                r[(w as usize >> 8) & 15] = v;
                pc += 1;
            }
            0x01 => {
                r[(w as usize >> 8) & 15] = r[(w as usize >> 12) & 15];
                pc += 1;
            }
            0x21 => {
                let from = (w as usize >> 12) & 15;
                r[(w as usize >> 8) & 15] = if from == 4 {
                    a.ok_or("NullPointerException")?.len()
                } else {
                    b.ok_or("NullPointerException")?.len()
                } as i64;
                pc += 1;
            }
            0x48 => {
                let arg = words[pc + 1];
                let arr = arg as u8 as usize;
                let idx = r[(arg >> 8) as usize] as usize;
                let ar = if arr == 4 { a } else { b };
                r[dst] = *ar
                    .ok_or("NullPointerException")?
                    .get(idx)
                    .ok_or("ArrayIndexOutOfBoundsException")? as i64;
                pc += 2;
            }
            0x32 | 0x35 => {
                let l = r[(w as usize >> 8) & 15];
                let rr = r[(w as usize >> 12) & 15];
                let taken = if op == 0x32 { l == rr } else { l >= rr };
                pc = if taken {
                    (pc as isize + words[pc + 1] as i16 as isize) as usize
                } else {
                    pc + 2
                };
            }
            0x39 => {
                pc = if r[dst] != 0 {
                    (pc as isize + words[pc + 1] as i16 as isize) as usize
                } else {
                    pc + 2
                };
            }
            0x28 => pc = (pc as isize + (w as i16 >> 8) as isize) as usize,
            0xd8 => {
                let arg = words[pc + 1];
                r[dst] = r[arg as u8 as usize].wrapping_add((arg as i16 >> 8) as i64);
                pc += 2;
            }
            0x0f => return Ok(r[dst] != 0),
            _ => panic!("opcode {op:#x} pc{pc}"),
        }
    }
    panic!("oracle work budget")
}
#[test]
fn original_array_null_asymmetry_length_prefix_empty_and_byte_identity() {
    assert_eq!(prefix_oracle(None, None), Ok(false));
    assert_eq!(prefix_oracle(Some(&[]), None), Ok(false));
    assert_eq!(prefix_oracle(None, Some(&[])), Err("NullPointerException"));
    assert_eq!(prefix_oracle(Some(&[1, 2, 3]), Some(&[1, 2])), Ok(true));
    assert_eq!(prefix_oracle(Some(&[1]), Some(&[1, 2])), Ok(false));
    assert_eq!(prefix_oracle(Some(&[]), Some(&[])), Ok(true));
    assert_eq!(prefix_oracle(Some(&[-1, 0, 1]), Some(&[-1, 0])), Ok(true));
    assert_eq!(prefix_oracle(Some(&[-1]), Some(&[1])), Ok(false));
}
#[test]
#[ignore = "requires integrated renderer and javac/java on PATH"]
fn unchanged_emitted_prefix_source_matches_original_dex_nulls_prefix_and_signed_bytes() {
    let c = fixture(0);
    let s = native_java::render_method("sample.TerminalTail", &c, &c.methods[0])
        .unwrap()
        .source;
    let dir = std::env::temp_dir().join(format!("rdx-shared-terminal-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let arrays: Vec<Option<Vec<i8>>> = vec![
        None,
        Some(vec![]),
        Some(vec![0]),
        Some(vec![1]),
        Some(vec![-1]),
        Some(vec![1, 2]),
        Some(vec![1, 2, 3]),
        Some(vec![-1, 0, 1]),
    ];
    let mut checks = String::new();
    for a in &arrays {
        for b in &arrays {
            let lit = |a: &Option<Vec<i8>>| match a {
                None => "null".to_string(),
                Some(v) => format!(
                    "new byte[]{{{}}}",
                    v.iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(",")
                ),
            };
            let expected = match prefix_oracle(a.as_deref(), b.as_deref()) {
                Ok(v) => v.to_string(),
                Err(e) => e.into(),
            };
            checks.push_str(&format!(
                "check({}, {}, \"{}\");\n",
                lit(a),
                lit(b),
                expected
            ));
        }
    }
    let java = format!(
        "package sample; public class TerminalTail {{\n{}\nstatic void check(byte[] a,byte[] b,String expected) {{ String actual;try{{actual=Boolean.toString(prefix(a,b));}}catch(Throwable t){{actual=t.getClass().getSimpleName();}}if(!actual.equals(expected))throw new AssertionError(actual+\" != \"+expected); }} public static void main(String[] args) {{ {} System.out.println(\"64 DEX prefix cases passed\"); }} }}",
        s, checks
    );
    fs::write(dir.join("sample/TerminalTail.java"), java).unwrap();
    for cmd in [
        ("javac", vec!["sample/TerminalTail.java"]),
        ("java", vec!["-Xverify:all", "sample.TerminalTail"]),
    ] {
        let r = Command::new(cmd.0)
            .args(cmd.1)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            r.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&r.stdout),
            String::from_utf8_lossy(&r.stderr)
        );
    }
}

fn value_oracle(
    kind: usize,
    seed: i64,
    limit: i64,
    bypass: i64,
    fail_at: i64,
    error: bool,
) -> String {
    let c = fixture(kind);
    let words = &c.methods[0].code.as_ref().unwrap().instructions;
    let mut r = [0i64; 7];
    let wide = kind == 2;
    r[if wide { 3 } else { 4 }] = seed;
    r[if wide { 5 } else { 2 }] = limit;
    r[if wide { 6 } else { 3 }] = bypass;
    let mut pc = 0;
    let mut pending = 0;
    let mut trace = Vec::new();
    for _ in 0..1000 {
        let w = words[pc];
        let op = w as u8;
        let dst = (w >> 8) as usize;
        match op {
            0x12 => {
                r[(w as usize >> 8) & 15] = ((w as i16) >> 12) as i64;
                pc += 1;
            }
            0x01 | 0x04 | 0x07 => {
                r[(w as usize >> 8) & 15] = r[(w as usize >> 12) & 15];
                pc += 1;
            }
            0x35 => {
                pc = if r[(w as usize >> 8) & 15] >= r[(w as usize >> 12) & 15] {
                    (pc as isize + words[pc + 1] as i16 as isize) as usize
                } else {
                    pc + 2
                };
            }
            0x39 | 0x3c => {
                let taken = if op == 0x39 { r[dst] != 0 } else { r[dst] > 0 };
                pc = if taken {
                    (pc as isize + words[pc + 1] as i16 as isize) as usize
                } else {
                    pc + 2
                };
            }
            0x28 => pc = (pc as isize + (w as i16 >> 8) as isize) as usize,
            0x71 => {
                let i = r[(words[pc + 2] & 15) as usize];
                trace.push(i);
                if i == fail_at {
                    return format!(
                        "throw:{}|{:?}",
                        if error { "Error" } else { "RuntimeException" },
                        trace
                    );
                };
                pending = match kind {
                    1 => i * 7 - 3,
                    2 => 5_000_000_000 + i * 7,
                    3 => 1 + i % 2,
                    _ => unreachable!(),
                };
                pc += 3;
            }
            0x0a..=0x0c => {
                r[dst] = pending;
                pc += 1;
            }
            0xd8 => {
                let a = words[pc + 1];
                r[dst] = r[a as u8 as usize] + (a as i16 >> 8) as i64;
                pc += 2;
            }
            0x0f..=0x11 => return format!("return:{}|{:?}", r[dst], trace),
            _ => panic!("unsupported{op:#x}"),
        }
    }
    panic!("oracle budget")
}
#[test]
fn original_terminal_frame_changes_and_exceptions_are_oracled_from_registers() {
    assert_eq!(value_oracle(1, 91, 4, 1, -1, false), "return:91|[]");
    assert_eq!(value_oracle(1, 91, 0, 0, -1, false), "return:91|[]");
    assert_eq!(value_oracle(1, 91, 4, 0, -1, false), "return:4|[0, 1]");
    assert_eq!(
        value_oracle(2, -5_000_000_000, 4, 0, -1, false),
        "return:5000000007|[0, 1]"
    );
    assert_eq!(value_oracle(3, 0, 4, 0, -1, false), "return:2|[0, 1]");
    assert_eq!(value_oracle(1, 91, 4, 0, 1, true), "throw:Error|[0, 1]");
}
#[test]
#[ignore = "requires integrated renderer and javac/java on PATH"]
fn unchanged_emitted_scalar_wide_reference_terminal_frames_and_call_faults_match_dex() {
    let dir =
        std::env::temp_dir().join(format!("rdx-shared-terminal-values-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let mut methods = String::new();
    let mut checks = String::new();
    let mut cases = 0;
    for kind in 1..4 {
        let c = fixture(kind);
        methods.push_str(
            &native_java::render_method("sample.TerminalTail", &c, &c.methods[0])
                .unwrap()
                .source,
        );
        for limit in -1..=5 {
            for bypass in 0..=1 {
                for seed_index in -1..=1 {
                    for fail_at in -1..=1 {
                        for error in [false, true] {
                            let seed = match kind {
                                2 => seed_index * 5_000_000_000,
                                3 => seed_index + 1,
                                _ => seed_index,
                            };
                            let expected = value_oracle(kind, seed, limit, bypass, fail_at, error);
                            let input = match kind {
                                2 => format!("{seed}L"),
                                3 => ["null", "Effects.A", "Effects.B"][seed as usize].into(),
                                _ => seed.to_string(),
                            };
                            let name = ["", "scalar", "wide", "reference"][kind];
                            let arguments = if kind == 2 {
                                format!("{input},{limit},{bypass}")
                            } else {
                                format!("{limit},{bypass},{input}")
                            };
                            checks.push_str(&format!("Effects.trace.clear();Effects.failAt={fail_at};Effects.error={error};actual=\"\";try{{Object result={name}({arguments});actual=\"return:\"+{};}}catch(Throwable t){{actual=\"throw:\"+t.getClass().getSimpleName();}}check(actual+\"|\"+Effects.trace,\"{expected}\");\n",if kind==3{"(result==null?0:result==Effects.A?1:result==Effects.B?2:-1)"}else{"result"}));
                            cases += 1;
                        }
                    }
                }
            }
        }
    }
    let java = format!(
        "package sample;public class TerminalTail{{ {methods} static void check(String actual,String expected){{if(!actual.equals(expected))throw new AssertionError(actual+\" != \"+expected);}}public static void main(String[] args){{String actual;{checks}System.out.println(\"{cases} terminal value cases passed\");}}}}"
    );
    fs::write(dir.join("sample/TerminalTail.java"), java).unwrap();
    fs::write(dir.join("sample/Effects.java"),"package sample; public class Effects {static final Object A=new Object(),B=new Object();static java.util.List<Integer> trace=new java.util.ArrayList<>();static int failAt;static boolean error;static void hit(int i){trace.add(i);if(i==failAt){if(error)throw new Error();throw new RuntimeException();}}public static int touch(int i){hit(i);return i*7-3;}public static long touchWide(int i){hit(i);return 5000000000L+i*7;}public static Object touchReference(int i){hit(i);return i%2==0?A:B;}}").unwrap();
    for cmd in [
        (
            "javac",
            vec!["sample/TerminalTail.java", "sample/Effects.java"],
        ),
        ("java", vec!["-Xverify:all", "sample.TerminalTail"]),
    ] {
        let r = Command::new(cmd.0)
            .args(cmd.1)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            r.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&r.stdout),
            String::from_utf8_lossy(&r.stderr)
        );
    }
}
