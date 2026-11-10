use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_java,
};
use std::sync::Arc;

fn fixture_without_hierarchy() -> DexClass {
    // new StringBuilder; const-string; init(String); append(Object) with ignored
    // result; toString; move-result; unrelated receiver read/call; return.
    let words = vec![
        0x0022, 0, 0x011a, 0, 0x2070, 0, 0x0010, 0x206e, 1, 0x0030, 0x106e, 2, 0, 0x020c, 0x0071,
        3, 0, 0x1071, 4, 0x0002, 0x0211,
    ];
    DexClass {
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
    }
}

fn fixture() -> DexClass {
    let class = fixture_without_hierarchy();
    let hierarchy = rdx::native_hierarchy::TypeHierarchy::from_classes([&class]).unwrap();
    class.symbols.hierarchy.set(Arc::new(hierarchy)).unwrap();
    class
}

fn conversion_order_fixture(constant_only: bool) -> DexClass {
    let mut class = fixture_without_hierarchy();
    let symbols = Arc::get_mut(&mut class.symbols).unwrap();
    symbols.types.push("Ljava/lang/String;".into());
    symbols.strings.extend(["state".into(), "tail".into()]);
    symbols.fields.push((1, 2, 6));
    symbols.protos.push((
        "Ljava/lang/StringBuilder;".into(),
        vec!["Ljava/lang/String;".into()],
    ));
    symbols.methods.push((0, 5, 2));
    class.methods[0].name = if constant_only {
        "constant"
    } else {
        "converted"
    }
    .into();
    class.methods[0].code.as_mut().unwrap().instructions = if constant_only {
        vec![
            0x0022, 0, 0x011a, 0, 0x2070, 0, 0x0010, 0x011a, 7, 0x206e, 5, 0x0010, 0x106e, 2, 0,
            0x020c, 0x0211,
        ]
    } else {
        // append(Object p0); read Hook.state; append(String state); toString.
        vec![
            0x0022, 0, 0x011a, 0, 0x2070, 0, 0x0010, 0x206e, 1, 0x0030, 0x0262, 0, 0x206e, 5,
            0x0020, 0x106e, 2, 0, 0x020c, 0x0211,
        ]
    };
    let hierarchy = rdx::native_hierarchy::TypeHierarchy::from_classes([&class]).unwrap();
    class.symbols.hierarchy.set(Arc::new(hierarchy)).unwrap();
    class
}

fn empty_result_fixture() -> DexClass {
    let mut class = conversion_order_fixture(true);
    class.methods[0].name = "empty".into();
    let symbols = Arc::get_mut(&mut class.symbols).unwrap();
    symbols.strings[0].clear();
    symbols.strings[7].clear();
    class
}

#[test]
fn folded_conversion_is_staged_before_field_reads_and_result_is_fresh() {
    let class = conversion_order_fixture(false);
    let source = native_java::render_method("sample.Test", &class, &class.methods[0])
        .unwrap()
        .source;
    let conversion = source
        .find("String.valueOf((java.lang.Object) p0)")
        .unwrap();
    let field = source.find("sample.Hook.state").unwrap();
    let result = source.find("new java.lang.String(").unwrap();
    assert!(conversion < field && field < result, "{source}");
    let class = conversion_order_fixture(true);
    let source = native_java::render_method("sample.Test", &class, &class.methods[0])
        .unwrap()
        .source;
    assert!(
        source.contains("new java.lang.String(\"tag:\" + \"tail\")"),
        "{source}"
    );
    let class = empty_result_fixture();
    let source = native_java::render_method("sample.Test", &class, &class.methods[0])
        .unwrap()
        .source;
    assert!(
        source.contains("StringBuilder") && source.contains(".toString()"),
        "{source}"
    );
}

