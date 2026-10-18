pub use rdx::{native_ir, native_ssa, native_types};
#[path = "../src/native_java/literal_read_proof.rs"]
mod proof;
use proof::Domain;
use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols},
    native_method::MethodAnalysis,
};
use std::sync::Arc;
fn fixture(domain: Domain, a: u64, b: u64) -> DexClass {
    let wide = domain == Domain::Raw64;
    let param = if wide { 2 } else { 1 };
    let constant = |bits: u64| {
        if wide {
            vec![
                0x0018,
                bits as u16,
                (bits >> 16) as u16,
                (bits >> 32) as u16,
                (bits >> 48) as u16,
            ]
        } else {
            vec![0x0014, bits as u16, (bits >> 16) as u16]
        }
    };
    // This dead integer lifetime defeats a physical-register constant scan.
    // The exact return SSA read sees only the two fresh constant lifetimes.
    let mut words = vec![
        0x1071,
        0,
        param,
        (param << 12) | 0x0001,
        (param << 8) | 0x0038,
        if wide { 8 } else { 6 },
    ];
    words.extend(constant(a));
    words.push(if wide { 0x0628 } else { 0x0428 });
    words.extend(constant(b));
    words.push(if wide {
        0x0010
    } else if domain == Domain::NullZero {
        0x0011
    } else {
        0x000f
    });
    let ret = match domain {
        Domain::Boolean => "Z",
        Domain::NullZero => "Ljava/lang/Object;",
        Domain::Raw32 => "F",
        Domain::Raw64 => "D",
    };
    DexClass {
        descriptor: "Lsample/LiteralRead;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        access_flags: 1,
        interfaces: vec![],
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols {
            types: vec!["Lsample/Effects;".into()],
            strings: vec!["take".into()],
            methods: vec![(0, 0, 0)],
            protos: vec![("V".into(), vec!["I".into()])],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: "Lsample/LiteralRead;".into(),
            name: match domain {
                Domain::Boolean => "boolValue",
                Domain::NullZero => "nullValue",
                Domain::Raw32 => "floatValue",
                Domain::Raw64 => "doubleValue",
            }
            .into(),
            return_type: ret.into(),
            parameters: vec!["I".into()],
            thrown_types: vec![],
            access_flags: 9,
            code: Some(DexCode {
                registers: param + 1,
                ins: 1,
                outs: 1,
                tries: 0,
                try_regions: vec![],
                offset: 0,
                instructions: words,
            }),
        }],
    }
}
fn boolean_loop_fixture() -> DexClass {
    let mut c = fixture(Domain::Boolean, 0, 1);
    let code = c.methods[0].code.as_mut().unwrap();
    code.registers = 3;
    code.instructions = vec![
        0x1071, 0, 2, 0x0012, 0x0112, 0x2135, 9, 0x1071, 0, 0, 0x1012, 0x01d8, 0x0101, 0xf828,
        0x000f,
    ];
    c
}
fn check(c: &DexClass, domain: Domain) -> bool {
    let a = MethodAnalysis::build(c, &c.methods[0]).unwrap();
    let t = a.infer_types().unwrap();
    let pc = c.methods[0].code.as_ref().unwrap().instructions.len() - 1;
    proof::proves_read(a.instructions(), a.ssa(), &t, pc, 0, domain)
}

fn null_loop_fixture() -> DexClass {
    let mut c = boolean_loop_fixture();
    let m = &mut c.methods[0];
    m.name = "nullValue".into();
    m.return_type = "Ljava/lang/Object;".into();
    let code = m.code.as_mut().unwrap();
    code.instructions[10] = 0x0012;
    code.instructions[14] = 0x0011;
    c
}
fn moved_null_loop_fixture() -> DexClass {
    let mut c = null_loop_fixture();
    let code = c.methods[0].code.as_mut().unwrap();
    code.registers = 4;
    code.instructions[2] = 3;
    code.instructions[5] = 0x3135;
    code.instructions[10] = 0x2001;
    code.instructions.insert(3, 0x0212);
    c
}
#[test]
fn zero_move_feeding_loop_phi_is_proved_at_original_read() {
    let c = moved_null_loop_fixture();
    assert!(check(&c, Domain::NullZero));
    rdx::native_java::render_method("sample.LiteralRead", &c, &c.methods[0]).unwrap();
}
#[test]
fn loop_zero_at_exact_object_return_renders_null_and_retains_effects() {
    let c = null_loop_fixture();
    assert!(check(&c, Domain::NullZero));
    let r = rdx::native_java::render_method("sample.LiteralRead", &c, &c.methods[0]).unwrap();
    assert!(
        r.source.contains("return ((java.lang.Object) null)"),
        "{}",
        r.source
    );
    assert!(r.source.contains("sample.Effects.take"), "{}", r.source);
}

