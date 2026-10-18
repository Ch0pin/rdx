//! A cleanup catch can begin inside its own protected interval and jump past
//! the enclosing branch join through a nonthrowing prefix.
use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_java,
};
use std::{fs, process::Command, sync::Arc};

fn fixture() -> DexClass {
    DexClass {
        descriptor: "Lsample/Effects;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols {
            strings: vec!["body".into(), "fail".into(), "cleanup".into()],
            types: vec!["Lsample/Effects;".into()],
            protos: vec![("V".into(), vec![])],
            methods: vec![(0, 0, 0), (0, 0, 1), (0, 0, 2)],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: "Lsample/Effects;".into(),
            name: "run".into(),
            return_type: "V".into(),
            parameters: vec!["I".into()],
            thrown_types: vec![],
            access_flags: 9,
            code: Some(DexCode {
                registers: 2,
                ins: 1,
                outs: 0,
                tries: 2,
                offset: 0,
                instructions: vec![
                    0x0012, // 00 null
                    0x0138, 8, // 01 if input == 0 -> 09
                    0x0071, 0, 0,      // 03 body()
                    0x000e, // 06 return (start of second protected range)
                    0x000d, // 07 move-exception v0
                    0x0628, // 08 goto 14 (past branch join 09)
                    0x0071, 1, 0, // 09 fail()
                    0x0012, 0x0027, // 12 throw null if fail returns
                    0x0071, 2, 0,      // 14 cleanup() outside self coverage
                    0x0027, // 17 rethrow identical exception
                ],
                try_regions: vec![
                    DexTryRegion {
                        start: 3,
                        end: 6,
                        catches: vec![(None, 7)].into(),
                    },
                    DexTryRegion {
                        start: 6,
                        end: 14,
                        catches: vec![(None, 7)].into(),
                    },
                ],
            }),
        }],
    }
}

#[test]
fn detached_self_covered_prefix_reconstructs() {
    let class = fixture();
    let source = native_java::render_method("sample.Effects", &class, &class.methods[0])
        .unwrap()
        .source;
    assert!(source.contains("catch (java.lang.Throwable"), "{source}");
    assert!(source.contains("sample.Effects.cleanup()"), "{source}");
}

#[test]
fn detached_self_covered_prefix_preserves_exception_identity_and_cleanup_order() {
    let class = fixture();
    let source = native_java::render_method("sample.Effects", &class, &class.methods[0])
        .unwrap()
        .source;
    let dir = std::env::temp_dir().join(format!("rdx-handler-prefix-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let java = format!(
        r#"package sample;
public class Effects {{
static String trace; static boolean bodyFails, cleanupFails, failReturns;
static final RuntimeException original = new RuntimeException("original");
static final RuntimeException replacement = new RuntimeException("cleanup");
static void body() {{ trace += "B"; if(bodyFails) throw original; }}
static void fail() {{ trace += "F"; if(!failReturns) throw original; }}
static void cleanup() {{ trace += "C"; if(cleanupFails) throw replacement; }}
{source}
static void check(int input, boolean b, boolean c, boolean f, String expected, Throwable error) {{
trace=""; bodyFails=b; cleanupFails=c; failReturns=f;
Throwable seen=null; try {{ run(input); }} catch(Throwable e) {{seen=e;}}
if(!trace.equals(expected) || seen!=error) throw new AssertionError(trace+" "+seen);
}}
public static void main(String[] args) {{
check(1,false,false,false,"B",null);
check(1,true,false,false,"BC",original);
check(1,true,true,false,"BC",replacement);
check(0,false,false,false,"FC",original);
check(0,false,true,false,"FC",replacement);
trace=""; failReturns=true; cleanupFails=false;
try {{run(0); throw new AssertionError();}} catch(NullPointerException expected) {{}}
if(!trace.equals("FC")) throw new AssertionError(trace);
}}
}}"#
    );
    fs::write(dir.join("sample/Effects.java"), java).unwrap();
    let compile = Command::new("javac")
        .arg(dir.join("sample/Effects.java"))
        .output()
        .expect("javac required");
    assert!(
        compile.status.success(),
        "{}\n{source}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new("java")
        .arg("-cp")
        .arg(&dir)
        .arg("sample.Effects")
        .output()
        .expect("java required");
    assert!(
        run.status.success(),
        "{}\n{source}",
        String::from_utf8_lossy(&run.stderr)
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn effectful_or_reentering_prefix_remains_rejected() {
    let mut class = fixture();
    // The catch now reaches its own protected throwing call, causing recursive
    // exception dispatch. It cannot be cloned into an ordinary Java catch.
    class.methods[0].code.as_mut().unwrap().instructions[8] = 0x0128;
    assert!(native_java::render_method("sample.Effects", &class, &class.methods[0]).is_err());
    // A prefix loop cannot be accepted as a terminating cleanup.
    class.methods[0].code.as_mut().unwrap().instructions[8] = 0x0028;
    assert!(native_java::render_method("sample.Effects", &class, &class.methods[0]).is_err());
}
