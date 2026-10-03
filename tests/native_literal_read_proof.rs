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
    words.push(if wide { 0x0010 } else { 0x000f });
    let ret = match domain {
        Domain::Boolean => "Z",
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
#[test]
fn exact_branch_reads_prove_boolean_and_raw_float_double_constants() {
    for (domain, a, b) in [
        (Domain::Boolean, 0, 1),
        (Domain::Raw32, 0x80000000, 0x7fc12345),
        (Domain::Raw64, 0x8000000000000000, 0x7ff8123456789abc),
    ] {
        assert!(check(&fixture(domain, a, b), domain));
    }
}
#[test]
fn valid_ssa_parameter_undefined_arithmetic_and_nonboolean_domains_are_rejected() {
    let mut c = fixture(Domain::Boolean, 0, 1);
    assert!(!check(&fixture(Domain::Boolean, 0, 2), Domain::Boolean));
    let words = &mut c.methods[0].code.as_mut().unwrap().instructions;
    words[10] = 0x1001;
    words[11] = 0;
    words[12] = 0;
    assert!(!check(&c, Domain::Boolean));
    let words = &mut c.methods[0].code.as_mut().unwrap().instructions;
    words[3] = 0;
    words[10] = 0;
    assert!(!check(&c, Domain::Boolean));
    let words = &mut c.methods[0].code.as_mut().unwrap().instructions;
    words[10] = 0x00d8;
    words[11] = 0x0101;
    assert!(!check(&c, Domain::Boolean));
}
#[test]
fn truncated_unknown_domains_and_corrupted_wide_pair_are_rejected() {
    let c = fixture(Domain::Raw64, 0, 0x8000000000000000);
    let a = MethodAnalysis::build(&c, &c.methods[0]).unwrap();
    let mut t = a.infer_types().unwrap();
    let pc = c.methods[0].code.as_ref().unwrap().instructions.len() - 1;
    let ids = &a
        .ssa()
        .instructions
        .iter()
        .find(|i| i.pc == pc)
        .unwrap()
        .reads[0]
        .words;
    t.values[ids[0]].bounds_truncated = true;
    assert!(!proof::proves_read(
        a.instructions(),
        a.ssa(),
        &t,
        pc,
        0,
        Domain::Raw64
    ));
    t.values[ids[0]].bounds_truncated = false;
    t.values[ids[1]].assignment = vec![native_types::AssignmentBound::Unknown];
    assert!(!proof::proves_read(
        a.instructions(),
        a.ssa(),
        &t,
        pc,
        0,
        Domain::Raw64
    ));
    let mut c = fixture(Domain::Raw64, 0, 1);
    let w = &mut c.methods[0].code.as_mut().unwrap().instructions;
    w.insert(w.len() - 1, 0x0112);
    let pc = w.len() - 1;
    let a = MethodAnalysis::build(&c, &c.methods[0]).unwrap();
    let t = a.infer_types().unwrap();
    assert!(!proof::proves_read(
        a.instructions(),
        a.ssa(),
        &t,
        pc,
        0,
        Domain::Raw64
    ));
}
#[test]
fn malformed_branch_is_front_end_failure_and_cannot_mask_semantic_negative() {
    let mut c = fixture(Domain::Raw32, 0, 1);
    c.methods[0].code.as_mut().unwrap().instructions[5] = 3;
    assert!(MethodAnalysis::build(&c, &c.methods[0]).is_err());
}
#[test]
fn generated_java_reifies_proven_lost_literal_flags_at_exact_use() {
    for (domain, a, b, expression) in [
        (Domain::Boolean, 0, 1, " != 0"),
        (
            Domain::Raw32,
            0x80000000,
            0x7fc12345,
            "Float.intBitsToFloat",
        ),
        (
            Domain::Raw64,
            0x8000000000000000,
            0x7ff8123456789abc,
            "Double.longBitsToDouble",
        ),
    ] {
        let c = if domain == Domain::Boolean {
            boolean_loop_fixture()
        } else {
            fixture(domain, a, b)
        };
        let body =
            rdx::native_java::render_method("sample.LiteralRead", &c, &c.methods[0]).unwrap();
        assert!(body.source.contains(expression), "{}", body.source);
        assert!(body.source.contains("Effects.take"));
    }
}
#[test]
#[ignore = "requires RDX_JAVA25_HOME"]
fn jvm_preserves_nan_payload_signed_zero_boolean_bits_and_prior_throw_identity() {
    use std::{fs, process::Command};
    let home = std::env::var("RDX_JAVA25_HOME").unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-literal-read-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let mut methods = String::new();
    for (domain, a, b) in [
        (Domain::Boolean, 0, 1),
        (Domain::Raw32, 0x80000000, 0x7fc12345),
        (Domain::Raw64, 0x8000000000000000, 0x7ff8123456789abc),
    ] {
        let c = if domain == Domain::Boolean {
            boolean_loop_fixture()
        } else {
            fixture(domain, a, b)
        };
        methods.push_str(
            &rdx::native_java::render_method("sample.LiteralRead", &c, &c.methods[0])
                .unwrap()
                .source,
        );
    }
    let source = r#"package sample;
class Effects {static int calls,last;static boolean fail;static final RuntimeException marker=new RuntimeException();static void take(int value){calls++;last=value;if(fail)throw marker;}}
public class LiteralRead {METHODS
public static void main(String[] args){for(int x:new int[]{-3,-1,0,1,3}){
Effects.calls=0;Effects.fail=false;if(boolValue(x)!=(x>0))throw new AssertionError();if(Effects.calls!=1+Math.max(0,x))throw new AssertionError();
if(Float.floatToRawIntBits(floatValue(x))!=(x==0?0x7fc12345:0x80000000))throw new AssertionError();
if(Double.doubleToRawLongBits(doubleValue(x))!=(x==0?0x7ff8123456789abcL:0x8000000000000000L))throw new AssertionError();if(Effects.calls!=3+Math.max(0,x))throw new AssertionError();
Effects.fail=true;for(int which=0;which<3;which++){try{if(which==0)boolValue(x);else if(which==1)floatValue(x);else doubleValue(x);throw new AssertionError();}catch(RuntimeException actual){if(actual!=Effects.marker)throw new AssertionError(actual);}}
}}
}"#;
    fs::write(
        dir.join("sample/LiteralRead.java"),
        source.replace("METHODS", &methods),
    )
    .unwrap();
    for (program, args) in [
        ("javac", vec!["sample/LiteralRead.java"]),
        ("java", vec!["-cp", ".", "sample.LiteralRead"]),
    ] {
        let out = Command::new(format!("{home}/bin/{program}"))
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