#[test]
fn undefined_nonzero_parameter_and_computed_ancestry_cannot_become_null() {
    for (pc, word) in [(3, 0), (3, 0x1012), (10, 0x2001), (10, 0x1001)] {
        let mut c = null_loop_fixture();
        c.methods[0].code.as_mut().unwrap().instructions[pc] = word;
        assert!(!check(&c, Domain::NullZero), "{pc}:{word:04x}");
        assert!(
            rdx::native_java::render_method("sample.LiteralRead", &c, &c.methods[0]).is_err(),
            "{pc}:{word:04x}"
        );
    }
}
#[test]
fn one_word_object_read_cannot_claim_a_zero_wide_pair() {
    let mut c = fixture(Domain::Raw64, 0, 0);
    let m = &mut c.methods[0];
    m.return_type = "Ljava/lang/Object;".into();
    let code = m.code.as_mut().unwrap();
    *code.instructions.last_mut().unwrap() = 0x0011;
    assert!(!check(&c, Domain::NullZero));
    assert!(rdx::native_java::render_method("sample.LiteralRead", &c, &c.methods[0]).is_err());
}

#[test]
fn exact_zero_branch_read_ignores_earlier_integer_lifetime_and_preserves_raw32_domain() {
    assert!(check(&fixture(Domain::NullZero, 0, 0), Domain::NullZero));
    assert!(!check(&fixture(Domain::NullZero, 0, 1), Domain::NullZero));
    assert!(check(&fixture(Domain::Raw32, 0, 1), Domain::Raw32));
}
#[test]
#[ignore = "requires JDK25 via RDX_JAVA25_HOME"]
fn zero_loop_return_jvm_preserves_effect_counts_order_and_identity() {
    use std::{fs, process::Command};
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    for moves in [false, true] {
        let c = if moves {
            moved_null_loop_fixture()
        } else {
            null_loop_fixture()
        };
        let r = rdx::native_java::render_method("sample.LiteralRead", &c, &c.methods[0]).unwrap();
        let dir =
            std::env::temp_dir().join(format!("rdx-null-zero-read-{}-{moves}", std::process::id()));
        let _cleanup = Cleanup(dir.clone());
        fs::create_dir_all(dir.join("sample")).unwrap();
        let java = r#"package sample;
class Effects {static String trace;static int calls,failAt;static final RuntimeException fault=new RuntimeException();static void take(int value){trace+=value+",";if(++calls==failAt)throw fault;}}
public class LiteralRead {
METHOD
static Object oracle(int n){Effects.take(n);int zero=0;for(int i=0;i<n;i++){Effects.take(zero);zero=0;}return null;}
public static void main(String[]args){for(int n=-1;n<=5;n++)for(int at=0;at<=7;at++){Effects.failAt=at;Effects.calls=0;Effects.trace="";boolean ax=false,ex=false;Object a=null,e=null;try{a=nullValue(n);}catch(RuntimeException fault){if(fault!=Effects.fault)throw new AssertionError();ax=true;}String trace=Effects.trace;int count=Effects.calls;Effects.calls=0;Effects.trace="";try{e=oracle(n);}catch(RuntimeException fault){if(fault!=Effects.fault)throw new AssertionError();ex=true;}if(a!=e||a!=null||ax!=ex||count!=Effects.calls||!trace.equals(Effects.trace))throw new AssertionError(trace+" != "+Effects.trace);}}
}"#;
        fs::write(
            dir.join("sample/LiteralRead.java"),
            java.replace("METHOD", &r.source),
        )
        .unwrap();
        let home = std::env::var("RDX_JAVA25_HOME").unwrap();
        for (program, args) in [
            ("javac", vec!["sample/LiteralRead.java"]),
            ("java", vec!["-cp", ".", "sample.LiteralRead"]),
        ] {
            let output = Command::new(format!("{home}/bin/{program}"))
                .args(args)
                .current_dir(&dir)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stderr),
                r.source
            );
        }
    }
}

#[test]
fn zero_read_requires_original_use_operands_and_complete_assignment_bounds() {
    let c = moved_null_loop_fixture();
    let a = MethodAnalysis::build(&c, &c.methods[0]).unwrap();
    let t = a.infer_types().unwrap();
    let pc = c.methods[0].code.as_ref().unwrap().instructions.len() - 1;
    assert!(proof::proves_read(
        a.instructions(),
        a.ssa(),
        &t,
        pc,
        0,
        Domain::NullZero
    ));
    assert!(!proof::proves_read(
        a.instructions(),
        a.ssa(),
        &t,
        pc,
        1,
        Domain::NullZero
    ));
    let mut ir = a.instructions().clone();
    ir.instructions.last_mut().unwrap().reads[0].register = 1;
    assert!(!proof::proves_read(
        &ir,
        a.ssa(),
        &t,
        pc,
        0,
        Domain::NullZero
    ));
    let mut t = t;
    let id = a.ssa().instructions.last().unwrap().reads[0].words[0];
    t.values[id].bounds_truncated = true;
    assert!(!proof::proves_read(
        a.instructions(),
        a.ssa(),
        &t,
        pc,
        0,
        Domain::NullZero
    ));
}

