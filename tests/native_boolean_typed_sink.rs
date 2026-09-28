use rdx::{
    native_dex::{self, DexClass, DexField, DexSymbols},
    native_java,
};
use std::{fs, process::Command, sync::Arc};

fn fixture(default: i32) -> DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class
        .methods
        .retain(|method| method.name.as_ref() == "answer");
    class.descriptor = "Lsample/BooleanStore;".into();
    class.fields = vec![DexField {
        declaring_type: class.descriptor.clone(),
        name: "value".into(),
        field_type: "Z".into(),
        access_flags: 9,
        is_static: true,
    }];
    class.symbols = Arc::new(DexSymbols {
        strings: vec!["value".into()],
        types: vec![class.descriptor.clone(), "Z".into()],
        fields: vec![(0, 1, 0)],
        ..Default::default()
    });
    let method = &mut class.methods[0];
    method.declaring_type = class.descriptor.clone();
    method.name = "choose".into();
    method.access_flags = 9;
    method.parameters = vec!["I".into()];
    method.return_type = "V".into();
    let code = method.code.as_mut().unwrap();
    code.registers = 3;
    code.ins = 1;
    code.outs = 0;
    code.tries = 0;
    code.try_regions.clear();
    code.instructions = vec![
        (default as u16) << 12 | 0x0012,
        0x0238,
        6,
        0x1012,
        0x02d8,
        0xff02,
        0xfb28,
        0x006a,
        0,
        0x000e,
    ];
    class
}

fn source(class: &DexClass) -> anyhow::Result<String> {
    Ok(native_java::render_method("sample.BooleanStore", class, &class.methods[0])?.source)
}

fn invoke_fixture(default: i32) -> DexClass {
    let mut class = fixture(default);
    class.fields.clear();
    class.symbols = Arc::new(DexSymbols {
        strings: vec!["consume".into()],
        types: vec![class.descriptor.clone(), "Z".into()],
        protos: vec![("V".into(), vec!["Z".into()])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    class.methods[0].code.as_mut().unwrap().instructions = vec![
        (default as u16) << 12 | 0x0012,
        0x0238,
        6,
        0x1012,
        0x02d8,
        0xff02,
        0xfb28,
        0x1071,
        0,
        0,
        0x000e,
    ];
    class
}

fn float_fixture() -> DexClass {
    let mut class = fixture(0);
    class.fields[0].field_type = "F".into();
    class.symbols = Arc::new(DexSymbols {
        strings: vec!["value".into()],
        types: vec![class.descriptor.clone(), "F".into()],
        fields: vec![(0, 1, 0)],
        ..Default::default()
    });
    class.methods[0].code.as_mut().unwrap().instructions = vec![
        0x0014, 0x0000, 0x8000, // v0 = negative zero float bits
        0x0238, 8, // if count == 0, exit
        0x0014, 0x1234, 0x7fc0, // v0 = quiet NaN with payload
        0x02d8, 0xff02, // count--
        0xf928, // goto loop guard
        0x0067, 0, // sput v0 into float field
        0x000e,
    ];
    class
}

#[test]
fn zero_one_loop_value_writes_boolean_field() {
    let java = source(&fixture(0)).unwrap();
    assert!(java.contains("while ("), "{java}");
    assert!(
        java.contains("value = (") && java.contains("!= 0"),
        "{java}"
    );
}

#[test]
fn arbitrary_integer_loop_value_does_not_write_boolean_field() {
    let error = source(&fixture(2)).unwrap_err();
    assert!(
        error.to_string().contains("nonboolean literal")
            || error
                .to_string()
                .contains("unsupported register type conversion I to Z"),
        "{error:#}"
    );
}

#[test]
fn zero_one_loop_value_passes_boolean_call_argument() {
    let java = source(&invoke_fixture(0)).unwrap();
    assert!(
        java.contains("consume((") && java.contains("!= 0"),
        "{java}"
    );
}

#[test]
fn arbitrary_integer_parameter_does_not_pass_boolean_call_argument() {
    let mut class = invoke_fixture(0);
    class.methods[0].code.as_mut().unwrap().instructions = vec![0x1071, 0, 2, 0x000e];
    let error = source(&class).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("unsupported register type conversion I to Z"),
        "{error:#}"
    );
}

#[test]
fn raw_constant_loop_value_reinterprets_as_float_bits() {
    let java = source(&float_fixture()).unwrap();
    assert!(java.contains("Float.intBitsToFloat("), "{java}");
}

#[test]
fn arithmetic_integer_loop_value_is_not_reinterpreted_as_float() {
    let mut class = float_fixture();
    class.methods[0].code.as_mut().unwrap().instructions = vec![
        0x0014, 0x0000, 0x8000, 0x0238, 7, 0x00d8, 0x0100, 0x02d8, 0xff02, 0xfa28, 0x0067, 0,
        0x000e,
    ];
    let error = source(&class).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("unsupported register type conversion I to F"),
        "{error:#}"
    );
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn zero_one_loop_field_write_jvm_behavior() {
    let directory = std::env::temp_dir().join(format!("rdx-boolean-store-{}", std::process::id()));
    fs::create_dir_all(directory.join("sample")).unwrap();
    let java = format!(
        "package sample; public class BooleanStore {{\nstatic boolean value;\n{}\npublic static void main(String[] args) {{ for (int n=0; n<=3; n++) {{ value=false; choose(n); if (value != (n>0)) throw new AssertionError(n); }} }}\n}}",
        source(&fixture(0)).unwrap()
    );
    fs::write(directory.join("sample/BooleanStore.java"), java).unwrap();
    for (program, argument) in [
        ("javac", "sample/BooleanStore.java"),
        ("java", "sample.BooleanStore"),
    ] {
        let result = Command::new(program)
            .arg(argument)
            .current_dir(&directory)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{program}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn raw_float_bits_loop_jvm_behavior() {
    let directory = std::env::temp_dir().join(format!("rdx-float-bits-{}", std::process::id()));
    fs::create_dir_all(directory.join("sample")).unwrap();
    let java = format!(
        "package sample; public class BooleanStore {{\nstatic float value;\n{}\npublic static void main(String[] args) {{ for (int n=0; n<=3; n++) {{ value=0; choose(n); int bits=Float.floatToRawIntBits(value); int expected=n==0 ? 0x80000000 : 0x7fc01234; if (bits != expected) throw new AssertionError(Integer.toHexString(bits)); }} }}\n}}",
        source(&float_fixture()).unwrap()
    );
    fs::write(directory.join("sample/BooleanStore.java"), java).unwrap();
    for (program, argument) in [
        ("javac", "sample/BooleanStore.java"),
        ("java", "sample.BooleanStore"),
    ] {
        let result = Command::new(program)
            .arg(argument)
            .current_dir(&directory)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{program}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}
