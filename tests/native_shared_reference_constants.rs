//! G01-B-reference-constants: public renderer and JVM checks for pool operands.
use rdx::{
    native_dex::{self, DexClass, DexSymbols},
    native_java,
};
use std::{fs, process::Command, sync::Arc};

fn fixture(name: &str, result: &str, words: Vec<u16>, registers: u16) -> DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class
        .methods
        .retain(|method| method.name.as_ref() == "answer");
    let method = &mut class.methods[0];
    method.name = name.into();
    method.access_flags = 9;
    method.return_type = result.into();
    method.parameters.clear();
    let code = method.code.as_mut().unwrap();
    code.registers = registers;
    code.ins = 0;
    code.outs = 0;
    code.instructions = words;
    class
}

fn source(class: &DexClass) -> String {
    native_java::render_method("sample.Hello", class, &class.methods[0])
        .unwrap()
        .source
}

#[test]
fn string_forms_resolve_the_selected_pool_entry_and_destination() {
    let mut strings = vec!["decoy".to_owned(); 65_537];
    strings[2] = "quote \" slash \\\\ newline\nΕλληνικά 🦀".into();
    strings[65_536] = r"\u{d800}x\u{dfff}".into();
    for (name, words, expected) in [
        (
            "compact",
            vec![0x031a, 2, 0x0311],
            r#"quote \" slash \\ newline\nΕλληνικά 🦀"#,
        ),
        ("jumbo", vec![0x031b, 0, 1, 0x0311], r"\ud800x\udfff"),
    ] {
        let mut class = fixture(name, "Ljava/lang/String;", words, 4);
        class.symbols = Arc::new(DexSymbols {
            strings: strings.clone(),
            ..Default::default()
        });
        let rendered = source(&class);
        assert!(rendered.contains(expected), "{name}: {rendered}");
        assert!(rendered.contains("return "), "{name}: {rendered}");
        assert!(!rendered.contains("decoy"), "{name}: {rendered}");
    }
}

#[test]
fn class_literal_links_cover_exact_identifier_and_preserve_type_identity() {
    for (index, descriptor, display, label) in [
        (1, "Lsample/Target;", "sample.Target", Some("sample.Target")),
        (
            2,
            "[Lsample/Target;",
            "sample.Target[]",
            Some("sample.Target"),
        ),
        (3, "I", "int", None),
    ] {
        let mut class = fixture("type", "Ljava/lang/Class;", vec![0x021c, index, 0x0211], 3);
        class.symbols = Arc::new(DexSymbols {
            types: ["Lsample/Decoy;", "Lsample/Target;", "[Lsample/Target;", "I"]
                .into_iter()
                .map(Into::into)
                .collect(),
            ..Default::default()
        });
        let rendered =
            native_java::render_method("sample.Hello", &class, &class.methods[0]).unwrap();
        assert!(rendered.source.contains(&format!("{display}.class")));
        assert!(!rendered.source.contains("Decoy.class"));
        let literal_links: Vec<_> = rendered
            .links
            .iter()
            .filter(|link| {
                rendered
                    .source
                    .chars()
                    .skip(link.end)
                    .take(6)
                    .collect::<String>()
                    == ".class"
            })
            .collect();
        match label {
            Some(label) => {
                assert_eq!(literal_links.len(), 1, "{}", rendered.source);
                let link = literal_links[0];
                assert_eq!(link.label, label);
                assert_eq!(
                    rendered
                        .source
                        .chars()
                        .skip(link.start)
                        .take(link.end - link.start)
                        .collect::<String>(),
                    display
                );
            }
            None => assert!(literal_links.is_empty(), "{}", rendered.source),
        }
        assert_eq!(
            descriptor.starts_with('L') || descriptor.starts_with('['),
            label.is_some()
        );
    }
}

#[test]
fn overwritten_reference_retains_earlier_value() {
    // v0 = first; v1 = v0; v0 = second; return v1.
    let mut class = fixture(
        "snapshot",
        "Ljava/lang/String;",
        vec![0x001a, 0, 0x0107, 0x001a, 1, 0x0111],
        2,
    );
    class.symbols = Arc::new(DexSymbols {
        strings: vec!["first".into(), "second".into()],
        ..Default::default()
    });
    let rendered = source(&class);
    assert!(rendered.contains("first"), "{rendered}");
    assert!(rendered.contains("return "), "{rendered}");
}

