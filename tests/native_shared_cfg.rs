//! G01-C-start: bounded forward diamonds through the public Java renderer.
//! Conditions and JVM reference outcomes are independent of production CFG code.
use rdx::{
    native_dex::{self, DexClass, DexSymbols},
    native_java,
};
use std::{fs, process::Command, sync::Arc};
struct Case {
    name: String,
    opcode: u16,
    parameters: Vec<&'static str>,
    result: &'static str,
    words: Vec<u16>,
    effect: bool,
}
fn width(ty: &str) -> u16 {
    if matches!(ty, "J" | "D") { 2 } else { 1 }
}
fn fixture(case: &Case) -> DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class.methods.retain(|m| m.name.as_ref() == "answer");
    let m = &mut class.methods[0];
    m.name = case.name.as_str().into();
    m.access_flags = 9;
    m.parameters = case.parameters.iter().copied().map(Into::into).collect();
    m.return_type = case.result.into();
    let code = m.code.as_mut().unwrap();
    code.ins = case.parameters.iter().map(|ty| width(ty)).sum();
    code.registers = 2 + code.ins;
    code.outs = u16::from(case.effect);
    code.instructions = case.words.clone();
    if case.effect {
        class.symbols = Arc::new(DexSymbols {
            strings: ["condition", "left", "right", "tail"]
                .into_iter()
                .map(Into::into)
                .collect(),
            types: vec!["Lsample/Hook;".into()],
            protos: vec![("I".into(), vec!["I".into()]), ("I".into(), vec![])],
            methods: vec![(0, 0, 0), (0, 1, 1), (0, 1, 2), (0, 0, 3)],
            ..Default::default()
        });
    }
    class
}
fn render(case: &Case) -> anyhow::Result<String> {
    let class = fixture(case);
    Ok(native_java::render_method("sample.Diamonds", &class, &class.methods[0])?.source)
}
fn diamond(name: &str, opcode: u16, ty: &'static str, goto: u16) -> Case {
    let pair = opcode < 0x38;
    let gw = goto - 0x27;
    let mut words = vec![
        if pair {
            0x3200 | opcode
        } else {
            0x0200 | opcode
        },
        3 + gw,
        0x9012,
    ];
    match goto {
        0x28 => words.push(((gw + 1) << 8) | goto),
        0x29 => words.extend([goto, gw + 1]),
        0x2a => words.extend([goto, gw + 1, 0]),
        _ => unreachable!(),
    }
    words.extend([0x5012, 0x00d8, 0x0b00, 0x000f]);
    Case {
        name: name.into(),
        opcode,
        parameters: if pair { vec![ty, ty] } else { vec![ty] },
        result: "I",
        words,
        effect: false,
    }
}
fn cases() -> Vec<Case> {
    let mut out = (0x32..=0x3d)
        .map(|op| diamond(&format!("signed{op:02x}"), op, "I", 0x28 + (op - 0x32) % 3))
        .collect::<Vec<_>>();
    for op in [0x32, 0x33, 0x38, 0x39] {
        out.push(diamond(
            &format!("reference{op:02x}"),
            op,
            "Ljava/lang/Object;",
            0x28,
        ));
        out.push(diamond(&format!("boolean{op:02x}"), op, "Z", 0x29));
    }
    out.push(Case {
        name: "bypass".into(),
        opcode: 0x38,
        parameters: vec!["I"],
        result: "I",
        words: vec![0x5012, 0x0238, 3, 0x9012, 0x00d8, 0x0b00, 0x000f],
        effect: false,
    });
    out.push(Case {
        name: "referenceLiveout".into(),
        opcode: 0x38,
        parameters: vec!["Ljava/lang/Object;", "Ljava/lang/Object;"],
        result: "Ljava/lang/Object;",
        words: vec![0x0238, 4, 0x3007, 0x0228, 0x2007, 0x0011],
        effect: false,
    });
    out.push(Case {
        name: "wideLiveout".into(),
        opcode: 0x38,
        parameters: vec!["I", "J", "J"],
        result: "J",
        words: vec![0x0238, 4, 0x5004, 0x0228, 0x3004, 0x0010],
        effect: false,
    });
    out.push(Case {
        name: "deadIncompatible".into(),
        opcode: 0x38,
        parameters: vec!["I", "Ljava/lang/Object;"],
        result: "I",
        words: vec![0x0238, 5, 0x9012, 0x1112, 0x0328, 0x5012, 0x3107, 0x000f],
        effect: false,
    });
    out.push(Case {
        name: "effects".into(),
        opcode: 0x38,
        parameters: vec!["I"],
        result: "I",
        words: vec![
            0x1071, 0, 2, 0x010a, 0x0138, 7, 0x0071, 1, 0, 0x000a, 0x0528, 0x0071, 2, 0, 0x000a,
            0x1071, 3, 0, 0x000a, 0x000f,
        ],
        effect: true,
    });
    out.push(Case {
        name: "bothReturn".into(),
        opcode: 0x38,
        parameters: vec!["I"],
        result: "I",
        words: vec![0x0238, 4, 0x9012, 0x000f, 0x5012, 0x000f],
        effect: false,
    });
    out.push(Case {
        name: "earlyThrow".into(),
        opcode: 0x38,
        parameters: vec!["I", "Ljava/lang/RuntimeException;"],
        result: "I",
        words: vec![0x0238, 3, 0x0327, 0x5012, 0x000f],
        effect: false,
    });
    out.push(Case {
        name: "bothThrow".into(),
        opcode: 0x38,
        parameters: vec![
            "I",
            "Ljava/lang/RuntimeException;",
            "Ljava/lang/RuntimeException;",
        ],
        result: "V",
        words: vec![0x0238, 3, 0x0327, 0x0427],
        effect: false,
    });
    out
}
#[test]
fn forward_diamonds_cover_all_conditions_goto_widths_and_liveout_kinds() {
    let cases = cases();
    assert_eq!(cases.len(), 28);
    assert_eq!(
        cases
            .iter()
            .map(|c| c.opcode)
            .collect::<std::collections::BTreeSet<_>>(),
        (0x32..=0x3d).collect()
    );
    for case in cases {
        let source = render(&case).unwrap_or_else(|e| panic!("{}: {e:#}", case.name));
        assert!(source.contains("if ("), "{}: {source}", case.name);
        assert_eq!(
            source.matches("return ").count(),
            if case.name == "bothReturn" {
                2
            } else if case.name == "bothThrow" {
                0
            } else {
                1
            },
            "{} duplicated shared exit: {source}",
            case.name
        );
        if case.name == "effects" {
            for method in ["condition", "left", "right", "tail"] {
                assert_eq!(
                    source.matches(&format!("Hook.{method}(")).count(),
                    1,
                    "{source}"
                );
            }
        }
    }
}
#[test]
fn malformed_edges_and_incompatible_live_conditions_fail_closed() {
    let base = || diamond("bad", 0x32, "I", 0x28);
    for offset in [1, 99, 0] {
        let mut case = base();
        case.words[1] = offset;
        assert!(render(&case).is_err(), "accepted branch offset {offset}");
    }
    let mut inside_tail = base();
    inside_tail.words[3] = 0x0328;
    assert!(
        render(&inside_tail).is_err(),
        "accepted goto into operand word"
    );
    let mut outside = base();
    outside.words[0] = 0xf232;
    assert!(render(&outside).is_err(), "accepted out-of-frame condition");
    for ty in ["Ljava/lang/Object;", "J", "F", "D"] {
        let case = diamond("badType", 0x34, ty, 0x28);
        assert!(render(&case).is_err(), "accepted ordered condition {ty}");
    }
    let undefined = Case {
        name: "undefinedLiveout".into(),
        opcode: 0x38,
        parameters: vec!["I"],
        result: "I",
        words: vec![0x0238, 3, 0x1012, 0x000f],
        effect: false,
    };
    assert!(render(&undefined).is_err());
    let incompatible = Case {
        name: "incompatibleLiveout".into(),
        opcode: 0x38,
        parameters: vec!["I", "Ljava/lang/Object;"],
        result: "I",
        words: vec![0x0238, 4, 0x1012, 0x0228, 0x3007, 0x000f],
        effect: false,
    };
    assert!(render(&incompatible).is_err());
}
fn condition(opcode: u16) -> &'static str {
    match opcode {
        0x32 | 0x38 => "==",
        0x33 | 0x39 => "!=",
        0x34 | 0x3a => "<",
        0x35 | 0x3b => ">=",
        0x36 | 0x3c => ">",
        0x37 | 0x3d => "<=",
        _ => unreachable!(),
    }
}
#[test]
#[ignore = "requires javac and java on PATH"]
fn diamonds_jvm_match_both_arms_values_identity_effect_order_and_exceptions() {
    let methods = cases()
        .iter()
        .map(|case| render(case).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    let mut checks = String::new();
    for case in cases().into_iter().take(20) {
        let ty = if case.name.starts_with("reference") {
            "Object"
        } else if case.name.starts_with("boolean") {
            "boolean"
        } else {
            "int"
        };
        let values = match ty {
            "Object" => "new Object[]{null, token, other}",
            "boolean" => "new boolean[]{false, true}",
            _ => "new int[]{Integer.MIN_VALUE, -1, 0, 1, Integer.MAX_VALUE}",
        };
        let pair = case.opcode < 0x38;
        let rhs = if pair {
            "y"
        } else if ty == "Object" {
            "null"
        } else if ty == "boolean" {
            "false"
        } else {
            "0"
        };
        let args = if pair { "x, y" } else { "x" };
        checks.push_str(&format!("for ({ty} x : {values}) {{\n{} int expected = (x {} {rhs}) ? 16 : 4;\nif ({}({args}) != expected) throw new AssertionError(\"{}\");\n{}\n", if pair { format!("for ({ty} y : {values}) {{") } else { String::new() }, condition(case.opcode), case.name, case.name, "}".repeat(if pair { 2 } else { 1 })));
    }
    let java = format!(
        r#"package sample;
class Hook {{
static String trace = ""; static char fail; static final RuntimeException sentinel = new RuntimeException("identity");
static void mark(char stage) {{ trace += stage; if (fail == stage) throw sentinel; }}
static int condition(int x) {{ mark('C'); return x; }}
static int left() {{ mark('L'); return 19; }} static int right() {{ mark('R'); return 29; }}
static int tail(int x) {{ mark('T'); return x * 3; }}
}}
public class Diamonds {{
{methods}
public static void main(String[] args) {{
Object token = new Object(), other = new Object();
{checks}
for (int x : new int[]{{Integer.MIN_VALUE, -1, 0, 1, Integer.MAX_VALUE}}) {{
  if (bypass(x) != (x == 0 ? 16 : 4)) throw new AssertionError("bypass");
  if (referenceLiveout(x == 0 ? null : token, other) != (x == 0 ? null : other)) throw new AssertionError("reference liveout");
  if (deadIncompatible(x, token) != (x == 0 ? 5 : -7)) throw new AssertionError("dead incompatible register");
  for (long left : new long[]{{Long.MIN_VALUE, -1L, 0L, Long.MAX_VALUE}}) for (long right : new long[]{{Long.MIN_VALUE, -1L, 0L, Long.MAX_VALUE}})
    if (wideLiveout(x, left, right) != (x == 0 ? left : right)) throw new AssertionError("wide liveout");
}}
RuntimeException leftException = new RuntimeException("left"), rightException = new RuntimeException("right");
for (int x : new int[]{{Integer.MIN_VALUE, -1, 0, 1, Integer.MAX_VALUE}}) {{
  if (bothReturn(x) != (x == 0 ? 5 : -7)) throw new AssertionError("terminal return arms");
  try {{ int actual = earlyThrow(x, leftException); if (x != 0 || actual != 5) throw new AssertionError("early throw return"); }}
  catch (RuntimeException actual) {{ if (x == 0 || actual != leftException) throw new AssertionError("early throw identity", actual); }}
  try {{ bothThrow(x, leftException, rightException); throw new AssertionError("both throw returned"); }}
  catch (RuntimeException actual) {{ if (actual != (x == 0 ? rightException : leftException)) throw new AssertionError("both throw identity", actual); }}
}}
for (int x : new int[]{{0, 7}}) for (char failure : new char[]{{0, 'C', 'L', 'R', 'T'}}) {{
  Hook.trace = ""; Hook.fail = failure;
  char selected = x == 0 ? 'R' : 'L';
  boolean fails = failure == 'C' || failure == selected || failure == 'T';
  String expectedTrace = failure == 'C' ? "C" : failure == selected ? "C" + selected : "C" + selected + "T";
  try {{ int actual = effects(x); if (fails || actual != (x == 0 ? 87 : 57)) throw new AssertionError("effect result"); }}
  catch (RuntimeException actual) {{ if (!fails || actual != Hook.sentinel) throw new AssertionError("effect exception identity", actual); }}
  if (!Hook.trace.equals(expectedTrace)) throw new AssertionError("effect order " + Hook.trace + " expected " + expectedTrace);
}}
}}
}}
"#
    );
    let dir = std::env::temp_dir().join(format!("rdx-shared-cfg-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(dir.join("sample/Diamonds.java"), &java).unwrap();
    for (program, argument) in [
        ("javac", "sample/Diamonds.java"),
        ("java", "sample.Diamonds"),
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
            dir.join("sample/Diamonds.java").display()
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
