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
            .contains("new sample.A((v1 = new sample.B(sample.Source.f())))"),
        "{}",
        code.source
    );
    assert_eq!(code.source.matches("sample.Source.f()").count(), 1);
    assert!(
        code.source.find("new sample.A").unwrap() < code.source.find("new sample.B").unwrap()
            && code.source.find("new sample.B").unwrap()
                < code.source.find("sample.Source.f()").unwrap(),
        "{}",
        code.source
    );
    assert!(code.source.contains("sample.B v1;"), "{}", code.source);
    assert!(code.source.contains("return v1;"), "{}", code.source);
}
#[test]
fn rejects_effect_before_child_with_live_parent_allocation() {
    let class = fixture(
        vec![
            0x0022, 0, 0x0071, 2, 0, 0x020a, 0x0122, 1, 0x2070, 1, 0x0021, 0x2070, 0, 0x0010,
            0x0111,
        ],
        vec!["Lsample/B;"],
        vec!["I"],
    );
    // Keeping the earlier parent allocation cannot move this call into a later
    // child's argument. Preserve rejection until complete lifetime ownership exists.
    assert!(native_java::render_method("sample.Test", &class, &class.methods[0]).is_err());
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
fn rejects_sibling_allocations_before_either_constructor() {
    // A allocated, B allocated twice, only then the two B constructors execute.
    let class = fixture(
        vec![
            0x0022, 0, 0x0122, 1, 0x0222, 1, 0x1070, 1, 1, 0x1070, 1, 2, 0x3070, 0, 0x0210, 0x0211,
        ],
        vec!["Lsample/B;", "Lsample/B;"],
        vec![],
    );
    assert!(native_java::render_method("sample.Test", &class, &class.methods[0]).is_err());
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
    assert!(code.source.contains("v0.append(v1)"), "{}", code.source);
    assert!(!code.source.contains("v2.append("), "{}", code.source);
    assert!(!code.source.contains("v3.append("), "{}", code.source);
    assert!(code.source.contains("v0.toString()"), "{}", code.source);
    assert!(code.source.contains("return v0;"), "{}", code.source);
}

#[test]
#[ignore = "requires javac and java"]
fn custom_append_results_are_discarded_and_original_receiver_mutated_twice() {
    use std::process::Command;
    let class = builder_fixture("Lsample/Builder;");
    let code = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-custom-append-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("sample")).unwrap();
    let source = format!(
        r#"package sample;
public class Test {{
{}
public static void main(String[] args) {{
 Builder value = make();
 if(value != Builder.original || value.calls != 2 || Builder.alien.calls != 0 || !A.text.equals(".mp4.mp4")) throw new AssertionError();
}}
}}
class Builder {{
 static final Builder alien = new Builder(false);
 static Builder original;
 int calls;
 String text = "";
 Builder() {{ original = this; }}
 Builder(boolean ignored) {{}}
 Builder append(String s) {{ calls++; text += s; return alien; }}
 public String toString() {{ return text; }}
}}
class A {{ static String text; A(String s) {{ text = s; }} }}
"#,
        code.source
    );
    let file = dir.join("sample/Test.java");
    std::fs::write(&file, source).unwrap();
    let compiled = Command::new("javac").arg(&file).output().unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let result = Command::new("java")
        .args(["-cp", dir.to_str().unwrap(), "sample.Test"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
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
    assert!(native_java::render_method("sample.Test", &class, &class.methods[0]).is_err());
    class
        .symbols
        .hierarchy
        .set(Arc::new(
            rdx::native_hierarchy::TypeHierarchy::from_classes([&class]).unwrap(),
        ))
        .unwrap();
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
fn ignored_object_builder_overload_keeps_the_outer_allocation() {
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
fn completed_child_and_void_effects_keep_order_before_later_parent_allocation() {
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
    assert!(native_java::render_method("sample.Test", &class, &class.methods[0]).is_err());
    // This is a DIFFERENT positive DEX fixture: the parent new-instance now
    // occurs after the completed child and void effect, before its own ctor.
    // The original nested lifetime above stays rejected.
    let words = &mut class.methods[0].code.as_mut().unwrap().instructions;
    let parent_allocation: Vec<_> = words.drain(..2).collect();
    words.splice(12..12, parent_allocation);
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

fn strict_object_builder_fixture() -> (DexClass, DexClass) {
    let mut class = builder_fixture("Ljava/lang/StringBuilder;");
    let mut leaf = fixture(vec![0x000e], vec![], vec![]);
    leaf.descriptor = "Lsample/MessageException;".into();
    leaf.superclass = Some("Ljava/lang/IllegalArgumentException;".into());
    leaf.methods.clear();
    leaf.symbols = Arc::new(DexSymbols::default());
    let symbols = Arc::get_mut(&mut class.symbols).unwrap();
    symbols.types[0] = leaf.descriptor.clone();
    symbols
        .types
        .push("Ljava/lang/IllegalArgumentException;".into());
    symbols.methods[0].0 = 3;
    symbols.protos[3].1[0] = "Ljava/lang/Object;".into();
    let method = &mut class.methods[0];
    method.parameters = vec!["Ljava/lang/Object;".into()];
    let code = method.code.as_mut().unwrap();
    code.registers = 5;
    code.ins = 1;
    code.instructions.splice(8..10, [0x4207]);
    let hierarchy =
        Arc::new(rdx::native_hierarchy::TypeHierarchy::from_classes([&class, &leaf]).unwrap());
    class.symbols.hierarchy.set(hierarchy.clone()).unwrap();
    leaf.symbols.hierarchy.set(hierarchy).unwrap();
    (class, leaf)
}

#[test]
fn strict_object_append_chain_preserves_two_calls_and_original_live_builder_alias() {
    let (class, _) = strict_object_builder_fixture();
    let code = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
    assert_eq!(
        code.source.matches(".append(").count(),
        2,
        "{}",
        code.source
    );
    assert_eq!(
        code.source.matches("new java.lang.StringBuilder()").count(),
        1,
        "{}",
        code.source
    );
    assert_eq!(
        code.source.matches(".toString()").count(),
        1,
        "{}",
        code.source
    );
    let new_at = code.source.find("new sample.MessageException").unwrap();
    assert!(
        new_at < code.source.find(".append(").unwrap(),
        "{}",
        code.source
    );
    assert_eq!(
        code.links
            .iter()
            .filter(|l| l.label
                == "java.lang.StringBuilder.append(Ljava/lang/Object;)Ljava/lang/StringBuilder;")
            .count(),
        2
    );
    assert!(
        code.links
            .iter()
            .any(|l| l.label == "java.lang.IllegalArgumentException.<init>(Ljava/lang/String;)V")
    );
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn strict_object_append_jvm_preserves_class_init_text_aliases_and_throw_identity() {
    use std::{fs, process::Command};
    let (class, leaf) = strict_object_builder_fixture();
    let code = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
    let leaf_code = native_java::render("sample.MessageException", &leaf).unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-object-builder-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(
        dir.join("sample/MessageException.java"),
        leaf_code
            .source
            .replacen("{", "{ static { Effects.trace += \"A\"; }", 1),
    )
    .unwrap();
    let harness = r#"package sample;
class Effects {
    static String trace=""; static int throwAt; static int calls; static final RuntimeException marker=new RuntimeException();
    public String toString() { trace += "T"; if (++calls == throwAt) throw marker; return "x"; }
}
public class Test {
METHOD
    public static void main(String[] args) {
        StringBuilder result=make(new Effects());
        if (!Effects.trace.equals("ATT") || !result.toString().equals("xx") || Effects.calls!=2) throw new AssertionError(Effects.trace);
        for (int n=1;n<=2;n++) {
            Effects.trace=""; Effects.calls=0; Effects.throwAt=n;
            try { make(new Effects()); throw new AssertionError(); }
            catch(RuntimeException actual) { if(actual!=Effects.marker || Effects.calls!=n || !Effects.trace.equals(n==1?"T":"TT")) throw new AssertionError(Effects.trace); }
        }
        Effects.throwAt=0; Effects.trace=""; Effects.calls=0;
        if(!make(null).toString().equals("nullnull") || Effects.calls!=0) throw new AssertionError();
    }
}"#;
    fs::write(
        dir.join("sample/Test.java"),
        harness.replace("METHOD", &code.source),
    )
    .unwrap();
    for (program, arguments) in [
        (
            "javac",
            vec!["sample/Test.java", "sample/MessageException.java"],
        ),
        ("java", vec!["sample.Test"]),
    ] {
        let result = Command::new(program)
            .args(arguments)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{program}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
