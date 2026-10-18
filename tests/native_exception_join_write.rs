use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_java,
};
use std::sync::Arc;
fn fixture(ty: &str) -> (DexClass, DexMethod) {
    let names = ["protectedCall", "protectedEnd", "normal", "handler", "join"];
    let symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/Api;".into()],
        strings: names.into_iter().map(Into::into).collect(),
        protos: vec![("V".into(), vec![]), (ty.into(), vec![])],
        methods: (0..5)
            .map(|i| (0, if i == 2 || i == 3 { 1 } else { 0 }, i))
            .collect(),
        ..Default::default()
    });
    let result = if ty == "J" || ty == "D" {
        0x000b
    } else if ty.starts_with('L') {
        0x000c
    } else {
        0x000a
    };
    let ret = if ty == "J" || ty == "D" {
        0x0010
    } else if ty.starts_with('L') {
        0x0011
    } else {
        0x000f
    };
    let m = DexMethod {
        declaring_type: "Lsample/Example;".into(),
        name: "run".into(),
        return_type: ty.into(),
        parameters: vec![],
        thrown_types: vec![],
        access_flags: 9,
        code: Some(DexCode {
            registers: 3,
            ins: 0,
            outs: 0,
            tries: 1,
            try_regions: vec![DexTryRegion {
                start: 0,
                end: 6,
                catches: vec![(Some("Ljava/lang/RuntimeException;".into()), 11)].into(),
            }],
            instructions: vec![
                0x0071, 0, 0, 0x0071, 1, 0, 0x0071, 2, 0, result, 0x0628, 0x020d, 0x0071, 3, 0,
                result, 0x0071, 4, 0, ret,
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
fn normal_and_catch_values_are_stored_at_original_reaching_arms() {
    for ty in ["I", "J", "D", "Ljava/lang/Object;"] {
        let (c, m) = fixture(ty);
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
fn missing_catch_join_value_rejects() {
    let (c, mut m) = fixture("I");
    m.code.as_mut().unwrap().instructions[15] = 0;
    assert!(native_java::render_method("sample.Example", &c, &m).is_err());
}
#[test]
fn partial_wide_catch_value_rejects() {
    let (c, mut m) = fixture("J");
    m.code.as_mut().unwrap().instructions[15] = 0x1012;
    assert!(native_java::render_method("sample.Example", &c, &m).is_err());
}
#[test]
#[ignore = "requires javac and java on PATH"]
fn jvm_reaching_values_and_failure_ownership() {
    for (ty, java_ty, normal, handled) in [
        ("I", "int", "123", "456"),
        ("J", "long", "0x100000002L", "0x200000003L"),
        (
            "F",
            "float",
            "Float.intBitsToFloat(0x7fc00123)",
            "Float.intBitsToFloat(0x7fc00456)",
        ),
        (
            "D",
            "double",
            "Double.longBitsToDouble(0x7ff8000000000123L)",
            "Double.longBitsToDouble(0x7ff8000000000456L)",
        ),
        ("Ljava/lang/Object;", "Object", "N", "H"),
    ] {
        let (c, m) = fixture(ty);
        let source = native_java::render_method("sample.Example", &c, &m)
            .unwrap()
            .source;
        let equal = if ty == "F" {
            "Float.floatToRawIntBits(actual)==Float.floatToRawIntBits(expected)"
        } else if ty == "D" {
            "Double.doubleToRawLongBits(actual)==Double.doubleToRawLongBits(expected)"
        } else {
            "actual==expected"
        };
        let java=format!(r#"package sample;class Api{{static int mode;static String trace;static final Object N=new Object(),H=new Object();static RuntimeException original=new RuntimeException(),second=new RuntimeException();static void protectedCall(){{trace+="P";if(mode==1||mode==3)throw original;}}static void protectedEnd(){{trace+="E";if(mode==4)throw original;}}static {java_ty} normal(){{trace+="N";if(mode==2)throw original;return {normal};}}static {java_ty} handler(){{trace+="H";if(mode==3)throw second;return {handled};}}static void join(){{trace+="J";if(mode==5)throw original;}}}}class Example{{{source}}}public class Runner{{public static void main(String[]args){{String[]traces={{"PENJ","PHJ","PEN","PH","PEHJ","PENJ"}};for(int mode=0;mode<6;mode++){{Api.mode=mode;Api.trace="";try{{{java_ty} actual=Example.run(),expected=mode==1||mode==4?Api.handlerValue():Api.normalValue();if(!({equal})||mode==2||mode==3||mode==5)throw new AssertionError("reaching value");}}catch(RuntimeException caught){{if(caught!=(mode==3?Api.second:Api.original)||(mode!=2&&mode!=3&&mode!=5))throw new AssertionError("identity");}}if(!Api.trace.equals(traces[mode]))throw new AssertionError(Api.trace);}}}}}}"#).replace("static void join()",&format!("static {java_ty} normalValue(){{return {normal};}}static {java_ty} handlerValue(){{return {handled};}}static void join()"));
        let dir = std::env::temp_dir().join(format!(
            "rdx-join-store-{}-{}",
            std::process::id(),
            ty.replace('/', "_").replace(';', "")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(dir.clone());
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

fn branch_fixture() -> (DexClass, DexMethod) {
    let (mut c, mut m) = fixture("I");
    let symbols = Arc::get_mut(&mut c.symbols).unwrap();
    symbols.strings.push("normalAlternative".into());
    symbols.methods.push((0, 1, 5));
    m.parameters = vec!["I".into()];
    let code = m.code.as_mut().unwrap();
    code.ins = 1;
    code.try_regions[0].catches = vec![(Some("Ljava/lang/RuntimeException;".into()), 18)].into();
    code.instructions = vec![
        0x0071, 0, 0, 0x0071, 1, 0, 0x0238, 7, 0x0071, 2, 0, 0x000a, 0x0528, 0x0071, 5, 0, 0x000a,
        0x0628, 0x020d, 0x0071, 3, 0, 0x000a, 0x0071, 4, 0, 0x000f,
    ];
    (c, m)
}
#[test]
fn normal_tail_branch_merge_has_complete_reaching_values() {
    let (c, m) = branch_fixture();
    let source = native_java::render_method("sample.Example", &c, &m)
        .unwrap()
        .source;
    assert!(source.contains(".normalAlternative("), "{source}");
    assert!(
        source.find("catch").unwrap() < source.find(".normal(").unwrap(),
        "{source}"
    );
}

#[test]
fn incompatible_reaching_types_and_unmaterialized_strings_reject() {
    let (mut c, m) = fixture("Ljava/lang/Object;");
    let symbols = Arc::get_mut(&mut c.symbols).unwrap();
    symbols.protos.push(("I".into(), vec![]));
    symbols.methods[3].1 = 2;
    assert!(native_java::render_method("sample.Example", &c, &m).is_err());
    let (mut c, mut m) = fixture("Ljava/lang/Object;");
    Arc::get_mut(&mut c.symbols)
        .unwrap()
        .strings
        .push("late string".into());
    m.code.as_mut().unwrap().instructions[6..10].copy_from_slice(&[0x001a, 5, 0, 0]);
    assert!(native_java::render_method("sample.Example", &c, &m).is_err());
}

#[test]
fn missing_value_on_one_normal_branch_rejects() {
    let (c, mut m) = branch_fixture();
    m.code.as_mut().unwrap().instructions[16] = 0;
    assert!(native_java::render_method("sample.Example", &c, &m).is_err());
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn jvm_each_normal_branch_stores_its_original_value() {
    let (c, m) = branch_fixture();
    let source = native_java::render_method("sample.Example", &c, &m)
        .unwrap()
        .source;
    let java = format!(
        r#"package sample;class Api{{static int mode;static String trace;static RuntimeException original=new RuntimeException(),second=new RuntimeException();static void protectedCall(){{trace+="P";if(mode==1||mode==3)throw original;}}static void protectedEnd(){{trace+="E";if(mode==4)throw original;}}static int normal(){{trace+="A";if(mode==2)throw original;return 123;}}static int normalAlternative(){{trace+="B";if(mode==2)throw original;return 456;}}static int handler(){{trace+="H";if(mode==3)throw second;return 789;}}static void join(){{trace+="J";if(mode==5)throw original;}}}}class Example{{{source}}}public class Runner{{public static void main(String[]args){{for(int flag=0;flag<2;flag++)for(int mode=0;mode<6;mode++){{Api.mode=mode;Api.trace="";String n=flag==0?"B":"A";String[]trace={{"PE"+n+"J","PHJ","PE"+n,"PH","PEHJ","PE"+n+"J"}};try{{int value=Example.run(flag);if(value!=(mode==1||mode==4?789:flag==0?456:123)||mode==2||mode==3||mode==5)throw new AssertionError("value");}}catch(RuntimeException caught){{if(caught!=(mode==3?Api.second:Api.original)||(mode!=2&&mode!=3&&mode!=5))throw new AssertionError("identity");}}if(!Api.trace.equals(trace[mode]))throw new AssertionError(Api.trace);}}}}}}"#
    );
    let dir = std::env::temp_dir().join(format!("rdx-join-store-branches-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(dir.clone());
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
#[ignore = "requires javac and java on PATH"]
fn jvm_ordered_two_catches_preserve_terminal_and_reaching_paths() {
    let (mut c, mut m) = fixture("I");
    let symbols = Arc::get_mut(&mut c.symbols).unwrap();
    symbols
        .protos
        .push(("I".into(), vec!["Ljava/lang/Throwable;".into()]));
    symbols.methods[3].1 = 2;
    symbols.strings.push("terminal".into());
    symbols.methods.push((0, 2, 5));
    let code = m.code.as_mut().unwrap();
    code.try_regions[0].catches =
        vec![(Some("Ljava/io/IOException;".into()), 20), (None, 11)].into();
    code.instructions[12] = 0x1071;
    code.instructions[14] = 2;
    code.instructions
        .extend([0x020d, 0x1071, 5, 2, 0x000a, 0x000f]);
    let source = native_java::render_method("sample.Example", &c, &m)
        .unwrap()
        .source;
    assert!(
        source.contains("instanceof java.io.IOException"),
        "{source}"
    );
    let java = format!(
        r#"package sample;class Api{{static int mode;static String trace;static java.io.IOException io=new java.io.IOException();static RuntimeException original=new RuntimeException(),second=new RuntimeException();static void protectedCall()throws java.io.IOException{{trace+="P";if(mode==1||mode==3)throw io;if(mode==6)throw original;}}static void protectedEnd(){{trace+="E";if(mode==4)throw original;}}static int normal(){{trace+="N";if(mode==2)throw original;return 123;}}static int handler(Throwable e){{trace+="H";if(e!=original)throw new AssertionError("catch identity");if(mode==6)throw second;return 456;}}static int terminal(Throwable e){{trace+="T";if(e!=io)throw new AssertionError("catch identity");if(mode==3)throw second;return 789;}}static void join(){{trace+="J";if(mode==5)throw original;}}}}class Example{{{source}}}public class Runner{{public static void main(String[]args){{String[]traces={{"PENJ","PT","PEN","PT","PEHJ","PENJ","PH"}};for(int mode=0;mode<7;mode++){{Api.mode=mode;Api.trace="";try{{int actual=Example.run(),expected=mode==1?789:mode==4?456:123;if(actual!=expected||mode==2||mode==3||mode==5||mode==6)throw new AssertionError("default storage observed");}}catch(RuntimeException caught){{if(caught!=(mode==3||mode==6?Api.second:Api.original)||(mode!=2&&mode!=3&&mode!=5&&mode!=6))throw new AssertionError("throw identity");}}if(!Api.trace.equals(traces[mode]))throw new AssertionError(Api.trace);}}}}}}"#
    );
    let dir = std::env::temp_dir().join(format!("rdx-join-store-catches-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(dir.clone());
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

fn literal_fixture(ty: &str, value: i32, downstream: bool) -> (DexClass, DexMethod) {
    let (mut c, mut m) = fixture(ty);
    m.code.as_mut().unwrap().instructions[15] = 0x0012 | ((value as u16) << 12);
    if downstream {
        let symbols = Arc::get_mut(&mut c.symbols).unwrap();
        symbols.strings.push("use".into());
        symbols.protos.push((ty.into(), vec![ty.into()]));
        symbols.methods.push((0, 2, 5));
        let code = m.code.as_mut().unwrap();
        code.instructions.pop();
        code.instructions.extend([0x1071, 5, 0, 0x000c, 0x0011]);
    }
    (c, m)
}

#[test]
fn literal_null_and_boolean_joins_keep_typed_storage() {
    for (ty, n) in [("Ljava/lang/String;", 0), ("Z", 0), ("Z", 1)] {
        let (c, m) = literal_fixture(ty, n, false);
        let source = native_java::render_method("sample.Example", &c, &m)
            .unwrap()
            .source;
        assert!(
            source.contains(if ty == "Z" {
                "boolean"
            } else {
                "java.lang.String"
            }),
            "{source}"
        );
    }
}
#[test]
fn nonnull_reference_and_nonboolean_literals_remain_rejected() {
    for (ty, n) in [("Ljava/lang/String;", 1), ("Z", 2), ("Z", -1)] {
        let (c, m) = literal_fixture(ty, n, false);
        assert!(native_java::render_method("sample.Example", &c, &m).is_err());
    }
}
#[test]
#[ignore = "requires javac and java on PATH"]
fn jvm_literal_values_typed_overloads_and_varargs_keep_ownership() {
    for (ty, n, downstream) in [
        ("Ljava/lang/String;", 0, true),
        ("Z", 0, false),
        ("Z", 1, false),
    ] {
        let (c, m) = literal_fixture(ty, n, downstream);
        let source = native_java::render_method("sample.Example", &c, &m)
            .unwrap()
            .source;
        let java_ty = if ty == "Z" { "boolean" } else { "String" };
        let normal = if ty == "Z" { "true" } else { "\"normal\"" };
        let expected = if downstream {
            "mode==1||mode==4?\"string:null\":\"string:normal\""
        } else if n == 0 {
            "mode!=1&&mode!=4"
        } else {
            "true"
        };
        let extra = if downstream {
            "static String use(String x){trace+=\"U\";return \"string:\"+x;}static String use(Object x){throw new AssertionError(\"Object overload\");}static String use(String... x){throw new AssertionError(\"varargs overload\");}"
        } else {
            ""
        };
        let normal_trace = if downstream { "PENJU" } else { "PENJ" };
        let caught_trace = if downstream { "PHJU" } else { "PHJ" };
        let end_trace = if downstream { "PEHJU" } else { "PEHJ" };
        let java = format!(
            r#"package sample;class Api{{static int mode;static String trace;static RuntimeException original=new RuntimeException(),second=new RuntimeException();static void protectedCall(){{trace+="P";if(mode==1||mode==3)throw original;}}static void protectedEnd(){{trace+="E";if(mode==4)throw original;}}static {java_ty} normal(){{trace+="N";if(mode==2)throw original;return {normal};}}static {java_ty} handler(){{trace+="H";if(mode==3)throw second;return {normal};}}static void join(){{trace+="J";if(mode==5)throw original;}}{extra}}}class Example{{{source}}}public class Runner{{public static void main(String[]args){{String[]traces={{"{normal_trace}","{caught_trace}","PEN","PH","{end_trace}","PENJ"}};for(int mode=0;mode<6;mode++){{Api.mode=mode;Api.trace="";try{{{java_ty} actual=Example.run(),expected={expected};if(!java.util.Objects.equals(actual,expected)||mode==2||mode==3||mode==5)throw new AssertionError("value");}}catch(RuntimeException caught){{if(caught!=(mode==3?Api.second:Api.original)||(mode!=2&&mode!=3&&mode!=5))throw new AssertionError("identity");}}if(!Api.trace.equals(traces[mode]))throw new AssertionError(Api.trace);}}}}}}"#
        );
        let dir = std::env::temp_dir().join(format!(
            "rdx-literal-join-{}-{n}-{downstream}",
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
fn earlier_nonzero_literal_cannot_join_later_reference_arm() {
    let (c, mut m) = fixture("Ljava/lang/String;");
    let code = m.code.as_mut().unwrap();
    code.instructions[9] = 0x0012;
    code.instructions[15] = 0x1012;
    code.try_regions[0].catches =
        vec![(Some("Ljava/io/IOException;".into()), 11), (None, 20)].into();
    code.instructions
        .extend([0x020d, 0x0071, 3, 0, 0x000c, 0xf728]);
    assert!(native_java::render_method("sample.Example", &c, &m).is_err());
}

fn zero_cleanup_fixture(handler: Option<i32>) -> (DexClass, DexMethod) {
    let (c, mut m) = fixture("I");
    let code = m.code.as_mut().unwrap();
    code.instructions[9] = 0x0012;
    code.instructions[15] = handler.map_or(0, |n| 0x0012 | ((n as u16) << 12));
    code.instructions.pop();
    code.instructions
        .extend([0x0038, 3, 0x0027, 0x7012, 0x000f]);
    (c, m)
}
#[test]
fn identical_reaching_zero_retains_null_throw_proof() {
    let (c, m) = zero_cleanup_fixture(Some(0));
    let source = native_java::render_method("sample.Example", &c, &m)
        .unwrap()
        .source;
    assert!(source.contains("throw null;"), "{source}");
    assert!(source.contains("if (0 != 0)"), "{source}");
}
#[test]
fn mixed_or_undefined_cleanup_value_cannot_become_null_throw() {
    for value in [Some(1), None] {
        let (c, m) = zero_cleanup_fixture(value);
        assert!(native_java::render_method("sample.Example", &c, &m).is_err());
    }
}
#[test]
#[ignore = "requires javac and java on PATH"]
fn jvm_zero_cleanup_preserves_all_reaching_paths_and_fault_identity() {
    let (c, m) = zero_cleanup_fixture(Some(0));
    let source = native_java::render_method("sample.Example", &c, &m)
        .unwrap()
        .source;
    let java = format!(
        r#"package sample;class Api{{static int mode;static String trace;static RuntimeException original=new RuntimeException(),second=new RuntimeException();static void protectedCall(){{trace+="P";if(mode==1||mode==3)throw original;}}static void protectedEnd(){{trace+="E";if(mode==4)throw original;}}static int normal(){{trace+="N";if(mode==2)throw original;return 123;}}static int handler(){{trace+="H";if(mode==3)throw second;return 456;}}static void join(){{trace+="J";if(mode==5)throw original;}}}}class Example{{{source}}}public class Runner{{public static void main(String[]args){{String[]traces={{"PENJ","PHJ","PEN","PH","PEHJ","PENJ"}};for(int mode=0;mode<6;mode++){{Api.mode=mode;Api.trace="";try{{if(Example.run()!=7||mode==2||mode==3||mode==5)throw new AssertionError("cleanup value");}}catch(RuntimeException caught){{if(caught!=(mode==3?Api.second:Api.original)||(mode!=2&&mode!=3&&mode!=5))throw new AssertionError("identity");}}if(!Api.trace.equals(traces[mode]))throw new AssertionError(Api.trace);}}}}}}"#
    );
    let dir = std::env::temp_dir().join(format!("rdx-zero-cleanup-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(dir.clone());
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

fn identical_wide_literal_fixture(bits: u64) -> (DexClass, DexMethod) {
    let (c, mut m) = fixture("D");
    let code = m.code.as_mut().unwrap();
    code.try_regions[0].catches = vec![(Some("Ljava/lang/RuntimeException;".into()), 15)].into();
    let literal = [
        0x0018,
        bits as u16,
        (bits >> 16) as u16,
        (bits >> 32) as u16,
        (bits >> 48) as u16,
    ];
    let mut words = vec![0x0071, 0, 0, 0x0071, 1, 0, 0x0071, 2, 0];
    words.extend(literal);
    words.push(0x0a28);
    words.extend([0x020d, 0x0071, 3, 0]);
    words.extend(literal);
    words.extend([0x0071, 4, 0, 0x0010]);
    code.instructions = words;
    (c, m)
}
#[test]
fn identical_wide_literal_keeps_raw_signed_zero_and_nan_bits() {
    for bits in [0x8000000000000000, 0x7ff8000000000123] {
        let (c, m) = identical_wide_literal_fixture(bits);
        let source = native_java::render_method("sample.Example", &c, &m)
            .unwrap()
            .source;
        assert!(source.contains("return "), "{source}");
        assert!(
            !source.contains("long v"),
            "equal literal must retain literal metadata: {source}"
        );
    }
}
#[test]
#[ignore = "requires javac and java on PATH"]
fn jvm_identical_wide_literal_preserves_raw_bits_on_normal_and_catch_paths() {
    for bits in [0x8000000000000000u64, 0x7ff8000000000123] {
        let (c, m) = identical_wide_literal_fixture(bits);
        let source = native_java::render_method("sample.Example", &c, &m)
            .unwrap()
            .source;
        let java = format!(
            r#"package sample;class Api{{static int mode;static String trace;static RuntimeException original=new RuntimeException(),second=new RuntimeException();static void protectedCall(){{trace+="P";if(mode==1||mode==3)throw original;}}static void protectedEnd(){{trace+="E";if(mode==4)throw original;}}static double normal(){{trace+="N";if(mode==2)throw original;return 1;}}static double handler(){{trace+="H";if(mode==3)throw second;return 2;}}static void join(){{trace+="J";if(mode==5)throw original;}}}}class Example{{{source}}}public class Runner{{public static void main(String[]args){{String[]trace={{"PENJ","PHJ","PEN","PH","PEHJ","PENJ"}};for(int mode=0;mode<6;mode++){{Api.mode=mode;Api.trace="";try{{if(Double.doubleToRawLongBits(Example.run())!=0x{bits:016x}L||mode==2||mode==3||mode==5)throw new AssertionError("wide literal bits");}}catch(RuntimeException caught){{if(caught!=(mode==3?Api.second:Api.original)||(mode!=2&&mode!=3&&mode!=5))throw new AssertionError("identity");}}if(!Api.trace.equals(trace[mode]))throw new AssertionError(Api.trace);}}}}}}"#
        );
        let dir =
            std::env::temp_dir().join(format!("rdx-identical-wide-{}-{bits}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(dir.clone());
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
