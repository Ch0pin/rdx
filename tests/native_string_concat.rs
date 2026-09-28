use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols},
    native_java,
};
use std::sync::Arc;

fn fixture() -> DexClass {
    // new StringBuilder; const-string; init(String); append(Object) with ignored
    // result; toString; move-result; unrelated receiver read/call; return.
    let words = vec![
        0x0022, 0, 0x011a, 0, 0x2070, 0, 0x0010, 0x206e, 1, 0x0030, 0x106e, 2, 0, 0x020c, 0x0071,
        3, 0, 0x1071, 4, 0x0002, 0x0211,
    ];
    let class = DexClass {
        descriptor: "Lsample/Test;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols {
            strings: vec![
                "tag:".into(),
                "<init>".into(),
                "append".into(),
                "toString".into(),
                "readLogger".into(),
                "after".into(),
            ],
            types: vec!["Ljava/lang/StringBuilder;".into(), "Lsample/Hook;".into()],
            protos: vec![
                ("V".into(), vec!["Ljava/lang/String;".into()]),
                (
                    "Ljava/lang/StringBuilder;".into(),
                    vec!["Ljava/lang/Object;".into()],
                ),
                ("Ljava/lang/String;".into(), vec![]),
                ("V".into(), vec![]),
                ("V".into(), vec!["Ljava/lang/String;".into()]),
            ],
            methods: vec![(0, 0, 1), (0, 1, 2), (0, 2, 3), (1, 3, 4), (1, 4, 5)],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: "Lsample/Test;".into(),
            name: "text".into(),
            return_type: "Ljava/lang/String;".into(),
            parameters: vec!["Ljava/lang/Object;".into()],
            thrown_types: vec![],
            access_flags: 9,
            code: Some(DexCode {
                registers: 4,
                ins: 1,
                outs: 2,
                tries: 0,
                try_regions: vec![],
                instructions: words,
                offset: 0,
            }),
        }],
    };
    let hierarchy = rdx::native_hierarchy::TypeHierarchy::from_classes([&class]).unwrap();
    class.symbols.hierarchy.set(Arc::new(hierarchy)).unwrap();
    class
}

#[test]
fn exact_single_use_builder_becomes_ordered_concat_with_valid_links() {
    let class = fixture();
    let code = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
    assert!(code.source.contains("\"tag:\" + p0"), "{}", code.source);
    assert!(!code.source.contains("StringBuilder"), "{}", code.source);
    assert!(!code.source.contains(".append("), "{}", code.source);
    assert!(!code.source.contains(".toString()"), "{}", code.source);
    let concat = code.source.find("\"tag:\" + p0").unwrap();
    let receiver = code.source.find("Hook.readLogger()").unwrap();
    let after = code.source.find("Hook.after(").unwrap();
    assert!(concat < receiver && receiver < after, "{}", code.source);
    let chars: Vec<_> = code.source.chars().collect();
    for link in &code.links {
        assert!(link.start < link.end && link.end <= chars.len());
        assert!(
            !chars[link.start..link.end]
                .iter()
                .collect::<String>()
                .is_empty()
        );
        assert!(!link.label.contains("StringBuilder"), "{}", link.label);
    }
}

#[test]
fn alternate_append_overloads_and_extra_builder_use_stay_explicit() {
    for (descriptor, parameter) in [
        ("Ljava/lang/String;", "Ljava/lang/String;"),
        ("Ljava/lang/CharSequence;", "Ljava/lang/CharSequence;"),
        ("C", "C"),
        ("I", "I"),
        ("Z", "Z"),
    ] {
        let mut class = fixture();
        class.methods[0].parameters = vec![parameter.into()];
        let symbols = Arc::get_mut(&mut class.symbols).unwrap();
        symbols.protos[1].1[0] = descriptor.into();
        let code = native_java::render_method("sample.Test", &class, &class.methods[0])
            .unwrap_or_else(|e| panic!("{descriptor}: {e:#}"));
        assert!(
            code.source.contains(".append("),
            "{descriptor}: {}",
            code.source
        );
    }
    let mut class = fixture();
    let words = &mut class.methods[0].code.as_mut().unwrap().instructions;
    words.splice(14..14, [0x106e, 2, 0]); // an extra ignored toString on the same builder
    let code = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
    assert!(code.source.contains(".append("), "{}", code.source);
    assert!(code.source.matches(".toString()").count() >= 2);
}

