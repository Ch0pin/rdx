//! Generic regressions extracted from coroutine/network method fallbacks.
use rdx::{
    native_dex::{self, DexClass, DexSymbols, DexTryRegion},
    native_java,
};
use std::{fs, process::Command, sync::Arc};

fn fixture(name: &str, params: &[&str], ret: &str, registers: u16, words: Vec<u16>) -> DexClass {
    let mut c = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    c.descriptor = "Lsample/Probe;".into();
    c.methods.retain(|m| m.name.as_ref() == "answer");
    c.symbols = Arc::new(DexSymbols {
        strings: vec!["maybe".into(), "<init>".into()],
        types: vec!["Lsample/Probe;".into()],
        protos: vec![
            ("I".into(), vec!["I".into()]),
            ("V".into(), vec!["I".into()]),
        ],
        methods: vec![(0, 0, 0), (0, 1, 1)],
        ..Default::default()
    });
    let m = &mut c.methods[0];
    m.declaring_type = c.descriptor.clone();
    m.name = name.into();
    m.access_flags = 9;
    m.parameters = params.iter().map(|p| (*p).into()).collect();
    m.return_type = ret.into();
    let code = m.code.as_mut().unwrap();
    code.registers = registers;
    code.ins = params.len() as u16;
    code.outs = 2;
    code.instructions = words;
    c
}
fn loop_case() -> DexClass {
    fixture(
        "scan",
        &["I", "I", "I"],
        "I",
        4,
        vec![
            0x0012, 0x1035, 9, 0x00d8, 0x0100, 0x2032, 4, 0x3033, 0xfffa, 0x000f, 0xf012, 0x000f,
        ],
    )
}
fn handler_case() -> DexClass {
    let mut c = fixture(
        "guarded",
        &["I"],
        "I",
        2,
        vec![
            0x0012, 0x1071, 0, 1, 0x000a, 0x0039, 4, 0x0012, 0x0228, 0x1012, 0x000f,
        ],
    );
    let code = c.methods[0].code.as_mut().unwrap();
    code.tries = 1;
    code.try_regions = vec![DexTryRegion {
        start: 1,
        end: 10,
        catches: vec![(Some("Ljava/lang/RuntimeException;".into()), 7)].into(),
    }];
    c
}
fn allocation_case() -> DexClass {
    let mut c = fixture(
        "allocate",
        &["I"],
        "Ljava/lang/Object;",
        5,
        vec![
            0x0012, 0x0122, 0, 0x1207, 0x4301, 0x2070, 1, 0x0032, 0x0111, 0x010d, 0x0011,
        ],
    );
    let code = c.methods[0].code.as_mut().unwrap();
    code.tries = 1;
    code.try_regions = vec![DexTryRegion {
        start: 1,
        end: 8,
        catches: vec![(Some("Ljava/lang/RuntimeException;".into()), 9)].into(),
    }];
    c
}
fn render(c: &DexClass) -> String {
    native_java::render_method("sample.Probe", c, &c.methods[0])
        .unwrap()
        .source
}
#[test]
fn shared_post_latch_return_constructor_alias_and_pure_handler_reconstruct() {
    let source = render(&loop_case());
    assert!(source.contains("while (true)"), "{source}");
    let source = render(&handler_case());
    assert!(
        source.contains("catch (java.lang.RuntimeException"),
        "{source}"
    );
    let source = render(&allocation_case());
    assert_eq!(source.matches("new sample.Probe(").count(), 1, "{source}");
}
#[test]
fn throwing_shared_handler_and_unrelated_constructor_receiver_are_rejected() {
    let mut c = handler_case();
    c.methods[0].code.as_mut().unwrap().instructions[7] = 0x0027;
    assert!(native_java::render_method("sample.Probe", &c, &c.methods[0]).is_err());
    let mut c = allocation_case();
    c.methods[0].code.as_mut().unwrap().instructions[3] = 0x0212;
    assert!(native_java::render_method("sample.Probe", &c, &c.methods[0]).is_err());
}
#[test]
#[ignore = "requires javac and java on PATH"]
fn reconstructed_methods_execute_short_circuit_and_exception_paths() {
    let methods = [loop_case(), handler_case(), allocation_case()]
        .iter()
        .map(render)
        .collect::<Vec<_>>()
        .join("\n");
    let dir = std::env::temp_dir().join(format!("rdx-reviewer-recovery-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(dir.join("sample/Probe.java"),format!(r#"
        package sample;
        public class Probe {{
          int value; Probe(int n) {{ if(n<0) throw new IllegalArgumentException(); value=n; }}
          static int maybe(int n) {{ if(n<0) throw new IllegalArgumentException(); return n; }}
          {methods}
          public static void main(String[] args) {{
            for(int n=0;n<6;n++) for(int a=0;a<7;a++) for(int b=0;b<7;b++) {{
              int expected=-1; for(int i=1;i<=n;i++) if(i==a||i==b) {{expected=i;break;}}
              if(scan(n,a,b)!=expected) throw new AssertionError("loop "+n+","+a+","+b);
            }}
            for(int n=-1;n<=2;n++) {{
              if(guarded(n)!=(n>0?1:0)) throw new AssertionError("handler");
              Object result=allocate(n);
              if(n<0 ? result!=null : !(result instanceof Probe)||((Probe)result).value!=n) throw new AssertionError("allocation");
            }}
          }}
        }}"#)).unwrap();
    for (exe, args) in [
        ("javac", vec!["sample/Probe.java"]),
        ("java", vec!["sample.Probe"]),
    ] {
        let output = Command::new(exe)
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{exe}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
