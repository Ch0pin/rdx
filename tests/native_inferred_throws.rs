use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::sync::Arc;
fn fixture(flags: u32, superclass: &str, ty: &str) -> DexClass {
    let receiver = usize::from(flags & 8 == 0);
    DexClass {
        descriptor: "Lsample/Thrower;".into(),
        superclass: Some(superclass.into()),
        interfaces: vec![],
        access_flags: 0x11,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols::default()),
        methods: vec![DexMethod {
            declaring_type: "Lsample/Thrower;".into(),
            name: "test".into(),
            return_type: "V".into(),
            parameters: vec![ty.into()],
            thrown_types: vec![],
            access_flags: flags,
            code: Some(DexCode {
                registers: (receiver + 1) as u16,
                ins: (receiver + 1) as u16,
                outs: 0,
                tries: 0,
                try_regions: vec![],
                instructions: vec![((receiver as u16) << 8) | 0x27],
                offset: 0,
            }),
        }],
    }
}
fn render(class: &DexClass) -> anyhow::Result<String> {
    if class.symbols.hierarchy.get().is_none() {
        class
            .symbols
            .hierarchy
            .set(Arc::new(TypeHierarchy::from_classes([class])?))
            .ok();
    }
    Ok(native_java::render_method("sample.Thrower", class, &class.methods[0])?.source)
}

fn with_loaded_parent(parent_throws: &[&str], parent_flags: u32) -> DexClass {
    let class = fixture(1, "Lsample/Base;", "Ljava/io/IOException;");
    let mut parent = fixture(parent_flags, "Ljava/lang/Object;", "Ljava/io/IOException;");
    parent.descriptor = "Lsample/Base;".into();
    parent.access_flags = 1;
    parent.methods[0].declaring_type = parent.descriptor.clone();
    parent.methods[0].thrown_types = parent_throws.iter().map(|ty| Arc::from(*ty)).collect();
    parent.methods[0].code = None;
    class
        .symbols
        .hierarchy
        .set(Arc::new(
            TypeHierarchy::from_classes([&class, &parent]).unwrap(),
        ))
        .unwrap();
    class
}

#[test]
fn loaded_override_family_allows_only_checked_subtypes_of_every_contract() {
    for contract in ["Ljava/io/IOException;", "Ljava/lang/Exception;"] {
        let class = with_loaded_parent(&[contract], 0x401);
        let source = render(&class).unwrap();
        assert!(source.contains("throws java.io.IOException"), "{source}");
    }
    for contract in ["Ljava/io/FileNotFoundException;", ""] {
        let types = if contract.is_empty() {
            vec![]
        } else {
            vec![contract]
        };
        let class = with_loaded_parent(&types, 0x401);
        assert!(render(&class).is_err(), "parent contract {contract}");
    }
    let class = with_loaded_parent(&[], 2);
    assert!(
        render(&class).is_ok(),
        "private parent method is not overridden"
    );
}

#[test]
fn missing_parent_cycle_and_conflicting_interfaces_reject_widening() {
    let unknown = fixture(1, "Lsample/Missing;", "Ljava/io/IOException;");
    assert!(render(&unknown).is_err());

    let mut cycle_parent = fixture(0x401, "Lsample/Thrower;", "Ljava/io/IOException;");
    cycle_parent.descriptor = "Lsample/Base;".into();
    cycle_parent.methods[0].declaring_type = cycle_parent.descriptor.clone();
    cycle_parent.methods[0].thrown_types = vec!["Ljava/io/IOException;".into()];
    cycle_parent.methods[0].code = None;
    let cycle = fixture(1, "Lsample/Base;", "Ljava/io/IOException;");
    cycle
        .symbols
        .hierarchy
        .set(Arc::new(
            TypeHierarchy::from_classes([&cycle, &cycle_parent]).unwrap(),
        ))
        .unwrap();
    assert!(render(&cycle).is_err());

    let mut class = fixture(1, "Ljava/lang/Object;", "Ljava/io/IOException;");
    class.interfaces = vec!["Lsample/Wide;".into(), "Lsample/Narrow;".into()];
    let mut wide = fixture(0x401, "Ljava/lang/Object;", "Ljava/io/IOException;");
    wide.descriptor = "Lsample/Wide;".into();
    wide.access_flags = 0x601;
    wide.methods[0].declaring_type = wide.descriptor.clone();
    wide.methods[0].thrown_types = vec!["Ljava/lang/Exception;".into()];
    wide.methods[0].code = None;
    let mut narrow = fixture(0x401, "Ljava/lang/Object;", "Ljava/io/IOException;");
    narrow.descriptor = "Lsample/Narrow;".into();
    narrow.access_flags = 0x601;
    narrow.methods[0].declaring_type = narrow.descriptor.clone();
    narrow.methods[0].thrown_types.clear();
    narrow.methods[0].code = None;
    class
        .symbols
        .hierarchy
        .set(Arc::new(
            TypeHierarchy::from_classes([&class, &wide, &narrow]).unwrap(),
        ))
        .unwrap();
    assert!(render(&class).is_err());
}

