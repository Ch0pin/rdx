use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::sync::Arc;

fn empty(descriptor: &str, parent: &str) -> DexClass {
    DexClass {
        descriptor: descriptor.into(),
        superclass: Some(parent.into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![],
        symbols: Arc::new(DexSymbols::default()),
    }
}
fn fixture(known_parent: bool) -> DexClass {
    let parent = empty("Lsample/Parent;", "Ljava/lang/Object;");
    let child = empty(
        "Lsample/Child;",
        if known_parent {
            "Lsample/Parent;"
        } else {
            "Lunknown/Base;"
        },
    );
    let mut caller = empty("Lsample/Carry;", "Ljava/lang/Object;");
    caller.symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/Source;".into()],
        strings: vec!["touch".into()],
        protos: vec![("V".into(), vec!["Lsample/Parent;".into()])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    caller.methods.push(DexMethod {
        declaring_type: caller.descriptor.clone(),
        name: "run".into(),
        return_type: "Lsample/Parent;".into(),
        parameters: vec![
            "Lsample/Child;".into(),
            "Lsample/Parent;".into(),
            "I".into(),
        ],
        thrown_types: vec![],
        access_flags: 9,
        code: Some(DexCode {
            registers: 5,
            ins: 3,
            outs: 1,
            tries: 0,
            try_regions: vec![],
            offset: 0,
            instructions: vec![
                0x2007, 0x0112, 0x4135, 9, 0x1071, 0, 0, 0x3007, 0x01d8, 0x0101, 0xf828, 0x0011,
            ],
        }),
    });
    caller
        .symbols
        .hierarchy
        .set(Arc::new(
            TypeHierarchy::from_classes([&caller, &parent, &child]).unwrap(),
        ))
        .unwrap();
    caller
}

#[test]
fn reference_loop_phi_widens_child_entry_to_exact_proven_parent() {
    let class = fixture(true);
    let analysis = rdx::native_method::MethodAnalysis::build(&class, &class.methods[0]).unwrap();
    let types = analysis.infer_types().unwrap();
    let phi = analysis
        .ssa()
        .phis
        .iter()
        .find(|phi| phi.register == 0 && analysis.ssa().graph.blocks[phi.block].start == 2)
        .unwrap();
    assert!(
        matches!(&types.values[phi.result].resolution, rdx::native_types::TypeResolution::Resolved(ty) if ty.as_ref() == "Lsample/Parent;")
    );
    let source = native_java::render_method("sample.Carry", &class, &class.methods[0])
        .unwrap()
        .source;
    assert!(source.contains("sample.Parent "), "{source}");
    assert!(!source.contains("((sample.Child)"), "{source}");
}

#[test]
fn unknown_or_nonreference_reaching_definitions_do_not_authorize_phi_casts() {
    let class = fixture(false);
    assert!(native_java::render_method("sample.Carry", &class, &class.methods[0]).is_err());
    for overwrite in [0x1012, 0x1001] {
        let mut class = fixture(true);
        class.methods[0].code.as_mut().unwrap().instructions[7] = overwrite;
        rdx::native_method::MethodAnalysis::build(&class, &class.methods[0]).unwrap();
        assert!(native_java::render_method("sample.Carry", &class, &class.methods[0]).is_err());
    }
}

#[test]
#[ignore = "requires javac and java"]
fn jvm_reference_phi_preserves_zero_iteration_identity_and_each_carried_reference() {
    use std::process::Command;
    let class = fixture(true);
    let source = native_java::render_method("sample.Carry", &class, &class.methods[0])
        .unwrap()
        .source;
    let dir = std::env::temp_dir().join(format!("rdx-reference-phi-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("sample")).unwrap();
    let file = dir.join("sample/Carry.java");
    std::fs::write(&file, format!(r#"package sample; public class Carry {{
{source}
public static void main(String[] args) {{
 Child left=new Child(); Parent right=new Parent();
 for(int n=-2;n<=9;n++) for(Parent replacement:new Parent[]{{right,null}}) {{
  Source.reset(-1,left,replacement);
  if(run(left,replacement,n)!=(n<=0?left:replacement)) throw new AssertionError();
  String expected=n<=0?"":"L"+"R".repeat(n-1);
  if(!Source.trace.equals(expected)) throw new AssertionError(Source.trace);
 }}
 for(int at:new int[]{{0,3,7}}) {{
  Source.reset(at,left,right);
  try{{run(left,right,9);throw new AssertionError();}}
  catch(RuntimeException e){{if(e!=Source.sentinel)throw new AssertionError(e);}}
  if(!Source.trace.equals("L"+"R".repeat(at)))throw new AssertionError(Source.trace);
 }}
}}
}}
class Parent{{}} class Child extends Parent{{}}
class Source{{
 static Parent left,right; static int throwAt,calls; static String trace;
 static final RuntimeException sentinel=new RuntimeException();
 static void reset(int at,Parent l,Parent r){{throwAt=at;calls=0;trace="";left=l;right=r;}}
 static void touch(Parent value){{trace+=value==left?"L":value==right?"R":"?";if(calls++==throwAt)throw sentinel;}}
}}
"#)).unwrap();
    let compile = Command::new("javac").arg(file).output().unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new("java")
        .args(["-cp", dir.to_str().unwrap(), "sample.Carry"])
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}
