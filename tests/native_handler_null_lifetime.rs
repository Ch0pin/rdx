pub use rdx::{native_hierarchy, native_ssa, native_types};
#[allow(dead_code)]
#[path = "../src/native_java/handler_null_type.rs"]
mod proof;
use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_java,
    native_method::MethodAnalysis,
};
use std::{fs, process::Command, sync::Arc};
fn fixture() -> DexClass {
    let descriptor: Arc<str> = "Lsample/Snapshot;".into();
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
            types: vec!["Lsample/Effects;".into()],
            strings: vec!["maybe".into(), "caught".into()],
            protos: vec![
                ("Ljava/lang/String;".into(), vec!["Z".into()]),
                ("V".into(), vec![]),
            ],
            methods: vec![(0, 0, 0), (0, 1, 1)],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: descriptor,
            name: "run".into(),
            access_flags: 9,
            return_type: "Ljava/lang/String;".into(),
            parameters: vec!["Z".into()],
            thrown_types: vec![],
            code: Some(DexCode {
                registers: 3,
                ins: 1,
                outs: 1,
                tries: 1,
                try_regions: vec![DexTryRegion {
                    start: 1,
                    end: 5,
                    catches: vec![(Some("Ljava/lang/RuntimeException;".into()), 6)].into(),
                }],
                offset: 0,
                instructions: vec![
                    0x0012, // @0 untyped zero before protected result overwrites it
                    0x1071, 0, 2,      // @1 maybe(flag)String; handler observes pre-write zero
                    0x000c, // @4 normal String result
                    0x0528, // @5 goto continuation @10
                    0x010d, // @6 caught exception
                    0x0071, 1, 0,      // @7 handler effect
                    0x0107, // @10 move-object v1,v0 from normal String / handler zero
                    0x0111, // @11 return-object v1
                ],
            }),
        }],
    }
}
fn source(c: &DexClass) -> anyhow::Result<String> {
    Ok(native_java::render_method("sample.Snapshot", c, &c.methods[0])?.source)
}
#[test]
fn handler_continuation_joins_literal_null_with_the_normal_reference_result() {
    let c = fixture();
    let a = MethodAnalysis::build(&c, &c.methods[0]).unwrap();
    let t = a.infer_types().unwrap();
    assert_eq!(
        proof::select(a.ssa(), &t, 6, 0, None).as_deref(),
        Some("Ljava/lang/String;")
    );
    let text = source(&c).unwrap();
    assert!(
        text.contains("String ") && text.contains("catch ("),
        "{text}"
    );
}
#[test]
fn nonzero_integer_and_undefined_entry_values_cannot_create_null_storage() {
    for prefix in [0x1012, 0x2001, 0] {
        let mut c = fixture();
        c.methods[0].code.as_mut().unwrap().instructions[0] = prefix;
        let a = MethodAnalysis::build(&c, &c.methods[0]).unwrap();
        let t = a.infer_types().unwrap();
        assert!(proof::select(a.ssa(), &t, 6, 0, None).is_none());
        assert!(source(&c).is_err());
    }
}
#[test]
fn incomplete_duplicate_and_rootless_continuation_phis_decline() {
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
            .find(|p| p.register == 0 && s.graph.blocks[p.block].start == 10)
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
        assert!(proof::select(&s, &t, 6, 0, None).is_none());
    }
}
#[test]
#[ignore = "requires javac/java; root runs serially"]
fn jvm_normal_reference_and_catch_null_keep_identity_effect_order_and_exceptions() {
    let text = source(&fixture()).unwrap();
    let dir =
        std::env::temp_dir().join(format!("rdx-handler-null-lifetime-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let file = dir.join("sample/Snapshot.java");
    fs::write(&file,format!(r#"package sample; public class Snapshot {{
{text}
public static void main(String[] args){{
 for(boolean fail:new boolean[]{{false,true}}){{
  Effects.trace="";Effects.failCatch=false;
  if(run(fail)!=(fail?null:Effects.token)||!Effects.trace.equals(fail?"MC":"M"))throw new AssertionError(Effects.trace);
 }}
 Effects.trace="";Effects.failCatch=true;
 try{{run(true);throw new AssertionError();}}
 catch(RuntimeException e){{if(e!=Effects.catchSentinel||!Effects.trace.equals("MC"))throw new AssertionError(e);}}
}}
}}
class Effects{{
 static String trace="";static boolean failCatch;static final String token=new String("receipt");
 static final RuntimeException sentinel=new RuntimeException(),catchSentinel=new RuntimeException();
 static String maybe(boolean fail){{trace+="M";if(fail)throw sentinel;return token;}}
 static void caught(){{trace+="C";if(failCatch)throw catchSentinel;}}
}}"#)).unwrap();
    let c = Command::new("javac").arg(&file).output().unwrap();
    assert!(c.status.success(), "{}", String::from_utf8_lossy(&c.stderr));
    let r = Command::new("java")
        .args(["-cp", dir.to_str().unwrap(), "sample.Snapshot"])
        .output()
        .unwrap();
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
}
