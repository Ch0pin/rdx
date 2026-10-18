use rdx::{
    native_dex::{self, DexClass, DexSymbols},
    native_java,
};
use std::sync::Arc;

fn fixture(protected: bool, interior: bool) -> DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class.methods.retain(|m| m.name.as_ref() == "answer");
    class.symbols = Arc::new(DexSymbols {
        strings: vec![
            "more".into(),
            "matches".into(),
            "success".into(),
            "failure".into(),
            "cleanup".into(),
            "finish".into(),
        ],
        types: vec!["Lsample/Search;".into()],
        protos: vec![("Z".into(), vec![]), ("V".into(), vec![])],
        methods: vec![
            (0, 0, 0),
            (0, 0, 1),
            (0, 1, 2),
            (0, 1, 3),
            (0, 1, 4),
            (0, 1, 5),
        ],
        ..Default::default()
    });
    let method = &mut class.methods[0];
    method.name = "test".into();
    method.access_flags = 9;
    method.parameters = vec!["Z".into()];
    method.return_type = "V".into();
    let code = method.code.as_mut().unwrap();
    code.registers = 3;
    code.ins = 1;
    code.outs = 0;
    code.instructions = vec![
        0x0239,
        if interior { 8 } else { 14 },
        0x0071,
        0,
        0,
        0x000a,
        0x0038,
        12,
        0x0071,
        1,
        0,
        0x000a,
        0x0038,
        0xfff6,
        0x0071,
        2,
        0,
        0x0428,
        0x0071,
        3,
        0,
        0x0071,
        5,
        0,
        0x000e,
    ];
    code.tries = u16::from(protected);
    code.try_regions.clear();
    if protected {
        code.try_regions.push(native_dex::DexTryRegion {
            start: 0,
            end: 21,
            catches: vec![(None, 25)].into(),
        });
        code.instructions.extend([0x010d, 0x0071, 4, 0, 0x0127]);
    }
    class
}

#[test]
fn external_success_predecessor_does_not_enter_loop_body() {
    for protected in [false, true] {
        let class = fixture(protected, false);
        let source = native_java::render_method("sample.Search", &class, &class.methods[0])
            .unwrap_or_else(|error| panic!("protected={protected}: {error:#}"))
            .source;
        assert!(source.contains("while"), "{source}");
        assert_eq!(source.matches(".success()").count(), 1, "{source}");
    }
}

#[test]
fn external_body_entry_copies_first_match_before_the_original_search_loop() {
    let class = fixture(false, true);
    let rendered = native_java::render_method("sample.Search", &class, &class.methods[0]).unwrap();
    assert!(rendered.source.contains("while"), "{}", rendered.source);
    assert_eq!(
        rendered.source.matches(".matches()").count(),
        2,
        "{}",
        rendered.source
    );
    assert_eq!(
        rendered
            .links
            .iter()
            .filter(|link| link.label == "sample.Search.matches()Z")
            .count(),
        2
    );
}

#[test]
fn protected_actual_body_entry_retains_exception_ownership_guard() {
    let class = fixture(true, true);
    assert!(native_java::render_method("sample.Search", &class, &class.methods[0]).is_err());
}

#[test]
#[ignore = "requires javac and java; supports RDX_JAVA25_HOME"]
fn actual_body_entry_jvm_preserves_first_match_loop_calls_and_fault_identity() {
    use std::{fs, process::Command};
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let class = fixture(false, true);
    let rendered = native_java::render_method("sample.Search", &class, &class.methods[0]).unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-search-body-entry-{}", std::process::id()));
    let _cleanup = Cleanup(dir.clone());
    fs::create_dir_all(dir.join("sample")).unwrap();
    let source = r#"package sample;
public class Search {
 static int n,target,index,mode;static String trace;static final RuntimeException problem=new IllegalStateException("failure");
 static boolean more(){trace+="more"+index+",";if(mode==1)throw problem;return index<n;}
 static boolean matches(){trace+="matches"+index+",";if(mode==2)throw problem;return index++==target;}
 static void success(){trace+="success,";if(mode==3)throw problem;}
 static void failure(){trace+="failure,";if(mode==4)throw problem;}
 static void finish(){trace+="finish,";if(mode==5)throw problem;}
 METHOD
 static void encodedOracle(boolean entry){boolean body=entry;for(;;){if(!body&&!more()){failure();break;}if(matches()){success();break;}body=false;}finish();}
 static void run(boolean entry){index=0;trace="";boolean actualThrow=false;try{test(entry);}catch(RuntimeException fault){if(fault!=problem)throw new AssertionError("identity");actualThrow=true;}String actualTrace=trace;int actualIndex=index;index=0;trace="";boolean expectedThrow=false;try{encodedOracle(entry);}catch(RuntimeException fault){if(fault!=problem)throw new AssertionError("oracle identity");expectedThrow=true;}if(actualThrow!=expectedThrow||actualIndex!=index||!actualTrace.equals(trace))throw new AssertionError(actualTrace+" != "+trace);}
 public static void main(String[]args){for(n=0;n<=4;n++)for(target=-1;target<=5;target++)for(mode=0;mode<=5;mode++)for(boolean entry:new boolean[]{false,true})run(entry);}
}"#;
    fs::write(
        dir.join("sample/Search.java"),
        source.replace("METHOD", &rendered.source),
    )
    .unwrap();
    for (program, args) in [
        ("javac", vec!["sample/Search.java"]),
        ("java", vec!["-cp", ".", "sample.Search"]),
    ] {
        let executable = std::env::var("RDX_JAVA25_HOME")
            .map(|home| std::path::PathBuf::from(home).join("bin").join(program))
            .unwrap_or_else(|_| program.into());
        let result = Command::new(executable)
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{program}: {}\n{}",
            String::from_utf8_lossy(&result.stderr),
            rendered.source
        );
    }
}

