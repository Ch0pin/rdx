use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols},
    native_java,
};
use std::sync::Arc;
fn fixture(words: Vec<u16>, outer: Vec<&str>, inner: Vec<&str>) -> DexClass {
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
            strings: vec!["<init>".into(), "f".into()],
            types: vec![
                "Lsample/A;".into(),
                "Lsample/B;".into(),
                "Lsample/Source;".into(),
            ],
            protos: vec![
                ("V".into(), outer.into_iter().map(Into::into).collect()),
                ("V".into(), inner.into_iter().map(Into::into).collect()),
                ("I".into(), vec![]),
            ],
            methods: vec![(0, 0, 0), (1, 1, 0), (2, 2, 1)],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: "Lsample/Test;".into(),
            name: "make".into(),
            return_type: "Lsample/B;".into(),
            parameters: vec![],
            thrown_types: vec![],
            access_flags: 9,
            code: Some(DexCode {
                registers: 4,
                ins: 0,
                outs: 4,
                tries: 0,
                try_regions: vec![],
                instructions: words,
                offset: 0,
            }),
        }],
    }
}
#[test]
fn nested_allocation_aliases_and_constructor_links_survive_java_emission() {
    // new A v0; new B v1; alias v2=v1; B.init; A.init(v1,v2); return v2.
    let class = fixture(
        vec![
            0x0022, 0, 0x0122, 1, 0x1207, 0x1070, 1, 1, 0x3070, 0, 0x0210, 0x0211,
        ],
        vec!["Lsample/B;", "Lsample/B;"],
        vec![],
    );
    let code = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
    assert!(
        code.source
            .contains("new sample.A((v0 = new sample.B()), v0)"),
        "{}",
        code.source
    );
    assert!(code.source.contains("return v0;"), "{}", code.source);
    assert_eq!(code.source.matches("new sample.B").count(), 1);
    for label in [
        "sample.A.<init>(Lsample/B;Lsample/B;)V",
        "sample.B.<init>()V",
    ] {
        let link = code.links.iter().find(|l| l.label == label).unwrap();
        let token: String = code
            .source
            .chars()
            .skip(link.start)
            .take(link.end - link.start)
            .collect();
        assert!(matches!(token.as_str(), "sample.A" | "sample.B"), "{token}");
    }
}
#[test]
fn child_arguments_capture_call_after_child_allocation() {
    let class = fixture(
        vec![
            0x0022, 0, 0x0122, 1, 0x0071, 2, 0, 0x020a, 0x2070, 1, 0x0021, 0x2070, 0, 0x0010,
            0x0111,
        ],
        vec!["Lsample/B;"],
        vec!["I"],
    );
    let code = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
    assert!(
        code.source
            .contains("new sample.A((v1 = new sample.B((v0 = sample.Source.f()))))"),
        "{}",
        code.source
    );
    assert!(code.source.contains("return v1;"), "{}", code.source);
}
#[test]
fn stages_call_before_child_constructor_without_reordering_effects() {
    let class = fixture(
        vec![
            0x0022, 0, 0x0071, 2, 0, 0x020a, 0x0122, 1, 0x2070, 1, 0x0021, 0x2070, 0, 0x0010,
            0x0111,
        ],
        vec!["Lsample/B;"],
        vec!["I"],
    );
    let code = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
    assert!(code.source.contains("new sample.A"), "{}", code.source);
    assert_eq!(code.source.matches("new sample.A").count(), 1);
}
#[test]
fn rejects_crossed_constructor_lifetimes() {
    let class = fixture(
        vec![0x0022, 0, 0x0122, 1, 0x1070, 0, 0, 0x1070, 1, 1, 0x0111],
        vec![],
        vec![],
    );
    assert!(native_java::render_method("sample.Test", &class, &class.methods[0]).is_err());
}
#[test]
fn earlier_capture_can_be_reused_inside_child_when_root_argument_preserves_order() {
    let class = fixture(
        vec![
            0x0022, 0, 0x0071, 2, 0, 0x020a, 0x0122, 1, 0x2070, 1, 0x0021, 0x3070, 0, 0x0120,
            0x0111,
        ],
        vec!["I", "Lsample/B;"],
        vec!["I"],
    );
    let code = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
    assert!(
        code.source
            .contains("new sample.A((v0 = sample.Source.f()), (v1 = new sample.B(v0)))"),
        "{}",
        code.source
    );
}
#[test]
fn stages_sibling_constructors_in_original_order() {
    // A allocated, B allocated twice, only then the two B constructors execute.
    let class = fixture(
        vec![
            0x0022, 0, 0x0122, 1, 0x0222, 1, 0x1070, 1, 1, 0x1070, 1, 2, 0x3070, 0, 0x0210, 0x0211,
        ],
        vec!["Lsample/B;", "Lsample/B;"],
        vec![],
    );
    let code = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
    assert!(code.source.contains("new sample.A"), "{}", code.source);
    assert_eq!(code.source.matches("new sample.A").count(), 1);
}

