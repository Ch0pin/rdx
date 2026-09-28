use rdx::{
    native_dex::{self, DexClass, DexSymbols},
    native_java,
};
use std::{fs, process::Command, sync::Arc};

fn fixture() -> DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class
        .methods
        .retain(|method| method.name.as_ref() == "answer");
    class.descriptor = "Lsample/BooleanOr;".into();
    class.symbols = Arc::new(DexSymbols {
        strings: vec!["tick".into()],
        types: vec![class.descriptor.clone()],
        protos: vec![("Z".into(), vec![])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    let method = &mut class.methods[0];
    method.declaring_type = class.descriptor.clone();
    method.name = "run".into();
    method.return_type = "Z".into();
    method.parameters = vec!["I".into()];
    method.access_flags = 9;
    let code = method.code.as_mut().unwrap();
    code.registers = 3;
    code.ins = 1;
    code.outs = 0;
    code.tries = 0;
    code.try_regions.clear();
    code.instructions = vec![
        0x0012, // v0 = 0
        0x0238, 10, // if-eqz v2, return
        0x0071, 0, 0,      // tick()Z
        0x010a, // v1 = result
        0x10b6, // v0 |= v1
        0x02d8, 0xff02, // v2--
        0xf728, // goto loop guard
        0x000f, // return v0
    ];
    class
}

fn source(class: &DexClass) -> anyhow::Result<String> {
    Ok(native_java::render_method("sample.BooleanOr", class, &class.methods[0])?.source)
}

fn parameter_fixture(parameter: &str) -> DexClass {
    let mut class = fixture();
    let method = &mut class.methods[0];
    method.parameters = vec![parameter.into()];
    let code = method.code.as_mut().unwrap();
    code.registers = 2;
    code.instructions = vec![0x0012, 0x10b6, 0x000f]; // v0 = 0; v0 |= p0; return v0
    class
}

#[test]
fn zero_initialized_boolean_or_loop_reconstructs() {
    let java = source(&fixture()).unwrap();
    assert!(java.contains("tick()"), "{java}");
    assert!(java.contains("while ("), "{java}");
    assert!(java.contains("!= 0"), "{java}");
}

#[test]
fn exact_boolean_parameter_can_feed_boolean_or() {
    let java = source(&parameter_fixture("Z")).unwrap();
    assert!(java.contains("false | p0"), "{java}");
}

#[test]
fn integer_parameter_cannot_feed_boolean_or() {
    let error = source(&parameter_fixture("I")).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("unsupported register type conversion I to Z"),
        "{error:#}"
    );
}

#[test]
fn nonboolean_initial_value_is_rejected() {
    let mut class = fixture();
    class.methods[0].code.as_mut().unwrap().instructions[0] = 0x2012;
    let error = source(&class).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("unsupported register type conversion I to Z"),
        "{error:#}"
    );
}

#[test]
fn nonboolean_argument_is_rejected() {
    let mut class = fixture();
    class.methods[0].code.as_mut().unwrap().instructions[7] = 0x20b6; // v0 |= integer v2
    let error = source(&class).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("unsupported register type conversion I to Z"),
        "{error:#}"
    );
}

#[test]
fn nonboolean_backedge_write_is_rejected() {
    let mut class = fixture();
    class.methods[0].code.as_mut().unwrap().instructions[7] = 0x2012; // v0 = 2 on every lap
    let error = source(&class).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("unsupported register type conversion I to Z"),
        "{error:#}"
    );
}

#[test]
fn undefined_initial_value_is_rejected() {
    let mut class = fixture();
    class.methods[0].code.as_mut().unwrap().instructions[0] = 0x0000; // v0 uninitialized
    let error = source(&class).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("loop exit value has no established entry type")
            || error.to_string().contains("undefined")
            || error
                .to_string()
                .contains("unsupported register type conversion"),
        "{error:#}"
    );
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn loop_or_preserves_zero_many_iteration_effects_and_exception_identity() {
    let directory = std::env::temp_dir().join(format!("rdx-boolean-or-{}", std::process::id()));
    fs::create_dir_all(directory.join("sample")).unwrap();
    let java = format!(
        r#"package sample;
public class BooleanOr {{
 static int calls, mode;
 static final RuntimeException sentinel = new RuntimeException("tick failure");
 static boolean tick() {{ calls++; if (mode == 4 && calls == 2) throw sentinel; return calls == 2; }}
 {}
 public static void main(String[] args) {{
  for (int count : new int[] {{0, 1, 3}}) {{
   mode = 0; calls = 0;
   boolean value = run(count);
   if (value != (count >= 2) || calls != count) throw new AssertionError(count + ":" + value + ":" + calls);
  }}
  mode = 4; calls = 0;
  try {{ run(3); throw new AssertionError("missing failure"); }}
  catch (RuntimeException failure) {{ if (failure != sentinel || calls != 2) throw new AssertionError("identity/effects", failure); }}
 }}
}}"#,
        source(&fixture()).unwrap()
    );
    fs::write(directory.join("sample/BooleanOr.java"), &java).unwrap();
    for (program, argument) in [
        ("javac", "sample/BooleanOr.java"),
        ("java", "sample.BooleanOr"),
    ] {
        let result = Command::new(program)
            .arg(argument)
            .current_dir(&directory)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{program}: {}\n{java}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    fs::remove_dir_all(directory).unwrap();
}