#[test]
fn static_checked_declaration_ignores_unrelated_interface_family() {
    let mut class = fixture(9, "Ljava/lang/Object;", "Ljava/io/IOException;");
    class.interfaces.push("Lsample/ExternalInterface;".into());
    class
        .symbols
        .hierarchy
        .set(Arc::new(TypeHierarchy::from_classes([&class]).unwrap()))
        .unwrap();
    let source = render(&class).unwrap();
    assert!(source.contains("throws java.io.IOException"), "{source}");

    let mut instance = fixture(1, "Ljava/lang/Object;", "Ljava/io/IOException;");
    instance
        .interfaces
        .push("Lsample/ExternalInterface;".into());
    instance
        .symbols
        .hierarchy
        .set(Arc::new(TypeHierarchy::from_classes([&instance]).unwrap()))
        .unwrap();
    assert!(render(&instance).is_err());
}

#[test]
fn unprinted_inferred_parent_cannot_authorize_child_checked_throw() {
    let exception = "Ljava/security/NoSuchAlgorithmException;";
    let class = fixture(1, "Lsample/Base;", exception);
    let mut parent = fixture(1, "Ljava/lang/Object;", exception);
    parent.descriptor = "Lsample/Base;".into();
    parent.access_flags = 1;
    parent.methods[0].declaring_type = parent.descriptor.clone();
    parent.interfaces = vec![
        "Ljava/io/Serializable;".into(),
        "Ljava/lang/Comparable;".into(),
    ];
    parent.methods[0].code.as_mut().unwrap().instructions =
        vec![0x001a, 0, 0x1071, 0, 0, 0x000c, 0x000e];
    parent.methods[0].code.as_mut().unwrap().registers = 3;
    parent.methods[0].code.as_mut().unwrap().ins = 2;
    let symbols = Arc::get_mut(&mut parent.symbols).unwrap();
    symbols.types = vec!["Ljava/security/MessageDigest;".into()];
    symbols.strings = vec!["SHA-256".into(), "getInstance".into()];
    symbols.protos = vec![(
        "Ljava/security/MessageDigest;".into(),
        vec!["Ljava/lang/String;".into()],
    )];
    symbols.methods = vec![(0, 0, 1)];
    class
        .symbols
        .hierarchy
        .set(Arc::new(
            TypeHierarchy::from_classes([&class, &parent]).unwrap(),
        ))
        .unwrap();
    assert!(
        render(&class).is_err(),
        "nonfinal parent cannot publish an inferred contract"
    );

    let unknown_interface = fixture(1, "Lsample/Base;", exception);
    parent.interfaces.push("Lsample/Unknown;".into());
    unknown_interface
        .symbols
        .hierarchy
        .set(Arc::new(
            TypeHierarchy::from_classes([&unknown_interface, &parent]).unwrap(),
        ))
        .unwrap();
    assert!(render(&unknown_interface).is_err());
}
#[test]
fn checked_explicit_throws_are_inferred_only_for_unconstrained_declarations() {
    for flags in [9, 2] {
        let source = render(&fixture(
            flags,
            "Ljava/lang/Object;",
            "Ljava/io/IOException;",
        ))
        .unwrap();
        assert!(source.contains("throws java.io.IOException"), "{source}");
        assert!(source.contains("throw p0;"), "{source}");
    }
    let root_instance = render(&fixture(1, "Ljava/lang/Object;", "Ljava/io/IOException;"))
        .expect("new Object-root method has no inherited checked contract");
    assert!(root_instance.contains("throws java.io.IOException"));
    assert!(
        render(&fixture(1, "Lsample/Base;", "Ljava/io/IOException;")).is_err(),
        "unknown instance override contract"
    );
    assert!(
        render(&fixture(9, "Lsample/Base;", "Ljava/io/IOException;")).is_err(),
        "unknown static hiding contract"
    );
    let private_source = render(&fixture(2, "Lsample/Base;", "Ljava/io/IOException;"))
        .expect("private methods cannot override a superclass method");
    assert!(
        private_source.contains("throws java.io.IOException"),
        "{private_source}"
    );
    assert!(
        render(&fixture(9, "Ljava/lang/Object;", "Ljava/lang/Object;")).is_err(),
        "unproven throwable"
    );
    let unchecked = render(&fixture(
        9,
        "Ljava/lang/Object;",
        "Ljava/lang/RuntimeException;",
    ))
    .unwrap();
    assert!(!unchecked.contains(" throws "), "{unchecked}");
}

