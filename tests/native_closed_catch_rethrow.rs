use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::sync::Arc;
const WORDS: &[u16] = &[
    0x0071, 0, 0, 0x0138, 5, 0x1071, 1, 1, 0x000e, 0x000d, 0x0138, 5, 0x1071, 1, 1, 0x0027,
];
fn fixture(checked: bool) -> DexClass {
    let symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/Worker;".into()],
        strings: vec!["touch".into(), "cleanup".into()],
        protos: vec![("V".into(), vec![]), ("V".into(), vec!["I".into()])],
        methods: vec![(0, 0, 0), (0, 1, 1)],
        ..Default::default()
    });
    let method = |owner: &str, name: &str, parameters: Vec<Arc<str>>| DexMethod {
        declaring_type: owner.into(),
        name: name.into(),
        return_type: "V".into(),
        parameters,
        thrown_types: vec![],
        access_flags: 0x109,
        code: None,
    };
    let class = |descriptor: &str, methods| DexClass {
        descriptor: descriptor.into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods,
        symbols: symbols.clone(),
    };
    let mut worker = class(
        "Lsample/Worker;",
        vec![
            method("Lsample/Worker;", "touch", vec![]),
            method("Lsample/Worker;", "cleanup", vec!["I".into()]),
        ],
    );
    if checked {
        worker.methods[0].thrown_types = vec!["Ljava/io/IOException;".into()];
    }
    let mut root_method = method("Lsample/Root;", "run", vec!["I".into()]);
    root_method.access_flags = 9;
    root_method.code = Some(DexCode {
        registers: 2,
        ins: 1,
        outs: 1,
        tries: 1,
        try_regions: vec![DexTryRegion {
            start: 0,
            end: 3,
            catches: vec![(None, 9)].into(),
        }],
        instructions: WORDS.to_vec(),
        offset: 0,
    });
    let root = class("Lsample/Root;", vec![root_method]);
    symbols
        .hierarchy
        .set(Arc::new(
            TypeHierarchy::from_classes([&root, &worker]).unwrap(),
        ))
        .unwrap();
    root
}
#[test]
fn loaded_native_headers_allow_same_catch_identity_through_conditional_cleanup() {
    let class = fixture(false);
    let source = native_java::render("sample.Root", &class).unwrap().source;
    assert!(source.contains("catch ("), "{source}");
    assert!(source.contains("throw "), "{source}");
    let declaration = source.lines().find(|line| line.contains(" run(")).unwrap();
    assert!(!declaration.contains(" throws "), "{source}");
}
#[test]
fn checked_dependency_keeps_identity_and_primitive_replacement_declines() {
    let class = fixture(true);
    let source = native_java::render("sample.Root", &class).unwrap().source;
    assert!(source.contains("throw _rdx_caught_throw_"), "{source}");
    assert!(
        !source
            .lines()
            .find(|line| line.contains(" run("))
            .unwrap()
            .contains(" throws "),
        "{source}"
    );
    let mut class = fixture(false);
    let code = class.methods[0].code.as_mut().unwrap();
    code.instructions[9] = 0x1012; // nonzero primitive cannot replace caught reference
    assert!(native_java::render("sample.Root", &class).is_err());
}
#[test]
#[ignore = "requires javac/java; root runs serially"]
fn jvm_conditional_cleanup_matches_raw_dex_exception_identity_and_effects() {
    use std::{fs, process::Command};
    let class = fixture(false);
    let base = std::env::temp_dir().join(format!("rdx-closed-catch-{}", std::process::id()));
    fs::create_dir_all(base.join("sample")).unwrap();
    fs::write(
        base.join("sample/Root.java"),
        native_java::render("sample.Root", &class).unwrap().source,
    )
    .unwrap();
    fs::write(base.join("sample/Worker.java"),r#"package sample;
public class Worker { static int mode;static String events="";
static final java.io.IOException PRIMARY=new java.io.IOException();static final Error CLEANUP=new Error();
@SuppressWarnings("unchecked") static <E extends Throwable> void sneaky(Throwable value) throws E {throw (E)value;}
public static void touch(){events+="T";if(mode>0)sneaky(PRIMARY);}
public static void cleanup(int flag){events+="C";if(mode==2)sneaky(CLEANUP);}
}"#).unwrap();
    let words = WORDS
        .iter()
        .map(|word| word.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let driver=r#"package sample;public class Check {
static final int[] words={WORDS};
static void raw(int flag) throws Throwable {int pc=0;Throwable caught=null;for(int budget=0;budget<50;budget++){
int op=words[pc]&255;try{if(op==0x71){if(words[pc+1]==0)Worker.touch();else Worker.cleanup(flag);pc+=3;}
else if(op==0x38)pc+=flag==0?(short)words[pc+1]:2;
else if(op==0x0e)return;
else if(op==0x0d)pc++;
else if(op==0x27)throw caught;
else throw new AssertionError("opcode "+op);
}catch(Throwable error){if(pc<3){caught=error;pc=9;}else throw error;}}
throw new AssertionError("budget");}
static String run(int mode,int flag,boolean emitted){Worker.mode=mode;Worker.events="";Throwable error=null;
try{if(emitted)Root.run(flag);else raw(flag);}catch(Throwable value){error=value;}
String identity=error==null?"return":error==Worker.PRIMARY?"primary":error==Worker.CLEANUP?"cleanup":"wrong";
if(identity.equals("wrong"))throw new AssertionError(error);return identity+":"+Worker.events;}
public static void main(String[]args){for(int mode=0;mode<3;mode++)for(int flag=0;flag<=1;flag++){String a=run(mode,flag,false),b=run(mode,flag,true);if(!a.equals(b))throw new AssertionError(a+" != "+b);}System.out.print("ok");}}
"#.replace("WORDS",&words);
    fs::write(base.join("sample/Check.java"), driver).unwrap();
    let compiled = Command::new("javac")
        .args([
            "-Xlint:all",
            "-Werror",
            "sample/Root.java",
            "sample/Worker.java",
            "sample/Check.java",
        ])
        .current_dir(&base)
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let executed = Command::new("java")
        .args(["-cp", ".", "sample.Check"])
        .current_dir(&base)
        .output()
        .unwrap();
    assert!(
        executed.status.success(),
        "{}",
        String::from_utf8_lossy(&executed.stderr)
    );
    assert_eq!(executed.stdout, b"ok");
    fs::remove_dir_all(base).unwrap();
}
