pub use rdx::{
    native_call_values, native_dex, native_hierarchy, native_ir, native_ssa, native_types,
};
#[allow(dead_code)]
#[path = "../src/native_java/handler_null_type.rs"]
mod handler_null_type;
#[path = "../src/native_java/loop_null_reference.rs"]
mod proof;
use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols},
    native_java,
    native_method::MethodAnalysis,
};
use std::{fs, process::Command, sync::Arc};
fn fixture() -> DexClass {
    DexClass {
        descriptor: "Lsample/FieldLoop;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols {
            types: vec![
                "Lsample/FieldLoop;".into(),
                "Lsample/Box;".into(),
                "Ljava/lang/String;".into(),
                "Ljava/lang/Object;".into(),
                "I".into(),
            ],
            strings: vec![
                "number".into(),
                "payload".into(),
                "copy".into(),
                "last".into(),
            ],
            fields: vec![(1, 2, 1), (1, 2, 2), (1, 3, 3)],
            protos: vec![("V".into(), vec!["I".into()])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: "Lsample/FieldLoop;".into(),
            name: "run".into(),
            access_flags: 9,
            return_type: "Ljava/lang/String;".into(),
            parameters: vec!["I".into(), "Lsample/Box;".into()],
            thrown_types: vec![],
            code: Some(DexCode {
                registers: 4,
                ins: 2,
                outs: 1,
                tries: 0,
                try_regions: vec![],
                offset: 0,
                instructions: vec![
                    0x0012, 0x1071, 0, 0, // @0 shared-zero integer prefix
                    0x0238, 6,      // @4 count==0 -> fields @10
                    0x3007, // @6 r0=box
                    0x02d8, 0xff02, // @7 count--
                    0xfb28, // @9 backedge @4
                    0x0154, 0, // @10 r1=r0.payload (reference as receiver)
                    0x015b, 1, // @12 r0.copy=r1
                    0x305b, 2,      // @14 box.last=r0 (reference as stored value)
                    0x0111, // @16 return payload
                ],
            }),
        }],
    }
}
fn selected(c: &DexClass) -> Option<String> {
    let a = MethodAnalysis::build(c, &c.methods[0]).ok()?;
    let t = a.infer_types().ok()?;
    proof::select(
        c,
        &c.methods[0],
        a.instructions(),
        a.ssa(),
        a.calls(),
        &t,
        4,
        0,
    )
}
fn source(c: &DexClass) -> anyhow::Result<String> {
    Ok(native_java::render_method("sample.FieldLoop", c, &c.methods[0])?.source)
}
#[test]
fn original_reference_fields_accept_only_proven_receiver_and_value_roles() {
    let c = fixture();
    assert_eq!(selected(&c).as_deref(), Some("Lsample/Box;"));
    let text = source(&c).unwrap();
    assert!(
        text.contains(".payload") && text.contains(".copy =") && text.contains(".last ="),
        "{text}"
    );
}
#[test]
fn unrelated_receivers_incompatible_values_primitive_roles_and_bad_field_indices_decline() {
    for shape in 0..5 {
        let mut c = fixture();
        let mut fields = c.symbols.fields.clone();
        match shape {
            0 => fields[0].0 = 2,
            1 => fields[2].1 = 2,
            2 => {
                fields[2].1 = 4;
                c.methods[0].code.as_mut().unwrap().instructions[14] = 0x3059;
            }
            3 => c.methods[0].code.as_mut().unwrap().instructions[10] = 0x0152,
            _ => c.methods[0].code.as_mut().unwrap().instructions[15] = 99,
        }
        c.symbols = Arc::new(DexSymbols {
            types: c.symbols.types.clone(),
            strings: c.symbols.strings.clone(),
            fields,
            protos: c.symbols.protos.clone(),
            methods: c.symbols.methods.clone(),
            ..Default::default()
        });
        assert!(selected(&c).is_none(), "shape {shape}");
    }
}
#[test]
fn field_consumers_preserve_complete_phi_and_concrete_root_requirements() {
    let c = fixture();
    let a = MethodAnalysis::build(&c, &c.methods[0]).unwrap();
    let t = a.infer_types().unwrap();
    for shape in 0..3 {
        let mut s = native_ssa::SsaMethod::build(
            c.methods[0].code.as_ref().unwrap(),
            a.instructions(),
            a.blocks(),
        )
        .unwrap();
        let p = s
            .phis
            .iter_mut()
            .find(|p| p.register == 0 && s.graph.blocks[p.block].start == 4)
            .unwrap();
        match shape {
            0 => {
                p.incoming.pop();
            }
            1 => p.incoming.push(p.incoming[0]),
            _ => {
                for (_, id) in &mut p.incoming {
                    *id = p.result;
                }
            }
        }
        assert!(
            proof::select(&c, &c.methods[0], a.instructions(), &s, a.calls(), &t, 4, 0).is_none()
        );
    }
}
#[test]
#[ignore = "requires javac/java; root runs serially"]
fn jvm_field_reads_and_stores_preserve_identity_null_failure_and_prefix_effect() {
    let text = source(&fixture()).unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-loop-fields-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let file = dir.join("sample/FieldLoop.java");
    fs::write(&file,format!(r#"package sample;public class FieldLoop{{
{text}
static int trace;static boolean fail;static final RuntimeException sentinel=new RuntimeException();
static void number(int n){{if(n!=0)throw new AssertionError();trace++;if(fail)throw sentinel;}}
public static void main(String[] args){{for(int n=0;n<5;n++)for(boolean absent:new boolean[]{{false,true}})for(boolean fault:new boolean[]{{false,true}}){{Box b=absent?null:new Box();trace=0;fail=fault;try{{String actual=run(n,b);if(fault||n==0||absent||actual!=b.payload||b.copy!=b.payload||b.last!=b)throw new AssertionError();}}catch(RuntimeException e){{if(fault){{if(e!=sentinel)throw new AssertionError(e);}}else if(!(e instanceof NullPointerException)||(n>0&&!absent))throw new AssertionError(e);if(b!=null&&(b.copy!=null||b.last!=null))throw new AssertionError("store before failure");}}if(trace!=1)throw new AssertionError("prefix");}}}}
}}
class Box{{final String payload=new String("payload");String copy;Object last;}}"#)).unwrap();
    let c = Command::new("javac").arg(&file).output().unwrap();
    assert!(c.status.success(), "{}", String::from_utf8_lossy(&c.stderr));
    let r = Command::new("java")
        .args(["-cp", dir.to_str().unwrap(), "sample.FieldLoop"])
        .output()
        .unwrap();
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
}