#[test]
fn branch_phi_carrying_builder_identity_stays_explicit() {
    let mut class = fixture();
    let symbols = Arc::get_mut(&mut class.symbols).unwrap();
    symbols.strings.push("consume".into());
    symbols
        .protos
        .push(("V".into(), vec!["Ljava/lang/Object;".into()]));
    symbols.methods.push((1, 5, 6));
    let words = &mut class.methods[0].code.as_mut().unwrap().instructions;
    words.splice(14..14, [0x0338, 3, 0x0012, 0x1071, 5, 0]);
    let code = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
    assert!(code.source.contains(".append("), "{}", code.source);
}

#[test]
fn implicit_this_argument_keeps_existing_builder_route() {
    let mut class = fixture();
    class.methods[0].access_flags = 1; // instance method; v3 is implicit this
    class.methods[0].parameters.clear();
    let code = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
    assert!(code.source.contains(".append("), "{}", code.source);
    assert!(!code.source.contains("\"tag:\" + this"), "{}", code.source);
}

#[test]
#[ignore = "requires javac and java"]
fn jvm_preserves_object_conversion_effects_exception_identity_and_string_identity() {
    use std::{fs, process::Command};
    let class = fixture();
    let code = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-concat-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let java = format!(
        r#"package sample;
public class Test {{
{}
  static String original(Object value) {{
    StringBuilder b = new StringBuilder("tag:");
    b.append(value);
    String result = b.toString();
    Hook.readLogger();
    Hook.after(result);
    return result;
  }}
  static void check(boolean value) {{ if (!value) throw new AssertionError(Hook.trace); }}
  static void run(Object value) {{
    Hook.reset(); String before = original(value); String beforeTrace = Hook.trace;
    Hook.reset(); String after = text(value); String afterTrace = Hook.trace;
    check(before.equals(after) && beforeTrace.equals(afterTrace));
  }}
  public static void main(String[] args) {{
    run(null); run(new Token("hello", false)); run(new Token(null, false));
    run(new char[] {{'a', 'b'}});
    Hook.reset(); String a = text(null), b = text(null); check(a != b);
    Hook.reset(); try {{ text(new Token("bad", true)); throw new AssertionError(); }}
    catch (RuntimeException actual) {{ check(actual == Hook.sentinel && Hook.trace.equals("T")); }}
    Hook.reset(); try {{ original(new Token("bad", true)); throw new AssertionError(); }}
    catch (RuntimeException actual) {{ check(actual == Hook.sentinel && Hook.trace.equals("T")); }}
    System.out.print("ok");
  }}
}}
class Hook {{
  static String trace = ""; static String receiver = "old";
  static final RuntimeException sentinel = new RuntimeException("sentinel");
  static void reset() {{ trace = ""; receiver = "old"; }}
  static void readLogger() {{ trace += "R" + receiver + ";"; }}
  static void after(String value) {{ trace += "A" + value + ";"; }}
}}
class Token implements CharSequence {{
  final String text; final boolean fail;
  Token(String text, boolean fail) {{ this.text = text; this.fail = fail; }}
  public String toString() {{ Hook.trace += "T"; Hook.receiver = "new"; if (fail) throw Hook.sentinel; return text; }}
  public int length() {{ throw new AssertionError("wrong CharSequence overload"); }}
  public char charAt(int index) {{ throw new AssertionError("wrong CharSequence overload"); }}
  public CharSequence subSequence(int start, int end) {{ throw new AssertionError("wrong CharSequence overload"); }}
}}
"#,
        code.source
    );
    fs::write(dir.join("sample/Test.java"), java).unwrap();
    let compiled = Command::new("javac")
        .arg(dir.join("sample/Test.java"))
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&compiled.stderr),
        code.source
    );
    let run = Command::new("java")
        .arg("-cp")
        .arg(&dir)
        .arg("sample.Test")
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(run.stdout, b"ok");
    fs::remove_dir_all(dir).unwrap();
}
