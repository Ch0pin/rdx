use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols},
    native_java,
};
use std::{fs, process::Command, sync::Arc};

fn fixture() -> DexClass {
    let mut c = rdx::native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    c.descriptor = "Lsample/Test;".into();
    c.superclass = Some("Ljava/lang/Object;".into());
    c.fields.clear();
    c.symbols = Arc::new(DexSymbols {
        types: vec![
            "Lsample/Test;".into(),
            "Ljava/lang/IllegalArgumentException;".into(),
        ],
        strings: vec!["touch".into(), "<init>".into()],
        protos: vec![("V".into(), vec!["I".into()]), ("V".into(), vec![])],
        methods: vec![(0, 0, 0), (1, 1, 1)],
        ..Default::default()
    });
    c.methods = vec![DexMethod {
        declaring_type: c.descriptor.clone(),
        name: "run".into(),
        return_type: "V".into(),
        parameters: vec!["I".into(), "I".into(), "Z".into()],
        access_flags: 9,
        thrown_types: vec![],
        code: Some(DexCode {
            registers: 4,
            ins: 3,
            outs: 1,
            tries: 0,
            try_regions: vec![],
            offset: 0,
            instructions: vec![
                0x012b, 28, 0,      // switch selector: 0 -> 4; 1 -> 10
                0x000e, // default
                0x0338, 14, // case 0: !hit -> shared return
                0x1071, 0, 2,      // touch(n)
                0x0928, // -> shared return
                0x0012, // case 1: i=0
                0x2035, 11, // i>=n -> throw island
                0x0338, 6, // !hit -> increment
                0x1071, 0, 0,      // touch(i)
                0x000e, // @18 shared return, physically inside loop
                0x00d8, 0x0100, // i++
                0xf628, // -> loop header @11
                0x0022, 1, 0x1070, 1, 0, 0x0027, // throwing tail
                0x0100, 2, 0, 0, 4, 0, 10, 0,
            ],
        }),
    }];
    c
}
fn source() -> String {
    let c = fixture();
    native_java::render_method("sample.Test", &c, &c.methods[0])
        .unwrap()
        .source
}
#[test]
fn unrelated_switch_arms_share_return_inside_terminal_loop() {
    let text = source();
    assert!(
        text.contains("while (") && text.contains("IllegalArgumentException"),
        "{text}"
    );
    assert_eq!(text.matches("touch(").count(), 2, "{text}");
}
#[test]
#[ignore = "requires RDX_JAVA_HOME pointing to JDK17+"]
fn jvm_shared_return_preserves_every_switch_and_loop_exit() {
    let dir = std::env::temp_dir().join(format!("rdx-shared-switch-return-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let text = source();
    fs::write(dir.join("sample/Test.java"),format!(r#"package sample;
public class Test {{static int calls, value; static void touch(int n){{calls++; value=n;}}
{text}
public static void main(String[] args){{int cases=0;
for(int selector=-1;selector<=2;selector++)for(int n=-1;n<=3;n++)for(boolean hit:new boolean[]{{false,true}}){{
calls=0;value=-99;boolean thrown=false;
try{{run(selector,n,hit);}}catch(IllegalArgumentException expected){{thrown=true;}}
boolean expectedThrow=selector==1&&(!hit||n<=0);
int expectedCalls=selector==0&&hit||selector==1&&hit&&n>0?1:0;
int expectedValue=expectedCalls==0?-99:selector==0?n:0;
if(thrown!=expectedThrow||calls!=expectedCalls||value!=expectedValue)throw new AssertionError(selector+":"+n+":"+hit);
cases++;}}System.out.print("ok:"+cases);}}
}}
"#)).unwrap();
    let home = std::env::var("RDX_JAVA_HOME").unwrap();
    let result = Command::new(format!("{home}/bin/javac"))
        .arg(dir.join("sample/Test.java"))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}\n{text}",
        String::from_utf8_lossy(&result.stderr)
    );
    let result = Command::new(format!("{home}/bin/java"))
        .args(["-Xverify:all", "-cp"])
        .arg(&dir)
        .arg("sample.Test")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(result.stdout, b"ok:40");
    fs::remove_dir_all(dir).unwrap();
}
