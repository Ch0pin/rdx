use rdx::{
    native_dex::{self, DexClass, DexSymbols, DexTryRegion},
    native_java,
};
use std::sync::Arc;

fn fixture() -> DexClass {
    let mut c = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    c.methods.retain(|m| m.name.as_ref() == "answer");
    c.symbols = Arc::new(DexSymbols {
        strings: vec![
            "<init>".into(),
            "exists".into(),
            "log".into(),
            "Denied read-access to the files".into(),
        ],
        types: vec!["Ljava/io/File;".into(), "Lsample/Log;".into()],
        protos: vec![
            ("V".into(), vec!["Ljava/lang/String;".into()]),
            ("Z".into(), vec![]),
            (
                "V".into(),
                vec!["Ljava/lang/String;".into(), "Ljava/lang/Exception;".into()],
            ),
        ],
        methods: vec![(0, 0, 0), (0, 1, 1), (1, 2, 2)],
        ..Default::default()
    });
    let m = &mut c.methods[0];
    m.name = "filesExist".into();
    m.access_flags = 1;
    m.parameters = vec!["[Ljava/lang/String;".into()];
    m.return_type = "Z".into();
    let b = m.code.as_mut().unwrap();
    b.registers = 6;
    b.ins = 2;
    b.outs = 2;
    b.tries = 1;
    b.instructions = vec![
        0x0412, 0x5021, 0x4101, 0x0135, 0x0016, 0x0246, 0x0105, 0x0322, 0, 0x2070, 0, 0x0023,
        0x106e, 1, 0x0003, 0x020a, 0x0238, 4, 0x1412, 0x040f, 0x01d8, 0x0101, 0xed28, 0x050d,
        0x0228, 0x040f, 0x001a, 3, 0x2071, 2, 0x0050, 0x040f,
    ];
    b.try_regions = vec![DexTryRegion {
        start: 1,
        end: 16,
        catches: vec![(Some("Ljava/lang/Exception;".into()), 23)].into(),
    }];
    c
}

#[test]
fn protected_array_loop_with_early_return_reconstructs() {
    let c = fixture();
    let source = native_java::render_method("sample.Hello", &c, &c.methods[0])
        .unwrap()
        .source;
    println!("{source}");
    assert!(source.contains("catch (java.lang.Exception"), "{source}");
    assert!(source.contains(".exists()"), "{source}");
    assert!(source.contains("return true;"), "{source}");
}

#[test]
fn added_exception_effects_and_normal_handler_entries_still_decline() {
    for mutation in 0..5 {
        let mut c = fixture();
        let b = c.methods[0].code.as_mut().unwrap();
        match mutation {
            // A throwing const-string would acquire a new catch if widened.
            0 => {
                b.instructions[18] = 0x001a;
                b.instructions[19] = 3;
            }
            // Normal flow must never enter the exception-only entry.
            1 => b.instructions[17] = 7,
            // Throwing from the original handler must not gain self-catching.
            2 => b.instructions[24] = 0x0527,
            // Changes on the repeating path are visible to a later exception.
            3 => {
                b.instructions[20] = 0x04d8;
                b.instructions[21] = 0x0104;
            }
            // File.exists was outside the original try and must remain so.
            4 => b.try_regions[0].end = 12,
            _ => unreachable!(),
        }
        assert!(
            native_java::render_method("sample.Hello", &c, &c.methods[0]).is_err(),
            "mutation {mutation}"
        );
    }
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn generated_java_preserves_short_circuit_and_exception_behavior() {
    use std::{fs, process::Command};
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("sample")).unwrap();
    let mut c = fixture();
    let exact = native_java::render_method("sample.Hello", &c, &c.methods[0])
        .unwrap()
        .source;
    // The same control flow with observable constructor/exists calls checks
    // ordering, exception identity, and uncaught Error propagation.
    Arc::get_mut(&mut c.symbols).unwrap().types[0] = "Lsample/ProbeFile;".into();
    c.methods[0].name = "probe".into();
    let probe = native_java::render_method("sample.Hello", &c, &c.methods[0])
        .unwrap()
        .source;
    let source = format!(
        r#"package sample;
public class Hello {{
{exact}
{probe}
 static void check(boolean value) {{ if (!value) throw new AssertionError(); }}
 public static void main(String[] args) throws Exception {{
   Hello h = new Hello();
   java.nio.file.Path file = java.nio.file.Files.createTempFile("rdx-loop", ".tmp");
   try {{
     String yes = file.toString(), no = yes + ".missing";
     check(!h.filesExist(new String[]{{}})); check(!h.filesExist(new String[]{{no}}));
     check(h.filesExist(new String[]{{no, yes}})); check(h.filesExist(new String[]{{yes, null}}));
     check(Log.calls == 0);
     check(!h.filesExist(null)); check(Log.calls == 1 && Log.last instanceof NullPointerException);
     check(!h.filesExist(new String[]{{null, yes}})); check(Log.calls == 2);
     for (String bad : new String[]{{"ctor", "exists"}}) {{
       Log.calls = 0; ProbeFile.trace = "";
       check(!h.probe(new String[]{{bad,"yes"}}));
       check(Log.calls == 1 && Log.last == ProbeFile.failure);
       check(ProbeFile.trace.equals(bad.equals("ctor") ? "new:ctor;" : "new:exists;exists:exists;"));
     }}
     Log.calls = 0; ProbeFile.trace = "";
     check(h.probe(new String[]{{"no","yes","ctor"}}));
     check(ProbeFile.trace.equals("new:no;exists:no;new:yes;exists:yes;") && Log.calls == 0);
     try {{ h.probe(new String[]{{"fatal"}}); throw new AssertionError(); }}
     catch (Error e) {{ check(e == ProbeFile.fatal && Log.calls == 0); }}
     Log.throwOnLog = true;
     try {{ h.probe(new String[]{{"ctor"}}); throw new AssertionError(); }}
     catch (RuntimeException e) {{ check(e == Log.failure && Log.calls == 1); }}
   }} finally {{ java.nio.file.Files.delete(file); }}
 }}
}}
class Log {{
 static int calls; static Exception last; static boolean throwOnLog;
 static final RuntimeException failure = new RuntimeException("logger");
 static void log(String message, Exception e) {{
   if (!message.equals("Denied read-access to the files")) throw new AssertionError(message);
   calls++; last=e; if (throwOnLog) throw failure;
 }}
}}
class ProbeFile {{
 static String trace=""; static final RuntimeException failure=new SecurityException("denied");
 static final Error fatal=new Error("fatal"); final String path;
 ProbeFile(String p) {{ trace+="new:"+p+";"; path=p; if(p.equals("ctor"))throw failure; }}
 boolean exists() {{ trace+="exists:"+path+";"; if(path.equals("exists"))throw failure; if(path.equals("fatal"))throw fatal; return path.equals("yes"); }}
}}
"#
    );
    fs::write(dir.path().join("sample/Hello.java"), &source).unwrap();
    for (program, arg) in [("javac", "sample/Hello.java"), ("java", "sample.Hello")] {
        let result = Command::new(program)
            .arg(arg)
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{program}: {}\n{source}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}
