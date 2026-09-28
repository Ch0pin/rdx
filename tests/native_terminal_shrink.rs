use rdx::{
    native_dex::{self, DexClass, DexSymbols},
    native_java,
};
use std::{fs, process::Command, sync::Arc};

fn fixture(ret: &str) -> DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class.descriptor = "Lsample/Terminal;".into();
    class.methods.retain(|m| m.name.as_ref() == "answer");
    class.symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/Source;".into()],
        strings: vec!["read".into()],
        protos: vec![(ret.into(), vec![])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    let method = &mut class.methods[0];
    method.declaring_type = class.descriptor.clone();
    method.name = "read".into();
    method.parameters.clear();
    method.return_type = ret.into();
    method.access_flags = 9;
    let code = method.code.as_mut().unwrap();
    code.registers = 2;
    code.ins = 0;
    code.outs = 0;
    code.tries = 0;
    code.try_regions.clear();
    let (mv, rv) = if ret == "J" || ret == "D" {
        (0xb, 0x10)
    } else if ret.starts_with('L') {
        (0xc, 0x11)
    } else {
        (0xa, 0xf)
    };
    code.instructions = vec![0x0071, 0, 0, mv, rv];
    class
}

#[test]
fn adjacent_result_return_keeps_bound_call_links_and_exact_return_type() {
    for ty in ["I", "J", "F", "D", "Ljava/lang/String;"] {
        let class = fixture(ty);
        let rendered =
            native_java::render_method("sample.Terminal", &class, &class.methods[0]).unwrap();
        assert!(
            rendered.source.contains("return sample.Source.read();"),
            "{}",
            rendered.source
        );
        assert!(!rendered.source.contains(" = "), "{}", rendered.source);
        let link = rendered
            .links
            .iter()
            .find(|link| link.label == format!("sample.Source.read(){ty}"))
            .unwrap();
        assert_eq!(
            rendered
                .source
                .chars()
                .skip(link.start)
                .take(link.end - link.start)
                .collect::<String>(),
            "read"
        );
    }
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn terminal_call_preserves_once_only_effect_exception_identity_and_wide_value() {
    let class = fixture("J");
    let method = native_java::render_method("sample.Terminal", &class, &class.methods[0]).unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-terminal-shrink-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(
        dir.join("sample/Terminal.java"),
        format!(
            "package sample; public class Terminal {{ {} }}",
            method.source
        ),
    )
    .unwrap();
    fs::write(dir.join("sample/Source.java"), r#"package sample;
public class Source {
 static int calls; static boolean fail; static final RuntimeException ERROR = new RuntimeException();
 public static long read() { calls++; if(fail) throw ERROR; return Long.MIN_VALUE + 91; }
 public static void main(String[] args) {
  if (Terminal.read() != Long.MIN_VALUE + 91 || calls != 1) throw new AssertionError();
  fail=true;
  try { Terminal.read(); throw new AssertionError(); } catch(RuntimeException e) { if(e != ERROR || calls != 2) throw new AssertionError(); }
 }
}"#).unwrap();
    let compile = Command::new("javac")
        .current_dir(&dir)
        .args(["sample/Terminal.java", "sample/Source.java"])
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new("java")
        .current_dir(&dir)
        .args(["-cp", ".", "sample.Source"])
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn silent_dex_instructions_are_terminal_shrinking_barriers() {
    for middle in [vec![0x0000], vec![0x0112], vec![0x0101], vec![0x011a, 0]] {
        let mut class = fixture("I");
        let words = &mut class.methods[0].code.as_mut().unwrap().instructions;
        words.splice(4..4, middle);
        let body =
            native_java::render_method("sample.Terminal", &class, &class.methods[0]).unwrap();
        assert!(
            body.source.contains("int v0 = sample.Source.read();"),
            "{}",
            body.source
        );
        assert!(body.source.contains("return v0;"), "{}", body.source);
    }
}
