use rdx::native_dex::{
    DexAnnotation, DexAnnotationDirectory, DexClass, DexCode, DexMethod, DexSymbols, DexValue,
};
use rdx::native_java;
use std::{collections::BTreeMap, sync::Arc};

fn set(annotation: DexAnnotation) -> Arc<[Arc<DexAnnotation>]> {
    Arc::from(vec![Arc::new(annotation)])
}

fn span(source: &str, start: usize, end: usize) -> String {
    source.chars().skip(start).take(end - start).collect()
}

fn annotated_class() -> DexClass {
    let marker = set(DexAnnotation {
        visibility: 1,
        type_idx: 3,
        elements: vec![],
    });
    let values = set(DexAnnotation {
        visibility: 1,
        type_idx: 4,
        elements: vec![
            (1, DexValue::String(2)),
            (5, DexValue::Boolean(true)),
            (6, DexValue::Array(vec![DexValue::Int(1), DexValue::Int(2)])),
            (7, DexValue::Char(10)),
        ],
    });
    let parameter: Arc<[Arc<DexAnnotation>]> = Arc::from(vec![
        Arc::new(DexAnnotation {
            visibility: 0,
            type_idx: 6,
            elements: vec![],
        }),
        Arc::new(DexAnnotation {
            visibility: 1,
            type_idx: 7,
            elements: vec![(1, DexValue::MethodHandle(0))],
        }),
    ]);
    let directory = DexAnnotationDirectory {
        class: Some(values),
        fields: vec![],
        methods: vec![Some(marker)],
        parameters: vec![vec![Some(parameter)]],
    };
    DexClass {
        symbols: Arc::new(DexSymbols {
            strings: vec![
                "unused".into(),
                "value".into(),
                "bridge".into(),
                "Landroid/webkit/JavascriptInterface;".into(),
                "Lsample/Config;".into(),
                "enabled".into(),
                "numbers".into(),
                "newline".into(),
                "Lsample/Parameter;".into(),
                "Lsample/Unsupported;".into(),
            ],
            types: vec![
                "V".into(),
                "Lsample/Target;".into(),
                "Ljava/lang/Object;".into(),
                "Landroid/webkit/JavascriptInterface;".into(),
                "Lsample/Config;".into(),
                "I".into(),
                "Lsample/Parameter;".into(),
                "Lsample/Unsupported;".into(),
                "Ljava/lang/String;".into(),
                "Lsample/Parameter;".into(),
            ],
            annotations: BTreeMap::from([(1, Arc::new(directory))]),
            ..Default::default()
        }),
        descriptor: "Lsample/Target;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 1,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![DexMethod {
            declaring_type: "Lsample/Target;".into(),
            name: "send".into(),
            return_type: "V".into(),
            parameters: vec!["I".into()],
            thrown_types: vec![],
            access_flags: 1,
            code: Some(DexCode {
                registers: 2,
                ins: 2,
                outs: 0,
                tries: 0,
                try_regions: vec![],
                instructions: vec![0x000e],
                offset: 1,
            }),
        }],
    }
}

#[test]
fn renders_class_method_parameter_annotations_values_imports_and_spans() {
    let class = annotated_class();
    let code = native_java::render("sample.Target", &class).unwrap();
    assert!(
        code.source
            .contains("import android.webkit.JavascriptInterface;"),
        "{}",
        code.source
    );
    assert!(
        code.source.contains("@JavascriptInterface\n"),
        "{}",
        code.source
    );
    assert!(code.source.contains("public void send("), "{}", code.source);
    assert!(code.source.contains("@Parameter "), "{}", code.source);
    assert!(
        code.source
            .contains("DEX parameter annotations are retained"),
        "{}",
        code.source
    );
    assert!(
        code.source.contains(
            "DEX annotation retained but cannot be expressed as Java: @sample.Unsupported"
        ),
        "{}",
        code.source
    );
    assert!(code.source.contains("newline = '\\n'"), "{}", code.source);
    assert!(code.source.contains("numbers = {1, 2}"), "{}", code.source);
    let method = code
        .definitions
        .iter()
        .find(|definition| definition.kind == "method")
        .unwrap();
    assert_eq!(span(&code.source, method.start, method.end), "send");
    let link = code
        .links
        .iter()
        .find(|link| link.label == "android.webkit.JavascriptInterface")
        .unwrap();
    assert_eq!(
        span(&code.source, link.start, link.end),
        "JavascriptInterface"
    );
}

