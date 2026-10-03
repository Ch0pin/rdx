use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::sync::Arc;

fn fixture() -> DexClass {
    let class = DexClass {
        descriptor: "Lsample/Calls;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols {
            types: vec![
                "Lsample/Calls;".into(),
                "Lsample/Marker;".into(),
                "Ljava/lang/RuntimeException;".into(),
            ],
            strings: vec!["log".into()],
            protos: vec![("V".into(), vec!["Lsample/Marker;".into()])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: "Lsample/Calls;".into(),
            name: "test".into(),
            return_type: "V".into(),
            parameters: vec!["Lsample/Foo;".into(), "Lsample/Bar;".into(), "Z".into()],
            thrown_types: vec![],
            access_flags: 9,
            code: Some(DexCode {
                registers: 4,
                ins: 3,
                outs: 1,
                tries: 0,
                offset: 0,
                try_regions: vec![],
                instructions: vec![0x0338, 4, 0x1007, 0x0228, 0x2007, 0x1071, 0, 0, 0x000e],
            }),
        }],
    };
    let subtype = |name: &str| DexClass {
        descriptor: name.into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec!["Lsample/Marker;".into()],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![],
        symbols: class.symbols.clone(),
    };
    let first_subclass = subtype("Lsample/Foo;");
    let second_subclass = subtype("Lsample/Bar;");
    let mut marker = subtype("Lsample/Marker;");
    marker.interfaces.clear();
    marker.access_flags = 0x601;
    let hierarchy =
        TypeHierarchy::from_classes([&class, &first_subclass, &second_subclass, &marker]).unwrap();
    class.symbols.hierarchy.set(Arc::new(hierarchy)).unwrap();
    class
}

#[test]
fn compatible_dex_assignments_allow_a_cast_from_widened_java_storage() {
    let class = fixture();
    let code = native_java::render_method("sample.Calls", &class, &class.methods[0]).unwrap();
    assert!(
        code.source.contains("java.lang.Object v"),
        "{}",
        code.source
    );
    assert!(code.source.contains("((sample.Marker)"), "{}", code.source);
    assert_eq!(code.source.matches(".log(").count(), 1);
}

#[test]
fn one_incompatible_phi_arm_is_not_repaired_by_a_java_cast() {
    let mut class = fixture();
    class.methods[0].parameters[1] = "Ljava/lang/Object;".into();
    rdx::native_method::MethodAnalysis::build(&class, &class.methods[0])
        .unwrap()
        .infer_types()
        .unwrap();
    let error = native_java::render_method("sample.Calls", &class, &class.methods[0]).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("DEX reference argument is not assignable"),
        "{error:#}"
    );
}

#[test]
fn original_check_cast_establishes_the_invoked_argument_type() {
    let mut class = fixture();
    class.methods[0].parameters = vec!["Ljava/lang/Object;".into()];
    let code = class.methods[0].code.as_mut().unwrap();
    code.registers = 2;
    code.ins = 1;
    code.instructions = vec![0x1007, 0x001f, 2, 0x1071, 0, 0, 0x000e];
    Arc::get_mut(&mut class.symbols).unwrap().protos[0].1[0] =
        "Ljava/lang/RuntimeException;".into();
    let source = native_java::render_method("sample.Calls", &class, &class.methods[0])
        .unwrap()
        .source;
    assert!(
        source.contains("((java.lang.RuntimeException) p0)"),
        "{source}"
    );
    assert_eq!(source.matches(".log(").count(), 1);
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn widened_storage_cast_preserves_selected_subclass_and_null_identity_on_jvm() {
    use std::{fs, process::Command};
    let class = fixture();
    let method = native_java::render_method("sample.Calls", &class, &class.methods[0])
        .unwrap()
        .source;
    let dir = std::env::temp_dir().join(format!("rdx-call-domains-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let java = r#"
package sample;
interface Marker {} class Foo implements Marker {} class Bar implements Marker {}
class Calls { static Marker seen; static void log(Marker m){seen=m;} METHOD }
public class Harness {
 public static void main(String[] args){
  for(boolean left:new boolean[]{false,true})for(boolean right:new boolean[]{false,true})for(boolean select:new boolean[]{false,true}){
   Foo foo=left?new Foo():null;Bar bar=right?new Bar():null;
   Calls.test(foo,bar,select);if(Calls.seen!=(select?foo:bar))throw new AssertionError("identity");
  }
  System.out.print("verified");
 }
}
"#.replace("METHOD", &method);
    fs::write(dir.join("sample/Harness.java"), java).unwrap();
    let compiled = Command::new("javac")
        .arg(dir.join("sample/Harness.java"))
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let run = Command::new("java")
        .arg("-cp")
        .arg(&dir)
        .arg("sample.Harness")
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(run.stdout, b"verified");
}