fn builder_fixture(owner: &str) -> DexClass {
    let mut class = fixture(
        vec![
            0x0022, 0, 0x0122, 1, 0x1070, 1, 1, 0x1307, 0x021a, 3, 0x206e, 3, 0x0021, 0x206e, 3,
            0x0023, 0x106e, 4, 1, 0x020c, 0x2070, 0, 0x0020, 0x0311,
        ],
        vec!["Ljava/lang/String;"],
        vec![],
    );
    let symbols = Arc::get_mut(&mut class.symbols).unwrap();
    symbols.types[1] = owner.into();
    symbols
        .strings
        .extend(["append".into(), ".mp4".into(), "toString".into()]);
    symbols
        .protos
        .push((owner.into(), vec!["Ljava/lang/String;".into()]));
    symbols.protos.push(("Ljava/lang/String;".into(), vec![]));
    symbols.methods.extend([(1, 3, 2), (1, 4, 4)]);
    class.methods[0].return_type = owner.into();
    class
}

#[test]
fn ignored_builder_appends_preserve_mutations_and_live_aliases() {
    let class = builder_fixture("Ljava/lang/StringBuilder;");
    let code = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
    assert_eq!(
        code.source.matches(".append(").count(),
        2,
        "{}",
        code.source
    );
    assert_eq!(
        code.source.matches("new java.lang.StringBuilder()").count(),
        1
    );
    assert_eq!(code.source.matches(".toString()").count(), 1);
    assert!(code.source.contains("return v3;"), "{}", code.source);
    let links: Vec<_> = code
        .links
        .iter()
        .filter(|l| {
            l.label == "java.lang.StringBuilder.append(Ljava/lang/String;)Ljava/lang/StringBuilder;"
        })
        .collect();
    assert_eq!(links.len(), 2);
    for link in links {
        assert_eq!(
            code.source
                .chars()
                .skip(link.start)
                .take(link.end - link.start)
                .collect::<String>(),
            "append"
        );
    }
}

#[test]
fn arbitrary_append_return_is_not_assumed_to_be_receiver() {
    let class = builder_fixture("Lsample/Builder;");
    let code = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
    assert!(code.source.contains("new sample.A"), "{}", code.source);
    assert_eq!(
        code.source.matches("v0.append(v1)").count(),
        2,
        "{}",
        code.source
    );
    assert!(code.source.contains("v0.toString()"), "{}", code.source);
    assert!(code.source.contains("return v0;"), "{}", code.source);
}