#[test]
fn overwritten_class_literal_stays_before_following_effect() {
    // Even without a live result, const-class can resolve and must not move
    // past a later call or vanish when the destination register is overwritten.
    let mut class = fixture(
        "orderedOverwrite",
        "Ljava/lang/String;",
        vec![0x001c, 0, 0x0071, 0, 0, 0x001a, 1, 0x0011],
        1,
    );
    class.symbols = Arc::new(DexSymbols {
        strings: vec!["mark".into(), "second".into()],
        types: vec!["Lsample/Target;".into(), "Lsample/ReferenceChecks;".into()],
        protos: vec![("V".into(), vec![])],
        methods: vec![(1, 0, 0)],
        ..Default::default()
    });
    let rendered = source(&class);
    let class_at = rendered.find("sample.Target.class").unwrap();
    let effect_at = rendered.find("sample.ReferenceChecks.mark()").unwrap();
    let overwrite_at = rendered.find("\"second\"").unwrap();
    assert!(
        class_at < effect_at && effect_at < overwrite_at,
        "{rendered}"
    );
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn reference_constants_jvm_preserves_units_identity_and_class_initialization_order() {
    let mut string = fixture("string", "Ljava/lang/String;", vec![0x001a, 0, 0x0011], 1);
    string.symbols = Arc::new(DexSymbols {
        strings: vec!["quote \" slash \\\\ newline\nΕλληνικά 🦀".into()],
        ..Default::default()
    });
    let mut surrogate = fixture(
        "surrogate",
        "Ljava/lang/String;",
        vec![0x001b, 0, 1, 0x0011],
        1,
    );
    let mut strings = vec![String::new(); 65_537];
    strings[65_536] = r"\u{d800}x\u{dfff}".into();
    surrogate.symbols = Arc::new(DexSymbols {
        strings,
        ..Default::default()
    });
    let mut literal = fixture(
        "targetType",
        "Ljava/lang/Class;",
        vec![0x001c, 0, 0x0011],
        1,
    );
    literal.symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/Target;".into()],
        ..Default::default()
    });
    let mut ordered = fixture(
        "ordered",
        "Ljava/lang/Class;",
        vec![0x001c, 0, 0x0071, 0, 0, 0x0011],
        1,
    );
    ordered.symbols = Arc::new(DexSymbols {
        strings: vec!["mark".into()],
        types: vec!["Lsample/Target;".into(), "Lsample/ReferenceChecks;".into()],
        protos: vec![("V".into(), vec![])],
        methods: vec![(1, 0, 0)],
        ..Default::default()
    });
    let mut snapshot = fixture(
        "snapshot",
        "Ljava/lang/String;",
        vec![0x001a, 0, 0x0107, 0x001a, 1, 0x0111],
        2,
    );
    snapshot.symbols = Arc::new(DexSymbols {
        strings: vec!["first".into(), "second".into()],
        ..Default::default()
    });
    let body = [&string, &surrogate, &literal, &ordered, &snapshot]
        .into_iter()
        .map(source)
        .collect::<Vec<_>>()
        .join("\n");
    let java = format!(
        r#"package sample;
public class ReferenceChecks {{
  static int initialized;
  static String trace = "";
  public static void mark() {{ trace += "M"; }}
  {body}
  public static void main(String[] args) {{
    if (!string().equals("quote \" slash \\ newline\nΕλληνικά 🦀")) throw new AssertionError("string");
    String s = surrogate();
    if (s.length() != 3 || s.charAt(0) != 0xd800 || s.charAt(1) != 'x' || s.charAt(2) != 0xdfff) throw new AssertionError("surrogate");
    if (!snapshot().equals("first")) throw new AssertionError("overwrite");
    if (targetType() != Target.class || initialized != 0) throw new AssertionError("class literal initialized Target");
    if (ordered() != Target.class || !trace.equals("M") || initialized != 0) throw new AssertionError("effect order");
    new Target();
    if (initialized != 1 || !trace.equals("M")) throw new AssertionError("initializer");
  }}
}}
class Target {{ static {{ ReferenceChecks.initialized++; }} }}
"#
    );
    let dir = std::env::temp_dir().join(format!("rdx-shared-references-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(dir.join("sample/ReferenceChecks.java"), &java).unwrap();
    for (program, argument) in [
        ("javac", "sample/ReferenceChecks.java"),
        ("java", "sample.ReferenceChecks"),
    ] {
        let output = Command::new(program)
            .arg(argument)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{program}: {}\n{java}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
