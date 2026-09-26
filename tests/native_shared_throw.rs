//! G01-B-throw: public rendering and independent exception identity/effect proof.
use rdx::{
    native_dex::{self, DexClass, DexSymbols},
    native_java,
};
use std::{fs, process::Command, sync::Arc};
struct Case {
    name: String,
    parameter: Option<&'static str>,
    result: &'static str,
    declared: Vec<&'static str>,
    registers: u16,
    words: Vec<u16>,
    effect: bool,
}
fn fixture(case: &Case) -> DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class.methods.retain(|m| m.name.as_ref() == "answer");
    let method = &mut class.methods[0];
    method.name = case.name.as_str().into();
    method.access_flags = 9;
    method.parameters = case.parameter.into_iter().map(Into::into).collect();
    method.return_type = case.result.into();
    method.thrown_types = case.declared.iter().copied().map(Into::into).collect();
    let code = method.code.as_mut().unwrap();
    code.registers = case.registers;
    code.ins = u16::from(case.parameter.is_some());
    code.outs = 0;
    code.instructions = case.words.clone();
    if case.effect {
        class.symbols = Arc::new(DexSymbols {
            strings: vec!["before".into()],
            types: vec!["Lsample/Hook;".into()],
            protos: vec![("V".into(), vec![])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        });
    }
    class
}
fn render(case: &Case) -> anyhow::Result<String> {
    let class = fixture(case);
    Ok(native_java::render_method("sample.Throwing", &class, &class.methods[0])?.source)
}
fn typed(name: &str, ty: &'static str, result: &'static str, declared: Vec<&'static str>) -> Case {
    Case {
        name: name.into(),
        parameter: Some(ty),
        result,
        declared,
        registers: 3,
        words: vec![0x0227],
        effect: false,
    }
}
fn cases() -> Vec<Case> {
    let mut out = Vec::new();
    for (tag, result) in [
        ("Void", "V"),
        ("Int", "I"),
        ("Long", "J"),
        ("Float", "F"),
        ("Double", "D"),
        ("Object", "Ljava/lang/Object;"),
    ] {
        out.push(typed(
            &format!("terminal{tag}"),
            "Ljava/lang/RuntimeException;",
            result,
            vec![],
        ));
    }
    out.push(typed(
        "illegalState",
        "Ljava/lang/IllegalStateException;",
        "V",
        vec![],
    ));
    out.push(typed("error", "Ljava/lang/Error;", "V", vec![]));
    for (name, ty) in [
        ("checkedIo", "Ljava/io/IOException;"),
        ("checkedException", "Ljava/lang/Exception;"),
        ("checkedThrowable", "Ljava/lang/Throwable;"),
    ] {
        out.push(typed(name, ty, "V", vec![ty]));
    }
    out.push(typed("inferredIo", "Ljava/io/IOException;", "V", vec![]));
    out.push(Case {
        name: "nullLiteral".into(),
        parameter: None,
        result: "I",
        declared: vec![],
        registers: 1,
        words: vec![0x0012, 0x0027],
        effect: false,
    });
    out.push(Case {
        name: "null16".into(),
        parameter: None,
        result: "V",
        declared: vec![],
        registers: 1,
        words: vec![0x0013, 0, 0x0027],
        effect: false,
    });
    let mut high = typed("highRegister", "Ljava/lang/RuntimeException;", "V", vec![]);
    high.registers = 256;
    high.words = vec![0xff27];
    out.push(high);
    // Move the exact exception identity, destroy its original register, then
    // evaluate a preceding effect before throwing the saved value.
    out.push(Case {
        name: "effectSnapshot".into(),
        parameter: Some("Ljava/lang/RuntimeException;"),
        result: "V",
        declared: vec![],
        registers: 3,
        words: vec![0x2007, 0x0212, 0x0071, 0, 0, 0x0027],
        effect: true,
    });
    let mut checked_effect = typed(
        "checkedEffect",
        "Ljava/io/IOException;",
        "V",
        vec!["Ljava/io/IOException;"],
    );
    checked_effect.words = vec![0x0071, 0, 0, 0x0227];
    checked_effect.effect = true;
    out.push(checked_effect);
    out
}
#[test]
fn reference_null_high_register_and_checked_throws_preserve_terminal_identity() {
    let cases = cases();
    assert_eq!(cases.len(), 17);
    for case in cases {
        let source = render(&case).unwrap_or_else(|e| panic!("{}: {e:#}", case.name));
        assert_eq!(
            source.matches("throw ").count(),
            1,
            "{}: {source}",
            case.name
        );
        assert!(
            !source.contains("return ") && !source.contains("return;"),
            "{}: {source}",
            case.name
        );
        if case.parameter.is_none() {
            assert!(source.contains("throw null;"), "{source}");
        }
        if case.name == "effectSnapshot" || case.name == "checkedEffect" {
            assert_eq!(source.matches("Hook.before()").count(), 1, "{source}");
            assert!(
                source.find("Hook.before()").unwrap() < source.find("throw ").unwrap(),
                "{source}"
            );
        }
        if case.parameter == Some("Ljava/io/IOException;") {
            assert!(source.contains("throws java.io.IOException"), "{source}");
            let class = fixture(&case);
            let code =
                native_java::render_method("sample.Throwing", &class, &class.methods[0]).unwrap();
            let links = code
                .links
                .iter()
                .filter(|l| l.label == "java.io.IOException")
                .collect::<Vec<_>>();
            assert!(!links.is_empty());
            for link in links {
                assert_eq!(
                    code.source
                        .chars()
                        .skip(link.start)
                        .take(link.end - link.start)
                        .collect::<String>(),
                    "java.io.IOException"
                );
            }
        }
    }
}
#[test]
fn invalid_types_registers_and_nonterminal_tails_fail_closed() {
    for ty in [
        "I",
        "F",
        "Ljava/lang/Object;",
        "Ljava/lang/String;",
        "[Ljava/lang/Exception;",
        "Lunknown/MaybeException;",
    ] {
        let case = typed("badType", ty, "V", vec![]);
        assert!(render(&case).is_err(), "accepted {ty}");
    }
    for words in [
        vec![0x0027],
        vec![0x0227],
        vec![0x1012, 0x0027],
        vec![0x0027, 0x000e],
    ] {
        let case = Case {
            name: "badWords".into(),
            parameter: None,
            result: "V",
            declared: vec![],
            registers: 1,
            words,
            effect: false,
        };
        assert!(render(&case).is_err());
    }
    let mut tail = typed(
        "unreachableEffect",
        "Ljava/lang/RuntimeException;",
        "V",
        vec![],
    );
    tail.effect = true;
    tail.words = vec![0x0227, 0x0071, 0, 0, 0x000e];
    assert!(render(&tail).is_err());
    for declared in ["Ljava/lang/Exception;", "Ljava/lang/RuntimeException;"] {
        let case = typed(
            "wrongDeclaration",
            "Ljava/io/IOException;",
            "V",
            vec![declared],
        );
        assert!(
            render(&case).is_err(),
            "accepted unproved declared relationship {declared}"
        );
    }
    for ty in ["Ljava/lang/String;", "Lunknown/MaybeException;"] {
        assert!(render(&typed("untrustedDeclaration", ty, "V", vec![ty])).is_err());
    }
}
#[test]
fn checked_initializer_and_constructor_boundaries_remain_conservative() {
    let io = typed("io", "Ljava/io/IOException;", "V", vec![]);
    let mut class = fixture(&io);
    class.superclass = Some("Lunknown/Base;".into());
    assert!(native_java::render_method("sample.Throwing", &class, &class.methods[0]).is_err());
    let mut class = fixture(&typed(
        "clinit",
        "Ljava/io/IOException;",
        "V",
        vec!["Ljava/io/IOException;"],
    ));
    class.methods[0].name = "<clinit>".into();
    class.methods[0].parameters.clear();
    let code = class.methods[0].code.as_mut().unwrap();
    code.ins = 0;
    code.instructions = vec![0x0012, 0x0027];
    assert!(native_java::render_method("sample.Throwing", &class, &class.methods[0]).is_err());
    let mut class = fixture(&typed("ctor", "Ljava/lang/RuntimeException;", "V", vec![]));
    class.methods[0].name = "<init>".into();
    class.methods[0].access_flags = 1;
    let code = class.methods[0].code.as_mut().unwrap();
    code.registers = 2;
    code.ins = 2;
    code.instructions = vec![0x0127];
    let accepted =
        native_java::render_method("sample.Throwing", &class, &class.methods[0]).unwrap();
    assert!(accepted.source.contains("throw p0;"));
    class.superclass = Some("Lunknown/Base;".into());
    assert!(native_java::render_method("sample.Throwing", &class, &class.methods[0]).is_err());
    class.superclass = Some("Ljava/lang/Object;".into());
    class.methods[0].code.as_mut().unwrap().instructions = vec![0x0027];
    assert!(
        native_java::render_method("sample.Throwing", &class, &class.methods[0]).is_err(),
        "accepted uninitialized this as exception"
    );
}
#[test]
#[ignore = "requires javac and java on PATH"]
fn throw_jvm_preserves_identity_null_conversion_and_preceding_effects() {
    let methods = cases()
        .iter()
        .map(|case| render(case).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    let java = format!(
        r#"package sample;
class Hook {{ static int calls; static RuntimeException fail; static void before() {{ calls++; if (fail != null) throw fail; }} }}
public class Throwing {{
{methods}
interface Action {{ void run() throws Throwable; }}
static void same(Throwable expected, Action action) {{
  try {{ action.run(); }} catch (Throwable actual) {{ if (actual == expected) return; throw new AssertionError("exception identity", actual); }}
  throw new AssertionError("missing throw");
}}
static void npe(Action action) {{
  try {{ action.run(); }} catch (Throwable actual) {{ if (actual.getClass() == NullPointerException.class) return; throw new AssertionError("null exception", actual); }}
  throw new AssertionError("missing null throw");
}}
public static void main(String[] args) {{
RuntimeException ex = new RuntimeException("identity");
same(ex, () -> terminalVoid(ex)); same(ex, () -> terminalInt(ex)); same(ex, () -> terminalLong(ex)); same(ex, () -> terminalFloat(ex)); same(ex, () -> terminalDouble(ex)); same(ex, () -> terminalObject(ex)); same(ex, () -> highRegister(ex));
IllegalStateException state = new IllegalStateException(); same(state, () -> illegalState(state));
Error err = new AssertionError(); same(err, () -> error(err));
java.io.IOException io = new java.io.IOException(); same(io, () -> checkedIo(io)); same(io, () -> inferredIo(io));
Exception checked = new Exception(); same(checked, () -> checkedException(checked));
Throwable throwable = new Throwable(); same(throwable, () -> checkedThrowable(throwable));
npe(() -> terminalVoid(null)); npe(() -> checkedIo(null)); npe(() -> checkedThrowable(null)); npe(() -> nullLiteral()); npe(() -> null16());
Hook.calls = 0; same(ex, () -> effectSnapshot(ex)); if (Hook.calls != 1) throw new AssertionError("effect once");
Hook.calls = 0; npe(() -> effectSnapshot(null)); if (Hook.calls != 1) throw new AssertionError("effect before null throw");
Hook.calls = 0; same(io, () -> checkedEffect(io)); if (Hook.calls != 1) throw new AssertionError("checked effect once");
RuntimeException earlier = new RuntimeException("earlier"); Hook.fail = earlier; Hook.calls = 0;
same(earlier, () -> effectSnapshot(ex)); if (Hook.calls != 1) throw new AssertionError("earlier effect failure");
Hook.calls = 0; same(earlier, () -> checkedEffect(io)); if (Hook.calls != 1) throw new AssertionError("checked earlier failure");
}}
}}
"#
    );
    let dir = std::env::temp_dir().join(format!("rdx-shared-throw-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(dir.join("sample/Throwing.java"), &java).unwrap();
    for (program, argument) in [
        ("javac", "sample/Throwing.java"),
        ("java", "sample.Throwing"),
    ] {
        let output = Command::new(program)
            .arg(argument)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{program}: {}\nSource: {}",
            String::from_utf8_lossy(&output.stderr),
            dir.join("sample/Throwing.java").display()
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
