//! G01-B-fields: public renderer fixtures independent of shared operand helpers.
use rdx::{
    native_dex::{self, DexClass, DexSymbols},
    native_java,
};
use std::{fs, process::Command, sync::Arc};

const TYPES: [(&str, &str, u16); 9] = [
    ("I", "I", 0),
    ("F", "F", 0),
    ("J", "J", 1),
    ("D", "D", 1),
    ("O", "Ljava/lang/Object;", 2),
    ("Z", "Z", 3),
    ("B", "B", 4),
    ("C", "C", 5),
    ("S", "S", 6),
];
struct Case {
    name: String,
    opcode: u16,
    ty: &'static str,
    field: String,
    parameters: Vec<&'static str>,
    result: &'static str,
    registers: u16,
    words: Vec<u16>,
}
fn width(ty: &str) -> u16 {
    if matches!(ty, "J" | "D") { 2 } else { 1 }
}
fn ret(ty: &str) -> u16 {
    if ty.starts_with('L') {
        0x11
    } else if width(ty) == 2 {
        0x10
    } else {
        0x0f
    }
}
fn fixture(case: &Case) -> DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class.methods.retain(|m| m.name.as_ref() == "answer");
    class.symbols = Arc::new(DexSymbols {
        strings: vec![case.field.as_str().into()],
        types: vec!["Lsample/State;".into(), case.ty.into()],
        fields: vec![(0, 1, 0)],
        ..Default::default()
    });
    let method = &mut class.methods[0];
    method.name = case.name.as_str().into();
    method.access_flags = 9;
    method.parameters = case.parameters.iter().copied().map(Into::into).collect();
    method.return_type = case.result.into();
    let code = method.code.as_mut().unwrap();
    code.ins = case.parameters.iter().map(|ty| width(ty)).sum();
    code.registers = case.registers;
    code.outs = 0;
    code.instructions = case.words.clone();
    class
}
fn render(case: &Case) -> anyhow::Result<String> {
    let class = fixture(case);
    Ok(native_java::render_method("sample.Fields", &class, &class.methods[0])?.source)
}
fn cases() -> Vec<Case> {
    let mut cases = Vec::new();
    for (tag, ty, family) in TYPES {
        for (base, is_static, put) in [
            (0x52, false, false),
            (0x59, false, true),
            (0x60, true, false),
            (0x67, true, true),
        ] {
            let opcode = base + family;
            let parameters = match (is_static, put) {
                (false, false) => vec!["Lsample/State;"],
                (false, true) => vec!["Lsample/State;", ty],
                (true, false) => vec![],
                (true, true) => vec![ty],
            };
            let operand = match (is_static, put) {
                (false, false) => 0x2000,
                (false, true) => 0x2300,
                (true, false) => 0,
                (true, true) => 0x0200,
            };
            let mut words = vec![operand | opcode, 0];
            if !is_static && !put {
                words.push(0x0212);
            } // destroy receiver after reading: result must be snapshotted.
            words.push(if put { 0x000e } else { ret(ty) });
            let registers = 2 + parameters.iter().map(|ty| width(ty)).sum::<u16>();
            cases.push(Case {
                name: format!("op{opcode:02x}{tag}"),
                opcode,
                ty,
                field: format!("{}{tag}", if is_static { "s" } else { "f" }),
                parameters,
                result: if put { "V" } else { ty },
                registers,
                words,
            });
        }
    }
    cases.push(Case {
        name: "aliasGet".into(),
        opcode: 0x54,
        ty: "Ljava/lang/Object;",
        field: "fO".into(),
        parameters: vec!["Lsample/State;"],
        result: "Ljava/lang/Object;",
        registers: 3,
        words: vec![0x2254, 0, 0x0211],
    });
    cases.push(Case {
        name: "aliasPut".into(),
        opcode: 0x5b,
        ty: "Ljava/lang/Object;",
        field: "fO".into(),
        parameters: vec!["Lsample/State;"],
        result: "V",
        registers: 3,
        words: vec![0x225b, 0, 0x000e],
    });
    cases.push(Case {
        name: "exchangeInstance".into(),
        opcode: 0x52,
        ty: "I",
        field: "fI".into(),
        parameters: vec!["Lsample/State;", "I"],
        result: "I",
        registers: 4,
        words: vec![0x2052, 0, 0x2359, 0, 0x000f],
    });
    cases.push(Case {
        name: "exchangeStatic".into(),
        opcode: 0x60,
        ty: "I",
        field: "sI".into(),
        parameters: vec!["I"],
        result: "I",
        registers: 3,
        words: vec![0x0060, 0, 0x0267, 0, 0x000f],
    });
    cases
}

