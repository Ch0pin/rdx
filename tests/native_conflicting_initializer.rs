//! An inlined superclass initializer must not call a retained, different constructor.
use rdx::{
    native_dex::{self, DexClass, DexCode, DexMethod, DexSymbols},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::{fs, process::Command, sync::Arc};
fn fixture() -> (DexClass, DexClass) {
    let mut leaf = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    leaf.descriptor = "Lsample/Failure;".into();
    leaf.superclass = Some("Ljava/lang/RuntimeException;".into());
    leaf.access_flags = 17;
    leaf.fields.clear();
    leaf.methods.clear();
    leaf.annotations_offset = 0;
    leaf.static_values.clear();
    leaf.symbols = Arc::new(DexSymbols {
        types: vec![
            leaf.descriptor.clone(),
            "Ljava/lang/RuntimeException;".into(),
        ],
        strings: vec!["<init>".into(), "retained-message".into()],
        protos: vec![
            ("V".into(), vec!["Ljava/lang/String;".into()]),
            ("V".into(), vec![]),
        ],
        methods: vec![(1, 0, 0), (1, 1, 0)],
        ..Default::default()
    });
    leaf.methods.push(DexMethod {
        declaring_type: leaf.descriptor.clone(),
        name: "<init>".into(),
        parameters: vec![],
        return_type: "V".into(),
        thrown_types: vec![],
        access_flags: 0x10001,
        code: Some(DexCode {
            registers: 2,
            ins: 1,
            outs: 2,
            tries: 0,
            try_regions: vec![],
            offset: 0,
            instructions: vec![0x001a, 1, 0x2070, 0, 0x0001, 0x000e],
        }),
    });
    let mut caller = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    caller.symbols = leaf.symbols.clone();
    caller.fields.clear();
    caller.annotations_offset = 0;
    caller.static_values.clear();
    caller.descriptor = "Lsample/Factory;".into();
    caller.superclass = Some("Ljava/lang/Object;".into());
    caller.methods.clear();
    caller.methods.push(DexMethod {
        declaring_type: caller.descriptor.clone(),
        name: "make".into(),
        parameters: vec![],
        return_type: leaf.descriptor.clone(),
        thrown_types: vec![],
        access_flags: 9,
        code: Some(DexCode {
            registers: 1,
            ins: 0,
            outs: 1,
            tries: 0,
            try_regions: vec![],
            offset: 0,
            instructions: vec![0x0022, 0, 0x1070, 1, 0, 0x0011],
        }),
    });
    (leaf, caller)
}
fn sources() -> (String, String) {
    let (leaf, caller) = fixture();
    let h = Arc::new(TypeHierarchy::from_classes([&leaf, &caller]).unwrap());
    leaf.symbols.hierarchy.set(h.clone()).unwrap();
    let a = native_java::render("sample.Failure", &leaf).unwrap().source;
    let b = native_java::render_method("sample.Factory", &caller, &caller.methods[0]).unwrap();
    assert!(a.contains("retained-message"));
    assert!(a.contains("Source-only initializer bridge"));
    assert!(a.contains("Void _rdxInitializer"), "{a}");
    assert!(b.source.contains("((java.lang.Void) null)"));
    assert!(
        b.links
            .iter()
            .any(|l| l.label == "java.lang.RuntimeException.<init>()V")
    );
    assert!(!h.equivalent_constructor("Lsample/Failure;", "Ljava/lang/RuntimeException;", &[]));
    (a, b.source)
}
#[test]
fn conflicting_initializer_preserves_retained_constructor_and_raw_reference() {
    sources();
}
#[test]
fn marker_collision_or_final_field_rejects_bridge() {
    for marker in [true, false] {
        let (mut leaf, caller) = fixture();
        if marker {
            let (_, mut extra) = fixture();
            let mut m = extra.methods.remove(0);
            m.name = "<init>".into();
            m.return_type = "V".into();
            m.declaring_type = leaf.descriptor.clone();
            m.parameters = vec!["Ljava/lang/Void;".into()];
            leaf.methods.push(m);
        } else {
            leaf.fields.push(rdx::native_dex::DexField {
                declaring_type: leaf.descriptor.clone(),
                name: "value".into(),
                field_type: "I".into(),
                access_flags: 17,
                is_static: false,
            });
        }
        let h = Arc::new(TypeHierarchy::from_classes([&leaf, &caller]).unwrap());
        leaf.symbols.hierarchy.set(h).unwrap();
        assert!(native_java::render_method("sample.Factory", &caller, &caller.methods[0]).is_err());
    }
}
#[test]
#[ignore = "requires javac and java on PATH"]
fn initializer_bridge_executes_distinct_exception_messages() {
    let (leaf, method) = sources();
    let dir = std::env::temp_dir().join(format!("rdx-init-bridge-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(dir.join("sample/Failure.java"), leaf).unwrap();
    fs::write(dir.join("sample/Factory.java"),format!("package sample; public class Factory {{ {method} public static void main(String[] args) {{ if(make().getMessage()!=null || !new Failure().getMessage().equals(\"retained-message\")) throw new AssertionError(); }} }}")).unwrap();
    for (exe, args) in [
        ("javac", vec!["sample/Failure.java", "sample/Factory.java"]),
        ("java", vec!["sample.Factory"]),
    ] {
        let r = Command::new(exe)
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    }
    fs::remove_dir_all(dir).unwrap();
}
