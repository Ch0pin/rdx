use rdx::{
    native_dex::{DexClass, DexCode, DexField, DexMethod, DexSymbols, DexTryRegion},
    native_java,
    native_method::MethodAnalysis,
};
use std::{fs, process::Command, sync::Arc};

fn fixture(words: Vec<u16>) -> DexClass {
    DexClass {
        descriptor: "Lsample/Hello;".into(),
        superclass: Some("Lsample/Base;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols {
            types: vec![
                "Lsample/Hello;".into(),
                "Lsample/Base;".into(),
                "Ljava/lang/Object;".into(),
                "Lsample/Hooks;".into(),
            ],
            strings: vec![
                "<init>".into(),
                "check".into(),
                "consume".into(),
                "peer".into(),
            ],
            protos: vec![
                ("V".into(), vec!["Ljava/lang/Object;".into()]),
                ("V".into(), vec!["I".into()]),
            ],
            methods: vec![(1, 0, 0), (3, 1, 1), (3, 0, 2)],
            fields: vec![(0, 2, 3)],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: "Lsample/Hello;".into(),
            name: "<init>".into(),
            return_type: "V".into(),
            parameters: vec!["Ljava/lang/Object;".into()],
            thrown_types: vec![],
            access_flags: 1,
            code: Some(DexCode {
                registers: 2,
                ins: 2,
                outs: 2,
                tries: 0,
                try_regions: vec![],
                offset: 0,
                instructions: words,
            }),
        }],
    }
}
fn taken_throw() -> DexClass {
    // A dead terminal arm reuses the physical receiver register as integer/null.
    fixture(vec![
        0x0138, 6, 0x2070, 0, 0x0010, 0x000e, 0x0012, 0x1071, 1, 0, 0x0012, 0x0027,
    ])
}
fn fallthrough_throw() -> DexClass {
    fixture(vec![
        0x0139, 8, 0x0012, 0x1071, 1, 0, 0x0012, 0x0027, 0x2070, 0, 0x0010, 0x000e,
    ])
}
fn source(class: &DexClass) -> String {
    let analysis = MethodAnalysis::build(class, &class.methods[0]).unwrap();
    assert_eq!(analysis.infer_types().unwrap().wide_pair_issues, 0);
    native_java::render_method("sample.Hello", class, &class.methods[0])
        .unwrap()
        .source
}
fn substring(source: &str, start: usize, end: usize) -> String {
    source.chars().skip(start).take(end - start).collect()
}
#[test]
fn both_throw_arm_orientations_keep_delegation_only_on_success_and_exact_links() {
    for class in [taken_throw(), fallthrough_throw()] {
        let text = source(&class);
        assert_eq!(text.matches("super(").count(), 1, "{text}");
        assert_eq!(text.matches("sample.Hooks.check").count(), 1, "{text}");
        assert!(
            text.find("throw ").unwrap() < text.find("super(").unwrap(),
            "{text}"
        );
        assert!(!text.contains("else"), "{text}");
        let body = native_java::render_method("sample.Hello", &class, &class.methods[0]).unwrap();
        for (label, span) in [
            ("sample.Base.<init>(Ljava/lang/Object;)V", "super"),
            ("sample.Hooks.check(I)V", "check"),
        ] {
            let link = body.links.iter().find(|link| link.label == label).unwrap();
            assert_eq!(substring(&body.source, link.start, link.end), span);
        }
    }
}
fn reject(class: &DexClass) {
    // A valid decoded/SSA graph is required, so malformed CFG cannot mask the guard.
    MethodAnalysis::build(class, &class.methods[0])
        .unwrap()
        .infer_types()
        .unwrap();
    assert!(native_java::render_method("sample.Hello", class, &class.methods[0]).is_err());
}
fn with_arm(arm: &[u16]) -> DexClass {
    let mut words = vec![0x0138, 6, 0x2070, 0, 0x0010, 0x000e];
    words.extend_from_slice(arm);
    fixture(words)
}
#[test]
fn terminal_arm_cannot_read_escape_or_delegate_through_uninitialized_this() {
    reject(&with_arm(&[0x1071, 2, 0, 0x0012, 0x0027])); // consume(this)
    reject(&with_arm(&[0x2070, 0, 0x0010, 0x0012, 0x0027])); // super in masked arm
    let mut read = with_arm(&[0x0154, 0, 0x0012, 0x0027]); // iget-object r1,this.peer
    read.fields.push(DexField {
        declaring_type: "Lsample/Hello;".into(),
        name: "peer".into(),
        field_type: "Ljava/lang/Object;".into(),
        access_flags: 1,
        is_static: false,
    });
    reject(&read);
    let mut condition = taken_throw();
    condition.methods[0].code.as_mut().unwrap().instructions[0] = 0x0038; // condition(this)
    reject(&condition);
}
#[test]
fn undefined_terminal_operand_and_success_without_delegation_remain_rejected() {
    let mut undefined = fixture(vec![
        0x0238, 6, 0x2070, 0, 0x0021, 0x000e, 0, 0x1071, 1, 0, 0x0012, 0x0027,
    ]);
    undefined.methods[0].code.as_mut().unwrap().registers = 3; // this=r1, p0=r2, r0 undefined
    reject(&undefined);
    let mut missing = taken_throw();
    missing.methods[0].code.as_mut().unwrap().instructions[2..5].fill(0); // no constructor call
    reject(&missing);
}

