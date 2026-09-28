//! A terminal protected arm can sit after a monitor's sole normal release.
use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_java,
};
use std::{fs, process::Command, sync::Arc};

fn fixture() -> DexClass {
    DexClass {
        descriptor: "Lsample/MonitorTerminal;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols {
            strings: vec!["normal".into(), "terminal".into()],
            types: vec!["Lsample/Hook;".into()],
            protos: vec![("I".into(), vec![]), ("V".into(), vec![])],
            methods: vec![(0, 0, 0), (0, 1, 1)],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: "Lsample/MonitorTerminal;".into(),
            name: "test".into(),
            return_type: "I".into(),
            parameters: vec!["Ljava/lang/Object;".into(), "Z".into()],
            thrown_types: vec![],
            access_flags: 9,
            code: Some(DexCode {
                registers: 4,
                ins: 2,
                outs: 0,
                tries: 2,
                try_regions: vec![
                    DexTryRegion {
                        start: 1,
                        end: 7,
                        catches: vec![(None, 14)].into(),
                    },
                    DexTryRegion {
                        start: 9,
                        end: 14,
                        catches: vec![(None, 14)].into(),
                    },
                ],
                instructions: vec![
                    0x021d, // monitor-enter v2
                    0x0338, 8, // if-eqz v3, 9
                    0x0071, 0, 0,      // Hook.normal()
                    0x000a, // move-result v0
                    0x021e, // monitor-exit v2
                    0x000f, // return v0
                    0x0071, 1, 0,      // Hook.terminal()
                    0x0012, // const/4 v0, 0
                    0x0027, // throw v0
                    0x000d, // move-exception v0
                    0x021e, // monitor-exit v2
                    0x0027, // throw v0
                ],
                offset: 0,
            }),
        }],
    }
}
fn render(class: &DexClass) -> anyhow::Result<String> {
    Ok(native_java::render_method("sample.MonitorTerminal", class, &class.methods[0])?.source)
}
#[test]
fn terminal_arm_stays_under_same_monitor() {
    let source = render(&fixture()).unwrap();
    assert_eq!(source.matches("synchronized (p0)").count(), 1, "{source}");
    let terminal = source.find("Hook.terminal()").unwrap();
    let close = source.rfind('}').unwrap();
    assert!(terminal < close, "{source}");
    assert_eq!(source.matches("Hook.normal()").count(), 1, "{source}");
}
#[test]
fn rejects_unprotected_arm_cycle_and_return_without_release() {
    let mut class = fixture();
    class.methods[0].code.as_mut().unwrap().try_regions.pop();
    assert!(
        render(&class).is_err(),
        "unprotected terminal effect accepted"
    );

    let mut class = fixture();
    class.methods[0].code.as_mut().unwrap().instructions[13] = 0xfc28; // goto 9
    assert!(render(&class).is_err(), "terminal arm cycle accepted");

    let mut class = fixture();
    class.methods[0].code.as_mut().unwrap().instructions[13] = 0x000f;
    assert!(render(&class).is_err(), "return without release accepted");
}
#[test]
#[ignore = "requires javac and java on PATH"]
fn jvm_preserves_effect_order_exception_identity_and_lock_release() {
    let method = render(&fixture()).unwrap();
    let java = format!(
        r#"package sample;
class Hook {{
  static Object lock;
  static int normalCount, terminalCount;
  static boolean failNormal, failTerminal;
  static final RuntimeException NORMAL = new RuntimeException("normal");
  static final RuntimeException TERMINAL = new RuntimeException("terminal");
  static int normal() {{
    if (!Thread.holdsLock(lock)) throw new AssertionError("normal lock");
    normalCount++;
    if (failNormal) throw NORMAL;
    return 17;
  }}
  static void terminal() {{
    if (!Thread.holdsLock(lock)) throw new AssertionError("terminal lock");
    terminalCount++;
    if (failTerminal) throw TERMINAL;
  }}
}}
public class MonitorTerminal {{
{method}
  static void released(Object lock) throws Exception {{
    if (Thread.holdsLock(lock)) throw new AssertionError("lock remains held");
    final boolean[] acquired = {{false}};
    Thread thread = new Thread(() -> {{ synchronized(lock) {{ acquired[0] = true; }} }});
    thread.start(); thread.join(2000);
    if (!acquired[0]) throw new AssertionError("other thread blocked");
  }}
  public static void main(String[] args) throws Exception {{
    Hook.lock = new Object();
    if (test(Hook.lock, true) != 17 || Hook.normalCount != 1 || Hook.terminalCount != 0) throw new AssertionError("normal");
    released(Hook.lock);
    Hook.failNormal = true;
    try {{ test(Hook.lock, true); throw new AssertionError("missing normal exception"); }}
    catch (RuntimeException error) {{ if (error != Hook.NORMAL) throw new AssertionError("normal identity"); }}
    if (Hook.normalCount != 2 || Hook.terminalCount != 0) throw new AssertionError("normal count");
    released(Hook.lock);
    Hook.failTerminal = true;
    try {{ test(Hook.lock, false); throw new AssertionError("missing terminal exception"); }}
    catch (RuntimeException error) {{ if (error != Hook.TERMINAL) throw new AssertionError("terminal identity"); }}
    if (Hook.normalCount != 2 || Hook.terminalCount != 1) throw new AssertionError("terminal count");
    released(Hook.lock);
    Hook.failTerminal = false;
    try {{ test(Hook.lock, false); throw new AssertionError("missing null throw"); }}
    catch (NullPointerException expected) {{ }}
    if (Hook.normalCount != 2 || Hook.terminalCount != 2) throw new AssertionError("null count");
    released(Hook.lock);
  }}
}}"#
    );
    let dir = std::env::temp_dir().join(format!("rdx-monitor-terminal-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(dir.join("sample/MonitorTerminal.java"), java).unwrap();
    for (program, arg) in [
        ("javac", "sample/MonitorTerminal.java"),
        ("java", "sample.MonitorTerminal"),
    ] {
        let output = Command::new(program)
            .arg(arg)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{program}: {}\n{}",
            String::from_utf8_lossy(&output.stderr),
            dir.display()
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
