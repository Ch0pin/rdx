use rdx::{
    native_dex::{DexClass, DexCode, DexField, DexMethod, DexSymbols},
    native_java,
};
use std::sync::Arc;

fn fixture(instructions: Vec<u16>) -> DexClass {
    DexClass {
        descriptor: "Lsample/Init;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        symbols: Arc::new(DexSymbols {
            strings: vec!["count".into()],
            types: vec!["Lsample/Init;".into(), "I".into()],
            fields: vec![(0, 1, 0)],
            ..Default::default()
        }),
        fields: vec![DexField {
            declaring_type: "Lsample/Init;".into(),
            name: "count".into(),
            field_type: "I".into(),
            access_flags: 9,
            is_static: true,
        }],
        methods: vec![DexMethod {
            declaring_type: "Lsample/Init;".into(),
            name: "<clinit>".into(),
            return_type: "V".into(),
            parameters: vec![],
            thrown_types: vec![],
            access_flags: 0x10008,
            code: Some(DexCode {
                registers: 1,
                ins: 0,
                outs: 0,
                tries: 0,
                try_regions: vec![],
                instructions,
                offset: 0,
            }),
        }],
    }
}

#[test]
fn initializer_block_omits_only_terminal_return_and_preserves_exact_links() {
    let class = fixture(vec![0x1012, 0x0067, 0, 0x000e]);
    let code = native_java::render("sample.Init", &class).unwrap();
    assert!(
        code.source
            .contains("    static {\n        Init.count = 1;\n    }"),
        "{}",
        code.source
    );
    assert!(!code.source.contains("return;"));
    assert!(!code.source.contains("void <clinit>"));
    let declaration = code
        .definitions
        .iter()
        .find(|d| d.name == "<clinit>")
        .unwrap();
    let span = |start: usize, end: usize| {
        code.source
            .chars()
            .skip(start)
            .take(end - start)
            .collect::<String>()
    };
    assert_eq!(span(declaration.start, declaration.end), "static");
    assert!(code.links.iter().any(|link| link.start == declaration.start
        && link.end == declaration.end
        && link.label == "sample.Init.<clinit>()V"));
    let field_links: Vec<_> = code
        .links
        .iter()
        .filter(|link| link.label == "sample.Init.count:I")
        .collect();
    assert_eq!(field_links.len(), 2); // declaration and the one write
    assert!(
        field_links
            .iter()
            .all(|link| span(link.start, link.end) == "count")
    );
    assert_eq!(code.source_hash, rdx::engine::source_identity(&code.source));
}

#[test]
fn initializer_allows_branches_that_share_one_terminal_return() {
    // Both arms assign a register, then the shared tail performs one static write.
    let class = fixture(vec![
        0x0012, 0x0038, 4, 0x1012, 0x0228, 0x2012, 0x0067, 0, 0x000e,
    ]);
    let code = native_java::render_method("sample.Init", &class, &class.methods[0]).unwrap();
    assert!(code.source.contains("if ("));
    assert_eq!(code.source.matches("sample.Init.count =").count(), 1);
    assert!(!code.source.contains("return;"));
}

#[test]
fn initializer_rejects_early_return_regions_and_invalid_signatures() {
    let class = fixture(vec![0x0012, 0x0038, 3, 0x000e, 0x000e]);
    assert!(native_java::render_method("sample.Init", &class, &class.methods[0]).is_err());
    for case in 0..7 {
        let mut class = fixture(vec![0x000e]);
        let method = &mut class.methods[0];
        match case {
            0 => method.access_flags = 0x10000,
            1 => method.return_type = "I".into(),
            2 => method.parameters.push("I".into()),
            3 => method.code = None,
            4 => method.access_flags |= 0x100,
            5 => method.access_flags |= 3,
            _ => method.declaring_type = "Lother/Class;".into(),
        }
        assert!(
            native_java::render_method("sample.Init", &class, &class.methods[0]).is_err(),
            "case {case}"
        );
    }
}

#[test]
fn empty_static_initializer_is_an_empty_java_block() {
    let class = fixture(vec![0x000e]);
    let code = native_java::render_method("sample.Init", &class, &class.methods[0]).unwrap();
    assert_eq!(code.source, "    static {\n    }\n");
}

#[test]
fn initializer_visibility_metadata_does_not_change_body_or_links() {
    let class = fixture(vec![0x1012, 0x0067, 0, 0x000e]);
    let expected = native_java::render_method("sample.Init", &class, &class.methods[0]).unwrap();
    for visibility in [1, 2, 4] {
        let mut visible = fixture(vec![0x1012, 0x0067, 0, 0x000e]);
        visible.methods[0].access_flags |= visibility;
        let actual =
            native_java::render_method("sample.Init", &visible, &visible.methods[0]).unwrap();
        assert_eq!(actual.source, expected.source);
        assert_eq!(actual.source_hash, expected.source_hash);
        let links = |code: &rdx::engine::DecompiledCode| {
            code.links
                .iter()
                .map(|link| (link.start, link.end, link.label.clone()))
                .collect::<Vec<_>>()
        };
        assert_eq!(links(&actual), links(&expected));
        assert!(actual.source.contains("sample.Init.count = 1;"));
    }
}

