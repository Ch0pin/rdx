//! DEX zero becomes null only at a common exit that proves reference use.
use rdx::{
    native_dex::{self, DexClass, DexSymbols},
    native_java,
};
use std::{fs, process::Command, sync::Arc};

fn fixture(seed: u16, cast_register: u16) -> DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class.methods.retain(|m| m.name.as_ref() == "answer");
    class.symbols = Arc::new(DexSymbols {
        types: vec!["Ljava/lang/String;".into()],
        ..Default::default()
    });
    let method = &mut class.methods[0];
    method.name = "choose".into();
    method.access_flags = 9;
    method.parameters = vec!["Ljava/lang/String;".into(), "I".into()];
    method.return_type = "Ljava/lang/String;".into();
    let code = method.code.as_mut().unwrap();
    code.registers = 4;
    code.ins = 2;
    code.outs = 0;
    code.instructions = vec![
        0x3001,
        0x0112 | (seed << 12),
        0x003d,
        7,
        0x00d8,
        0xff00,
        0x003c,
        0xfffc,
        0x2107,
        0x001f | (cast_register << 8),
        0,
        0x0138,
        3,
        0x0111,
        0x0111,
    ];
    class
}
fn source_for(cast_register: u16) -> String {
    let class = fixture(0, cast_register);
    native_java::render_method("sample.NullExit", &class, &class.methods[0])
        .unwrap()
        .source
}
fn source() -> String {
    source_for(1)
}
#[test]
fn existing_zero_exit_seed_accepts_reference_loop_values() {
    let rendered = source();
    assert!(
        rendered.contains("Object") && rendered.contains("null"),
        "{rendered}"
    );
    assert!(rendered.contains("while"), "{rendered}");
}
#[test]
fn typed_parameter_move_establishes_reference_exit_without_exit_register_cast() {
    // v1 is zero on the bypass and exactly the String parameter after move-object.
    // The later check-cast of v2 is unrelated and supplies no proof about v1.
    let rendered = source_for(2);
    assert!(
        rendered.contains("while") && rendered.contains("null"),
        "{rendered}"
    );
}
#[test]
fn nonzero_or_integer_use_does_not_widen_primitive_exit_seed() {
    let class = fixture(1, 1);
    assert!(native_java::render_method("sample.NullExit", &class, &class.methods[0]).is_err());
    let mut class = fixture(0, 1);
    // A nonzero primitive on the one-time tail cannot become a reference.
    class.methods[0].code.as_mut().unwrap().instructions[8] = 0x1112;
    assert!(native_java::render_method("sample.NullExit", &class, &class.methods[0]).is_err());
    let mut class = fixture(0, 1);
    // Exit evidence does not authorize treating the same header value as int.
    let words = &mut class.methods[0].code.as_mut().unwrap().instructions;
    words[4] = 0x01d8;
    words[5] = 0xff01;
    assert!(native_java::render_method("sample.NullExit", &class, &class.methods[0]).is_err());
}
#[test]
#[ignore = "requires javac and java on PATH"]
fn null_exit_jvm_preserves_zero_and_multiple_iterations() {
    let dir = std::env::temp_dir().join(format!("rdx-null-exit-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let java = format!(
        r#"package sample;
public class NullExit {{
{}
{}
public static void main(String[] args) {{
  for(int n=-2;n<10;n++) for(String value:new String[]{{null,"payload"}})
    {{
      String expected=n<=0?null:value;
      if(choose(value,n)!=expected) throw new AssertionError("cast exit result "+n);
      if(chooseWithoutExitCast(value,n)!=expected) throw new AssertionError("typed move result "+n);
    }}
}}
}}"#,
        source(),
        source_for(2).replace("choose(", "chooseWithoutExitCast(")
    );
    fs::write(dir.join("sample/NullExit.java"), &java).unwrap();
    for (program, argument) in [
        ("javac", "sample/NullExit.java"),
        ("java", "sample.NullExit"),
    ] {
        let mut command = Command::new(program);
        if program == "java" {
            command.arg("-Xverify:all");
        }
        let result = command.arg(argument).current_dir(&dir).output().unwrap();
        assert!(
            result.status.success(),
            "{program}: {}\n{java}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
