//! Normal-edge closure for disjoint loops which return from their own bodies.
use rdx::{
    native_dex::{self, DexSymbols},
    native_java,
};
use std::{process::Command, sync::Arc};

fn fixture() -> native_dex::DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class.methods.retain(|m| m.name.as_ref() == "answer");
    class.symbols = Arc::new(DexSymbols {
        strings: vec!["step".into()],
        types: vec!["Lsample/Terminal;".into()],
        protos: vec![("I".into(), vec!["I".into()])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    let m = &mut class.methods[0];
    m.name = "run".into();
    m.access_flags = 9;
    m.return_type = "I".into();
    m.parameters = vec!["I".into(), "I".into()];
    let code = m.code.as_mut().unwrap();
    code.registers = 3;
    code.ins = 2;
    code.outs = 1;
    code.instructions = vec![
        0x0138, 14, // choose sibling before either loop
        0x023c, 5, 0x0013, 11, 0x000f, // terminal first arm
        0x1071, 0, 2, 0x020a, 0x02d8, 0xff02, 0xf528, // step, decrement, back to 2
        0x023c, 5, 0x0013, 22, 0x000f, // terminal sibling arm
        0x1071, 0, 2, 0x020a, 0x02d8, 0xff02, 0xf528,
    ];
    class
}

#[test]
fn sibling_terminal_loops_have_no_fabricated_fallthrough() {
    let c = fixture();
    let source = native_java::render_method("sample.Terminal", &c, &c.methods[0])
        .unwrap()
        .source;
    assert_eq!(source.matches("while (true)").count(), 2, "{source}");
    assert_eq!(
        source.matches("sample.Terminal.step(").count(),
        2,
        "{source}"
    );
    assert!(
        source.contains("return 11;") && source.contains("return 22;"),
        "{source}"
    );
}

#[test]
fn sibling_loop_interior_entry_stays_rejected() {
    let mut c = fixture();
    c.methods[0].code.as_mut().unwrap().instructions[1] = 7;
    assert!(native_java::render_method("sample.Terminal", &c, &c.methods[0]).is_err());
}

#[test]
fn normal_edge_to_sibling_remains_a_loop_break() {
    let mut c = fixture();
    c.methods[0].code.as_mut().unwrap().instructions[6] = 0x0828;
    let source = native_java::render_method("sample.Terminal", &c, &c.methods[0])
        .unwrap()
        .source;
    assert!(source.contains("break;"), "{source}");
    assert!(source.contains("return 22;"), "{source}");
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn sibling_terminal_loops_jvm_preserve_calls_and_results() {
    let c = fixture();
    let method = native_java::render_method("sample.Terminal", &c, &c.methods[0])
        .unwrap()
        .source;
    let dir = std::env::temp_dir().join(format!("rdx-terminal-siblings-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("sample")).unwrap();
    let source = format!(
        "package sample; public class Terminal {{ static String trace=\"\"; public static int step(int x) {{ trace += x + \",\"; return x; }} {method} public static void main(String[] a) {{ for(int mode=0;mode<=1;mode++) for(int n:new int[]{{-1,0,1,4}}) {{ trace=\"\"; int result=run(mode,n); System.out.println(mode+\":\"+n+\":\"+result+\":\"+trace); }} }} }}"
    );
    std::fs::write(dir.join("sample/Terminal.java"), source).unwrap();
    let compile = Command::new("javac")
        .arg("sample/Terminal.java")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}\n{method}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new("java")
        .arg("sample.Terminal")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "0:-1:22:\n0:0:22:\n0:1:22:1,\n0:4:22:4,3,2,1,\n1:-1:11:\n1:0:11:\n1:1:11:1,\n1:4:11:4,3,2,1,\n"
    );
}
