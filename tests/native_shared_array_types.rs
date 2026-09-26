//! G01-B-array-types: public renderer and independent JVM array/type fixtures.
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
    parameters: Vec<String>,
    result: String,
    registers: u16,
    types: Vec<String>,
    words: Vec<u16>,
}
fn width(ty: &str) -> u16 {
    if matches!(ty, "J" | "D") { 2 } else { 1 }
}
fn ret(ty: &str) -> u16 {
    if ty.starts_with(['L', '[']) {
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
        types: case.types.iter().map(|ty| ty.as_str().into()).collect(),
        ..Default::default()
    });
    let m = &mut class.methods[0];
    m.name = case.name.as_str().into();
    m.access_flags = 9;
    m.parameters = case
        .parameters
        .iter()
        .map(|ty| ty.as_str().into())
        .collect();
    m.return_type = case.result.as_str().into();
    let code = m.code.as_mut().unwrap();
    code.ins = case.parameters.iter().map(|ty| width(ty)).sum();
    code.registers = case.registers;
    code.outs = 0;
    code.instructions = case.words.clone();
    class
}
fn render(case: &Case) -> anyhow::Result<String> {
    let class = fixture(case);
    Ok(native_java::render_method("sample.ArrayTypes", &class, &class.methods[0])?.source)
}
fn make(
    name: &str,
    opcode: u16,
    parameters: &[&str],
    result: &str,
    types: &[&str],
    words: Vec<u16>,
) -> Case {
    Case {
        name: name.into(),
        opcode,
        parameters: parameters.iter().map(|s| (*s).into()).collect(),
        result: result.into(),
        registers: 2 + parameters.iter().map(|ty| width(ty)).sum::<u16>(),
        types: types.iter().map(|s| (*s).into()).collect(),
        words,
    }
}
fn cases() -> Vec<Case> {
    let mut out = Vec::new();
    for (tag, ty, family) in TYPES {
        let array = format!("[{ty}");
        out.push(make(
            &format!("get{tag}"),
            0x44 + family,
            &[&array, "I"],
            ty,
            &[],
            vec![0x44 + family, 0x0302, 0x0212, 0x0312, ret(ty)],
        ));
        let value_ty = if matches!(ty, "B" | "C" | "S") {
            "I"
        } else {
            ty
        };
        out.push(make(
            &format!("put{tag}"),
            0x4b + family,
            &[&array, "I", value_ty],
            "V",
            &[],
            vec![0x0400 | (0x4b + family), 0x0302, 0x000e],
        ));
        out.push(make(
            &format!("length{tag}"),
            0x21,
            &[&array],
            "I",
            &[],
            vec![0x2021, 0x0212, 0x000f],
        ));
        out.push(make(
            &format!("new{tag}"),
            0x23,
            &["I"],
            &array,
            &[&array],
            vec![0x2023, 0, 0x0212, 0x0011],
        ));
    }
    for (tag, ty) in [
        ("String", "Ljava/lang/String;"),
        ("IntArray", "[I"),
        ("Objects", "[Ljava/lang/Object;"),
    ] {
        out.push(make(
            &format!("cast{tag}"),
            0x1f,
            &["Ljava/lang/Object;"],
            ty,
            &[ty],
            vec![0x021f, 0, 0x2007, 0x0212, 0x0011],
        ));
        out.push(make(
            &format!("is{tag}"),
            0x20,
            &["Ljava/lang/Object;"],
            "Z",
            &[ty],
            vec![0x2020, 0, 0x0212, 0x000f],
        ));
    }
    out.push(make(
        "newIntMatrix",
        0x23,
        &["I"],
        "[[I",
        &["[[I"],
        vec![0x2023, 0, 0x0011],
    ));
    out.push(make(
        "newStringMatrix",
        0x23,
        &["I"],
        "[[Ljava/lang/String;",
        &["[[Ljava/lang/String;"],
        vec![0x2023, 0, 0x0011],
    ));
    out.push(make(
        "aliasGet",
        0x46,
        &["[Ljava/lang/Object;", "I"],
        "Ljava/lang/Object;",
        &[],
        vec![0x0246, 0x0302, 0x0211],
    ));
    out.push(make(
        "aliasPut",
        0x4d,
        &["[Ljava/lang/Object;", "I"],
        "V",
        &[],
        vec![0x024d, 0x0302, 0x000e],
    ));
    out.push(make(
        "exchange",
        0x44,
        &["[I", "I", "I"],
        "I",
        &[],
        vec![0x0044, 0x0302, 0x044b, 0x0302, 0x000f],
    ));
    out.push(make(
        "putStringArray",
        0x4d,
        &["[Ljava/lang/String;", "I", "Ljava/lang/Object;"],
        "V",
        &[],
        vec![0x044d, 0x0302, 0x000e],
    ));
    out
}
#[test]
fn all_array_type_opcodes_render_and_link_exact_pool_types() {
    let cases = cases();
    assert_eq!(cases.len(), 48);
    let expected = [0x1f, 0x20, 0x21, 0x23]
        .into_iter()
        .chain(0x44..=0x51)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        cases
            .iter()
            .map(|c| c.opcode)
            .collect::<std::collections::BTreeSet<_>>(),
        expected
    );
    for case in cases {
        let class = fixture(&case);
        let code = native_java::render_method("sample.ArrayTypes", &class, &class.methods[0])
            .unwrap_or_else(|e| panic!("{}: {e:#}", case.name));
        assert!(
            code.source.contains("return"),
            "{}: {}",
            case.name,
            code.source
        );
        for ty in &case.types {
            let component = ty.trim_start_matches('[');
            if component.starts_with('L') {
                let label = component
                    .trim_start_matches('L')
                    .trim_end_matches(';')
                    .replace('/', ".");
                let links = code
                    .links
                    .iter()
                    .filter(|link| link.label == label)
                    .collect::<Vec<_>>();
                assert!(
                    !links.is_empty(),
                    "{} lost navigation to {label}",
                    case.name
                );
                for link in links {
                    let span = code
                        .source
                        .chars()
                        .skip(link.start)
                        .take(link.end - link.start)
                        .collect::<String>();
                    let dimensions = ty.chars().take_while(|c| *c == '[').count();
                    let declared = format!("{label}{}", "[]".repeat(dimensions));
                    assert!(
                        span == label || span == declared,
                        "{} navigation span {span:?} must identify {label}",
                        case.name
                    );
                }
            }
        }
    }
}
#[test]
fn malformed_pool_and_array_component_families_fail_closed() {
    for case in cases().into_iter().filter(|c| !c.types.is_empty()) {
        let mut missing = fixture(&case);
        missing.symbols = Arc::new(DexSymbols::default());
        assert!(
            native_java::render_method("sample.ArrayTypes", &missing, &missing.methods[0]).is_err(),
            "{} accepted missing pool",
            case.name
        );
        let mut primitive = fixture(&case);
        primitive.symbols = Arc::new(DexSymbols {
            types: vec!["I".into()],
            ..Default::default()
        });
        assert!(
            native_java::render_method("sample.ArrayTypes", &primitive, &primitive.methods[0])
                .is_err(),
            "{} accepted primitive pool type",
            case.name
        );
    }
    for mut case in cases()
        .into_iter()
        .filter(|c| (0x44..=0x51).contains(&c.opcode))
    {
        case.parameters[0] = if case.opcode == 0x46 || case.opcode == 0x4d {
            "[I"
        } else {
            "[Ljava/lang/Object;"
        }
        .into();
        assert!(
            render(&case).is_err(),
            "{} accepted wrong component",
            case.name
        );
    }
    let mut length = cases().into_iter().find(|c| c.name == "lengthI").unwrap();
    length.parameters[0] = "I".into();
    assert!(render(&length).is_err());
}
#[test]
fn wide_array_accesses_reject_out_of_frame_pairs() {
    for mut case in cases()
        .into_iter()
        .filter(|c| matches!(c.opcode, 0x45 | 0x4c))
    {
        let last = case.registers - 1;
        case.words[0] = (last << 8) | case.opcode;
        assert!(
            render(&case).is_err(),
            "{} accepted out-of-frame pair",
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
            "new float[]{Float.intBitsToFloat(0x7fc12345), -0.0f, 0.0f, Float.POSITIVE_INFINITY}"
        }
        "J" => "new long[]{Long.MIN_VALUE, -1L, 0L, Long.MAX_VALUE}",
        "D" => {
            "new double[]{Double.longBitsToDouble(0x7ff8123456789abcL), -0.0d, 0.0d, Double.NEGATIVE_INFINITY}"
        }
        "O" => "new Object[]{null, new Object(), new int[0]}",
        "Z" => "new boolean[]{false, true}",
        "B" | "C" | "S" => {
            "new int[]{Integer.MIN_VALUE, -32769, -129, -1, 0, 128, 32768, 65535, Integer.MAX_VALUE}"
        }
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
fn array_types_jvm_preserves_values_casts_allocation_and_exception_order() {
    let methods = cases()
        .iter()
        .map(|c| render(c).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    let mut checks = String::new();
    for (tag, ty, _) in TYPES {
        let jt = java_type(ty);
        let narrow = matches!(ty, "B" | "C" | "S");
        let vt = if narrow { "int" } else { jt };
        let expected = if narrow {
            format!("({jt}) x")
        } else {
            "x".into()
        };
        let geteq = equal(tag, &format!("get{tag}(a, 1)"), "expected");
        let puteq = equal(tag, "a[0]", "expected");
        checks.push_str(&format!("for ({vt} x : {}) {{\n{jt} expected = {expected}; {jt}[] a = new {jt}[2]; a[1] = expected;\nif (!({geteq})) throw new AssertionError(\"get{tag}\");\nput{tag}(a, 0, x); if (!({puteq})) throw new AssertionError(\"put{tag}\");\nif (length{tag}(a) != 2) throw new AssertionError(\"length{tag}\");\nexpect(NullPointerException.class, () -> get{tag}(null, -1));\nexpect(NullPointerException.class, () -> put{tag}(null, -1, x));\nexpect(ArrayIndexOutOfBoundsException.class, () -> get{tag}(a, -1));\nexpect(ArrayIndexOutOfBoundsException.class, () -> put{tag}(a, 2, x));\n}}\nfor (int n : new int[]{{0, 1, 3}}) {{ if (new{tag}(n).length != n) throw new AssertionError(\"new{tag}\"); }}\nexpect(NegativeArraySizeException.class, () -> new{tag}(-1));\nexpect(NullPointerException.class, () -> length{tag}(null));\n", values(tag)));
    }
    let java = format!(
        r#"package sample; public class ArrayTypes {{
{methods}
static void expect(Class<? extends Throwable> type, Runnable action) {{
  try {{ action.run(); }} catch (Throwable ex) {{ if (ex.getClass() == type) return; throw new AssertionError("wrong exception", ex); }}
  throw new AssertionError("missing " + type);
}}
public static void main(String[] args) {{
{checks}
String s = new String("identity"); Object[] objects = new Object[1]; int[] ints = new int[1];
if (castString(s) != s || castString(null) != null || !isString(s) || isString(null) || isString(ints)) throw new AssertionError("string cast/test");
if (castObjects(objects) != objects || castObjects(null) != null || !isObjects(objects) || isObjects(null) || isObjects(ints)) throw new AssertionError("object array cast/test");
if (castIntArray(ints) != ints || castIntArray(null) != null || !isIntArray(ints) || isIntArray(null) || isIntArray(objects)) throw new AssertionError("primitive array cast/test");
expect(ClassCastException.class, () -> castString(ints)); expect(ClassCastException.class, () -> castObjects(ints)); expect(ClassCastException.class, () -> castIntArray(objects));
for (int n : new int[]{{0, 1, 3}}) {{
  int[][] matrix = newIntMatrix(n); String[][] strings = newStringMatrix(n);
  if (matrix.length != n || strings.length != n) throw new AssertionError("outer dimension");
  for (int[] row : matrix) if (row != null) throw new AssertionError("allocated inner dimension");
  for (String[] row : strings) if (row != null) throw new AssertionError("allocated reference inner dimension");
}}
expect(NegativeArraySizeException.class, () -> newIntMatrix(-1)); expect(NegativeArraySizeException.class, () -> newStringMatrix(-1));
objects[0] = s; if (aliasGet(objects, 0) != s) throw new AssertionError("alias get"); aliasPut(objects, 0); if (objects[0] != objects) throw new AssertionError("alias put");
String[] strings = new String[1]; Object incompatible = new Object();
expect(ArrayStoreException.class, () -> putStringArray(strings, 0, incompatible));
expect(ArrayIndexOutOfBoundsException.class, () -> putStringArray(strings, 1, incompatible));
expect(NullPointerException.class, () -> putStringArray(null, 1, incompatible));
if (strings[0] != null) throw new AssertionError("failed store modified array");
int[] ordered = new int[]{{19, 29}}; if (exchange(ordered, 0, 41) != 19 || ordered[0] != 41 || ordered[1] != 29) throw new AssertionError("snapshot/order");
expect(ArrayIndexOutOfBoundsException.class, () -> exchange(ordered, 2, 53)); if (ordered[0] != 41 || ordered[1] != 29) throw new AssertionError("effects after failed read");
}}
}}
"#
    );
    let dir = std::env::temp_dir().join(format!("rdx-shared-array-types-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(dir.join("sample/ArrayTypes.java"), &java).unwrap();
    for (program, argument) in [
        ("javac", "sample/ArrayTypes.java"),
        ("java", "sample.ArrayTypes"),
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
            dir.join("sample/ArrayTypes.java").display()
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
