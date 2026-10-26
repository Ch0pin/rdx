use rdx::{native_dex, native_java};
use std::io::Read;
use std::sync::Arc;

fn fixture() -> native_dex::DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class.superclass = Some("Lsample/Base;".into());
    class.fields.clear();
    class.methods.retain(|m| m.name.as_ref() == "answer");
    class.symbols = Arc::new(native_dex::DexSymbols {
        strings: vec!["step".into(), "<init>".into()],
        types: vec!["Lsample/Probe;".into(), "Lsample/Base;".into()],
        protos: vec![
            ("I".into(), vec!["I".into()]),
            ("V".into(), vec!["I".into()]),
        ],
        methods: vec![(0, 0, 0), (1, 1, 1)],
        ..Default::default()
    });
    let method = &mut class.methods[0];
    method.name = "<init>".into();
    method.access_flags = 1;
    method.parameters = vec!["I".into()];
    method.return_type = "V".into();
    let code = method.code.as_mut().unwrap();
    code.registers = 5;
    code.ins = 2;
    code.outs = 2;
    code.tries = 0;
    code.try_regions.clear();
    code.instructions = vec![
        0x0012, 0x0112, // sum and index
        0x4135, 10, // if index >= p0, delegate
        0x1071, 0, 1, 0x020a, // step(index)
        0x20b0, 0x01d8, 0x0101, // sum += result; index++
        0xf728, // backedge
        0x2070, 1, 0x0003, // super(sum)
        0x000e,
    ];
    class
}

#[test]
fn constructor_argument_loop_preserves_delegation_order_and_links() {
    let class = fixture();
    let source = native_java::render_method("sample.Hello", &class, &class.methods[0]).unwrap();
    assert!(
        source.source.find("while (").unwrap() < source.source.find("super(").unwrap(),
        "{}",
        source.source
    );
    assert_eq!(source.source.matches("sample.Probe.step(").count(), 1);
    assert_eq!(source.source.matches("super(").count(), 1);
    assert!(
        source
            .links
            .iter()
            .any(|l| l.label == "sample.Base.<init>(I)V")
    );
}

#[test]
fn constructor_loop_rejects_receiver_escape_overwrite_and_early_return() {
    let mut class = fixture();
    Arc::get_mut(&mut class.symbols).unwrap().protos[0].1 = vec!["Ljava/lang/Object;".into()];
    class.methods[0].code.as_mut().unwrap().instructions[6] = 3;
    assert!(native_java::render_method("sample.Hello", &class, &class.methods[0]).is_err());
    let mut class = fixture();
    class.methods[0].code.as_mut().unwrap().instructions[9] = 0x03d8;
    assert!(native_java::render_method("sample.Hello", &class, &class.methods[0]).is_err());
    let mut class = fixture();
    class.methods[0].code.as_mut().unwrap().instructions[8] = 0x000e;
    assert!(native_java::render_method("sample.Hello", &class, &class.methods[0]).is_err());
}

#[test]
#[ignore = "requires Java 25 javac and java on PATH"]
fn constructor_loop_executes_before_super_including_empty_and_throwing_paths() {
    let class = fixture();
    let source = native_java::render_method("sample.Hello", &class, &class.methods[0])
        .unwrap()
        .source;
    let dir = std::env::temp_dir().join(format!("rdx-constructor-loop-{}", std::process::id()));
    let package = dir.join("sample");
    std::fs::create_dir_all(&package).unwrap();
    let source = format!(
        r#"package sample;
public class Hello extends Base {{
{source}
    public static void main(String[] args) {{
        for (int n : new int[]{{0, 1, 4}}) {{
            for (int fail : new int[]{{-1, 0, 1}}) {{
                Probe.events = ""; Probe.fail = fail; Base.calls = 0;
                String expected = ""; int sum = 0; boolean throwsExpected = false;
                for (int i = 0; i < n; i++) {{
                    expected += "r" + i + ";";
                    if (i == fail) {{ throwsExpected = true; break; }}
                    sum += i + 1;
                }}
                if (!throwsExpected) expected += "b" + sum + ";";
                boolean threw = false;
                try {{ new Hello(n); }} catch (IllegalStateException e) {{ threw = true; }}
                if (threw != throwsExpected || !Probe.events.equals(expected)
                    || Base.calls != (throwsExpected ? 0 : 1))
                    throw new AssertionError(n + ":" + fail + ":" + Probe.events);
            }}
        }}
        System.out.println("9 constructor order and exception cases passed");
    }}
}}
class Base {{
    static int calls;
    Base(int n) {{ calls++; Probe.events += "b" + n + ";"; }}
}}
class Probe {{
    static int fail; static String events;
    static int step(int i) {{
        events += "r" + i + ";";
        if (i == fail) throw new IllegalStateException();
        return i + 1;
    }}
}}
"#
    );
    let path = package.join("Hello.java");
    std::fs::write(&path, source).unwrap();
    let compile = std::process::Command::new("javac")
        .args(["--release", "25", "-d"])
        .arg(&dir)
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let execute = std::process::Command::new("java")
        .arg("-cp")
        .arg(&dir)
        .arg("sample.Hello")
        .output()
        .unwrap();
    assert!(
        execute.status.success(),
        "{}",
        String::from_utf8_lossy(&execute.stderr)
    );
    eprintln!("{}", String::from_utf8_lossy(&execute.stdout));
}

#[test]
#[ignore = "requires RDX_CONSTRUCTOR_APK, RDX_CONSTRUCTOR_CLASS, and RDX_CONSTRUCTOR_OUTPUT"]
fn real_constructor_with_loop_before_delegation() {
    let path = std::env::var_os("RDX_CONSTRUCTOR_APK").unwrap();
    let name = std::env::var("RDX_CONSTRUCTOR_CLASS").unwrap();
    let descriptor = format!("L{};", name.replace('.', "/"));
    let mut archive = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).unwrap();
        if !entry.name().ends_with(".dex") {
            continue;
        }
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        let dex = native_dex::parse(&bytes).unwrap();
        if let Some(class) = dex
            .classes
            .iter()
            .find(|c| c.descriptor.as_ref() == descriptor)
        {
            let method = class
                .methods
                .iter()
                .find(|m| {
                    m.name.as_ref() == "<init>"
                        && m.parameters.len() == 1
                        && m.parameters[0].as_ref() == "Ljava/util/List;"
                })
                .unwrap();
            let source = native_java::render_method(&name, class, method).unwrap();
            std::fs::write(
                std::env::var_os("RDX_CONSTRUCTOR_OUTPUT").unwrap(),
                &source.source,
            )
            .unwrap();
            assert!(source.source.contains("super("));
            assert!(!source.source.contains(".method"));
            return;
        }
    }
    panic!("class not found: {name}");
}
