//! Address-order continuation tails can reenter a loop after its latch.
use rdx::{native_dex, native_java};
use std::{fs, process::Command};

fn fixture() -> native_dex::DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class
        .methods
        .retain(|method| method.name.as_ref() == "answer");
    let method = &mut class.methods[0];
    method.name = "sum".into();
    method.access_flags = 9;
    method.parameters = vec!["I".into(), "I".into()];
    method.return_type = "I".into();
    let code = method.code.as_mut().unwrap();
    code.registers = 3;
    code.ins = 2;
    code.outs = 0;
    // while (v0 < p0) { if (p1 == 0) v0 += 2; else v0++; }
    // The add-two arm occupies pc8..10 after the pc7 latch and jumps to it.
    code.instructions = vec![
        0x0012, 0x1035, 10, 0x0238, 5, 0x00d8, 0x0100, 0xfa28, 0x00d8, 0x0200, 0xfd28, 0x000f,
    ];
    class
}

#[test]
fn continuation_tail_reconstructs_and_rejects_a_branching_tail() {
    let mut class = fixture();
    let source = native_java::render_method("sample.Hello", &class, &class.methods[0])
        .unwrap()
        .source;
    assert!(source.contains("while (true)"), "{source}");
    assert!(source.contains("continue;"), "{source}");
    class.methods[0].code.as_mut().unwrap().instructions[10] = 0xfa28; // pc10 -> pc4, replay prior control
    assert!(native_java::render_method("sample.Hello", &class, &class.methods[0]).is_err());
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn continuation_tail_jvm_preserves_both_counter_paths() {
    let class = fixture();
    let source = native_java::render_method("sample.Hello", &class, &class.methods[0])
        .unwrap()
        .source;
    let dir = std::env::temp_dir().join(format!("rdx-loop-reentry-tail-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let java = format!(
        "package sample; public class Tail {{\n{source}\npublic static void main(String[] args) {{\nfor (int limit=-3; limit<15; limit++) for (int mode=0; mode<2; mode++) {{\nint actual=sum(limit,mode); int expected=limit<=0 ? 0 : mode==0 ? 2*((limit+1)/2) : limit;\nif(actual!=expected) throw new AssertionError(limit+\"/\"+mode+\": \"+actual+\" != \"+expected);\n}}\n}}\n}}"
    );
    fs::write(dir.join("sample/Tail.java"), &java).unwrap();
    for (program, argument) in [("javac", "sample/Tail.java"), ("java", "sample.Tail")] {
        let result = Command::new(program)
            .arg(argument)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{program}: {}\n{java}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