#[test]
fn aliased_annotation_names_keep_original_navigation_identity() {
    let mut class = annotated_class();
    Arc::get_mut(&mut class.symbols).unwrap().types[3] = "Lsample/Bad-Marker;".into();
    let code = native_java::render("sample.Target", &class).unwrap();
    let link = code
        .links
        .iter()
        .find(|link| link.label == "sample.Bad-Marker")
        .unwrap();
    assert_eq!(
        span(&code.source, link.start, link.end),
        "_rdx_4261642d4d61726b6572"
    );
    assert!(!code.source.contains("@Bad-Marker"));
}

fn annotation_type() -> DexClass {
    let mut c = annotated_class();
    c.descriptor = "Lsample/Url;".into();
    c.access_flags = 0x2601;
    c.interfaces = vec!["Ljava/lang/annotation/Annotation;".into()];
    c.methods.clear();
    c.annotations_offset = 0;
    c
}

#[test]
fn marker_annotation_is_java_declaration_with_navigation() {
    let c = annotation_type();
    let code = native_java::render("sample.Url", &c).unwrap();
    assert!(
        code.source.contains("public @interface Url {"),
        "{}",
        code.source
    );
    assert!(
        !code
            .source
            .contains("extends java.lang.annotation.Annotation")
    );
    let def = code.definitions.iter().find(|d| d.kind == "class").unwrap();
    assert_eq!(span(&code.source, def.start, def.end), "Url");
}

fn default_annotation_type() -> DexClass {
    let mut c = annotation_type();
    c.methods.push(DexMethod {
        declaring_type: c.descriptor.clone(),
        name: "value".into(),
        return_type: "Ljava/lang/String;".into(),
        parameters: vec![],
        thrown_types: vec![],
        access_flags: 0x401,
        code: None,
    });
    let symbols = Arc::get_mut(&mut c.symbols).unwrap();
    let own = symbols.types.len() as u32;
    symbols.types.push(c.descriptor.clone());
    let default = symbols.types.len() as u32;
    symbols
        .types
        .push("Ldalvik/annotation/AnnotationDefault;".into());
    symbols.annotations.insert(
        2,
        Arc::new(DexAnnotationDirectory {
            class: Some(set(DexAnnotation {
                visibility: 2,
                type_idx: default,
                elements: vec![(
                    1,
                    DexValue::Annotation {
                        type_idx: own,
                        elements: vec![(1, DexValue::String(2))],
                    },
                )],
            })),
            fields: vec![],
            methods: vec![],
            parameters: vec![],
        }),
    );
    c.annotations_offset = 2;
    c
}

#[test]
fn annotation_element_default_is_preserved_and_invalid_shapes_decline() {
    let c = default_annotation_type();
    let code = native_java::render("sample.Url", &c).unwrap();
    assert!(
        code.source.contains("String value() default \"bridge\";"),
        "{}",
        code.source
    );
    let mut invalid = default_annotation_type();
    invalid.methods[0].parameters.push("I".into());
    assert!(native_java::render("sample.Url", &invalid).is_err());
    let mut invalid = default_annotation_type();
    invalid.methods[0].name = "other".into();
    assert!(native_java::render("sample.Url", &invalid).is_err());
    let mut invalid = default_annotation_type();
    invalid.methods[0].return_type = "I".into();
    assert!(native_java::render("sample.Url", &invalid).is_err());
    let mut invalid = c;
    invalid.interfaces.push("Ljava/lang/Runnable;".into());
    assert!(native_java::render("sample.Url", &invalid).is_err());
}

#[test]
fn abstract_api_parameter_annotations_survive_and_link_to_declaration() {
    let mut c = annotated_class();
    c.access_flags = 0x601;
    c.methods[0].access_flags = 0x401;
    c.methods[0].code = None;
    let symbols = Arc::get_mut(&mut c.symbols).unwrap();
    symbols.types[6] = "Lsample/Url;".into();
    let code = native_java::render("sample.Target", &c).unwrap();
    assert!(code.source.contains("@Url "), "{}", code.source);
    let link = code.links.iter().find(|l| l.label == "sample.Url").unwrap();
    assert_eq!(span(&code.source, link.start, link.end), "Url");
}

