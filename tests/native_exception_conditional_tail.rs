use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_java,
};
use std::sync::Arc;
fn fixture() -> (DexClass, DexMethod) {
    let names = ["protectedCall", "protectedEnd", "normal", "handler", "join"];
    let symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/Api;".into()],
        strings: names.into_iter().map(Into::into).collect(),
        protos: vec![("V".into(), vec![])],
        methods: (0..5).map(|i| (0, 0, i)).collect(),
        ..Default::default()
    });
    let m = DexMethod {
        declaring_type: "Lsample/Example;".into(),
        name: "run".into(),
        return_type: "V".into(),
        parameters: vec![],
        thrown_types: vec![],
        access_flags: 9,
        code: Some(DexCode {
            registers: 1,
            ins: 0,
            outs: 0,
            tries: 1,
            try_regions: vec![DexTryRegion {
                start: 0,
                end: 6,
                catches: vec![(Some("Ljava/lang/RuntimeException;".into()), 10)].into(),
            }],
            instructions: vec![
                0x0071, 0, 0, 0x0071, 1, 0, 0x0071, 2, 0, 0x0528, 0x000d, 0x0071, 3, 0, 0x0071, 4,
                0, 0x000e,
            ],
            offset: 0,
        }),
    };
    let c = DexClass {
        symbols,
        descriptor: "Lsample/Example;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 0x11,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![],
    };
    (c, m)
}
#[test]
fn normal_only_effects_stay_after_catch_before_shared_join() {
    let (c, m) = fixture();
    let s = native_java::render_method("sample.Example", &c, &m)
        .unwrap()
        .source;
    assert!(s.contains("= false;"), "{s}");
    assert!(s.contains("= true;"), "{s}");
    let catch = s.find("catch").unwrap();
    let normal = s.find(".normal(").unwrap();
    let join = s.find(".join(").unwrap();
    assert!(catch < normal && normal < join, "{s}");
}
#[test]
fn normal_flow_entering_handler_is_rejected() {
    let (c, mut m) = fixture();
    m.code.as_mut().unwrap().instructions[9] = 0x0128;
    assert!(native_java::render_method("sample.Example", &c, &m).is_err());
}
#[test]
fn continuation_navigation_spelling_remains_exact() {
    let (c, m) = fixture();
    let s = native_java::render_method("sample.Example", &c, &m).unwrap();
    for label in [
        "sample.Api.normal()V",
        "sample.Api.handler()V",
        "sample.Api.join()V",
    ] {
        let l = s.links.iter().find(|l| l.label == label).unwrap();
        let name = label
            .split('.')
            .next_back()
            .unwrap()
            .split('(')
            .next()
            .unwrap();
        assert_eq!(
            s.source
                .chars()
                .skip(l.start)
                .take(l.end - l.start)
                .collect::<String>(),
            name
        );
    }
}
#[test]
#[ignore = "requires javac and java on PATH"]
fn jvm_faults_preserve_tail_ownership_and_identity() {
    let (c, m) = fixture();
    let source = native_java::render_method("sample.Example", &c, &m)
        .unwrap()
        .source;
    let dir = std::env::temp_dir().join(format!("rdx-conditional-tail-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(dir.clone());
    let java = format!(
        r#"package sample;class Api{{static int mode;static String trace;static RuntimeException original=new RuntimeException(),second=new RuntimeException();static void protectedCall(){{trace+="P";if(mode==1||mode==3)throw original;}}static void protectedEnd(){{trace+="E";if(mode==4)throw original;}}static void normal(){{trace+="N";if(mode==2)throw original;}}static void handler(){{trace+="H";if(mode==3)throw second;}}static void join(){{trace+="J";if(mode==5)throw original;}}}}class Example{{{source}}}public class Runner{{public static void main(String[]args){{String[]traces={{"PENJ","PHJ","PEN","PH","PEHJ","PENJ"}};for(int mode=0;mode<6;mode++){{Api.mode=mode;Api.trace="";try{{Example.run();if(mode==2||mode==3||mode==5)throw new AssertionError("lost failure");}}catch(RuntimeException caught){{if(caught!=(mode==3?Api.second:Api.original)||(mode!=2&&mode!=3&&mode!=5))throw new AssertionError("identity");}}if(!Api.trace.equals(traces[mode]))throw new AssertionError(Api.trace);}}}}}}"#
    );
    let path = dir.join("Runner.java");
    std::fs::write(&path, java).unwrap();
    let o = std::process::Command::new("javac")
        .arg("-d")
        .arg(&dir)
        .arg(path)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "{}\n{source}",
        String::from_utf8_lossy(&o.stderr)
    );
    let o = std::process::Command::new("java")
        .arg("-cp")
        .arg(&dir)
        .arg("sample.Runner")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}
#[test]
fn wide_join_live_input_keeps_complete_word_pair() {
    let (c, mut m) = fixture();
    m.return_type = "J".into();
    m.parameters = vec!["J".into()];
    let code = m.code.as_mut().unwrap();
    code.registers = 3;
    code.ins = 2;
    code.instructions[17] = 0x0110;
    let s = native_java::render_method("sample.Example", &c, &m)
        .unwrap()
        .source;
    assert!(s.contains("return p0;"), "{s}");
    assert!(s.contains("if ("), "{s}");
}
#[test]
fn declared_checked_tail_exception_remains_declared() {
    let (c, mut m) = fixture();
    m.thrown_types = vec!["Ljava/io/IOException;".into()];
    m.code.as_mut().unwrap().try_regions[0].catches =
        vec![(Some("Ljava/io/IOException;".into()), 10)].into();
    let s = native_java::render_method("sample.Example", &c, &m)
        .unwrap()
        .source;
    assert!(s.contains("throws java.io.IOException"), "{s}");
    assert!(
        s.find("catch").unwrap() < s.find(".normal(").unwrap(),
        "{s}"
    );
}
#[test]
fn normal_tail_writing_shared_join_value_has_path_owned_storage() {
    let (mut c, mut m) = fixture();
    let symbols = Arc::get_mut(&mut c.symbols).unwrap();
    symbols.protos.push(("I".into(), vec![]));
    symbols.methods[2].1 = 1;
    m.return_type = "I".into();
    let code = m.code.as_mut().unwrap();
    code.registers = 2;
    code.try_regions[0].start = 1;
    code.try_regions[0].end = 7;
    code.try_regions[0].catches = vec![(Some("Ljava/lang/RuntimeException;".into()), 12)].into();
    code.instructions = vec![
        0x0012, 0x0071, 0, 0, 0x0071, 1, 0, 0x0071, 2, 0, 0x000a, 0x0628, 0x010d, 0x0071, 3, 0,
        0x7012, 0x000f,
    ];
    let source = native_java::render_method("sample.Example", &c, &m)
        .unwrap()
        .source;
    assert!(
        source.find("catch").unwrap() < source.find(".normal(").unwrap(),
        "{source}"
    );
}
#[test]
fn handler_reentering_normal_tail_remains_rejected() {
    let (c, mut m) = fixture();
    m.code.as_mut().unwrap().instructions[11] = 0xfb28;
    m.code.as_mut().unwrap().instructions[12] = 0x0000;
    m.code.as_mut().unwrap().instructions[13] = 0x0000;
    assert!(native_java::render_method("sample.Example", &c, &m).is_err());
}
#[test]
fn handler_backward_escape_to_protected_body_is_rejected() {
    let (c, mut m) = fixture();
    let code = m.code.as_mut().unwrap();
    code.instructions[11] = 0xf528;
    code.instructions[12] = 0;
    code.instructions[13] = 0;
    assert!(native_java::render_method("sample.Example", &c, &m).is_err());
}
#[test]
fn overlapping_exception_ownership_is_rejected() {
    let (c, mut m) = fixture();
    let code = m.code.as_mut().unwrap();
    code.tries = 2;
    code.try_regions.push(DexTryRegion {
        start: 3,
        end: 9,
        catches: vec![(Some("Ljava/lang/RuntimeException;".into()), 10)].into(),
    });
    assert!(native_java::render_method("sample.Example", &c, &m).is_err());
}
#[test]
#[ignore = "requires javac and java on PATH"]
fn jvm_wide_join_and_declared_checked_identity() {
    for checked in [false, true] {
        let (c, mut m) = fixture();
        m.return_type = "J".into();
        m.parameters = vec!["J".into()];
        let code = m.code.as_mut().unwrap();
        code.registers = 3;
        code.ins = 2;
        code.instructions[17] = 0x0110;
        if checked {
            m.thrown_types = vec!["Ljava/io/IOException;".into()];
            m.code.as_mut().unwrap().try_regions[0].catches =
                vec![(Some("Ljava/io/IOException;".into()), 10)].into();
        }
        let source = native_java::render_method("sample.Example", &c, &m)
            .unwrap()
            .source;
        let ty = if checked {
            "java.io.IOException"
        } else {
            "RuntimeException"
        };
        let throws = if checked {
            "throws java.io.IOException"
        } else {
            ""
        };
        let dir = std::env::temp_dir().join(format!(
            "rdx-conditional-wide-{}-{checked}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(dir.clone());
        let java = format!(
            r#"package sample;class Api{{static int mode;static String trace;static {ty} original=new {ty}(),second=new {ty}();static void protectedCall() {throws} {{trace+="P";if(mode==1||mode==3)throw original;}}static void protectedEnd() {throws} {{trace+="E";if(mode==4)throw original;}}static void normal() {throws} {{trace+="N";if(mode==2)throw original;}}static void handler() {throws} {{trace+="H";if(mode==3)throw second;}}static void join() {throws} {{trace+="J";if(mode==5)throw original;}}}}class Example{{{source}}}public class Runner{{public static void main(String[]args)throws Exception{{String[]traces={{"PENJ","PHJ","PEN","PH","PEHJ","PENJ"}};long seed=0x100000002L;for(int mode=0;mode<6;mode++){{Api.mode=mode;Api.trace="";try{{long result=Example.run(seed);if(result!=seed||mode==2||mode==3||mode==5)throw new AssertionError("result");}}catch({ty} caught){{if(caught!=(mode==3?Api.second:Api.original)||(mode!=2&&mode!=3&&mode!=5))throw new AssertionError("identity");}}if(!Api.trace.equals(traces[mode]))throw new AssertionError(Api.trace);}}}}}}"#
        );
        let path = dir.join("Runner.java");
        std::fs::write(&path, java).unwrap();
        let o = std::process::Command::new("javac")
            .arg("-d")
            .arg(&dir)
            .arg(path)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "{}\n{source}",
            String::from_utf8_lossy(&o.stderr)
        );
        let o = std::process::Command::new("java")
            .arg("-cp")
            .arg(&dir)
            .arg("sample.Runner")
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    }
}
#[test]
fn terminal_normal_path_keeps_its_original_return() {
    let (c, mut m) = fixture();
    m.code.as_mut().unwrap().instructions[9] = 0x000e;
    let s = native_java::render_method("sample.Example", &c, &m)
        .unwrap()
        .source;
    assert!(!s.contains("= false;"), "{s}");
    assert!(s.contains(".normal();"), "{s}");
}
#[test]
fn nested_competing_exception_owner_remains_rejected() {
    let (c, mut m) = fixture();
    let code = m.code.as_mut().unwrap();
    code.tries = 2;
    code.try_regions.push(DexTryRegion {
        start: 3,
        end: 6,
        catches: vec![(Some("Ljava/lang/RuntimeException;".into()), 10)].into(),
    });
    assert!(native_java::render_method("sample.Example", &c, &m).is_err());
}
#[test]
#[ignore = "requires javac and java on PATH"]
fn jvm_terminal_normal_return_skips_shared_catch_continuation() {
    let (c, mut m) = fixture();
    m.code.as_mut().unwrap().instructions[9] = 0x000e;
    let source = native_java::render_method("sample.Example", &c, &m)
        .unwrap()
        .source;
    let dir = std::env::temp_dir().join(format!("rdx-conditional-terminal-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(dir.clone());
    let java = format!(
        r#"package sample;class Api{{static int mode;static String trace;static RuntimeException original=new RuntimeException();static void protectedCall(){{trace+="P";if(mode==1)throw original;}}static void protectedEnd(){{trace+="E";}}static void normal(){{trace+="N";if(mode==2)throw original;}}static void handler(){{trace+="H";}}static void join(){{trace+="J";}}}}class Example{{{source}}}public class Runner{{public static void main(String[]args){{String[]traces={{"PEN","PHJ","PEN"}};for(int mode=0;mode<3;mode++){{Api.mode=mode;Api.trace="";try{{Example.run();if(mode==2)throw new AssertionError("lost exception");}}catch(RuntimeException caught){{if(mode!=2||caught!=Api.original)throw new AssertionError("identity");}}if(!Api.trace.equals(traces[mode]))throw new AssertionError(Api.trace);}}}}}}"#
    );
    let path = dir.join("Runner.java");
    std::fs::write(&path, java).unwrap();
    let o = std::process::Command::new("javac")
        .arg("-d")
        .arg(&dir)
        .arg(path)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "{}\n{source}",
        String::from_utf8_lossy(&o.stderr)
    );
    let o = std::process::Command::new("java")
        .arg("-cp")
        .arg(&dir)
        .arg("sample.Runner")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}
fn prefix_fixture(wide: bool) -> (DexClass, DexMethod) {
    let (mut c, mut m) = fixture();
    let ty = if wide { "J" } else { "I" };
    let symbols = Arc::get_mut(&mut c.symbols).unwrap();
    symbols.protos.push((ty.into(), vec![]));
    symbols.protos.push(("V".into(), vec![ty.into()]));
    symbols.methods[0].1 = 1;
    symbols.methods[2].1 = 2;
    m.return_type = ty.into();
    let code = m.code.as_mut().unwrap();
    code.registers = 3;
    code.try_regions[0].end = 7;
    code.try_regions[0].catches = vec![(Some("Ljava/lang/RuntimeException;".into()), 11)].into();
    code.instructions = if wide {
        vec![
            0x0071, 0, 0, 0x000b, 0x0071, 1, 0, 0x2071, 2, 0x0010, 0x0728, 0x020d, 0x0016, 7,
            0x0071, 3, 0, 0x0071, 4, 0, 0x0010,
        ]
    } else {
        vec![
            0x0071, 0, 0, 0x000a, 0x0071, 1, 0, 0x1071, 2, 0, 0x0628, 0x020d, 0x7012, 0x0071, 3, 0,
            0x0071, 4, 0, 0x000f,
        ]
    };
    (c, m)
}
#[test]
fn protected_scalar_and_wide_prefix_locals_have_guarded_initialized_storage() {
    for wide in [false, true] {
        let (c, m) = prefix_fixture(wide);
        let s = native_java::render_method("sample.Example", &c, &m)
            .unwrap()
            .source;
        assert!(s.contains(if wide { "= 0L;" } else { "= 0;" }), "{s}");
        assert!(
            s.find("catch").unwrap() < s.find(".normal(").unwrap(),
            "{s}"
        );
        assert!(s.contains("= true;"), "{s}");
    }
}
#[test]
#[ignore = "requires javac and java on PATH"]
fn jvm_prefix_scalar_and_wide_storage_is_unobserved_on_failure() {
    for wide in [false, true] {
        let (c, m) = prefix_fixture(wide);
        let source = native_java::render_method("sample.Example", &c, &m)
            .unwrap()
            .source;
        let ty = if wide { "long" } else { "int" };
        let seed = if wide { "0x100000002L" } else { "123" };
        let dir =
            std::env::temp_dir().join(format!("rdx-prefix-storage-{}-{wide}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(dir.clone());
        let java = format!(
            r#"package sample;class Api{{static int mode;static String trace;static RuntimeException original=new RuntimeException(),second=new RuntimeException();static {ty} protectedCall(){{trace+="P";if(mode==1||mode==3)throw original;return {seed};}}static void protectedEnd(){{trace+="E";if(mode==4)throw original;}}static void normal({ty} value){{trace+="N";if(value!={seed})throw new AssertionError("default storage read");if(mode==2)throw original;}}static void handler(){{trace+="H";if(mode==3)throw second;}}static void join(){{trace+="J";if(mode==5)throw original;}}}}class Example{{{source}}}public class Runner{{public static void main(String[]args){{String[]traces={{"PENJ","PHJ","PEN","PH","PEHJ","PENJ"}};for(int mode=0;mode<6;mode++){{Api.mode=mode;Api.trace="";try{{{ty} result=Example.run();if(result!=(mode==1||mode==4?7:{seed})||mode==2||mode==3||mode==5)throw new AssertionError("result");}}catch(RuntimeException caught){{if(caught!=(mode==3?Api.second:Api.original)||(mode!=2&&mode!=3&&mode!=5))throw new AssertionError("identity");}}if(!Api.trace.equals(traces[mode]))throw new AssertionError(Api.trace);}}}}}}"#
        );
        let path = dir.join("Runner.java");
        std::fs::write(&path, java).unwrap();
        let o = std::process::Command::new("javac")
            .arg("-d")
            .arg(&dir)
            .arg(path)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "{}\n{source}",
            String::from_utf8_lossy(&o.stderr)
        );
        let o = std::process::Command::new("java")
            .arg("-cp")
            .arg(&dir)
            .arg("sample.Runner")
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    }
}
fn alias_prefix_fixture() -> (DexClass, DexMethod) {
    let (mut c, mut m) = prefix_fixture(false);
    let symbols = Arc::get_mut(&mut c.symbols).unwrap();
    symbols.protos[1].0 = "Ljava/lang/Object;".into();
    symbols.protos[2].1 = vec!["Ljava/lang/Object;".into(), "Ljava/lang/Object;".into()];
    m.return_type = "Ljava/lang/Object;".into();
    let code = m.code.as_mut().unwrap();
    code.try_regions[0].end = 8;
    code.try_regions[0].catches = vec![(Some("Ljava/lang/RuntimeException;".into()), 12)].into();
    code.instructions = vec![
        0x0071, 0, 0, 0x000c, 0x0107, 0x0071, 1, 0, 0x2071, 2, 0x0010, 0x0728, 0x020d, 0x0012,
        0x0112, 0x0071, 3, 0, 0x0071, 4, 0, 0x0011,
    ];
    (c, m)
}
fn branch_prefix_fixture() -> (DexClass, DexMethod) {
    let (mut c, mut m) = prefix_fixture(false);
    let symbols = Arc::get_mut(&mut c.symbols).unwrap();
    symbols.strings.push("protectedAlternative".into());
    symbols.methods.push((0, 1, 5));
    m.parameters = vec!["I".into()];
    let code = m.code.as_mut().unwrap();
    code.registers = 4;
    code.ins = 1;
    code.try_regions[0].end = 14;
    code.try_regions[0].catches = vec![(Some("Ljava/lang/RuntimeException;".into()), 18)].into();
    code.instructions = vec![
        0x0071, 0, 0, 0x000a, 0x0338, 7, 0x0071, 5, 0, 0x000a, 0x0128, 0x0071, 1, 0, 0x1071, 2, 0,
        0x0628, 0x020d, 0x7012, 0x0071, 3, 0, 0x0071, 4, 0, 0x000f,
    ];
    (c, m)
}
#[test]
fn aliases_and_protected_branch_merge_are_materialized_before_copy() {
    for (c, m) in [alias_prefix_fixture(), branch_prefix_fixture()] {
        let s = native_java::render_method("sample.Example", &c, &m)
            .unwrap()
            .source;
        assert!(
            s.find("catch").unwrap() < s.find(".normal(").unwrap(),
            "{s}"
        );
        assert!(s.contains("= true;"), "{s}");
    }
}
#[test]
fn wide_prefix_partial_overwrite_remains_rejected() {
    let (c, mut m) = prefix_fixture(true);
    let code = m.code.as_mut().unwrap();
    code.instructions[4] = 0x7112;
    code.instructions[5] = 0;
    code.instructions[6] = 0;
    assert!(native_java::render_method("sample.Example", &c, &m).is_err());
}
#[test]
#[ignore = "requires javac and java on PATH"]
fn jvm_prefix_alias_identity_and_branch_values() {
    for branch in [false, true] {
        let (c, m) = if branch {
            branch_prefix_fixture()
        } else {
            alias_prefix_fixture()
        };
        let source = native_java::render_method("sample.Example", &c, &m)
            .unwrap()
            .source;
        let (ty, seed, normal_params, normal_check, alternative, invoke, result_check) = if branch {
            (
                "int",
                "123",
                "int value",
                "if(value!=(flag==0?123:456))throw new AssertionError(\"branch value\");",
                "static int protectedAlternative(){trace+=\"A\";return 456;}",
                "int result=Example.run(flag);",
                "if(result!=(mode==1||mode==4?7:flag==0?123:456))throw new AssertionError(\"result\");",
            )
        } else {
            (
                "Object",
                "shared",
                "Object first,Object second",
                "if(first!=shared||second!=first)throw new AssertionError(\"alias identity\");",
                "",
                "Object result=Example.run();",
                "if(result!=(mode==1||mode==4?null:Api.shared))throw new AssertionError(\"result\");",
            )
        };
        let dir =
            std::env::temp_dir().join(format!("rdx-prefix-alias-{}-{branch}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(dir.clone());
        let java = format!(
            r#"package sample;class Api{{static int mode,flag;static String trace;static Object shared=new Object();static RuntimeException original=new RuntimeException(),second=new RuntimeException();static {ty} protectedCall(){{trace+="P";if(mode==1||mode==3)throw original;return {seed};}}{alternative}static void protectedEnd(){{trace+="E";if(mode==4)throw original;}}static void normal({normal_params}){{trace+="N";{normal_check}if(mode==2)throw original;}}static void handler(){{trace+="H";if(mode==3)throw second;}}static void join(){{trace+="J";if(mode==5)throw original;}}}}class Example{{{source}}}public class Runner{{public static void main(String[]args){{for(int flag=0;flag<2;flag++)for(int mode=0;mode<6;mode++){{Api.mode=mode;Api.flag=flag;Api.trace="";try{{{invoke}{result_check}if(mode==2||mode==3||mode==5)throw new AssertionError("lost failure");}}catch(RuntimeException caught){{if(caught!=(mode==3?Api.second:Api.original)||(mode!=2&&mode!=3&&mode!=5))throw new AssertionError("identity");}}String expected=new String[]{{"PENJ","PHJ","PEN","PH","PEHJ","PENJ"}}[mode];if({branch}&&flag!=0)expected=expected.replace("PE","PAE");if(!Api.trace.equals(expected))throw new AssertionError(Api.trace+" "+expected);}}}}}}"#
        );
        let path = dir.join("Runner.java");
        std::fs::write(&path, java).unwrap();
        let o = std::process::Command::new("javac")
            .arg("-d")
            .arg(&dir)
            .arg(path)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "{}\n{source}",
            String::from_utf8_lossy(&o.stderr)
        );
        let o = std::process::Command::new("java")
            .arg("-cp")
            .arg(&dir)
            .arg("sample.Runner")
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    }
}

#[test]
fn changed_protected_string_literal_requires_original_materialization() {
    let (mut c, mut m) = fixture();
    let symbols = Arc::get_mut(&mut c.symbols).unwrap();
    symbols.strings.push("protected literal".into());
    symbols
        .protos
        .push(("V".into(), vec!["Ljava/lang/String;".into()]));
    symbols.methods[2].1 = 1;
    let code = m.code.as_mut().unwrap();
    code.instructions[0..3].copy_from_slice(&[0x001a, 5, 0x0000]);
    code.instructions[6] = 0x1071;
    let result = native_java::render_method("sample.Example", &c, &m);
    assert!(
        result.is_err(),
        "a changed protected string must retain its original resolution region"
    );
}
