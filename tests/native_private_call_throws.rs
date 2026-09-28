use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::{process::Command, sync::Arc};

fn bare_fixture() -> DexClass {
    let descriptor: Arc<str> = "Lsample/Agent;".into();
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
            types: vec![
                "Ljava/io/FileNotFoundException;".into(),
                "Landroid/os/ParcelFileDescriptor;".into(),
            ],
            strings: vec!["<init>".into(), "open".into()],
            protos: vec![
                ("V".into(), vec![]),
                (
                    "Landroid/os/ParcelFileDescriptor;".into(),
                    vec![
                        "Ljava/io/File;".into(),
                        "I".into(),
                        "Landroid/os/Handler;".into(),
                        "Landroid/os/ParcelFileDescriptor$OnCloseListener;".into(),
                    ],
                ),
            ],
            methods: vec![(0, 0, 0), (1, 1, 1)],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: descriptor,
            name: "helper".into(),
            return_type: "Landroid/os/ParcelFileDescriptor;".into(),
            parameters: vec![
                "Ljava/io/File;".into(),
                "I".into(),
                "Landroid/os/Handler;".into(),
                "Landroid/os/ParcelFileDescriptor$OnCloseListener;".into(),
            ],
            thrown_types: vec![],
            access_flags: 2,
            code: Some(DexCode {
                registers: 6,
                ins: 5,
                outs: 4,
                tries: 0,
                try_regions: vec![],
                instructions: vec![
                    0x0022, 0, 0x1070, 0, 0, 0x0239, 3, 0x0027, 0x0477, 1, 2, 0x000c, 0x0011,
                ],
                offset: 0,
            }),
        }],
    }
}

fn fixture() -> DexClass {
    let class = bare_fixture();
    class
        .symbols
        .hierarchy
        .set(Arc::new(TypeHierarchy::from_classes([&class]).unwrap()))
        .unwrap();
    class
}

#[test]
#[ignore = "requires javac and java"]
fn private_helper_infers_exact_uncaught_platform_io_and_compiles() {
    let class = fixture();
    let source = native_java::render_method("sample.Agent", &class, &class.methods[0])
        .unwrap()
        .source;
    assert!(source.contains("java.io.IOException"), "{source}");
    assert!(source.contains("java.io.FileNotFoundException"), "{source}");
    let dir = std::env::temp_dir().join(format!("rdx-private-call-{}", std::process::id()));
    let package = dir.join("sample");
    let android = dir.join("android/os");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::create_dir_all(&android).unwrap();
    std::fs::write(
        android.join("Handler.java"),
        "package android.os; public class Handler {}",
    )
    .unwrap();
    std::fs::write(
        android.join("ParcelFileDescriptor$OnCloseListener.java"),
        "package android.os; public interface ParcelFileDescriptor$OnCloseListener {}",
    )
    .unwrap();
    std::fs::write(android.join("ParcelFileDescriptor.java"), r#"package android.os;
        public class ParcelFileDescriptor {
            public static ParcelFileDescriptor open(java.io.File f, int mode, Handler h, ParcelFileDescriptor$OnCloseListener l) throws java.io.IOException {
                if (mode == 1) throw new java.io.IOException("exact-call");
                return new ParcelFileDescriptor();
            }
        }"#).unwrap();
    std::fs::write(package.join("Agent.java"), format!(r#"package sample;
        public class Agent {{
            {source}
            public static void main(String[] ignored) throws Exception {{
                Agent a = new Agent();
                try {{ a.helper(null, 0, null, null); throw new AssertionError("missing FNF"); }}
                catch (java.io.FileNotFoundException expected) {{}}
                try {{ a.helper(new java.io.File("x"), 1, null, null); throw new AssertionError("missing IO"); }}
                catch (java.io.IOException expected) {{ if (!"exact-call".equals(expected.getMessage())) throw expected; }}
                if (a.helper(new java.io.File("x"), 0, null, null) == null) throw new AssertionError("result");
            }}
        }}"#)).unwrap();
    let compiled = Command::new("javac")
        .arg(package.join("Agent.java"))
        .arg(android.join("Handler.java"))
        .arg(android.join("ParcelFileDescriptor.java"))
        .arg(android.join("ParcelFileDescriptor$OnCloseListener.java"))
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}\n{source}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let ran = Command::new("java")
        .arg("-cp")
        .arg(&dir)
        .arg("sample.Agent")
        .output()
        .unwrap();
    assert!(
        ran.status.success(),
        "{}",
        String::from_utf8_lossy(&ran.stderr)
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn public_object_root_method_infers_platform_io_but_loaded_shadow_does_not() {
    let mut class = bare_fixture();
    class.access_flags = 0x11;
    class.methods[0].access_flags = 1;
    class
        .symbols
        .hierarchy
        .set(Arc::new(TypeHierarchy::from_classes([&class]).unwrap()))
        .unwrap();
    let source = native_java::render_method("sample.Agent", &class, &class.methods[0])
        .unwrap()
        .source;
    assert!(source.contains("java.io.IOException"), "{source}");

    let mut shadow = fixture();
    shadow.descriptor = "Landroid/os/ParcelFileDescriptor;".into();
    shadow.methods.clear();
    let class = fixture();
    // A project definition of the platform owner, even without this overload,
    // shadows the pinned declaration instead of importing it speculatively.
    let _ = class.symbols.hierarchy.get();
    let hierarchy = TypeHierarchy::from_classes([&class, &shadow]).unwrap();
    assert!(
        hierarchy
            .exact_static_call_thrown_types(
                "Landroid/os/ParcelFileDescriptor;",
                "open",
                &class.symbols.protos[1].1,
                "Landroid/os/ParcelFileDescriptor;"
            )
            .is_none()
    );
}

#[test]
fn loaded_exact_static_declaration_is_inferred_but_unchecked_is_not() {
    for declared in ["Ljava/io/IOException;", "Ljava/lang/RuntimeException;"] {
        let mut class = bare_fixture();
        let symbols = Arc::get_mut(&mut class.symbols).unwrap();
        symbols.types.push("Lsample/Api;".into());
        symbols.methods[1].0 = 2;
        let mut api = bare_fixture();
        api.descriptor = "Lsample/Api;".into();
        let mut method = api.methods.remove(0);
        method.declaring_type = api.descriptor.clone();
        method.name = "open".into();
        method.access_flags = 9;
        method.code = None;
        method.thrown_types = vec![declared.into()];
        api.methods = vec![method];
        class
            .symbols
            .hierarchy
            .set(Arc::new(
                TypeHierarchy::from_classes([&class, &api]).unwrap(),
            ))
            .unwrap();
        let source = native_java::render_method("sample.Agent", &class, &class.methods[0])
            .unwrap()
            .source;
        assert_eq!(
            source.contains("java.io.IOException"),
            declared == "Ljava/io/IOException;",
            "{source}"
        );
        let duplicate = TypeHierarchy::from_classes([&class, &api, &api]).unwrap();
        assert!(
            duplicate
                .exact_static_call_thrown_types(
                    "Lsample/Api;",
                    "open",
                    &class.symbols.protos[1].1,
                    "Landroid/os/ParcelFileDescriptor;",
                )
                .is_none()
        );
    }
}