#[test]
fn protected_arm_and_additional_incoming_edge_remain_outside_the_proof() {
    let mut protected = taken_throw();
    let code = protected.methods[0].code.as_mut().unwrap();
    code.instructions
        .extend_from_slice(&[0x000d, 0x0012, 0x0027]);
    code.tries = 1;
    code.try_regions = vec![DexTryRegion {
        start: 6,
        end: 10,
        catches: vec![(Some(Arc::from("Ljava/lang/RuntimeException;")), 12)].into(),
    }];
    reject(&protected);
    // Both condition PCs enter the same terminal arm. The selected arm is not
    // owned by one branch, so it cannot use the separate terminal context.
    let incoming = fixture(vec![
        0x0138, 8, 0x0138, 6, 0x2070, 0, 0x0010, 0x000e, 0x0012, 0x1071, 1, 0, 0x0012, 0x0027,
    ]);
    reject(&incoming);
}

// Independent small DEX evaluator, with invocation/throw events rather than
// source matching. It executes BOTH paths and injectable throwing operands.
fn dex_events(
    class: &DexClass,
    null: bool,
    fail_helper: bool,
    fail_super: bool,
) -> (&'static str, String) {
    let words = &class.methods[0].code.as_ref().unwrap().instructions;
    let mut zero = [false, null];
    let mut pc = 0;
    let mut events = String::new();
    for _ in 0..32 {
        let op = words[pc] as u8;
        match op {
            0x38 | 0x39 => {
                let r = (words[pc] >> 8) as usize;
                let taken = zero[r] == (op == 0x38);
                pc = if taken {
                    (pc as isize + words[pc + 1] as i16 as isize) as usize
                } else {
                    pc + 2
                };
            }
            0x12 => {
                zero[((words[pc] >> 8) & 15) as usize] = true;
                pc += 1;
            }
            0x71 => {
                assert_eq!(words[pc + 1], 1);
                events.push('H');
                if fail_helper {
                    return ("IllegalArgumentException", events);
                }
                pc += 3;
            }
            0x70 => {
                assert_eq!(words[pc + 1], 0);
                events.push('B');
                if fail_super {
                    return ("IllegalStateException", events);
                }
                pc += 3;
            }
            0x27 => {
                assert!(zero[(words[pc] >> 8) as usize]);
                return ("NullPointerException", events);
            }
            0x0e => return ("ok", events),
            _ => panic!("unexpected evaluator opcode {op:x}"),
        }
    }
    panic!("fixture did not terminate")
}
#[test]
#[ignore = "requires javac/java 25 on PATH"]
fn jvm_preserves_throwing_helper_and_superclass_event_order_for_both_paths() {
    let directory =
        std::env::temp_dir().join(format!("rdx-terminal-constructor-{}", std::process::id()));
    fs::create_dir_all(directory.join("sample")).unwrap();
    for class in [taken_throw(), fallthrough_throw()] {
        let mut cases = String::new();
        for null in [false, true] {
            for helper in [false, true] {
                for base in [false, true] {
                    let (exception, events) = dex_events(&class, null, helper, base);
                    cases.push_str(&format!(
                        "run({null},{helper},{base},\"{exception}\",\"{events}\");"
                    ));
                }
            }
        }
        let java = format!(
            r#"package sample;
class Hooks {{ static String events=""; static boolean failHelper,failBase;
 static void check(int value) {{ if(value!=0) throw new AssertionError(value); events+="H"; if(failHelper) throw new IllegalArgumentException(); }} }}
class Base {{ Base(Object value) {{ Hooks.events+="B"; if(value==null) throw new AssertionError(); if(Hooks.failBase) throw new IllegalStateException(); }} }}
public class Hello extends Base {{
{}
static void run(boolean nil,boolean helper,boolean base,String expected,String events) {{
 Hooks.events=""; Hooks.failHelper=helper; Hooks.failBase=base; String actual="ok";
 try {{ new Hello(nil?null:new Object()); }} catch(RuntimeException error) {{ actual=error.getClass().getSimpleName(); }}
 if(!actual.equals(expected)||!Hooks.events.equals(events)) throw new AssertionError(actual+":"+Hooks.events+" expected "+expected+":"+events);
}}
public static void main(String[] args) {{ {} }} }}"#,
            source(&class),
            cases
        );
        fs::write(directory.join("sample/Hello.java"), java).unwrap();
        for (program, arg) in [("javac", "sample/Hello.java"), ("java", "sample.Hello")] {
            let result = Command::new(program)
                .arg(arg)
                .current_dir(&directory)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{program}: {}",
                String::from_utf8_lossy(&result.stderr)
            );
        }
    }
}
