use rdx::{
    native_dex::{DexClass, DexCode, DexField, DexMethod, DexSymbols},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::sync::Arc;

fn fixture() -> (DexClass, DexClass) {
    let parent = DexClass {
        descriptor: "Lsample/Parent$Base;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 0x401,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![],
        symbols: Arc::new(DexSymbols::default()),
    };
    let mut child = DexClass {
        descriptor: "Lsample/Child;".into(),
        superclass: Some(parent.descriptor.clone()),
        interfaces: vec![],
        access_flags: 0x11,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![],
        symbols: Arc::new(DexSymbols {
            types: vec![
                "Ljava/lang/Object;".into(),
                "Lsample/Child;".into(),
                "Ljava/lang/String;".into(),
            ],
            strings: vec!["<init>".into(), "text".into()],
            protos: vec![("V".into(), vec![])],
            methods: vec![(0, 0, 0)],
            fields: vec![(1, 2, 1)],
            ..Default::default()
        }),
    };
    child.fields.push(DexField {
        declaring_type: child.descriptor.clone(),
        name: "text".into(),
        field_type: "Ljava/lang/String;".into(),
        access_flags: 0x11,
        is_static: false,
    });
    child.methods.push(DexMethod {
        declaring_type: child.descriptor.clone(),
        name: "<init>".into(),
        return_type: "V".into(),
        parameters: vec!["Ljava/lang/String;".into()],
        thrown_types: vec![],
        access_flags: 0x10001,
        code: Some(DexCode {
            registers: 2,
            ins: 2,
            outs: 1,
            tries: 0,
            try_regions: vec![],
            offset: 0,
            instructions: vec![0x1070, 0, 0, 0x015b, 0, 0x000e],
        }),
    });
    (parent, child)
}

#[test]
fn flat_fieldless_parent_recovers_observed_zero_argument_super() {
    for reverse in [false, true] {
        let (parent, child) = fixture();
        let classes = if reverse {
            vec![&child, &parent]
        } else {
            vec![&parent, &child]
        };
        let h = Arc::new(TypeHierarchy::from_classes(classes.iter().copied()).unwrap());
        assert!(h.equivalent_super_constructor(&child.descriptor, "Ljava/lang/Object;", &[]));
        for c in classes {
            c.symbols.hierarchy.set(h.clone()).unwrap();
        }
        let source = native_java::render_method("sample.Child", &child, &child.methods[0])
            .unwrap()
            .source;
        assert!(source.contains("super();"), "{source}");
        assert!(source.contains("this.text ="), "{source}");
        assert!(!source.contains(".method"), "{source}");
    }
}

#[test]
fn captured_final_or_incomplete_parent_declarations_remain_unproved() {
    for mutation in 0..3 {
        let (mut parent, child) = fixture();
        match mutation {
            0 => parent.fields.push(DexField {
                declaring_type: parent.descriptor.clone(),
                name: "capture".into(),
                field_type: "Ljava/lang/Object;".into(),
                access_flags: 0x1000,
                is_static: false,
            }),
            1 => parent.fields.push(DexField {
                declaring_type: parent.descriptor.clone(),
                name: "state".into(),
                field_type: "I".into(),
                access_flags: 0x10,
                is_static: false,
            }),
            _ => parent.annotations_offset = 1,
        }
        let h = TypeHierarchy::from_classes([&parent, &child]).unwrap();
        assert!(!h.equivalent_super_constructor(&child.descriptor, "Ljava/lang/Object;", &[]));
    }
}

#[test]
#[ignore = "root runs javac/java serially with RDX_FLAT_SUPER_JVM_DIR"]
fn flat_erased_super_generated_classes_preserve_reference_identity() {
    use std::{fs, path::PathBuf, process::Command};
    let base =
        PathBuf::from(std::env::var_os("RDX_FLAT_SUPER_JVM_DIR").expect("RDX_FLAT_SUPER_JVM_DIR"));
    let package = base.join("sample");
    fs::create_dir_all(&package).unwrap();
    let (parent, child) = fixture();
    let h = Arc::new(TypeHierarchy::from_classes([&parent, &child]).unwrap());
    for c in [&parent, &child] {
        c.symbols.hierarchy.set(h.clone()).unwrap();
    }
    for (name, c) in [("Parent$Base", &parent), ("Child", &child)] {
        let source = if name == "Child" {
            let method = native_java::render_method("sample.Child", c, &c.methods[0])
                .unwrap()
                .source;
            format!(
                "package sample; public final class Child extends Parent$Base {{ public final String text; {method} }}"
            )
        } else {
            native_java::render(&format!("sample.{name}"), c)
                .unwrap()
                .source
        };
        fs::write(package.join(format!("{name}.java")), source).unwrap();
    }
    // Independent interpretation of this fixture's literal DEX: Object.init
    // has no user effects; iput-object stores the exact incoming reference.
    let words = &child.methods[0].code.as_ref().unwrap().instructions;
    assert_eq!(words, &[0x1070, 0, 0, 0x015b, 0, 0x000e]);
    fs::write(
        package.join("Check.java"),
        r#"package sample;
public final class Check {
    public static void main(String[] args) {
        String[] inputs = { null, "", new String("identity"), "repeated" };
        for (String input : inputs) {
            Child actual = new Child(input);
            if (actual.text != input) throw new AssertionError("reference identity");
            if (!(actual instanceof Parent$Base)) throw new AssertionError("superclass");
        }
        System.out.println("4 literal DEX comparisons passed");
    }
}
"#,
    )
    .unwrap();
    let javac = Command::new("javac")
        .arg("-d")
        .arg(&base)
        .arg(package.join("Parent$Base.java"))
        .arg(package.join("Child.java"))
        .arg(package.join("Check.java"))
        .output()
        .unwrap();
    assert!(
        javac.status.success(),
        "{}",
        String::from_utf8_lossy(&javac.stderr)
    );
    let java = Command::new("java")
        .arg("-cp")
        .arg(&base)
        .arg("sample.Check")
        .output()
        .unwrap();
    assert!(
        java.status.success(),
        "{}",
        String::from_utf8_lossy(&java.stderr)
    );
    assert!(String::from_utf8_lossy(&java.stdout).contains("4 literal DEX comparisons passed"));
}