#[test]
fn all_field_encodings_preserve_types_receivers_and_field_links() {
    let cases = cases();
    assert_eq!(cases.len(), 40);
    assert_eq!(
        cases
            .iter()
            .map(|c| c.opcode)
            .collect::<std::collections::BTreeSet<_>>(),
        (0x52..=0x6d).collect()
    );
    for case in cases {
        let class = fixture(&case);
        let code = native_java::render_method("sample.Fields", &class, &class.methods[0])
            .unwrap_or_else(|e| panic!("{}: {e:#}", case.name));
        assert!(
            code.source.contains(&format!(".{}", case.field)),
            "{}: {}",
            case.name,
            code.source
        );
        let label = format!("sample.State.{}:{}", case.field, case.ty);
        assert!(
            code.links.iter().any(|link| link.label == label),
            "{} lost {label}",
            case.name
        );
        assert!(!code.source.contains(".method"));
    }
}

#[test]
fn field_opcode_types_and_missing_pool_entries_are_rejected() {
    for mut case in cases().into_iter().take(36) {
        let wrong = if case.ty == "Ljava/lang/Object;" {
            "I"
        } else {
            "Ljava/lang/Object;"
        };
        case.ty = wrong;
        let class = fixture(&case);
        assert!(
            native_java::render_method("sample.Fields", &class, &class.methods[0]).is_err(),
            "{} accepted wrong field type",
            case.name
        );
    }
    for slot in 0..4 {
        let case = cases().remove(0);
        let mut class = fixture(&case);
        let symbols = Arc::get_mut(&mut class.symbols).unwrap();
        match slot {
            0 => symbols.fields.clear(),
            1 => symbols.fields[0].0 = 99,
            2 => symbols.fields[0].1 = 99,
            3 => symbols.fields[0].2 = 99,
            _ => unreachable!(),
        }
        assert!(
            native_java::render_method("sample.Fields", &class, &class.methods[0]).is_err(),
            "accepted missing pool slot {slot}"
        );
    }
}
#[test]
fn wide_field_gets_cannot_write_a_pair_past_the_register_frame() {
    for mut case in cases()
        .into_iter()
        .filter(|c| matches!(c.ty, "J" | "D") && matches!(c.opcode, 0x53 | 0x61))
    {
        let destination = if case.opcode == 0x53 { 2 } else { 1 };
        case.words[0] = if case.opcode == 0x53 { 0x2253 } else { 0x0161 };
        *case.words.last_mut().unwrap() = (destination << 8) | 0x10;
        assert!(
            render(&case).is_err(),
            "{} accepted an out-of-frame wide write",
            case.name
        );
    }
}

#[test]
fn wide_field_puts_cannot_read_a_pair_past_the_register_frame() {
    for mut case in cases()
        .into_iter()
        .filter(|c| matches!(c.ty, "J" | "D") && matches!(c.opcode, 0x5a | 0x68))
    {
        // Keep the wide parameter itself valid, but select its upper word as
        // the encoded source head, whose second word lies outside the frame.
        let source = case.registers - 1;
        case.words[0] = if case.opcode == 0x5a {
            0x2000 | (source << 8) | 0x5a
        } else {
            (source << 8) | 0x68
        };
        assert!(
            render(&case).is_err(),
            "{} accepted an out-of-frame wide read",
            case.name
        );
    }
}

