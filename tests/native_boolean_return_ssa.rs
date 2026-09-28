use rdx::{
    native_dex::{self, DexClass, DexSymbols, DexTryRegion},
    native_java,
};
use std::{fs, process::Command, sync::Arc};

fn fixture(initial: u16, call_return: &str) -> DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class
        .methods
        .retain(|method| method.name.as_ref() == "answer");
    class.descriptor = "Lsample/BooleanReturn;".into();
    class.symbols = Arc::new(DexSymbols {
        strings: vec!["tick".into()],
        types: vec![class.descriptor.clone()],
        protos: vec![(call_return.into(), vec![])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    let method = &mut class.methods[0];
    method.declaring_type = class.descriptor.clone();
    method.name = "run".into();
    method.return_type = "Z".into();
    method.parameters.clear();
    method.access_flags = 9;
    let code = method.code.as_mut().unwrap();
    code.registers = 2;
    code.ins = 0;
    code.outs = 0;
    code.tries = 1;
    code.try_regions = vec![DexTryRegion {
        start: 1,
        end: 5,
        catches: vec![(Some("Ljava/lang/RuntimeException;".into()), 6)].into(),
    }];
    code.instructions = vec![
        initial, // v0 = false before the potentially throwing call
        0x0071, 0, 0,      // invoke-static {}, tick()Z
        0x000a, // move-result v0
        0x0228, // goto return
        0x010d, // move-exception v1
        0x000f, // return v0: zero or the call's Z result
    ];
    class
}

fn source(class: &DexClass) -> anyhow::Result<String> {
    Ok(native_java::render_method("sample.BooleanReturn", class, &class.methods[0])?.source)
}

fn copied_result_fixture() -> DexClass {
    let mut class = fixture(0x0012, "Z");
    let code = class.methods[0].code.as_mut().unwrap();
    code.instructions = vec![
        0x0112, // v1 = false
        0x0071, 0, 0,      // invoke-static {}, tick()Z
        0x000a, // move-result v0
        0x0101, // move v1, v0
        0x0228, // goto return
        0x000d, // move-exception v0
        0x010f, // return v1
    ];
    code.try_regions[0].end = 6;
    code.try_regions[0].catches = vec![(Some("Ljava/lang/RuntimeException;".into()), 7)].into();
    class
}

#[test]
fn merged_literal_and_boolean_call_result_return_boolean() {
    let java = source(&fixture(0x0012, "Z")).unwrap();
    assert!(java.contains("tick()"), "{java}");
    assert!(java.contains("catch ("), "{java}");
    assert!(java.contains("!= 0"), "{java}");
}

#[test]
fn copied_boolean_call_result_merges_with_prior_zero() {
    let java = source(&copied_result_fixture()).unwrap();
    assert!(java.contains("tick()"), "{java}");
    assert!(java.contains("!= 0"), "{java}");
}

#[test]
fn merged_nonboolean_literal_is_rejected() {
    let error = source(&fixture(0x2012, "Z")).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("unsupported register type conversion I to Z")
            || error.to_string().contains("nonboolean literal"),
        "{error:#}"
    );
}

#[test]
fn merged_integer_call_result_is_rejected() {
    let error = source(&fixture(0x0012, "I")).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("unsupported register type conversion I to Z"),
        "{error:#}"
    );
}

#[test]
fn merged_arbitrary_parameter_is_rejected() {
    let mut class = fixture(0x2001, "Z"); // move v0, v2 (the I parameter)
    let method = &mut class.methods[0];
    method.parameters = vec!["I".into()];
    let code = method.code.as_mut().unwrap();
    code.registers = 3;
    code.ins = 1;
    let error = source(&class).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("unsupported register type conversion I to Z"),
        "{error:#}"
    );
}

#[test]
fn merged_undefined_handler_value_is_rejected() {
    let error = source(&fixture(0x0000, "Z")).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("unsupported register type conversion I to Z")
            || error.to_string().contains("undefined"),
        "{error:#}"
    );
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn boolean_return_preserves_exception_state_and_call_effects() {
    let directory = std::env::temp_dir().join(format!("rdx-boolean-return-{}", std::process::id()));
    fs::create_dir_all(directory.join("sample")).unwrap();
    let java = format!(
        r#"package sample;
public class BooleanReturn {{
 static int mode, calls;
 static boolean tick() {{ calls++; if (mode == 2) throw new RuntimeException(); return mode == 1; }}
 {}
 public static void main(String[] args) {{
  for (mode = 0; mode < 3; mode++) {{
   calls = 0;
   boolean value = run();
   if (value != (mode == 1) || calls != 1) throw new AssertionError(mode + ":" + value + ":" + calls);
  }}
 }}
}}"#,
        source(&fixture(0x0012, "Z")).unwrap()
    );
    fs::write(directory.join("sample/BooleanReturn.java"), &java).unwrap();
    for (program, argument) in [
        ("javac", "sample/BooleanReturn.java"),
        ("java", "sample.BooleanReturn"),
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
