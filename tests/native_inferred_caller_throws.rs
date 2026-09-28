use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::sync::Arc;

fn fixture(parent: Option<DexClass>, caller_declares: bool) -> DexClass {
    let descriptor: Arc<str> = "Lsample/Thrower;".into();
    let checked: Arc<str> = "Ljava/io/IOException;".into();
    let class = DexClass {
        descriptor: descriptor.clone(),
        superclass: Some(if parent.is_some() {
            "Lsample/Base;".into()
        } else {
            "Ljava/lang/Object;".into()
        }),
        interfaces: vec![],
        access_flags: 0x11,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols {
            types: vec![descriptor.clone()],
            strings: vec!["helper".into()],
            protos: vec![("V".into(), vec![checked.clone()])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        }),
        methods: vec![
            DexMethod {
                declaring_type: descriptor.clone(),
                name: "helper".into(),
                return_type: "V".into(),
                parameters: vec![checked.clone()],
                thrown_types: vec![],
                access_flags: 9,
                code: Some(DexCode {
                    registers: 1,
                    ins: 1,
                    outs: 0,
                    tries: 0,
                    try_regions: vec![],
                    instructions: vec![0x0027],
                    offset: 0,
                }),
            },
            DexMethod {
                declaring_type: descriptor,
                name: "caller".into(),
                return_type: "V".into(),
                parameters: vec![checked],
                thrown_types: if caller_declares {
                    vec!["Ljava/io/IOException;".into()]
                } else {
                    vec![]
                },
                access_flags: 9,
                code: Some(DexCode {
                    registers: 1,
                    ins: 1,
                    outs: 1,
                    tries: 0,
                    try_regions: vec![],
                    instructions: vec![0x1071, 0, 0, 0x000e],
                    offset: 1,
                }),
            },
        ],
    };
    let hierarchy = match parent.as_ref() {
        Some(parent) => TypeHierarchy::from_classes([&class, parent]).unwrap(),
        None => TypeHierarchy::from_classes([&class]).unwrap(),
    };
    class.symbols.hierarchy.set(Arc::new(hierarchy)).unwrap();
    class
}

fn parent_without_throws() -> DexClass {
    let mut parent = fixture(None, false);
    parent.descriptor = "Lsample/Base;".into();
    parent.access_flags = 1;
    parent.methods.remove(0);
    parent.methods[0].declaring_type = parent.descriptor.clone();
    parent.methods[0].code = None;
    parent
}

#[test]
fn exact_local_static_helper_exception_reaches_caller() {
    let class = fixture(None, true);
    for method in &class.methods {
        let source = native_java::render_method("sample.Thrower", &class, method)
            .unwrap()
            .source;
        assert!(source.contains("throws java.io.IOException"), "{source}");
    }
}

#[test]
fn unhandled_exact_static_caller_blocks_new_helper_declaration() {
    let class = fixture(None, false);
    let error = native_java::render_method("sample.Thrower", &class, &class.methods[0])
        .unwrap_err()
        .to_string();
    assert!(error.contains("Throw requires"), "{error}");
}

#[test]
fn static_hiding_family_with_uncovered_caller_rejects_helper() {
    let class = fixture(Some(parent_without_throws()), false);
    assert!(native_java::render_method("sample.Thrower", &class, &class.methods[0]).is_err());
}

#[test]
#[ignore = "requires javac and java"]
fn exact_local_static_helper_and_caller_compile_with_same_exception_identity() {
    use std::process::Command;
    let class = fixture(None, true);
    let helper = native_java::render_method("sample.Thrower", &class, &class.methods[0])
        .unwrap()
        .source;
    let caller = native_java::render_method("sample.Thrower", &class, &class.methods[1])
        .unwrap()
        .source;
    let dir = std::env::temp_dir().join(format!("rdx-inferred-caller-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("sample")).unwrap();
    std::fs::write(
        dir.join("sample/Thrower.java"),
        format!(r#"package sample;
public class Thrower {{ {helper} {caller}
 public static void main(String[] args) {{
  java.io.IOException expected = new java.io.IOException("same");
  try {{ caller(expected); throw new AssertionError("not thrown"); }}
  catch (java.io.IOException actual) {{ if (actual != expected) throw new AssertionError("identity"); }}
 }}
}}"#),
    )
    .unwrap();
    for (program, args) in [
        ("javac", vec!["sample/Thrower.java"]),
        ("java", vec!["-cp", ".", "sample.Thrower"]),
    ] {
        let result = Command::new(program)
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{program}: {}\n{helper}\n{caller}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}