#[test]
#[ignore = "requires javac and java"]
fn annotation_defaults_compile_and_are_visible_to_reflection() {
    use std::{fs, process::Command};
    let c = default_annotation_type();
    let code = native_java::render("sample.Url", &c).unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-annotation-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(dir.join("sample/Url.java"), code.source).unwrap();
    fs::write(dir.join("sample/Check.java"), "package sample; public class Check {public static void main(String[] args)throws Exception {if(!Url.class.isAnnotation()||!\"bridge\".equals(Url.class.getMethod(\"value\").getDefaultValue())) throw new AssertionError();}}").unwrap();
    let compile = Command::new("javac")
        .current_dir(&dir)
        .args(["sample/Url.java", "sample/Check.java"])
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    assert!(
        Command::new("java")
            .current_dir(&dir)
            .args(["-cp", ".", "sample.Check"])
            .status()
            .unwrap()
            .success()
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore = "requires javac and java"]
fn marker_parameter_metadata_survives_with_original_and_obfuscated_type_names() {
    use std::{fs, process::Command};
    for (name, descriptor) in [("sample.Url", "Lsample/Url;"), ("hidden.a", "Lhidden/a;")] {
        let mut annotation = annotation_type();
        annotation.descriptor = descriptor.into();
        let symbols = Arc::get_mut(&mut annotation.symbols).unwrap();
        let retention = symbols.types.len() as u32;
        symbols
            .types
            .push("Ljava/lang/annotation/Retention;".into());
        let policy = symbols.types.len() as u16;
        symbols
            .types
            .push("Ljava/lang/annotation/RetentionPolicy;".into());
        let runtime = symbols.strings.len() as u32;
        symbols.strings.push("RUNTIME".into());
        let field = symbols.fields.len() as u32;
        symbols.fields.push((policy, policy, runtime));
        symbols.annotations.insert(
            3,
            Arc::new(DexAnnotationDirectory {
                class: Some(set(DexAnnotation {
                    visibility: 1,
                    type_idx: retention,
                    elements: vec![(1, DexValue::Enum(field))],
                })),
                fields: vec![],
                methods: vec![],
                parameters: vec![],
            }),
        );
        annotation.annotations_offset = 3;
        let annotation_code = native_java::render(name, &annotation).unwrap();
        let mut api = annotated_class();
        api.access_flags = 0x601;
        api.methods[0].access_flags = 0x401;
        api.methods[0].code = None;
        let symbols = Arc::get_mut(&mut api.symbols).unwrap();
        symbols.types[6] = descriptor.into();
        symbols.annotations.insert(
            1,
            Arc::new(DexAnnotationDirectory {
                class: None,
                fields: vec![],
                methods: vec![],
                parameters: vec![vec![Some(set(DexAnnotation {
                    visibility: 1,
                    type_idx: 6,
                    elements: vec![],
                }))]],
            }),
        );
        let api_code = native_java::render("sample.Target", &api).unwrap();
        let link = api_code.links.iter().find(|l| l.label == name).unwrap();
        assert_eq!(api_code.source.chars().nth(link.start - 1), Some('@'));
        let dir = std::env::temp_dir().join(format!("rdx-marker-{}", std::process::id()));
        fs::create_dir_all(dir.join("sample")).unwrap();
        fs::create_dir_all(dir.join("hidden")).unwrap();
        let annotation_file = format!("{}.java", name.replace('.', "/"));
        fs::write(dir.join(&annotation_file), annotation_code.source).unwrap();
        fs::write(dir.join("sample/Target.java"), api_code.source).unwrap();
        fs::write(dir.join("sample/Check.java"), format!("package sample; public class Check {{public static void main(String[] args)throws Exception {{ java.lang.annotation.Annotation[][] a=Target.class.getMethod(\"send\",int.class).getParameterAnnotations(); if(a.length!=1||a[0].length!=1||!a[0][0].annotationType().getName().equals(\"{name}\"))throw new AssertionError(); }} }}")).unwrap();
        let compile = Command::new("javac")
            .current_dir(&dir)
            .args([
                annotation_file.as_str(),
                "sample/Target.java",
                "sample/Check.java",
            ])
            .output()
            .unwrap();
        assert!(
            compile.status.success(),
            "{}",
            String::from_utf8_lossy(&compile.stderr)
        );
        assert!(
            Command::new("java")
                .current_dir(&dir)
                .args(["-cp", ".", "sample.Check"])
                .status()
                .unwrap()
                .success()
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
