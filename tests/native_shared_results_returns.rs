//! G01-B-results-returns: public Java-renderer and JVM behavior checks.
use rdx::{
    native_dex::{self, DexClass, DexSymbols},
    native_java,
};
use std::{fs, process::Command, sync::Arc};

struct Case {
    name: &'static str,
    parameter: Option<&'static str>,
    result: &'static str,
    words: Vec<u16>,
    registers: u16,
    producer: Option<(&'static str, &'static str, Vec<&'static str>)>,
}

fn fixture(case: &Case) -> DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class
        .methods
        .retain(|method| method.name.as_ref() == "answer");
    let method = &mut class.methods[0];
    method.name = case.name.into();
    method.access_flags = 9;
    method.parameters = case.parameter.into_iter().map(Into::into).collect();
    method.return_type = case.result.into();
    let code = method.code.as_mut().unwrap();
    code.registers = case.registers;
    code.ins = case
        .parameter
        .map_or(0, |ty| u16::from(matches!(ty, "J" | "D")) + 1);
    code.outs = case.producer.as_ref().map_or(0, |(_, _, args)| {
        args.iter()
            .map(|arg| u16::from(matches!(*arg, "J" | "D")) + 1)
            .sum()
    });
    code.instructions = case.words.clone();
    if let Some((name, result, args)) = &case.producer {
        class.symbols = Arc::new(DexSymbols {
            strings: vec![(*name).into()],
            types: vec!["Lsample/Results;".into()],
            protos: vec![(
                (*result).into(),
                args.iter().copied().map(Into::into).collect(),
            )],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        });
    }
    class
}

fn render(case: &Case) -> anyhow::Result<String> {
    let class = fixture(case);
    Ok(native_java::render_method("sample.Results", &class, &class.methods[0])?.source)
}

fn direct_return_cases() -> Vec<Case> {
    [
        ("returnInt", "I", 0x000f, 1),
        ("returnObject", "Ljava/lang/Object;", 0x0011, 1),
        ("returnLong", "J", 0x0010, 2),
        ("returnFloat", "F", 0x000f, 1),
        ("returnDouble", "D", 0x0010, 2),
    ]
    .into_iter()
    .map(|(name, ty, opcode, registers)| Case {
        name,
        parameter: Some(ty),
        result: ty,
        words: vec![opcode],
        registers,
        producer: None,
    })
    .chain(std::iter::once(Case {
        name: "returnVoid",
        parameter: None,
        result: "V",
        words: vec![0x000e],
        registers: 0,
        producer: None,
    }))
    .collect()
}

fn result_cases() -> Vec<Case> {
    [
        ("resultInt", "I", 0x000a, 0x000f, 2, vec![0x7112]),
        (
            "resultObject",
            "Ljava/lang/Object;",
            0x000c,
            0x0011,
            2,
            vec![0x0112],
        ),
        ("resultLong", "J", 0x000b, 0x0010, 4, vec![0x0216, 0]),
        ("resultFloat", "F", 0x000a, 0x000f, 2, vec![0x7112]),
        ("resultDouble", "D", 0x000b, 0x0010, 4, vec![0x0216, 0]),
    ]
    .into_iter()
    .map(|(name, ty, move_result, ret, registers, overwrite)| {
        let wide = matches!(ty, "J" | "D");
        let (invoke, input) = if wide { (0x2071, 0x0032) } else { (0x1071, 1) };
        let mut words = vec![invoke, 0, input, move_result];
        words.extend(overwrite);
        words.push(ret);
        Case {
            name,
            parameter: Some(ty),
            result: ty,
            words,
            registers,
            producer: Some(("produce", ty, vec![ty])),
        }
    })
    .collect()
}

fn effect_case() -> Case {
    Case {
        name: "orderedResult",
        parameter: None,
        result: "I",
        // Both results target v0; the second call must execute after the first.
        words: vec![0x0071, 0, 0, 0x000a, 0x0071, 0, 0, 0x000a, 0x000f],
        registers: 1,
        producer: Some(("effect", "I", vec![])),
    }
}

#[test]
fn return_family_preserves_declared_result_and_register_identity() {
    for case in direct_return_cases() {
        let source = render(&case).unwrap_or_else(|error| panic!("{}: {error:#}", case.name));
        assert!(source.contains("return"), "{}: {source}", case.name);
        assert!(!source.contains(".method"), "{}: {source}", case.name);
    }
}

#[test]
fn result_variants_bind_the_adjacent_call_and_snapshot_before_overwrite() {
    for case in result_cases() {
        let source = render(&case).unwrap_or_else(|error| panic!("{}: {error:#}", case.name));
        assert!(source.contains("produce("), "{}: {source}", case.name);
        assert!(source.contains("return "), "{}: {source}", case.name);
    }
    let source = render(&effect_case()).unwrap();
    assert_eq!(source.matches("effect()").count(), 2, "{source}");
}