#[test]
fn nonprivate_checked_inference_requires_a_caller_index() {
    for flags in [1, 9] {
        let class = fixture(flags, "Ljava/lang/Object;", "Ljava/io/IOException;");
        assert!(
            native_java::render_method("sample.Thrower", &class, &class.methods[0]).is_err(),
            "missing hierarchy must not infer nonprivate checked throws"
        );
    }
    let mut nonfinal = fixture(1, "Ljava/lang/Object;", "Ljava/io/IOException;");
    nonfinal.access_flags = 1;
    assert!(
        render(&nonfinal).is_err(),
        "nonfinal virtual alias is unproven"
    );
}
#[test]
fn locally_handled_checked_throw_does_not_change_override_contract() {
    let mut class = fixture(1, "Ljava/lang/Object;", "Ljava/io/IOException;");
    let code = class.methods[0].code.as_mut().unwrap();
    code.instructions = vec![0x0127, 0x000d, 0x000e];
    code.tries = 1;
    code.try_regions = vec![DexTryRegion {
        start: 0,
        end: 1,
        catches: vec![(Some("Ljava/io/IOException;".into()), 1)].into(),
    }];
    let source = render(&class).unwrap();
    assert!(!source.contains(" throws "), "{source}");
    assert!(source.contains("throw p0;"), "{source}");
}
#[test]
#[ignore = "requires javac and java"]
fn inferred_static_and_private_declarations_compile_and_preserve_exception_identity() {
    use std::process::Command;
    for (flags, superclass) in [
        (9, "Ljava/lang/Object;"),
        (2, "Ljava/lang/Object;"),
        (2, "Lsample/Base;"),
    ] {
        let source = render(&fixture(flags, superclass, "Ljava/io/IOException;")).unwrap();
        let dir = std::env::temp_dir().join(format!(
            "rdx-inferred-throws-{}-{flags}-{}",
            std::process::id(),
            if superclass == "Lsample/Base;" {
                "base"
            } else {
                "object"
            }
        ));
        std::fs::create_dir_all(dir.join("sample")).unwrap();
        let call = if flags & 8 != 0 {
            "test(expected)"
        } else {
            "new Thrower().test(expected)"
        };
        if superclass == "Lsample/Base;" {
            std::fs::write(
                dir.join("sample/Base.java"),
                "package sample; public class Base { private void test(java.io.IOException e) {} }",
            )
            .unwrap();
        }
        let extends = if superclass == "Lsample/Base;" {
            "extends Base"
        } else {
            ""
        };
        std::fs::write(dir.join("sample/Thrower.java"),format!(r#"package sample;
public class Thrower {extends} {{ {source}
 public static void main(String[] args){{java.io.IOException expected=new java.io.IOException("same");
  try{{{call};throw new AssertionError("not thrown");}}catch(java.io.IOException actual){{if(actual!=expected)throw new AssertionError("identity");}}
 }}
}}"#)).unwrap();
        let javac_sources = if superclass == "Lsample/Base;" {
            vec!["sample/Thrower.java", "sample/Base.java"]
        } else {
            vec!["sample/Thrower.java"]
        };
        for (command, args) in [
            ("javac", javac_sources),
            ("java", vec!["-cp", ".", "sample.Thrower"]),
        ] {
            let result = Command::new(command)
                .args(args)
                .current_dir(&dir)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{command}: {}",
                String::from_utf8_lossy(&result.stderr)
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[test]
#[ignore = "requires javac and java"]
fn loaded_override_checked_contract_compiles_and_preserves_exception_identity() {
    use std::process::Command;
    let class = with_loaded_parent(&["Ljava/io/IOException;"], 0x401);
    let source = render(&class).unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-loaded-override-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("sample")).unwrap();
    std::fs::write(
        dir.join("sample/Base.java"),
        "package sample; public abstract class Base { public abstract void test(java.io.IOException e) throws java.io.IOException; }",
    )
    .unwrap();
    std::fs::write(
        dir.join("sample/Thrower.java"),
        format!(r#"package sample;
public class Thrower extends Base {{ {source}
 public static void main(String[] args) {{
  java.io.IOException expected = new java.io.IOException("same");
  try {{ new Thrower().test(expected); throw new AssertionError("not thrown"); }}
  catch (java.io.IOException actual) {{ if (actual != expected) throw new AssertionError("identity"); }}
 }}
}}"#),
    )
    .unwrap();
    for (program, args) in [
        ("javac", vec!["sample/Base.java", "sample/Thrower.java"]),
        ("java", vec!["-cp", ".", "sample.Thrower"]),
    ] {
        let result = Command::new(program)
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{program}: {}\n{source}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}
