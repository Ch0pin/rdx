use crate::{
    engine::{CodeDefinition, CodeLink, DecompiledCode},
    native_dex::{DexClass, DexCode, DexField, DexMethod, DexSymbols},
};
use std::sync::Arc;
fn fixture() -> DexClass {
    let ty: Arc<str> = "Lsample/b;".into();
    DexClass {
        descriptor: ty.clone(),
        superclass: Some("Ljava/lang/Object;".into()),
        access_flags: 1,
        interfaces: vec![],
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![
            DexField {
                declaring_type: ty.clone(),
                name: "a".into(),
                field_type: "I".into(),
                access_flags: 9,
                is_static: true,
            },
            DexField {
                declaring_type: ty.clone(),
                name: "b".into(),
                field_type: "[J".into(),
                access_flags: 9,
                is_static: true,
            },
        ],
        methods: vec![
            DexMethod {
                declaring_type: ty.clone(),
                name: "read".into(),
                parameters: vec![],
                return_type: "I".into(),
                thrown_types: vec![],
                access_flags: 9,
                code: Some(DexCode {
                    registers: 1,
                    ins: 0,
                    outs: 0,
                    tries: 0,
                    try_regions: vec![],
                    offset: 0,
                    instructions: vec![0x0060, 0, 0x000f],
                }),
            },
            DexMethod {
                declaring_type: ty,
                name: "b".into(),
                parameters: vec!["I".into()],
                return_type: "I".into(),
                thrown_types: vec![],
                access_flags: 9,
                code: Some(DexCode {
                    registers: 1,
                    ins: 1,
                    outs: 1,
                    tries: 0,
                    try_regions: vec![],
                    offset: 0,
                    instructions: vec![
                        0x003d, 9, 0x00d8, 0xff00, 0x1071, 0, 0, 0x000a, 0x000f, 0x0012, 0x000f,
                    ],
                }),
            },
        ],
        symbols: Arc::new(DexSymbols {
            types: vec!["Lsample/b;".into(), "I".into(), "[J".into()],
            strings: vec!["a".into(), "b".into()],
            fields: vec![(0, 1, 0), (0, 2, 1)],
            methods: vec![(0, 0, 1)],
            protos: vec![("I".into(), vec!["I".into()])],
            ..Default::default()
        }),
    }
}
fn source(text: &str) -> DecompiledCode {
    let byte = text.find("class b").unwrap() + 6;
    let start = text[..byte].chars().count();
    let mut links = vec![];
    for needle in ["sample.b.a", "sample.b.b("] {
        if let Some(at) = text.find(needle) {
            links.push(CodeLink {
                start: text[..at].chars().count(),
                end: text[..at + needle.len() - usize::from(needle.ends_with('('))]
                    .chars()
                    .count(),
                label: if needle.ends_with('(') {
                    "sample.b.b(I)I"
                } else {
                    "sample.b.a:I"
                }
                .into(),
            });
        }
    }
    DecompiledCode {
        source: text.into(),
        links,
        definitions: vec![CodeDefinition {
            start,
            end: start + 1,
            kind: "class".into(),
            name: "sample.b".into(),
        }],
        source_hash: String::new(),
    }
}
#[test]
fn own_field_masks_owner_for_both_field_reads_and_method_calls() {
    let c = fixture();
    let code = crate::native_java::render("sample.b", &c).unwrap();
    assert!(code.source.contains("sample.b.a"), "{}", code.source);
    assert!(code.source.contains("sample.b.b("), "{}", code.source);
    for label in ["sample.b.a:I", "sample.b.b(I)I"] {
        let link = code.links.iter().find(|link| link.label == label).unwrap();
        let linked: String = code
            .source
            .chars()
            .skip(link.start)
            .take(link.end - link.start)
            .collect();
        assert!(
            matches!(linked.as_str(), "a" | "b" | "sample.b.a" | "sample.b.b"),
            "{linked}"
        );
    }
}
#[test]
fn local_and_parameter_masks_keep_qualified_owners_and_literal_text() {
    let mut c = fixture();
    c.fields.clear();
    for method in [
        "static int read(){int b=0;return sample.b.a + sample.b.b(1);}",
        "static int read(int b){return sample.b.a + sample.b.b(1);}",
    ] {
        let text =
            format!("/* é */ class b {{ {method} String s=\"sample.b.a\"; /* sample.b.b */ }}");
        let before = source(&text);
        let after = super::shorten("sample.b", &c, vec![before]).codes.remove(0);
        assert!(after.source.contains("sample.b.a + sample.b.b(1)"));
        assert!(after.source.contains("\"sample.b.a\"; /* sample.b.b */"));
        assert!(after.links.iter().all(|link| {
            after
                .source
                .chars()
                .skip(link.start)
                .take(link.end - link.start)
                .collect::<String>()
                .starts_with("sample.b.")
        }));
    }
}
#[test]
fn class_definition_alone_preserves_safe_current_owner_shortening() {
    let mut c = fixture();
    c.fields.clear();
    c.methods.clear();
    let code = source("class b { static int read() { return sample.b.a + sample.b.b(1); } }");
    let code = super::shorten("sample.b", &c, vec![code]).codes.remove(0);
    assert!(code.source.contains("b.a + b.b(1)"), "{}", code.source);
    assert!(code.links.iter().all(|link| {
        code.source
            .chars()
            .skip(link.start)
            .take(link.end - link.start)
            .collect::<String>()
            .starts_with("b.")
    }));
}
#[test]
#[ignore = "requires RDX_JAVA25_HOME"]
fn jvm_compiles_own_field_and_recursive_method_owner_collisions() {
    use std::{fs, process::Command};
    let home = std::env::var("RDX_JAVA25_HOME").unwrap();
    let c = fixture();
    let code = crate::native_java::render("sample.b", &c).unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-own-type-shadow-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(dir.join("sample/b.java"), code.source).unwrap();
    fs::write(dir.join("sample/Harness.java"),"package sample; public class Harness { public static void main(String[] args) { b.a=37; b.b=new long[]{5}; if(b.read()!=37 || b.b(4)!=0 || b.b[0]!=5)throw new AssertionError(); } }").unwrap();
    for (program, args) in [
        ("javac", vec!["sample/b.java", "sample/Harness.java"]),
        ("java", vec!["-cp", ".", "sample.Harness"]),
    ] {
        let out = Command::new(format!("{home}/bin/{program}"))
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