#[test]
fn protected_escape_writing_shared_continuation_input_stays_rejected() {
    let mut class = fixture(true, false);
    let symbols = Arc::get_mut(&mut class.symbols).unwrap();
    symbols.protos.push(("V".into(), vec!["Z".into()]));
    symbols.methods[5].1 = 2;
    let code = class.methods[0].code.as_mut().unwrap();
    code.instructions.insert(21, 0x0212);
    code.instructions[17] = 0x0528;
    code.instructions[22] = 0x1071;
    code.instructions[24] = 2;
    code.try_regions[0].end = 22;
    code.try_regions[0].catches = vec![(None, 26)].into();
    assert!(native_java::render_method("sample.Search", &class, &class.methods[0]).is_err());
}

#[test]
fn protected_escape_after_loop_updates_shared_input_stays_rejected() {
    let mut class = fixture(true, false);
    let symbols = Arc::get_mut(&mut class.symbols).unwrap();
    symbols.protos.push(("V".into(), vec!["Z".into()]));
    symbols.methods[5].1 = 2;
    let code = class.methods[0].code.as_mut().unwrap();
    code.instructions.insert(8, 0x0212);
    code.instructions[1] = 15;
    code.instructions[7] = 13;
    code.instructions[14] = 0xfff5;
    code.instructions[22] = 0x1071;
    code.instructions[24] = 2;
    code.try_regions[0].end = 22;
    code.try_regions[0].catches = vec![(None, 26)].into();
    assert!(native_java::render_method("sample.Search", &class, &class.methods[0]).is_err());
}

#[test]
fn unchanged_non_receiver_input_survives_protected_escape() {
    let class = unchanged_input_fixture();
    native_java::render_method("sample.Search", &class, &class.methods[0]).unwrap();
}

fn unchanged_input_fixture() -> DexClass {
    let mut class = fixture(true, false);
    let symbols = Arc::get_mut(&mut class.symbols).unwrap();
    symbols.protos.push(("V".into(), vec!["Z".into()]));
    symbols.methods[5].1 = 2;
    let code = class.methods[0].code.as_mut().unwrap();
    code.instructions[21] = 0x1071;
    code.instructions[23] = 2;
    class
}

#[test]
#[ignore = "requires javac and java"]
fn search_bypass_exhaustion_matches_and_throw_identity() {
    let class = unchanged_input_fixture();
    let source = native_java::render_method("sample.Search", &class, &class.methods[0])
        .unwrap()
        .source;
    let dir = std::env::temp_dir().join(format!("rdx-shared-search-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("sample")).unwrap();
    std::fs::write(dir.join("sample/Search.java"),format!(r#"package sample;
public class Search {{
 static int n,target,index,mode,successCalls,failureCalls,cleanupCalls,finishCalls;
 static boolean expectedBypass;
 static final RuntimeException problem=new IllegalStateException("failure");
 static boolean more(){{if(mode==1)throw problem;return index<n;}}
 static boolean matches(){{return index++==target;}}
 static void success(){{successCalls++;if(mode==2)throw problem;}}
 static void failure(){{failureCalls++;}}
 static void cleanup(){{cleanupCalls++;}}
 static void finish(boolean value){{if(value!=expectedBypass)throw new AssertionError("shared input");finishCalls++;if(mode==3)throw problem;}}
 {source}
 public static void main(String[]args){{for(n=0;n<4;n++)for(target=0;target<5;target++)for(mode=0;mode<4;mode++)for(boolean bypass:new boolean[]{{false,true}}){{
 index=successCalls=failureCalls=cleanupCalls=finishCalls=0;expectedBypass=bypass;boolean found=bypass||target<n;
 boolean protectedThrow=(!bypass&&mode==1)||(found&&mode==2);boolean throwsExpected=protectedThrow||mode==3;boolean threw=false;
 try{{test(bypass);}}catch(RuntimeException actual){{threw=true;if(actual!=problem)throw new AssertionError("identity");}}
 if(threw!=throwsExpected||cleanupCalls!=(protectedThrow?1:0)||finishCalls!=(protectedThrow?0:1))throw new AssertionError("boundary");
 int expectedSuccess=(!bypass&&mode==1)?0:(found?1:0);
 if(successCalls!=expectedSuccess||failureCalls!=(!found&&mode!=1?1:0))throw new AssertionError("effects");
 if(index!=(bypass||mode==1?0:Math.min(n,target+1)))throw new AssertionError("iterations");
 }}}}
}}"#)).unwrap();
    for (program, args) in [
        ("javac", vec!["sample/Search.java"]),
        ("java", vec!["-cp", ".", "sample.Search"]),
    ] {
        let result = std::process::Command::new(program)
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{program}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn protected_escape_cannot_absorb_effects_after_original_try_end() {
    let mut class = fixture(true, false);
    // Normal success leaves via the goto at 17, but the loop's exhaustion
    // effect at 18 is outside the protected interval. Do not clone it inside.
    class.methods[0].code.as_mut().unwrap().try_regions[0].end = 17;
    assert!(native_java::render_method("sample.Search", &class, &class.methods[0]).is_err());
}