#[test]
fn input_receiver_cast_is_a_structured_expression_not_an_unbound_local() {
    let mut class = fixture(
        vec![0x0022, 0, 0x106e, 2, 3, 0x010a, 0x2070, 0, 0x0010, 0x0011],
        vec!["I"],
        vec![],
    );
    class.superclass = Some("Lsample/Source;".into());
    class.methods[0].access_flags = 1;
    class.methods[0].return_type = "Lsample/A;".into();
    class.methods[0].code.as_mut().unwrap().ins = 1;
    let code = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
    assert!(
        code.source.contains("((sample.Source) this).f()"),
        "{}",
        code.source
    );
    assert_eq!(code.source.matches(".f()").count(), 1);
    assert!(code.links.iter().any(|l| l.label == "sample.Source.f()I"));
    assert!(code.links.iter().any(|l| l.label == "sample.Source"));
}

#[test]
fn ignored_char_append_preserves_overload_order_and_receiver_alias() {
    let mut class = builder_fixture("Ljava/lang/StringBuilder;");
    let symbols = Arc::get_mut(&mut class.symbols).unwrap();
    symbols
        .protos
        .push(("Ljava/lang/StringBuilder;".into(), vec!["C".into()]));
    symbols.methods.push((1, 5, 2));
    let words = &mut class.methods[0].code.as_mut().unwrap().instructions;
    words.splice(13..16, [0x0213, 46, 0x206e, 5, 0x0023]);
    let code = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
    assert_eq!(
        code.source.matches(".append(").count(),
        2,
        "{}",
        code.source
    );
    assert_eq!(
        code.source.matches(".append((char) 46)").count(),
        1,
        "{}",
        code.source
    );
    assert_eq!(
        code.source.matches("new java.lang.StringBuilder()").count(),
        1
    );
    assert!(code.source.find(".mp4").unwrap() < code.source.find("(char) 46").unwrap());
    assert!(code.source.find("(char) 46").unwrap() < code.source.find(".toString()").unwrap());
    assert!(code.source.contains("return v3;"), "{}", code.source);
    assert!(
        code.links
            .iter()
            .any(|link| link.label == "java.lang.StringBuilder.append(C)Ljava/lang/StringBuilder;")
    );
}

#[test]
fn unknown_ignored_builder_overload_is_staged_without_alias_assumption() {
    let mut class = builder_fixture("Ljava/lang/StringBuilder;");
    Arc::get_mut(&mut class.symbols).unwrap().protos[3].1[0] = "Ljava/lang/Object;".into();
    let hierarchy = rdx::native_hierarchy::TypeHierarchy::from_classes([&class]).unwrap();
    class.symbols.hierarchy.set(Arc::new(hierarchy)).unwrap();
    let code = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
    assert!(code.source.contains("new sample.A"), "{}", code.source);
    assert_eq!(code.source.matches("new sample.A").count(), 1);
}

#[test]
fn ignored_primitive_builder_appends_keep_effects_and_aliases() {
    for (ty, literal, expected) in [("I", 46, "46"), ("Z", 1, "true")] {
        let mut class = builder_fixture("Ljava/lang/StringBuilder;");
        let symbols = Arc::get_mut(&mut class.symbols).unwrap();
        symbols
            .protos
            .push(("Ljava/lang/StringBuilder;".into(), vec![ty.into()]));
        symbols.methods.push((1, 5, 2));
        class.methods[0]
            .code
            .as_mut()
            .unwrap()
            .instructions
            .splice(13..16, [0x0213, literal, 0x206e, 5, 0x0023]);
        let code = native_java::render_method("sample.Test", &class, &class.methods[0])
            .unwrap_or_else(|e| panic!("{ty}: {e}"));
        assert_eq!(
            code.source.matches(".append(").count(),
            2,
            "{}",
            code.source
        );
        assert!(
            code.source.contains(&format!(".append({expected})")),
            "{}",
            code.source
        );
        assert!(
            code.source.find(".mp4").unwrap()
                < code.source.find(&format!(".append({expected})")).unwrap()
        );
        assert!(code.source.contains("return v3;"), "{}", code.source);
        assert!(
            code.links.iter().any(|l| l.label
                == format!("java.lang.StringBuilder.append({ty})Ljava/lang/StringBuilder;"))
        );
    }
}

