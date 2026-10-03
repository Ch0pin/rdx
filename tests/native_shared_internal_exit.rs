use rdx::{native_dex, native_java};
fn fixture() -> native_dex::DexClass {
    let mut c = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    c.methods.retain(|m| m.name.as_ref() == "answer");
    let m = &mut c.methods[0];
    m.name = "compare".into();
    m.access_flags = 9;
    m.parameters = vec!["I".into(), "I".into()];
    m.return_type = "I".into();
    let code = m.code.as_mut().unwrap();
    code.registers = 3;
    code.ins = 2;
    code.outs = 0;
    code.instructions = vec![
        0x0012, 0x0238, 7, 0x1035, 10, 0x023b, 5, 0x0128, 0x0012, 0x0528, 0x00d8, 0x0100, 0xf728,
        0x1012, 0x000f,
    ];
    c
}
#[test]
fn shared_internal_exit_is_reconstructed() {
    let c = fixture();
    let result = native_java::render_method("sample.Hello", &c, &c.methods[0]).unwrap();
    assert!(result.source.contains("while"), "{}", result.source);
    println!("{}", result.source);
}
#[test]
fn external_entry_to_cyclic_body_still_rejected() {
    let mut c = fixture();
    c.methods[0].code.as_mut().unwrap().instructions[2] = 9;
    assert!(native_java::render_method("sample.Hello", &c, &c.methods[0]).is_err());
}

fn boolean_call_fixture() -> native_dex::DexClass {
    let mut c = fixture();
    c.symbols = std::sync::Arc::new(native_dex::DexSymbols {
        strings: vec!["accept".into()],
        types: vec!["Lsample/Sink;".into()],
        protos: vec![("Z".into(), vec!["Z".into()])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    let m = &mut c.methods[0];
    m.name = "flag".into();
    m.parameters = vec!["I".into()];
    m.return_type = "Z".into();
    let code = m.code.as_mut().unwrap();
    code.registers = 2;
    code.ins = 1;
    code.outs = 1;
    code.instructions = vec![
        0x7012, 0x0138, 4, 0x0012, 0x0228, 0x1012, 0x1071, 0, 0, 0x000a, 0x000f,
    ];
    c
}
#[test]
fn boolean_call_uses_exact_lifetime_and_rejects_non_boolean_phi() {
    let mut c = boolean_call_fixture();
    native_java::render_method("sample.Hello", &c, &c.methods[0]).unwrap();
    c.methods[0].code.as_mut().unwrap().instructions[5] = 0x2012;
    assert!(native_java::render_method("sample.Hello", &c, &c.methods[0]).is_err());
}
#[test]
#[ignore = "requires javac and java"]
fn jvm_checks_shared_exit_and_boolean_call() {
    let c = fixture();
    let b = boolean_call_fixture();
    let source = native_java::render_method("sample.Hello", &c, &c.methods[0])
        .unwrap()
        .source;
    let boolean = native_java::render_method("sample.Hello", &b, &b.methods[0])
        .unwrap()
        .source;
    let dir = std::env::temp_dir().join(format!("rdx-internal-exit-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("sample")).unwrap();
    let java = format!(
        "package sample; public class Check {{ {source} {boolean} public static void main(String[] args) {{ for(int n=-3;n<12;n++) for(int p=-2;p<3;p++) {{ int expected=p==0?0:n<=0||p>0?1:0; if(compare(n,p)!=expected) throw new AssertionError(); if(flag(p)!=(p==0)) throw new AssertionError(); }} }} }} class Sink {{ static boolean accept(boolean b) {{ return b; }} }}"
    );
    std::fs::write(dir.join("sample/Check.java"), &java).unwrap();
    for (cmd, arg) in [("javac", "sample/Check.java"), ("java", "sample.Check")] {
        let output = std::process::Command::new(cmd)
            .arg(arg)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{java}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}
