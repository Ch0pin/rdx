pub use rdx::{native_ir, native_ssa, native_types};
#[allow(dead_code)]
#[path = "../src/native_java/ssa_null_read.rs"]
mod proof;
use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols},
    native_java,
    native_method::MethodAnalysis,
};
use std::{fs, process::Command, sync::Arc};
fn fixture(ty: &str, left: i32, right: i32) -> DexClass {
    let descriptor: Arc<str> = "Lsample/NarrowRead;".into();
    DexClass {
        descriptor: descriptor.clone(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols {
            types: vec![descriptor.clone()],
            strings: vec!["accept".into()],
            protos: vec![("I".into(), vec![ty.into()])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: descriptor,
            name: match ty {
                "Z" => "truth",
                "B" => "bytes",
                "S" => "shorts",
                _ => "chars",
            }
            .into(),
            access_flags: 9,
            return_type: "I".into(),
            parameters: vec!["Z".into()],
            thrown_types: vec![],
            code: Some(DexCode {
                registers: 3,
                ins: 1,
                outs: 1,
                tries: 0,
                try_regions: vec![],
                offset: 0,
                instructions: vec![
                    0x0238,
                    6,
                    0x0014,
                    left as u16,
                    (left as u32 >> 16) as u16,
                    0x0428,
                    0x0014,
                    right as u16,
                    (right as u32 >> 16) as u16,
                    0x1071,
                    0,
                    0,
                    0x010a,
                    0x010f,
                ],
            }),
        }],
    }
}
fn admitted(class: &DexClass, ty: &str) -> bool {
    let a = MethodAnalysis::build(class, &class.methods[0]).unwrap();
    let t = a.infer_types().unwrap();
    proof::narrow_literal_at_read(a.instructions(), a.ssa(), &t, 9, 0, ty)
}
#[test]
fn exact_branch_literals_fit_boolean_byte_short_and_char_domains() {
    for (ty, left, right) in [
        ("Z", 0, 1),
        ("B", -128, 127),
        ("S", -32768, 32767),
        ("C", 0, 65535),
    ] {
        let c = fixture(ty, left, right);
        assert!(admitted(&c, ty), "{ty}");
        assert!(!admitted(&c, "I"));
    }
}
#[test]
fn repeated_register_operands_require_one_identical_reaching_value() {
    let mut c = fixture("B", -128, 127);
    c.symbols = Arc::new(DexSymbols {
        types: c.symbols.types.clone(),
        strings: c.symbols.strings.clone(),
        protos: vec![("I".into(), vec!["B".into(), "B".into()])],
        methods: c.symbols.methods.clone(),
        ..Default::default()
    });
    c.methods[0].code.as_mut().unwrap().outs = 2;
    c.methods[0].code.as_mut().unwrap().instructions[9] = 0x2071;
    assert!(admitted(&c, "B"));
    let a = MethodAnalysis::build(&c, &c.methods[0]).unwrap();
    let t = a.infer_types().unwrap();
    let mut s = native_ssa::SsaMethod::build(
        c.methods[0].code.as_ref().unwrap(),
        a.instructions(),
        a.blocks(),
    )
    .unwrap();
    let instruction = s.instructions.iter_mut().find(|i| i.pc == 9).unwrap();
    assert_eq!(instruction.reads.len(), 2);
    instruction.reads[1].words[0] = 0;
    assert!(!proof::narrow_literal_at_read(
        a.instructions(),
        &s,
        &t,
        9,
        0,
        "B"
    ));
}
#[test]
fn out_of_range_primitive_undefined_and_wide_roots_decline() {
    for (ty, left, right) in [
        ("Z", 0, 2),
        ("B", -129, 127),
        ("B", -128, 128),
        ("S", -32769, 0),
        ("S", 0, 32768),
        ("C", -1, 0),
        ("C", 0, 65536),
    ] {
        assert!(
            !admitted(&fixture(ty, left, right), ty),
            "{ty}:{left}:{right}"
        );
    }
    for opcode in [0x2001, 0x0000, 0x0016] {
        let mut c = fixture("B", 0, 1);
        c.methods[0].code.as_mut().unwrap().instructions[2..5].copy_from_slice(&[opcode, 0, 0]);
        assert!(!admitted(&c, "B"));
    }
}
#[test]
fn incomplete_phi_and_truncated_domains_decline() {
    let c = fixture("B", -128, 127);
    let a = MethodAnalysis::build(&c, &c.methods[0]).unwrap();
    let mut t = a.infer_types().unwrap();
    let id = a
        .ssa()
        .instructions
        .iter()
        .find(|i| i.pc == 9)
        .unwrap()
        .reads[0]
        .words[0];
    t.values[id].assignment_bounds_truncated = true;
    assert!(!proof::narrow_literal_at_read(
        a.instructions(),
        a.ssa(),
        &t,
        9,
        0,
        "B"
    ));
    let mut s = native_ssa::SsaMethod::build(
        c.methods[0].code.as_ref().unwrap(),
        a.instructions(),
        a.blocks(),
    )
    .unwrap();
    s.phis
        .iter_mut()
        .find(|p| p.result == id)
        .unwrap()
        .incoming
        .pop();
    let t = a.infer_types().unwrap();
    assert!(!proof::narrow_literal_at_read(
        a.instructions(),
        &s,
        &t,
        9,
        0,
        "B"
    ));
}
#[test]
#[ignore = "requires javac/java and root's exact-operand emission hook"]
fn jvm_literal_alternatives_preserve_narrow_values_effects_and_exception_identity() {
    let mut methods = String::new();
    for (ty, left, right) in [
        ("Z", 0, 1),
        ("B", -128, 127),
        ("S", -32768, 32767),
        ("C", 0, 65535),
    ] {
        let c = fixture(ty, left, right);
        methods.push_str(
            &native_java::render_method("sample.NarrowRead", &c, &c.methods[0])
                .unwrap()
                .source,
        );
    }
    let dir = std::env::temp_dir().join(format!("rdx-ssa-narrow-literals-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let file = dir.join("sample/NarrowRead.java");
    fs::write(&file,format!(r#"package sample; public class NarrowRead {{
{methods}
static int calls;static boolean fail;static final RuntimeException sentinel=new RuntimeException();
static int observe(int v){{calls++;if(fail)throw sentinel;return v;}}
static int accept(boolean v){{return observe(v?1:0);}}
static int accept(byte v){{return observe(v);}}
static int accept(short v){{return observe(v);}}
static int accept(char v){{return observe(v);}}
public static void main(String[] args){{
 for(boolean flag:new boolean[]{{false,true}}){{calls=0;fail=false;
 if(truth(flag)!=(flag?0:1)||bytes(flag)!=(flag?-128:127)||shorts(flag)!=(flag?-32768:32767)||chars(flag)!=(flag?0:65535)||calls!=4)throw new AssertionError();
 calls=0;fail=true;try{{bytes(flag);throw new AssertionError();}}catch(RuntimeException e){{if(e!=sentinel||calls!=1)throw new AssertionError(e);}}
 }}
}}
}}"#)).unwrap();
    let c = Command::new("javac").arg(&file).output().unwrap();
    assert!(c.status.success(), "{}", String::from_utf8_lossy(&c.stderr));
    let r = Command::new("java")
        .args(["-cp", dir.to_str().unwrap(), "sample.NarrowRead"])
        .output()
        .unwrap();
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
}