fn java_type(ty: &str) -> &'static str {
    match ty {
        "I" => "int",
        "F" => "float",
        "J" => "long",
        "D" => "double",
        "Ljava/lang/Object;" => "Object",
        "Z" => "boolean",
        "B" => "byte",
        "C" => "char",
        "S" => "short",
        _ => unreachable!(),
    }
}
fn values(tag: &str) -> &'static str {
    match tag {
        "I" => "new int[]{Integer.MIN_VALUE, -1, 0, Integer.MAX_VALUE}",
        "F" => {
            "new float[]{Float.intBitsToFloat(0x7fc12345), -0.0f, 0.0f, Float.POSITIVE_INFINITY, Float.MIN_VALUE}"
        }
        "J" => "new long[]{Long.MIN_VALUE, -1L, 0L, Long.MAX_VALUE}",
        "D" => {
            "new double[]{Double.longBitsToDouble(0x7ff8123456789abcL), -0.0d, 0.0d, Double.NEGATIVE_INFINITY, Double.MIN_VALUE}"
        }
        "O" => "new Object[]{null, new Object(), a, b}",
        "Z" => "new boolean[]{false, true}",
        "B" => "new byte[]{-128, -1, 0, 127}",
        "C" => "new char[]{0, 127, 32768, 65535}",
        "S" => "new short[]{-32768, -1, 0, 32767}",
        _ => unreachable!(),
    }
}
fn equal(tag: &str, left: &str, right: &str) -> String {
    match tag {
        "F" => format!("Float.floatToRawIntBits({left}) == Float.floatToRawIntBits({right})"),
        "D" => format!("Double.doubleToRawLongBits({left}) == Double.doubleToRawLongBits({right})"),
        _ => format!("{left} == {right}"),
    }
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn fields_jvm_preserves_identity_bits_null_exceptions_order_and_initialization() {
    let methods = cases()
        .iter()
        .map(|case| render(case).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    let mut declarations = String::new();
    let mut checks = String::new();
    for (tag, ty, family) in TYPES {
        declarations.push_str(&format!(
            "public {} f{tag}; public static {} s{tag};\n",
            java_type(ty),
            java_type(ty)
        ));
        let get = format!("op{:02x}{tag}", 0x52 + family);
        let put = format!("op{:02x}{tag}", 0x59 + family);
        let sget = format!("op{:02x}{tag}", 0x60 + family);
        let sput = format!("op{:02x}{tag}", 0x67 + family);
        let instance = equal(tag, &format!("{get}(a)"), "x");
        let stored = equal(tag, &format!("a.f{tag}"), "x");
        let static_read = equal(tag, &format!("{sget}()"), "x");
        let static_stored = equal(tag, &format!("State.s{tag}"), "x");
        let other = equal(tag, &format!("b.f{tag}"), "sentinel");
        checks.push_str(&format!("for ({} x : {}) {{\n{} sentinel = b.f{tag}; a.f{tag} = x;\nif (!({instance})) throw new AssertionError(\"{get}\");\na.f{tag} = sentinel; {put}(a, x); if (!({stored}) || !({other})) throw new AssertionError(\"{put} receiver\");\nState.s{tag} = x; if (!({static_read})) throw new AssertionError(\"{sget}\");\nState.s{tag} = sentinel; {sput}(x); if (!({static_stored})) throw new AssertionError(\"{sput}\");\ntry {{ {get}(null); throw new AssertionError(\"{get} null\"); }} catch (NullPointerException expected) {{}}\ntry {{ {put}(null, x); throw new AssertionError(\"{put} null\"); }} catch (NullPointerException expected) {{}}\n}}\n", java_type(ty), values(tag), java_type(ty)));
    }
    let java = format!(
        r#"package sample;
class Hook {{ static int count; static int initialize() {{ count++; return 73; }} }}
class State {{ {declarations} static {{ sI = Hook.initialize(); }} }}
public class Fields {{
{methods}
public static void main(String[] args) {{
if (Hook.count != 0) throw new AssertionError("premature initialization");
if (op60I() != 73 || Hook.count != 1) throw new AssertionError("static initialization");
if (op60I() != 73 || Hook.count != 1) throw new AssertionError("repeat initialization");
State a = new State(), b = new State();
{checks}
a.fO = b; if (aliasGet(a) != b) throw new AssertionError("alias get");
aliasPut(a); if (a.fO != a) throw new AssertionError("alias put");
a.fI = 19; b.fI = 29;
if (exchangeInstance(a, 41) != 19 || a.fI != 41 || b.fI != 29) throw new AssertionError("instance read/write order");
State.sI = 53; if (exchangeStatic(67) != 53 || State.sI != 67) throw new AssertionError("static read/write order");
State.sI = 79; try {{ exchangeInstance(null, 83); throw new AssertionError("missing null exception"); }} catch (NullPointerException expected) {{}}
if (State.sI != 79 || Hook.count != 1) throw new AssertionError("unexpected effects");
}}
}}
"#
    );
    let dir = std::env::temp_dir().join(format!("rdx-shared-fields-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(dir.join("sample/Fields.java"), &java).unwrap();
    for (program, argument) in [("javac", "sample/Fields.java"), ("java", "sample.Fields")] {
        let output = Command::new(program)
            .arg(argument)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{program}: {}\nSource: {}",
            String::from_utf8_lossy(&output.stderr),
            dir.join("sample/Fields.java").display()
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
