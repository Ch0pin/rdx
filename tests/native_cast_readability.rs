use rdx::{
    engine::DecompiledCode,
    native_dex::{self, DexClass, DexSymbols},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::{fs, process::Command, sync::Arc};

fn class(descriptor: &str, superclass: &str) -> DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class.descriptor = descriptor.into();
    class.superclass = Some(superclass.into());
    class.interfaces.clear();
    class
}

fn cast(source: &str, target: &str, hierarchy: Option<TypeHierarchy>) -> DecompiledCode {
    let mut class = class("Lsample/Cast;", "Ljava/lang/Object;");
    class
        .methods
        .retain(|method| method.name.as_ref() == "answer");
    class.symbols = Arc::new(DexSymbols {
        types: vec![target.into()],
        ..Default::default()
    });
    if let Some(hierarchy) = hierarchy {
        class.symbols.hierarchy.set(Arc::new(hierarchy)).unwrap();
    }
    let method = &mut class.methods[0];
    method.declaring_type = class.descriptor.clone();
    method.name = "cast".into();
    method.access_flags = 9;
    method.parameters = if source == "null" {
        vec![]
    } else {
        vec![source.into()]
    };
    method.return_type = target.into();
    let code = method.code.as_mut().unwrap();
    code.registers = 1;
    code.ins = u16::from(source != "null");
    code.outs = 0;
    code.instructions = if source == "null" {
        vec![0x0012, 0x001f, 0, 0x0011]
    } else {
        vec![0x001f, 0, 0x0011]
    };
    native_java::render_method("sample.Cast", &class, &class.methods[0]).unwrap()
}

#[test]
fn proven_upcasts_and_downcasts_keep_only_the_runtime_cast() {
    let base = class("Lsample/Base;", "Ljava/lang/Object;");
    let leaf = class("Lsample/Leaf;", "Lsample/Base;");
    for (source, target) in [
        ("Lsample/Leaf;", "Lsample/Base;"),
        ("Lsample/Base;", "Lsample/Leaf;"),
        ("[Lsample/Leaf;", "[Lsample/Base;"),
        ("[Lsample/Base;", "[Lsample/Leaf;"),
    ] {
        let hierarchy = TypeHierarchy::from_classes([&base, &leaf]).unwrap();
        let rendered = cast(source, target, Some(hierarchy));
        assert!(
            !rendered.source.contains("((java.lang.Object)"),
            "{}",
            rendered.source
        );
        assert!(rendered.source.contains("((sample."), "{}", rendered.source);
    }
}

#[test]
fn pinned_android_interface_downcast_avoids_object_bridge() {
    let hierarchy = TypeHierarchy::from_classes([]).unwrap();
    let rendered = cast(
        "Landroid/os/Parcelable;",
        "Landroid/content/Intent;",
        Some(hierarchy),
    );
    assert!(
        rendered.source.contains("((android.content.Intent) p0)"),
        "{}",
        rendered.source
    );
    assert!(
        !rendered.source.contains("((java.lang.Object)"),
        "{}",
        rendered.source
    );
    assert!(
        rendered.links.iter().any(|link| {
            link.label == "android.content.Intent"
                && rendered
                    .source
                    .chars()
                    .skip(link.start)
                    .take(link.end - link.start)
                    .collect::<String>()
                    == "android.content.Intent"
        }),
        "{:?}",
        rendered.links
    );
}

#[test]
fn unknown_and_unrelated_types_keep_object_bridge() {
    let left = class("Lsample/Left;", "Ljava/lang/Object;");
    let right = class("Lsample/Right;", "Ljava/lang/Object;");
    for (source, target, hierarchy) in [
        (
            "Lmissing/Left;",
            "Lmissing/Right;",
            TypeHierarchy::from_classes([]).unwrap(),
        ),
        (
            "Lsample/Left;",
            "Lsample/Right;",
            TypeHierarchy::from_classes([&left, &right]).unwrap(),
        ),
        ("[I", "[J", TypeHierarchy::from_classes([]).unwrap()),
    ] {
        let rendered = cast(source, target, Some(hierarchy));
        assert!(
            rendered.source.contains("((java.lang.Object) p0)"),
            "{}",
            rendered.source
        );
    }
    let rendered = cast("Lsample/Left;", "Lsample/Right;", None);
    assert!(
        rendered.source.contains("((java.lang.Object) p0)"),
        "{}",
        rendered.source
    );
    let shadow = class("Landroid/content/Intent;", "Ljava/lang/Object;");
    let rendered = cast(
        "Landroid/os/Parcelable;",
        "Landroid/content/Intent;",
        Some(TypeHierarchy::from_classes([&shadow]).unwrap()),
    );
    assert!(
        rendered.source.contains("((java.lang.Object) p0)"),
        "{}",
        rendered.source
    );
}

#[test]
fn null_cast_keeps_runtime_target_without_bridge() {
    let rendered = cast("null", "Lsample/Leaf;", None);
    assert!(
        rendered.source.contains("((sample.Leaf) null)"),
        "{}",
        rendered.source
    );
    assert!(
        !rendered.source.contains("java.lang.Object"),
        "{}",
        rendered.source
    );
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn proven_downcast_keeps_null_success_and_class_cast_exception() {
    let base = class("Lsample/Base;", "Ljava/lang/Object;");
    let leaf = class("Lsample/Leaf;", "Lsample/Base;");
    let hierarchy = TypeHierarchy::from_classes([&base, &leaf]).unwrap();
    let method = cast("Lsample/Base;", "Lsample/Leaf;", Some(hierarchy)).source;
    let directory = std::env::temp_dir().join(format!("rdx-readable-cast-{}", std::process::id()));
    fs::create_dir_all(directory.join("sample")).unwrap();
    let java = format!(
        "package sample; class Base {{}} class Leaf extends Base {{}} public class Cast {{ {method} public static void main(String[] args) {{ Leaf leaf = new Leaf(); if (cast(leaf) != leaf || cast(null) != null) throw new AssertionError(); try {{ cast(new Base()); throw new AssertionError(); }} catch (ClassCastException expected) {{}} }} }}"
    );
    fs::write(directory.join("sample/Cast.java"), java).unwrap();
    for (program, args) in [
        ("javac", vec!["sample/Cast.java"]),
        ("java", vec!["sample.Cast"]),
    ] {
        let output = Command::new(program)
            .args(args)
            .current_dir(&directory)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{program}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