fn null_call_fixture(array: bool) -> DexClass {
    let mut c = null_loop_fixture();
    let symbols = Arc::get_mut(&mut c.symbols).unwrap();
    symbols
        .strings
        .push(if array { "arrayValue" } else { "objectValue" }.into());
    symbols.protos.push((
        "V".into(),
        vec![
            if array {
                "[Ljava/lang/Object;"
            } else {
                "Ljava/lang/Object;"
            }
            .into(),
        ],
    ));
    symbols.methods.push((0, 1, 1));
    c.methods[0].code.as_mut().unwrap().instructions[8] = 1;
    c
}

#[test]
fn inferred_null_call_keeps_descriptor_and_exact_navigation() {
    for array in [false, true] {
        let c = null_call_fixture(array);
        let r = rdx::native_java::render_method("sample.LiteralRead", &c, &c.methods[0]).unwrap();
        let expected = if array {
            "((java.lang.Object[]) null)"
        } else {
            "((java.lang.Object) null)"
        };
        assert!(r.source.contains(expected), "{}", r.source);
        let target = if array {
            "sample.Effects.arrayValue([Ljava/lang/Object;)V"
        } else {
            "sample.Effects.objectValue(Ljava/lang/Object;)V"
        };
        assert!(format!("{:?}", r.links).contains(target), "{:?}", r.links);
    }
}

#[test]
#[ignore = "requires JDK25 via RDX_JAVA25_HOME"]
fn inferred_null_jvm_preserves_overload_and_null_varargs_array() {
    use std::{fs, process::Command};
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    for array in [false, true] {
        let c = null_call_fixture(array);
        let r = rdx::native_java::render_method("sample.LiteralRead", &c, &c.methods[0]).unwrap();
        let dir =
            std::env::temp_dir().join(format!("rdx-null-overload-{}-{array}", std::process::id()));
        let _cleanup = Cleanup(dir.clone());
        fs::create_dir_all(dir.join("sample")).unwrap();
        let java = r#"package sample;
class Effects {
static String trace;static int calls,failAt;static final RuntimeException fault=new RuntimeException();
static void record(String value){trace+=value+",";if(++calls==failAt)throw fault;}
static void take(int value){record("i"+value);}
static void objectValue(Object value){if(value!=null)throw new AssertionError();record("object");}
static void objectValue(String value){throw new AssertionError("narrow overload");}
static void arrayValue(Object... values){if(values!=null)throw new AssertionError("non-null varargs array");record("array");}
static void arrayValue(Object value){throw new AssertionError("object overload");}
}
public class LiteralRead {
METHOD
static Object oracle(int n){Effects.take(n);for(int i=0;i<n;i++){CALL;}return null;}
public static void main(String[]args){for(int n=-1;n<=5;n++)for(int at=0;at<=7;at++){Effects.failAt=at;Effects.calls=0;Effects.trace="";boolean ax=false,ex=false;Object a=null,e=null;try{a=nullValue(n);}catch(RuntimeException fault){if(fault!=Effects.fault)throw new AssertionError();ax=true;}String trace=Effects.trace;int count=Effects.calls;Effects.calls=0;Effects.trace="";try{e=oracle(n);}catch(RuntimeException fault){if(fault!=Effects.fault)throw new AssertionError();ex=true;}if(a!=e||ax!=ex||count!=Effects.calls||!trace.equals(Effects.trace))throw new AssertionError(trace+" != "+Effects.trace);}}
}"#;
        fs::write(
            dir.join("sample/LiteralRead.java"),
            java.replace("METHOD", &r.source).replace(
                "CALL",
                if array {
                    "Effects.arrayValue((Object[])null)"
                } else {
                    "Effects.objectValue((Object)null)"
                },
            ),
        )
        .unwrap();
        let home = std::env::var("RDX_JAVA25_HOME").unwrap();
        for (program, args) in [
            ("javac", vec!["sample/LiteralRead.java"]),
            ("java", vec!["-cp", ".", "sample.LiteralRead"]),
        ] {
            let result = Command::new(format!("{home}/bin/{program}"))
                .args(args)
                .current_dir(&dir)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&result.stderr),
                r.source
            );
        }
    }
}
