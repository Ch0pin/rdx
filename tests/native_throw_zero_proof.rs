pub use rdx::{native_ir, native_ssa, native_types};
#[path = "../src/native_java/throw_zero_proof.rs"]
mod proof;
use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols},
    native_method::MethodAnalysis,
};
use std::sync::Arc;
fn fixture(initial: u16, backedge: u16) -> DexClass {
    DexClass {
        descriptor: "Lsample/ZeroThrow;".into(),
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
            declaring_type: "Lsample/ZeroThrow;".into(),
            name: "zero".into(),
            return_type: "V".into(),
            parameters: vec!["I".into()],
            thrown_types: vec![],
            access_flags: 9,
            code: Some(DexCode {
                registers: 3,
                ins: 1,
                outs: 1,
                tries: 0,
                try_regions: vec![],
                offset: 0,
                instructions: vec![
                    initial, 0x0112, 0x2135, 9, 0x1071, 0, 0, backedge, 0x01d8, 0x0101, 0xf828,
                    0x0027,
                ],
            }),
        }],
    }
}
fn check(class: &DexClass) -> bool {
    let a = MethodAnalysis::build(class, &class.methods[0]).unwrap();
    let types = a.infer_types().unwrap();
    proof::proves_read(a.instructions(), a.ssa(), &types, 11, 0)
}
#[test]
fn zero_integer_carry_at_throw_is_proven_without_changing_integer_uses() {
    assert!(check(&fixture(0x0012, 0x0012)));
}
#[test]
fn nonzero_or_undefined_incoming_values_are_rejected_on_valid_ssa() {
    assert!(!check(&fixture(0x0012, 0x1012))); // nonzero loop incoming
    assert!(!check(&fixture(0x0000, 0x0012))); // real Undefined initial root
    assert!(!check(&fixture(0x0012, 0x2001))); // move from unconstrained integer parameter
}
#[test]
fn truncation_unknown_and_wide_domains_cannot_be_promoted_to_null() {
    let class = fixture(0x0012, 0x0012);
    let a = MethodAnalysis::build(&class, &class.methods[0]).unwrap();
    let mut types = a.infer_types().unwrap();
    let id = a
        .ssa()
        .instructions
        .iter()
        .find(|i| i.pc == 11)
        .unwrap()
        .reads[0]
        .words[0];
    types.values[id].bounds_truncated = true;
    assert!(!proof::proves_read(
        a.instructions(),
        a.ssa(),
        &types,
        11,
        0
    ));
    types.values[id].bounds_truncated = false;
    types.values[id].assignment = vec![native_types::AssignmentBound::Unknown];
    assert!(!proof::proves_read(
        a.instructions(),
        a.ssa(),
        &types,
        11,
        0
    ));
    types.values[id].assignment = vec![native_types::AssignmentBound::Literal {
        bits: 0,
        wide: true,
    }];
    assert!(!proof::proves_read(
        a.instructions(),
        a.ssa(),
        &types,
        11,
        0
    ));
}
#[test]
fn malformed_offsets_fail_the_front_end_instead_of_becoming_a_proof() {
    let mut c = fixture(0x0012, 0x0012);
    c.methods[0].code.as_mut().unwrap().instructions[3] = 3;
    assert!(MethodAnalysis::build(&c, &c.methods[0]).is_err());
}
#[test]
fn actual_wide_zero_definition_is_rejected_on_valid_decoding_and_ssa() {
    let mut c = fixture(0x0012, 0x0012);
    c.methods[0].code.as_mut().unwrap().instructions = vec![0x0016, 0, 0x0027];
    let a = MethodAnalysis::build(&c, &c.methods[0]).unwrap();
    let t = a.infer_types().unwrap();
    assert!(!proof::proves_read(a.instructions(), a.ssa(), &t, 2, 0));
}
#[test]
#[ignore = "requires RDX_JAVA25_HOME"]
fn jvm_zero_throw_preserves_integer_loop_effects_and_prior_throwing_operand() {
    use std::{fs, process::Command};
    let home = std::env::var("RDX_JAVA25_HOME").unwrap();
    let c = fixture(0x0012, 0x0012);
    let body = rdx::native_java::render_method("sample.ZeroThrow", &c, &c.methods[0]).unwrap();
    assert!(body.source.contains("throw null;"));
    let dir = std::env::temp_dir().join(format!("rdx-zero-throw-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let text = r#"package sample;
class Effects {static int calls,stop;static final RuntimeException marker=new RuntimeException();static void take(int value){if(value!=0)throw new AssertionError();calls++;if(calls==stop)throw marker;}}
public class ZeroThrow {
METHOD
public static void main(String[] args){for(int count:new int[]{Integer.MIN_VALUE,-1,0,1,3,17})for(int stop:new int[]{-1,1,2}){
 Effects.calls=0;Effects.stop=stop;boolean prior=stop>0&&count>=stop;
 try{zero(count);throw new AssertionError("throw was lost");}catch(RuntimeException actual){if(prior?actual!=Effects.marker:!(actual instanceof NullPointerException))throw new AssertionError(actual);}
 int expected=prior?stop:Math.max(0,count);if(Effects.calls!=expected)throw new AssertionError(Effects.calls+" != "+expected);
}}
}"#;
    fs::write(
        dir.join("sample/ZeroThrow.java"),
        text.replace("METHOD", &body.source),
    )
    .unwrap();
    for (program, args) in [
        ("javac", vec!["sample/ZeroThrow.java"]),
        ("java", vec!["-cp", ".", "sample.ZeroThrow"]),
    ] {
        let result = Command::new(format!("{home}/bin/{program}"))
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
