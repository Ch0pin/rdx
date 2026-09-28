//! A nullable default tail is shared by a loop guard and an external bypass.
use rdx::{
    native_dex::{self, DexClass, DexSymbols, DexTryRegion},
    native_java,
};
use std::{fs, process::Command, sync::Arc};

fn fixture() -> DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class
        .methods
        .retain(|method| method.name.as_ref() == "answer");
    class.symbols = Arc::new(DexSymbols {
        strings: vec![
            "more".into(),
            "next".into(),
            "matches".into(),
            "finish".into(),
            "cleanup".into(),
        ],
        types: vec!["Lsample/DefaultExit;".into()],
        protos: vec![
            ("Z".into(), vec![]),
            ("Ljava/lang/String;".into(), vec![]),
            ("Z".into(), vec!["Ljava/lang/String;".into()]),
            ("I".into(), vec!["Ljava/lang/String;".into()]),
            ("V".into(), vec![]),
        ],
        methods: vec![(0, 0, 0), (0, 1, 1), (0, 2, 2), (0, 3, 3), (0, 4, 4)],
        ..Default::default()
    });
    let method = &mut class.methods[0];
    method.name = "run".into();
    method.parameters = vec!["Z".into()];
    method.return_type = "I".into();
    method.access_flags = 9;
    let code = method.code.as_mut().unwrap();
    code.registers = 5;
    code.ins = 1;
    code.outs = 1;
    code.instructions = vec![
        0x0012, 0x0438, 19, 0x0071, 0, 0, 0x010a, 0x0138, 13, 0x0071, 1, 0, 0x020c, 0x1071, 2, 2,
        0x010a, 0x0138, 0xfff2, 0x0228, 0x0207, 0x1071, 3, 2, 0x000a, 0x000f, 0x010d, 0x0071, 4, 0,
        0x0127,
    ];
    code.tries = 1;
    code.try_regions = vec![DexTryRegion {
        start: 21,
        end: 25,
        catches: vec![(None, 26)].into(),
    }];
    class
}

fn source(class: &DexClass) -> anyhow::Result<String> {
    Ok(native_java::render_method("sample.DefaultExit", class, &class.methods[0])?.source)
}

#[test]
fn pure_shared_default_is_nullable_and_downstream_effect_has_one_owner() {
    let source = source(&fixture()).unwrap();
    assert!(source.contains("while"), "{source}");
    assert!(source.contains("null"), "{source}");
    assert_eq!(source.matches(".finish(").count(), 1, "{source}");
}

#[test]
fn effectful_default_or_body_entry_is_not_duplicated() {
    let mut effectful = fixture();
    let symbols = Arc::get_mut(&mut effectful.symbols).unwrap();
    symbols.strings.push("touch".into());
    symbols.methods.push((0, 4, 5));
    let code = effectful.methods[0].code.as_mut().unwrap();
    code.instructions.splice(20..21, [0x0071, 5, 0, 0x0207]);
    code.instructions[19] = 0x0528;
    code.try_regions[0].start += 3;
    code.try_regions[0].end += 3;
    code.try_regions[0].catches = vec![(None, 29)].into();
    let error = source(&effectful).unwrap_err();
    assert!(
        error.to_string().contains("region boundary") || error.to_string().contains("loop"),
        "{error:#}"
    );
    let mut body_entry = fixture();
    body_entry.methods[0].code.as_mut().unwrap().instructions[2] = 8;
    let error = source(&body_entry).unwrap_err();
    assert!(error.to_string().contains("interior entry"), "{error:#}");
}

#[test]
#[ignore = "requires javac and java"]
fn bypass_empty_miss_match_and_downstream_throw_match_jvm() {
    let source = source(&fixture()).unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-default-exit-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let java = format!(
        r#"package sample;
public class DefaultExit {{
 static int n,target,index,mode,moreCalls,nextCalls,matchCalls,finishCalls,cleanupCalls;
 static final RuntimeException failure=new IllegalStateException("downstream");
 static boolean more(){{moreCalls++;return index<n;}}
 static String next(){{nextCalls++;return Integer.toString(index++);}}
 static boolean matches(String value){{matchCalls++;return Integer.parseInt(value)==target;}}
 static int finish(String value){{finishCalls++;if(mode==1)throw failure;return value==null?-1:Integer.parseInt(value);}}
 static void cleanup(){{cleanupCalls++;}}
 {source}
 public static void main(String[]args){{for(boolean list:new boolean[]{{false,true}})for(n=0;n<4;n++)for(target=0;target<5;target++)for(mode=0;mode<2;mode++){{
 index=moreCalls=nextCalls=matchCalls=finishCalls=cleanupCalls=0;
 int expected=list&&target<n?target:-1;boolean threw=false;
 try{{int actual=run(list);if(mode==1||actual!=expected)throw new AssertionError("result");}}
 catch(RuntimeException actual){{threw=true;if(actual!=failure)throw new AssertionError("identity",actual);}}
 int iterations=list?Math.min(n,target+1):0;
 if(threw!=(mode==1)||finishCalls!=1||cleanupCalls!=(mode==1?1:0)
 ||moreCalls!=(list?iterations+(target>=n?1:0):0)||nextCalls!=iterations||matchCalls!=iterations)
 throw new AssertionError("effects");
 }}}}
}}"#
    );
    fs::write(dir.join("sample/DefaultExit.java"), &java).unwrap();
    for (program, args) in [
        ("javac", vec!["sample/DefaultExit.java"]),
        ("java", vec!["-cp", ".", "sample.DefaultExit"]),
    ] {
        let result = Command::new(program)
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{program}: {}\n{java}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
