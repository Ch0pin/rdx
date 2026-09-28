use rdx::{
    native_dex::{self, DexClass, DexSymbols},
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
    class.symbols = Arc::new(DexSymbols {
        strings: vec!["after".into()],
        types: vec!["Lsample/BooleanExit;".into()],
        protos: vec![("V".into(), vec![])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    let method = &mut class.methods[0];
    method.name = "choose".into();
    method.access_flags = 9;
    method.parameters = vec!["I".into(), "I".into()];
    method.return_type = "Z".into();
    let code = method.code.as_mut().unwrap();
    code.registers = 5;
    code.ins = 2;
    code.outs = 0;
    code.tries = 0;
    code.try_regions.clear();
    code.instructions = vec![
        0x1012,
        (default as u16) << 12 | 0x0112,
        0x0212,
        0x0438,
        9,
        0x3235,
        7,
        0x0238,
        6,
        0x02d8,
        0x0102,
        0xfa28,
        0x1001,
        0x0071,
        0,
        0,
        0x000f,
    ];
    class
}

fn source(class: &DexClass) -> anyhow::Result<String> {
    Ok(native_java::render_method("sample.BooleanExit", class, &class.methods[0])?.source)
}

#[test]
fn zero_one_loop_exit_reifies_boolean_return() {
    let java = source(&fixture(0)).unwrap();
    assert!(java.contains("while (true)"), "{java}");
    assert!(java.contains("!= 0"), "{java}");
}

#[test]
fn nonboolean_loop_exit_is_rejected() {
    let error = source(&fixture(2)).unwrap_err();
    assert!(
        error.to_string().contains("nonboolean literal"),
        "{error:#}"
    );
}

#[test]
fn copied_arbitrary_integer_is_not_booleanized() {
    let mut class = fixture(0);
    class.methods[0].code.as_mut().unwrap().instructions[1] = 0x3101;
    assert!(source(&class).is_err());
}

#[test]
fn later_write_with_backward_return_edge_is_not_ignored() {
    let mut class = fixture(0);
    let instructions = &mut class.methods[0].code.as_mut().unwrap().instructions;
    instructions[7] = 0x0238;
    instructions[8] = 10; // branch to the write after the lexical return
    instructions.extend([0x2012, 0xfe28]); // v0 = 2; goto return
    let error = source(&class).unwrap_err();
    assert!(
        error.to_string().contains("nonboolean literal"),
        "{error:#}"
    );
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn zero_one_loop_exit_jvm_behavior() {
    let directory = std::env::temp_dir().join(format!("rdx-boolean-exit-{}", std::process::id()));
    fs::create_dir_all(directory.join("sample")).unwrap();
    let java = format!(
        "package sample; public class BooleanExit {{\nstatic int calls; static void after() {{ calls++; }}\n{}\npublic static void main(String[] args) {{ for (int n=-2; n<=3; n++) for (int enabled=0; enabled<=1; enabled++) {{ calls=0; if (choose(n, enabled) != (enabled!=0 && n>0) || calls != 1) throw new AssertionError(n); }} }}\n}}",
        source(&fixture(0)).unwrap()
    );
    fs::write(directory.join("sample/BooleanExit.java"), java).unwrap();
    for (program, argument) in [
        ("javac", "sample/BooleanExit.java"),
        ("java", "sample.BooleanExit"),
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
