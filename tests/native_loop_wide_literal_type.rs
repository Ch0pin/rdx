pub use rdx::{native_ir, native_ssa, native_types};
#[path = "../src/native_java/loop_wide_literal_type.rs"]
mod proof;
use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols},
    native_method::MethodAnalysis,
};
use std::sync::Arc;
fn fixture(bits: u64) -> DexClass {
    let words = vec![
        0x0018,
        bits as u16,
        (bits >> 16) as u16,
        (bits >> 32) as u16,
        (bits >> 48) as u16,
        0x0412,
        0x5435,
        10,
        0x1071,
        0,
        4,
        0x020b,
        0x20cb,
        0x04d8,
        0x0104,
        0xf728,
        0x0010,
    ];
    DexClass {
        descriptor: "Lsample/DoubleCarry;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        access_flags: 1,
        interfaces: vec![],
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols {
            types: vec!["Lsample/Effects;".into()],
            strings: vec!["step".into()],
            methods: vec![(0, 0, 0)],
            protos: vec![("D".into(), vec!["I".into()])],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: "Lsample/DoubleCarry;".into(),
            name: "sum".into(),
            return_type: "D".into(),
            parameters: vec!["I".into()],
            thrown_types: vec![],
            access_flags: 9,
            code: Some(DexCode {
                registers: 6,
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
fn check(c: &DexClass, bits: u64) -> bool {
    let a = MethodAnalysis::build(c, &c.methods[0]).unwrap();
    let t = a.infer_types().unwrap();
    proof::double_header(a.instructions(), a.ssa(), &t, 6, 0, bits)
}
#[test]
fn coherent_mixed_literal_and_double_loop_phi_selects_double_storage() {
    for bits in [
        0,
        0x8000000000000000,
        0x7ff8123456789abc,
        0x3ff0000000000000,
    ] {
        assert!(check(&fixture(bits), bits));
    }
}
#[test]
fn valid_ssa_long_arithmetic_and_undefined_entry_are_rejected() {
    let mut c = fixture(0);
    Arc::get_mut(&mut c.symbols).unwrap().protos[0].0 = "J".into();
    c.methods[0].code.as_mut().unwrap().instructions[12] = 0x20bb;
    assert!(!check(&c, 0));
    let mut c = fixture(0);
    c.methods[0].code.as_mut().unwrap().instructions[..5].fill(0);
    assert!(!check(&c, 0));
}
#[test]
fn actual_long_parameter_seed_is_not_reinterpreted_as_double() {
    let mut c = fixture(0);
    c.methods[0].parameters = vec!["I".into(), "J".into()];
    let code = c.methods[0].code.as_mut().unwrap();
    code.registers = 8;
    code.ins = 3;
    code.instructions[..5].copy_from_slice(&[0x6004, 0, 0, 0, 0]);
    assert!(!check(&c, 0));
}
#[test]
fn valid_partial_tail_write_cannot_become_double_storage() {
    let mut c = fixture(0);
    let w = &mut c.methods[0].code.as_mut().unwrap().instructions;
    w[11] = 0x1112;
    assert!(!check(&c, 0));
}
#[test]
fn truncated_unknown_or_non_double_uses_are_rejected() {
    let c = fixture(0);
    let a = MethodAnalysis::build(&c, &c.methods[0]).unwrap();
    let mut t = a.infer_types().unwrap();
    let id = a
        .ssa()
        .phis
        .iter()
        .find(|p| p.register == 0 && a.ssa().graph.blocks[p.block].start == 6)
        .unwrap()
        .result;
    t.values[id].bounds_truncated = true;
    assert!(!proof::double_header(
        a.instructions(),
        a.ssa(),
        &t,
        6,
        0,
        0
    ));
    t.values[id].bounds_truncated = false;
    t.values[id]
        .assignment
        .push(native_types::AssignmentBound::Unknown);
    assert!(!proof::double_header(
        a.instructions(),
        a.ssa(),
        &t,
        6,
        0,
        0
    ));
    t.values[id].assignment.pop();
    t.values[id].required_types.push("J".into());
    assert!(!proof::double_header(
        a.instructions(),
        a.ssa(),
        &t,
        6,
        0,
        0
    ));
}
#[test]
fn generated_java_types_header_before_double_arithmetic() {
    let c = fixture(0x8000000000000000);
    let source = rdx::native_java::render_method("sample.DoubleCarry", &c, &c.methods[0])
        .unwrap()
        .source;
    assert!(
        source.contains("double ") && source.contains("Effects.step") && source.contains("while ("),
        "{source}"
    );
    assert!(!source.contains("long "), "{source}");
}
#[test]
#[ignore = "requires RDX_JAVA25_HOME"]
fn jvm_double_loop_preserves_signed_zero_nan_and_throwing_step_order() {
    use std::{fs, process::Command};
    let home = std::env::var("RDX_JAVA25_HOME").unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-wide-loop-type-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let mut methods = String::new();
    let mut calls = String::new();
    for (index, bits) in [
        0u64,
        0x8000000000000000,
        0x7ff8123456789abc,
        0x3ff0000000000000,
    ]
    .into_iter()
    .enumerate()
    {
        let mut c = fixture(bits);
        c.methods[0].name = format!("sum{index}").into();
        methods.push_str(
            &rdx::native_java::render_method("sample.DoubleCarry", &c, &c.methods[0])
                .unwrap()
                .source,
        );
        calls.push_str(&format!("check(sum{index}(count),0x{bits:016x}L,count);"));
    }
    let source = r#"package sample;
class Effects {static int calls,stop=-1;static final RuntimeException marker=new RuntimeException();static double step(int value){if(value!=calls)throw new AssertionError();calls++;if(value==stop)throw marker;return value==0?-0.0d:value+0.25d;}}
public class DoubleCarry {METHODS
static void check(double actual,long bits,int count){double expected=Double.longBitsToDouble(bits);for(int n=0;n<count;n++)expected+=n==0?-0.0d:n+0.25d;if(Double.doubleToRawLongBits(actual)!=Double.doubleToRawLongBits(expected)||Effects.calls!=Math.max(0,count))throw new AssertionError();Effects.calls=0;}
public static void main(String[]args){for(int count:new int[]{-3,-1,0,1,3,9}){Effects.calls=0;Effects.stop=-1;CALLS}for(int stop:new int[]{0,1,2}){Effects.calls=0;Effects.stop=stop;try{sum0(3);throw new AssertionError();}catch(RuntimeException actual){if(actual!=Effects.marker||Effects.calls!=stop+1)throw new AssertionError(actual);}}}
}"#;
    fs::write(
        dir.join("sample/DoubleCarry.java"),
        source.replace("METHODS", &methods).replace("CALLS", &calls),
    )
    .unwrap();
    for (program, args) in [
        ("javac", vec!["sample/DoubleCarry.java"]),
        ("java", vec!["-cp", ".", "sample.DoubleCarry"]),
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