#[test]
#[ignore = "requires javac and java"]
fn primitive_builder_java_preserves_text_order_and_returned_alias() {
    use std::{fs, process::Command};
    let dir = std::env::temp_dir().join(format!("rdx-builder-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    for (ty, value, expected) in [("I", 46, ".mp446"), ("Z", 1, ".mp4true")] {
        let mut class = builder_fixture("Ljava/lang/StringBuilder;");
        let symbols = Arc::get_mut(&mut class.symbols).unwrap();
        symbols
            .protos
            .push(("Ljava/lang/StringBuilder;".into(), vec![ty.into()]));
        symbols.methods.push((1, 5, 2));
        class.methods[0]
            .code
            .as_mut()
            .unwrap()
            .instructions
            .splice(13..16, [0x0213, value, 0x206e, 5, 0x0023]);
        let code = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
        fs::write(dir.join("sample/Test.java"), format!("package sample; public class Test {{ {} public static void main(String[] args) {{ StringBuilder b = make(); if (!b.toString().equals(\"{expected}\") || !A.text.equals(\"{expected}\") || A.calls != 1) throw new AssertionError(b.toString()); System.out.print(\"ok\"); }} }} class A {{ static String text; static int calls; A(String value) {{ text=value; calls++; }} }}", code.source)).unwrap();
        let result = Command::new("javac")
            .arg(dir.join("sample/Test.java"))
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stderr),
            code.source
        );
        let result = Command::new("java")
            .arg("-cp")
            .arg(&dir)
            .arg("sample.Test")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(result.stdout, b"ok");
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore = "requires javac and java"]
fn staged_nested_wide_and_void_effects_execute_once_in_order() {
    use std::{fs, process::Command};
    // A allocation; B allocation; f()->long; B.init(long); effect(); A.init(B).
    let mut class = fixture(
        vec![
            0x0022, 0, 0x0122, 1, 0x0071, 2, 0, 0x020b, 0x3070, 1, 0x0321, 0x0071, 3, 0, 0x2070, 0,
            0x0010, 0x0111,
        ],
        vec!["Lsample/B;"],
        vec!["J"],
    );
    let symbols = Arc::get_mut(&mut class.symbols).unwrap();
    symbols.protos[2].0 = "J".into();
    symbols.protos.push(("V".into(), vec![]));
    symbols.strings.push("effect".into());
    symbols.methods.push((2, 3, 2));
    let code = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-staged-effects-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let source = format!(
        r#"package sample;
public class Test {{
{}
static String log = ""; static boolean fail;
public static void main(String[] args) {{
 B b = make();
 if (!log.equals("fBeA") || b.value != 0x123456789abcdefL || A.saved != b) throw new AssertionError(log);
 log=""; fail=true;
 try {{ make(); throw new AssertionError("expected failure"); }} catch (IllegalStateException expected) {{}}
 if (!log.equals("fBe")) throw new AssertionError(log);
 System.out.print("ok");
}}
}}
class Source {{ static long f() {{ Test.log += "f"; return 0x123456789abcdefL; }} static void effect() {{ Test.log += "e"; if (Test.fail) throw new IllegalStateException(); }} }}
class B {{ final long value; B(long v) {{ Test.log += "B"; value=v; }} }}
class A {{ static B saved; A(B b) {{ Test.log += "A"; saved=b; }} }}
"#,
        code.source
    );
    fs::write(dir.join("sample/Test.java"), source).unwrap();
    let result = Command::new("javac")
        .arg(dir.join("sample/Test.java"))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stderr),
        code.source
    );
    let result = Command::new("java")
        .arg("-cp")
        .arg(&dir)
        .arg("sample.Test")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(result.stdout, b"ok");
    fs::remove_dir_all(dir).unwrap();
}