#[test]
#[ignore = "requires javac and java"]
fn jvm_interleaved_conversion_and_constant_results_preserve_semantics() {
    use std::{fs, process::Command};
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("sample")).unwrap();
    let mut methods: String = [false, true]
        .into_iter()
        .map(|constant| {
            let class = conversion_order_fixture(constant);
            native_java::render_method("sample.Test", &class, &class.methods[0])
                .unwrap()
                .source
        })
        .collect();
    let empty = empty_result_fixture();
    methods.push_str(
        &native_java::render_method("sample.Test", &empty, &empty.methods[0])
            .unwrap()
            .source,
    );
    let source = format!(
        r#"package sample;
public class Test {{
{methods}
 static String original(Object value) {{
   StringBuilder b = new StringBuilder("tag:"); b.append(value);
   String state = Hook.state; b.append(state); return b.toString();
 }}
 static String originalConstant() {{ return new StringBuilder("tag:").append("tail").toString(); }}
 public static void main(String[] args) {{
   Object token = new Object() {{ public String toString() {{ Hook.state="new"; return "value"; }} }};
   Hook.state="old"; String expected=original(token);
   Hook.state="old"; String actual=converted(token);
   if (!expected.equals(actual) || !actual.equals("tag:valuenew")) throw new AssertionError(actual);
   if (originalConstant()==originalConstant() || constant(null)==constant(null)) throw new AssertionError("identity");
   if (!originalConstant().equals(constant(null))) throw new AssertionError("content");
   boolean originalEmptyIdentity = new StringBuilder("").append("").toString() == new StringBuilder("").append("").toString();
   if ((empty(null)==empty(null)) != originalEmptyIdentity) throw new AssertionError("empty identity");
   final RuntimeException sentinel = new RuntimeException();
   Object throwing = new Object() {{ public String toString() {{ throw sentinel; }} }};
   try {{ converted(throwing); throw new AssertionError("missing throw"); }}
   catch (RuntimeException e) {{ if (e != sentinel) throw new AssertionError("exception identity"); }}
 }}
}}
class Hook {{ static String state="old"; }}
"#
    );
    fs::write(dir.path().join("sample/Test.java"), &source).unwrap();
    for (program, argument) in [("javac", "sample/Test.java"), ("java", "sample.Test")] {
        let output = Command::new(program)
            .arg(argument)
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{program}: {}\n{source}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn exact_single_use_builder_becomes_ordered_concat_with_valid_links() {
    let class = fixture();
    let code = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
    assert!(
        code.source
            .contains("String.valueOf((java.lang.Object) p0)"),
        "{}",
        code.source
    );
    assert!(!code.source.contains("StringBuilder"), "{}", code.source);
    assert!(!code.source.contains(".append("), "{}", code.source);
    assert!(!code.source.contains(".toString()"), "{}", code.source);
    let concat = code.source.find("new java.lang.String(\"tag:\" +").unwrap();
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
fn string_conversion_overloads_fold_while_char_sequence_and_extra_builder_use_stay_explicit() {
    for (descriptor, parameter, expected) in [
        ("Ljava/lang/String;", "Ljava/lang/String;", "\"tag:\" + p0"),
        ("C", "C", "\"tag:\" + p0"),
        ("I", "I", "\"tag:\" + p0"),
        ("Z", "Z", "\"tag:\" + p0"),
        ("I", "C", "\"tag:\" + ((int) p0)"),
    ] {
        let mut class = fixture();
        class.methods[0].parameters = vec![parameter.into()];
        let symbols = Arc::get_mut(&mut class.symbols).unwrap();
        symbols.protos[1].1[0] = descriptor.into();
        let code = native_java::render_method("sample.Test", &class, &class.methods[0])
            .unwrap_or_else(|e| panic!("{descriptor}: {e:#}"));
        assert!(
            code.source.contains(expected) && !code.source.contains(".append("),
            "{descriptor}: {}",
            code.source
        );
    }
    let mut class = fixture();
    class.methods[0].parameters = vec!["Ljava/lang/CharSequence;".into()];
    let symbols = Arc::get_mut(&mut class.symbols).unwrap();
    symbols.protos[1].1[0] = "Ljava/lang/CharSequence;".into();
    let code = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
    assert!(code.source.contains(".append("), "{}", code.source);
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
fn implicit_this_argument_folds_like_any_reference() {
    let mut class = fixture();
    class.methods[0].access_flags = 1; // instance method; v3 is implicit this
    class.methods[0].parameters.clear();
    let code = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
    assert!(
        code.source
            .contains("String.valueOf((java.lang.Object) this)"),
        "{}",
        code.source
    );
    assert!(!code.source.contains(".append("), "{}", code.source);
}

fn statement_form_words() -> Vec<u16> {
    // v0 = Hook.read(); new SB v1; const-string v2 "tag:"; v1.<init>(v2);
    // v3 = this.a; v1.append(v3); const-string v3 "')"; v1.append(v3);
    // v3 = v1.toString(); Hook.load(v3); return-void
    vec![
        0x0071, 5, 0, 0x000c, // invoke-static Hook.read; move-result-object v0
        0x0122, 0, // new-instance v1 StringBuilder
        0x021a, 0, // const-string v2 "tag:"
        0x2070, 0, 0x0021, // invoke-direct {v1, v2} <init>(String)
        0x4354, 0, // iget-object v3, v4, Test.a
        0x206e, 7, 0x0031, // invoke-virtual {v1, v3} append(String)
        0x031a, 11, // const-string v3 "')"
        0x206e, 7, 0x0031, // invoke-virtual {v1, v3} append(String)
        0x106e, 2, 1,      // invoke-virtual {v1} toString
        0x030c, // move-result-object v3
        0x1071, 6, 3,      // invoke-static {v3} Hook.load
        0x000e, // return-void
    ]
}

#[test]
fn statement_form_appends_with_interleaved_field_read_and_constant_fold_in_order() {
    let mut class = multi_fixture_class(statement_form_words());
    let code = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
    // The field read keeps its own statement; the fold reads the local.
    let field = code.source.find("java.lang.String v1 = this.a;").unwrap();
    let concat = code
        .source
        .find("java.lang.String v2 = new java.lang.String(\"tag:\" + v1 + \"')\");")
        .unwrap();
    let load = code.source.find("sample.Hook.load(v2);").unwrap();
    let read = code.source.find("sample.Hook.read();").unwrap();
    assert!(
        read < field && field < concat && concat < load,
        "{}",
        code.source
    );
    assert!(!code.source.contains("StringBuilder"), "{}", code.source);
    assert!(!code.source.contains(".append("), "{}", code.source);
    // An effectful instruction after the first append keeps the chain explicit.
    let words = &mut class.methods[0].code.as_mut().unwrap().instructions;
    words.splice(16..18, [0x0071, 5, 0, 0x030c]); // v3 = Hook.read() instead of the const-string
    let code = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
    assert!(code.source.contains(".append("), "{}", code.source);
}

#[test]
fn chained_appends_with_int_and_char_operands_fold_with_exact_conversions() {
    // new SB v1; const-string v2 "tag:"; v1.<init>(v2); const/4 v3 = 7;
    // v1 = v1.append(I v3); v3 = this.b; v1 = v1.append(String v3);
    // const/16 v3 = 65; v1 = v1.append(C v3); v3 = v1.toString(); Hook.load(v3)
    let words = vec![
        0x0122, 0, 0x021a, 0, 0x2070, 0, 0x0021, 0x7312, 0x206e, 8, 0x0031, 0x010c, 0x4354, 1,
        0x206e, 7, 0x0031, 0x010c, 0x0313, 65, 0x206e, 9, 0x0031, 0x010c, 0x106e, 2, 1, 0x030c,
        0x1071, 6, 3, 0x000e,
    ];
    let class = multi_fixture_class(words);
    let code = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
    assert!(
        code.source.contains(
            "java.lang.String v1 = new java.lang.String(\"tag:\" + 7 + v0 + ((char) 65));"
        ),
        "{}",
        code.source
    );
    assert!(
        code.source.contains("sample.Hook.load(v1);"),
        "{}",
        code.source
    );
    assert!(!code.source.contains("StringBuilder"), "{}", code.source);
}

#[test]
fn chain_inside_a_try_region_folds_and_keeps_the_handler() {
    // const-string v2 "tag:" (outside); try { new SB v1; v1.<init>(v2); v0 = Hook.read();
    // v1.append(v0); v3 = v1.toString() } catch (all) { v3 = "mid:" }; Hook.load(v3)
    let words = vec![
        0x021a, 0, // 0: const-string v2 "tag:"
        0x0122, 0, // 2: new-instance v1
        0x2070, 0, 0x0021, // 4: <init>(v2)
        0x0071, 5, 0, 0x000c, // 7: Hook.read(); move-result-object v0
        0x206e, 7, 0x0001, // 11: append(String v0)
        0x106e, 2, 1, 0x030c, // 14: toString; move-result-object v3
        0x0428, // 18: goto +4 -> 22, past the handler
        0x030d, // 19: move-exception v3 (handler)
        0x031a, 10, // 20: const-string v3 "mid:"
        0x1071, 6, 3,      // 22: Hook.load(v3)
        0x000e, // 25: return-void
    ];
    let mut class = multi_fixture_class(words);
    {
        let code = class.methods[0].code.as_mut().unwrap();
        code.tries = 1;
        code.try_regions = vec![DexTryRegion {
            start: 2,
            end: 18,
            catches: vec![(None, 19)].into(),
        }];
    }
    let code = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
    assert!(code.source.contains("\"tag:\" + "), "{}", code.source);
    assert!(!code.source.contains("StringBuilder"), "{}", code.source);
    assert!(code.source.contains("catch ("), "{}", code.source);
    assert!(code.source.contains("\"mid:\""), "{}", code.source);
}

#[test]
fn non_literal_prefix_escaping_builder_and_branch_into_chain_stay_explicit() {
    // Prefix from a call result.
    let mut words = statement_form_words();
    words.splice(6..8, [0x0071, 5, 0, 0x020c]); // v2 = Hook.read() instead of const-string
    let class = multi_fixture_class(words);
    let code = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
    assert!(code.source.contains(".append("), "{}", code.source);
    // The builder escapes to another call between the appends.
    let mut words = statement_form_words();
    words.splice(16..16, [0x1071, 10, 1]); // Hook.consume(v1)
    let class = multi_fixture_class(words);
    let code = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
    assert!(code.source.contains(".append("), "{}", code.source);
    // A move between the appends carries the builder past the chain.
    let mut words = statement_form_words();
    words.splice(28..28, [0x1071, 10, 0]); // Hook.consume(v0) after Hook.load
    words.splice(16..16, [0x1007]); // move-object v0, v1
    let class = multi_fixture_class(words);
    let code = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
    assert!(code.source.contains(".append("), "{}", code.source);
    // A conditional append is a branch into the chain and declines.
    let mut words = statement_form_words();
    words.splice(16..16, [0x0038, 7]); // if-eqz v0 -> the toString call, over one append
    let class = multi_fixture_class(words);
    let code = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
    assert!(code.source.contains(".append("), "{}", code.source);
}

fn multi_fixture_class(words: Vec<u16>) -> DexClass {
    let mut class = fixture_without_hierarchy();
    let symbols = Arc::get_mut(&mut class.symbols).unwrap();
    // strings 6..=12, types 2..=3, protos 5..=9, fields 0..=1, methods 5..=10
    symbols.strings.extend([
        "a".into(),
        "b".into(),
        "read".into(),
        "load".into(),
        "mid:".into(),
        "')".into(),
        "consume".into(),
    ]);
    symbols
        .types
        .extend(["Lsample/Test;".into(), "Ljava/lang/String;".into()]);
    symbols.protos.extend([
        ("Ljava/lang/String;".into(), vec![]),
        (
            "Ljava/lang/StringBuilder;".into(),
            vec!["Ljava/lang/String;".into()],
        ),
        ("Ljava/lang/StringBuilder;".into(), vec!["I".into()]),
        ("Ljava/lang/StringBuilder;".into(), vec!["C".into()]),
        ("V".into(), vec!["Ljava/lang/Object;".into()]),
    ]);
    symbols.fields.extend([(2, 3, 6), (2, 3, 7)]);
    symbols.methods.extend([
        (1, 5, 8),  // 5: Hook.read()String
        (1, 4, 9),  // 6: Hook.load(String)V
        (0, 6, 2),  // 7: StringBuilder.append(String)
        (0, 7, 2),  // 8: StringBuilder.append(I)
        (0, 8, 2),  // 9: StringBuilder.append(C)
        (1, 9, 12), // 10: Hook.consume(Object)V
    ]);
    class.fields = ["a", "b"]
        .into_iter()
        .map(|name| rdx::native_dex::DexField {
            declaring_type: "Lsample/Test;".into(),
            name: name.into(),
            field_type: "Ljava/lang/String;".into(),
            access_flags: 2,
            is_static: false,
        })
        .collect();
    let method = &mut class.methods[0];
    method.access_flags = 1;
    method.parameters.clear();
    method.return_type = "V".into();
    method.name = "run".into();
    let code = method.code.as_mut().unwrap();
    code.registers = 5;
    code.ins = 1;
    code.instructions = words;
    let hierarchy = rdx::native_hierarchy::TypeHierarchy::from_classes([&class]).unwrap();
    class.symbols.hierarchy.set(Arc::new(hierarchy)).unwrap();
    class
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