#[test]
fn invalid_result_ownership_and_return_kinds_are_rejected() {
    let base = result_cases().remove(0);
    let mut cases = Vec::new();
    let mut orphan = Case {
        words: vec![0x000a, 0x000f],
        ..base
    };
    orphan.name = "orphan";
    cases.push(orphan);
    let mut separated = result_cases().remove(0);
    separated.name = "separated";
    separated.words.insert(3, 0x0000);
    cases.push(separated);
    let mut wrong_kind = result_cases().remove(0);
    wrong_kind.name = "wrongKind";
    wrong_kind.words[3] = 0x000c;
    cases.push(wrong_kind);
    let mut void_result = result_cases().remove(0);
    void_result.name = "voidResult";
    void_result.producer = Some(("effect", "V", vec!["I"]));
    cases.push(void_result);
    for (name, result, words) in [
        ("voidWithValue", "V", vec![0x0012, 0x000f]),
        ("intAsVoid", "I", vec![0x000e]),
        ("intAsWide", "I", vec![0x0010]),
        ("longAsNarrow", "J", vec![0x000f]),
        ("objectAsNarrow", "Ljava/lang/Object;", vec![0x000f]),
    ] {
        cases.push(Case {
            name,
            parameter: None,
            result,
            words,
            registers: 2,
            producer: None,
        });
    }
    for case in cases {
        assert!(render(&case).is_err(), "{} was accepted", case.name);
    }
}

#[test]
fn constructor_cannot_return_before_initializing_this() {
    let case = Case {
        name: "ctor",
        parameter: None,
        result: "V",
        words: vec![0x000e],
        registers: 1,
        producer: None,
    };
    let mut class = fixture(&case);
    class.methods[0].name = "<init>".into();
    class.methods[0].access_flags = 1;
    class.methods[0].code.as_mut().unwrap().ins = 1;
    assert!(native_java::render_method("sample.Results", &class, &class.methods[0]).is_err());
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn results_returns_jvm_preserves_identity_raw_bits_effects_and_exceptions() {
    let methods = direct_return_cases()
        .into_iter()
        .chain(result_cases())
        .chain(std::iter::once(effect_case()))
        .map(|case| render(&case).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    let java = format!(
        r#"package sample;
public class Results {{
  static String trace = "";
  static int calls;
  static int failAt = -1;
  static final RuntimeException sentinel = new RuntimeException("sentinel");
  public static int produce(int x) {{ return x ^ 0x5a5a5a5a; }}
  public static Object produce(Object x) {{ return x; }}
  public static long produce(long x) {{ return x ^ 0x5a5a5a5a12345678L; }}
  public static float produce(float x) {{ return x; }}
  public static double produce(double x) {{ return x; }}
  public static int effect() {{ trace += ++calls; if (calls == failAt) throw sentinel; return calls; }}
  {methods}
  public static void main(String[] args) {{
    for (int x : new int[] {{Integer.MIN_VALUE, -1, 0, 1, Integer.MAX_VALUE}}) {{
      if (returnInt(x) != x || resultInt(x) != produce(x)) throw new AssertionError("narrow");
    }}
    for (long x : new long[] {{Long.MIN_VALUE, -1L, 0L, 1L, Long.MAX_VALUE}}) {{
      if (returnLong(x) != x || resultLong(x) != produce(x)) throw new AssertionError("wide");
    }}
    for (Object x : new Object[] {{null, new Object()}}) {{
      if (returnObject(x) != x || resultObject(x) != x) throw new AssertionError("identity");
    }}
    for (int bits : new int[] {{0x80000000, 0x7fc12345}}) {{
      float x = Float.intBitsToFloat(bits);
      if (Float.floatToRawIntBits(returnFloat(x)) != bits || Float.floatToRawIntBits(resultFloat(x)) != bits) throw new AssertionError("float bits");
    }}
    for (long bits : new long[] {{0x8000000000000000L, 0x7ff8123456789abcL}}) {{
      double x = Double.longBitsToDouble(bits);
      if (Double.doubleToRawLongBits(returnDouble(x)) != bits || Double.doubleToRawLongBits(resultDouble(x)) != bits) throw new AssertionError("double bits");
    }}
    returnVoid();
    for (int fail : new int[] {{-1, 1, 2}}) {{
      calls = 0; trace = ""; failAt = fail;
      try {{
        int result = orderedResult();
        if (fail != -1 || result != 2) throw new AssertionError("missing exception or result");
      }} catch (RuntimeException ex) {{
        if (fail == -1 || ex != sentinel) throw new AssertionError("exception identity", ex);
      }}
      String expected = fail == 1 ? "1" : "12";
      if (!trace.equals(expected)) throw new AssertionError("effect order " + trace);
    }}
  }}
}}
"#
    );
    let dir = std::env::temp_dir().join(format!("rdx-results-returns-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(dir.join("sample/Results.java"), &java).unwrap();
    for (program, argument) in [("javac", "sample/Results.java"), ("java", "sample.Results")] {
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