#[test]
fn initializer_own_field_does_not_shadow_class_qualifier() {
    let mut class = fixture(vec![0x1012, 0x0067, 0, 0x000e]);
    Arc::get_mut(&mut class.symbols).unwrap().strings[0] = "Init".into();
    class.fields[0].name = "Init".into();
    class.methods[0].access_flags |= 1;
    let code = native_java::render("sample.Init", &class).unwrap();
    assert!(code.source.contains("        Init = 1;"), "{}", code.source);
    assert!(!code.source.contains("Init.Init ="));
    let link = code.links.iter().find(|link| {
        link.label == "sample.Init.Init:I"
            && code
                .source
                .chars()
                .skip(link.start)
                .take(link.end - link.start)
                .collect::<String>()
                == "Init"
    });
    assert!(link.is_some());
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn initializer_shadowed_class_field_compiles_and_initializes_once() {
    use std::{fs, process::Command};
    let mut class = fixture(vec![0x1012, 0x0067, 0, 0x000e]);
    Arc::get_mut(&mut class.symbols).unwrap().strings[0] = "Init".into();
    class.fields[0].name = "Init".into();
    class.methods[0].access_flags |= 1;
    let source = native_java::render("sample.Init", &class).unwrap().source;
    let dir = std::env::temp_dir().join(format!("rdx-clinit-visibility-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(dir.join("sample/Init.java"), source).unwrap();
    fs::write(
        dir.join("sample/Check.java"),
        r#"package sample;
public class Check {
 public static void main(String[] args) throws Exception {
  Class<?> first = Class.forName("sample.Init");
  Class<?> second = Class.forName("sample.Init");
  if (first != second || first.getField("Init").getInt(null) != 1) throw new AssertionError();
 }
}"#,
    )
    .unwrap();
    for (program, args) in [
        ("javac", vec!["sample/Init.java", "sample/Check.java"]),
        ("java", vec!["sample.Check"]),
    ] {
        let output = Command::new(program)
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn initializer_package_prefix_collision_compiles() {
    use std::{fs, process::Command};
    let mut class = fixture(vec![0x1012, 0x0067, 0, 0x000e]);
    Arc::get_mut(&mut class.symbols).unwrap().strings[0] = "sample".into();
    class.fields[0].name = "sample".into();
    let code = native_java::render("sample.Init", &class).unwrap();
    assert!(
        code.source.contains("        sample = 1;"),
        "{}",
        code.source
    );
    let dir = std::env::temp_dir().join(format!("rdx-clinit-package-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(dir.join("sample/Init.java"), code.source).unwrap();
    let output = Command::new("javac")
        .arg("sample/Init.java")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore = "requires javac on PATH"]
fn initializer_blank_final_write_compiles_with_simple_name() {
    use std::{fs, process::Command};
    let mut class = fixture(vec![0x1012, 0x0067, 0, 0x000e]);
    class.fields[0].access_flags = 0x19;
    let body = native_java::render_method("sample.Init", &class, &class.methods[0])
        .unwrap()
        .source;
    assert!(body.contains("        count = 1;"), "{body}");
    assert!(!body.contains("Init.count ="));

    // A blank final has no encoded DEX initializer. Compile the reconstructed
    // method inside its equivalent Java declaration to check assignment rules.
    let source = format!(
        "package sample;\npublic class Init {{\n    public static final int count;\n{body}}}\n"
    );
    let dir = std::env::temp_dir().join(format!("rdx-clinit-blank-final-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(dir.join("sample/Init.java"), source).unwrap();
    let output = Command::new("javac")
        .arg("sample/Init.java")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore = "requires javac on PATH"]
fn initializer_digit_field_without_try_compiles_when_class_name_collides() {
    use std::{fs, process::Command};
    let mut class = fixture(vec![0x1012, 0x0067, 0, 0x000e]);
    class.descriptor = "Lsample/e0;".into();
    class.fields[0].declaring_type = class.descriptor.clone();
    class.fields[0].name = "e0".into();
    class.methods[0].declaring_type = class.descriptor.clone();
    Arc::get_mut(&mut class.symbols).unwrap().types[0] = class.descriptor.clone();
    Arc::get_mut(&mut class.symbols).unwrap().strings[0] = "e0".into();
    assert!(
        class.methods[0]
            .code
            .as_ref()
            .unwrap()
            .try_regions
            .is_empty()
    );
    let body = native_java::render_method("sample.e0", &class, &class.methods[0])
        .unwrap()
        .source;
    assert!(body.contains("        e0 = 1;"), "{body}");
    assert!(!body.contains("e0.e0 ="));

    let source =
        format!("package sample;\npublic class e0 {{\n    public static int e0;\n{body}}}\n");
    let dir = std::env::temp_dir().join(format!("rdx-clinit-digit-field-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(dir.join("sample/e0.java"), source).unwrap();
    let output = Command::new("javac")
        .arg("sample/e0.java")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn initializer_field_matching_generated_local_reserves_a_distinct_local() {
    // The separate Init field requires a bare write; reserve v0 for the field.
    let mut class = fixture(vec![0x0060, 0, 0x0067, 0, 0x000e]);
    Arc::get_mut(&mut class.symbols).unwrap().strings = vec!["v0".into(), "Init".into()];
    Arc::get_mut(&mut class.symbols).unwrap().fields = vec![(0, 1, 0), (0, 1, 1)];
    class.fields[0].name = "v0".into();
    class.fields.push(DexField {
        declaring_type: class.descriptor.clone(),
        name: "Init".into(),
        field_type: "I".into(),
        access_flags: 9,
        is_static: true,
    });
    let body = native_java::render_method("sample.Init", &class, &class.methods[0]).unwrap();
    assert!(
        body.source.contains("int v1 = sample.Init.v0;"),
        "{}",
        body.source
    );
    assert!(body.source.contains("v0 = v1;"), "{}", body.source);
    assert!(!body.source.contains("int v0 ="));
    assert_eq!(
        body.links
            .iter()
            .filter(|link| link.label == "sample.Init.v0:I")
            .count(),
        2
    );
}
