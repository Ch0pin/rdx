//! One DEX zero can have an integer prefix use and a reference loop lifetime.
use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols},
    native_java,
    native_method::MethodAnalysis,
    native_types::{AssignmentBound, TypeResolution},
};
use std::{fs, process::Command, sync::Arc};

fn fixture() -> DexClass {
    let descriptor: Arc<str> = "Lsample/SharedZero;".into();
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
            strings: vec!["number".into(), "next".into()],
            protos: vec![
                ("V".into(), vec!["I".into()]),
                (
                    "Ljava/lang/String;".into(),
                    vec!["Ljava/lang/String;".into()],
                ),
            ],
            methods: vec![(0, 0, 0), (0, 1, 1)],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: descriptor,
            name: "run".into(),
            return_type: "Ljava/lang/String;".into(),
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
                    0x0012, // @0 shared zero, integer at the prefix call
                    0x1071, 0, 0, // @1 number(v0)
                    0x0238, 9, // @4 if count == 0 -> @13
                    0x1071, 1, 0,      // @6 next(v0) -> String
                    0x000c, // @9 v0 = String result
                    0x02d8, 0xff02, // @10 count--
                    0xf828, // @12 goto @4
                    0x0011, // @13 return-object v0
                ],
            }),
        }],
    }
}
fn source(class: &DexClass) -> anyhow::Result<String> {
    Ok(native_java::render_method("sample.SharedZero", class, &class.methods[0])?.source)
}
#[test]
fn shared_zero_input_has_distinct_use_domains_and_an_exact_reference_header() {
    let class = fixture();
    let a = MethodAnalysis::build(&class, &class.methods[0]).unwrap();
    let types = a.infer_types().unwrap();
    let phi = a
        .ssa()
        .phis
        .iter()
        .find(|p| p.register == 0 && a.ssa().graph.blocks[p.block].start == 4)
        .unwrap();
    assert_eq!(
        types.values[phi.result].resolution,
        TypeResolution::Resolved("Ljava/lang/String;".into())
    );
    assert!(phi.incoming.iter().any(|(_, id)| {
        let v = &types.values[*id];
        v.assignment
            == [AssignmentBound::Literal {
                bits: 0,
                wide: false,
            }]
            && v.required_types.iter().any(|ty| ty.as_ref() == "I")
            && v.required_types
                .iter()
                .any(|ty| ty.as_ref() == "Ljava/lang/String;")
    }));
    let text = source(&class).unwrap();
    assert!(text.contains("number(0)"), "{text}");
    assert!(text.contains("String ") && text.contains("null"), "{text}");
}
#[test]
fn actual_integer_undefined_and_nonzero_inputs_retain_the_type_boundary() {
    for prefix in [vec![0x1012], vec![0], vec![0x2001], vec![0x0016, 0]] {
        let mut class = fixture();
        class.methods[0]
            .code
            .as_mut()
            .unwrap()
            .instructions
            .splice(0..1, prefix);
        MethodAnalysis::build(&class, &class.methods[0]).unwrap();
        assert!(source(&class).is_err());
    }
}
#[test]
#[ignore = "requires javac and java"]
fn jvm_prefix_integer_and_zero_or_many_reference_iterations_preserve_values_and_effects() {
    let text = source(&fixture()).unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-shared-zero-domain-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let file = dir.join("sample/SharedZero.java");
    fs::write(&file,format!(r#"package sample;
public class SharedZero {{
{text}
static String trace; static int calls, throwAt;
static final RuntimeException sentinel = new RuntimeException();
static void number(int value) {{ if(value!=0) throw new AssertionError(); trace+="N"; }}
static String next(String old) {{
 trace+="R";
 if(calls==throwAt) throw sentinel;
 String expected=calls==0?null:"s".repeat(calls);
 if(!java.util.Objects.equals(old,expected)) throw new AssertionError("old="+old);
 return "s".repeat(++calls);
}}
public static void main(String[] args) {{
 for(int n=0;n<10;n++) {{
  trace=""; calls=0; throwAt=-1;
  String actual=run(n), expected=n==0?null:"s".repeat(n);
  if(!java.util.Objects.equals(actual,expected)||!trace.equals("N"+"R".repeat(n))) throw new AssertionError(trace);
 }}
 for(int at:new int[]{{0,3,8}}) {{
  trace=""; calls=0; throwAt=at;
  try {{run(9);throw new AssertionError();}}
  catch(RuntimeException e) {{if(e!=sentinel) throw new AssertionError(e);}}
  if(!trace.equals("N"+"R".repeat(at+1))) throw new AssertionError(trace);
 }}
}}
}}"#)).unwrap();
    let c = Command::new("javac").arg(&file).output().unwrap();
    assert!(c.status.success(), "{}", String::from_utf8_lossy(&c.stderr));
    let r = Command::new("java")
        .args(["-cp", dir.to_str().unwrap(), "sample.SharedZero"])
        .output()
        .unwrap();
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
}
