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
    let descriptor: Arc<str> = "Lsample/NullReferences;".into();
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
            types: vec![descriptor.clone(), "Ljava/lang/String;".into()],
            strings: vec!["number".into(), "text".into(), "builder".into()],
            protos: vec![
                ("V".into(), vec!["I".into()]),
                (
                    "Ljava/lang/String;".into(),
                    vec!["Ljava/lang/Object;".into()],
                ),
                (
                    "Ljava/lang/StringBuilder;".into(),
                    vec!["Ljava/lang/Object;".into()],
                ),
            ],
            methods: vec![(0, 0, 0), (0, 1, 1), (0, 2, 2)],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: descriptor,
            name: "run".into(),
            access_flags: 9,
            return_type: "Ljava/lang/String;".into(),
            parameters: vec!["I".into(), "Z".into()],
            thrown_types: vec![],
            code: Some(DexCode {
                registers: 5,
                ins: 2,
                outs: 1,
                tries: 0,
                try_regions: vec![],
                offset: 0,
                instructions: vec![
                    0x0012, // @0 shared zero, also used as integer before the loop
                    0x1071, 0, 0, // @1 number(0)
                    0x0338, 16, // @4 count==0 -> original cast @20
                    0x0438, 7, // @6 choose builder arm @13
                    0x1071, 1, 0,      // @8 text(old)String
                    0x000c, // @11 result
                    0x0528, // @12 goto @17
                    0x1071, 2, 0,      // @13 builder(old)StringBuilder
                    0x000c, // @16 result
                    0x03d8, 0xff03, // @17 count--
                    0xf128, // @19 continue @4
                    0x001f, 1,      // @20 original runtime cast to String
                    0x0011, // @22 return String
                ],
            }),
        }],
    }
}
fn selected(c: &DexClass) -> Option<String> {
    let a = MethodAnalysis::build(c, &c.methods[0]).unwrap();
    let t = a.infer_types().unwrap();
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
    Ok(native_java::render_method("sample.NullReferences", c, &c.methods[0])?.source)
}
#[test]
fn shared_null_and_distinct_reference_bounds_keep_the_original_downcast() {
    let c = fixture();
    assert_eq!(selected(&c).as_deref(), Some("Ljava/lang/Object;"));
    let text = source(&c).unwrap();
    assert!(
        text.contains("java.lang.Object ") && text.contains("((java.lang.String) "),
        "{text}"
    );
}
#[test]
fn nonzero_integer_undefined_and_primitive_consumers_decline() {
    for shape in 0..4 {
        let mut c = fixture();
        if shape == 3 {
            let mut protos = c.symbols.protos.clone();
            protos[1].0 = "I".into();
            c.symbols = Arc::new(DexSymbols {
                types: c.symbols.types.clone(),
                strings: c.symbols.strings.clone(),
                methods: c.symbols.methods.clone(),
                protos,
                ..Default::default()
            });
        }
        let w = &mut c.methods[0].code.as_mut().unwrap().instructions;
        match shape {
            0 => w[0] = 0x1012,
            1 => w[0] = 0x3001,
            2 => w[0] = 0,
            _ => w[11] = 0x000a,
        }
        assert!(selected(&c).is_none(), "shape {shape}");
        assert!(source(&c).is_err());
    }
}
#[test]
fn missing_and_duplicate_header_phi_inputs_decline() {
    let c = fixture();
    let a = MethodAnalysis::build(&c, &c.methods[0]).unwrap();
    let t = a.infer_types().unwrap();
    for duplicate in [false, true] {
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
        if duplicate {
            p.incoming.push(p.incoming[0]);
        } else {
            p.incoming.pop();
        }
        assert!(
            proof::select(&c, &c.methods[0], a.instructions(), &s, a.calls(), &t, 4, 0).is_none()
        );
    }
}
#[test]
#[ignore = "requires javac/java; root runs serially"]
fn jvm_multi_reference_phi_preserves_null_identity_original_cast_and_effect_order() {
    let text = source(&fixture()).unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-loop-null-references-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let file = dir.join("sample/NullReferences.java");
    fs::write(&file,format!(r#"package sample;public class NullReferences{{
{text}
static String trace;static Object last;static int calls,throwAt;
static final String textToken=new String("text");static final StringBuilder builderToken=new StringBuilder("builder");
static final RuntimeException sentinel=new RuntimeException();
static void number(int n){{if(n!=0)throw new AssertionError();trace+="N";}}
static void observe(Object old,String label){{if(old!=last)throw new AssertionError("old identity");trace+=label;if(calls++==throwAt)throw sentinel;}}
static String text(Object old){{observe(old,"T");last=textToken;return textToken;}}
static StringBuilder builder(Object old){{observe(old,"B");last=builderToken;return builderToken;}}
static void reset(int at){{trace="";last=null;calls=0;throwAt=at;}}
public static void main(String[] args){{
 for(int n=0;n<=6;n++)for(boolean choose:new boolean[]{{false,true}}){{reset(-1);
  if(n>0&&!choose){{try{{run(n,choose);throw new AssertionError();}}catch(ClassCastException expected){{}}}}
  else if(run(n,choose)!=(n==0?null:textToken))throw new AssertionError("result");
  if(!trace.equals("N"+(choose?"T":"B").repeat(n)))throw new AssertionError(trace);
 }}
 for(int at=0;at<5;at++)for(boolean choose:new boolean[]{{false,true}}){{reset(at);try{{run(6,choose);throw new AssertionError();}}catch(RuntimeException e){{if(e!=sentinel)throw new AssertionError(e);}}if(!trace.equals("N"+(choose?"T":"B").repeat(at+1)))throw new AssertionError(trace);}}
}}
}}"#)).unwrap();
    let c = Command::new("javac").arg(&file).output().unwrap();
    assert!(c.status.success(), "{}", String::from_utf8_lossy(&c.stderr));
    let r = Command::new("java")
        .args(["-cp", dir.to_str().unwrap(), "sample.NullReferences"])
        .output()
        .unwrap();
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
}
