//! Crossing address intervals can be one loop with a reentry prefix.
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
    method.name = "count".into();
    method.access_flags = 9;
    method.parameters = vec!["I".into(), "I".into()];
    method.return_type = "I".into();
    let code = method.code.as_mut().unwrap();
    code.registers = 3;
    code.ins = 2;
    code.outs = 0;
    // The pc7 backedge reenters through the pc1 increment; the pc10 backedge
    // enters directly at the pc3 guard. Their raw intervals cross.
    code.instructions = vec![
        0x0012, 0x00d8, 0x0100, 0x1035, 8, 0x0238, 3, 0xfa28, 0x00d8, 0x0200, 0xf928, 0x000f,
    ];
    class
}

fn default_tail_fixture() -> native_dex::DexClass {
    let mut class = fixture();
    let method = &mut class.methods[0];
    method.name = "defaultTail".into();
    method.code.as_mut().unwrap().instructions = vec![
        0x0012, 0x1035, 7, 0x0238, 6, 0x00d8, 0x0100, 0xfa28, 0x7012, 0x000f,
    ];
    class
}

#[test]
fn crossing_backedges_reconstruct_as_one_loop_and_reject_impure_prefix_control() {
    let mut class = fixture();
    let source = native_java::render_method("sample.Hello", &class, &class.methods[0])
        .unwrap()
        .source;
    assert_eq!(source.matches("while (true)").count(), 1, "{source}");
    class.methods[0].code.as_mut().unwrap().instructions[1] = 0x0038; // prefix becomes branch
    assert!(native_java::render_method("sample.Hello", &class, &class.methods[0]).is_err());
}

#[test]
fn guarded_loop_can_skip_a_pure_default_exit_tail() {
    let class = default_tail_fixture();
    let source = native_java::render_method("sample.Hello", &class, &class.methods[0])
        .unwrap()
        .source;
    assert!(source.contains("while (true)"), "{source}");
    assert!(source.contains("break;"), "{source}");
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn crossing_backedges_jvm_preserve_distinct_reentry_paths() {
    let class = fixture();
    let source = native_java::render_method("sample.Hello", &class, &class.methods[0])
        .unwrap()
        .source;
    let default_class = default_tail_fixture();
    let default_source =
        native_java::render_method("sample.Hello", &default_class, &default_class.methods[0])
            .unwrap()
            .source;
    let dir = std::env::temp_dir().join(format!("rdx-crossing-loop-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let java = format!(
        "package sample; public class Crossing {{\n{source}\n{default_source}\npublic static void main(String[] args) {{\nfor (int limit=-2; limit<16; limit++) for (int mode=0; mode<2; mode++) {{\nint actual=count(limit,mode); int expected=mode==0 ? (limit<=1 ? 1 : 1+2*((limit-1+1)/2)) : Math.max(1,limit);\nif(actual!=expected) throw new AssertionError(limit+\"/\"+mode+\": \"+actual+\" != \"+expected);\nint tail=defaultTail(limit,mode); int expectedTail=mode==0 && limit>0 ? 0 : 7;\nif(tail!=expectedTail) throw new AssertionError(\"tail \"+limit+\"/\"+mode+\": \"+tail);\n}}\n}}\n}}"
    );
    fs::write(dir.join("sample/Crossing.java"), &java).unwrap();
    for (program, argument) in [
        ("javac", "sample/Crossing.java"),
        ("java", "sample.Crossing"),
    ] {
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
