pub use rdx::{native_cfg, native_dex, native_ir};
use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_java,
};
use std::{process::Command, sync::Arc};
#[path = "../src/native_java/constructor_throw_tails.rs"]
mod proof;

fn fixture() -> DexClass {
    DexClass {
        descriptor: "Lsample/CtorTail;".into(),
        superclass: Some("Lsample/Base;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols {
            types: vec![
                "Lsample/CtorTail;".into(),
                "Lsample/Base;".into(),
                "Lsample/Hooks;".into(),
            ],
            strings: vec![
                "<init>".into(),
                "error".into(),
                "boom".into(),
                "consume".into(),
            ],
            protos: vec![
                ("V".into(), vec![]),
                ("V".into(), vec!["Ljava/lang/String;".into()]),
                ("V".into(), vec!["Ljava/lang/Object;".into()]),
            ],
            methods: vec![(1, 0, 0), (2, 1, 1), (2, 2, 3)],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: "Lsample/CtorTail;".into(),
            name: "<init>".into(),
            return_type: "V".into(),
            parameters: vec!["I".into()],
            thrown_types: vec![],
            access_flags: 1,
            code: Some(DexCode {
                registers: 4,
                ins: 2,
                outs: 1,
                tries: 0,
                try_regions: vec![],
                offset: 0,
                instructions: vec![
                    0x0338, 6, 0x0339, 8, 0x1012, 0, 0x1070, 0, 2, 0x000e, 0x021a, 2, 0x1071, 1, 2,
                    0x0012, 0x0027,
                ],
            }),
        }],
    }
}
#[test]
fn detached_throw_tail_keeps_constructor_join_and_receiver_identity() {
    let c = fixture();
    let code = c.methods[0].code.as_ref().unwrap();
    let plans = proof::select(code);
    let p = plans.iter().find(|p| p.start == 10).unwrap();
    assert_eq!(p.end, 17);
    assert!(p.owned.contains(&10) && p.owned.contains(&16) && !p.owned.contains(&6));
    let out = native_java::render_method("sample.CtorTail", &c, &c.methods[0]).unwrap();
    assert_eq!(out.source.matches("super(").count(), 1, "{}", out.source);
    assert_eq!(
        out.source.matches("sample.Hooks.error(").count(),
        1,
        "{}",
        out.source
    );
    assert!(!out.source.contains("masked"), "{}", out.source);
    assert!(out.links.iter().any(|l| l.label == "sample.Base.<init>()V"));
}
fn unrelated_post_init_handler_fixture() -> DexClass {
    let mut c = fixture();
    let code = c.methods[0].code.as_mut().unwrap();
    code.instructions = vec![
        0x0338, 6, 0x0339, 11, 0x1012, 0, 0x1070, 0, 2, 0x1071, 2, 2, 0x000e, 0x021a, 2, 0x1071, 1,
        2, 0x0012, 0x0027, 0x000d, 0x0027,
    ];
    code.tries = 1;
    code.try_regions.push(DexTryRegion {
        start: 9,
        end: 12,
        catches: Arc::from([(None, 20)]),
    });
    c
}

#[test]
fn unrelated_post_initialization_handler_preserves_detached_throw_proof() {
    let c = unrelated_post_init_handler_fixture();
    let code = c.methods[0].code.as_ref().unwrap();
    assert!(proof::select(code).iter().any(|plan| plan.start == 13));
    let out = native_java::render_method("sample.CtorTail", &c, &c.methods[0]).unwrap();
    assert_eq!(out.source.matches("super(").count(), 1, "{}", out.source);
    assert!(out.source.contains("Hooks.consume("), "{}", out.source);
    assert!(out.source.contains("Hooks.error("), "{}", out.source);
}

#[test]
fn returns_cycles_protection_and_uninitialized_receiver_reads_reject() {
    for variant in 0..4 {
        let mut c = fixture();
        let code = c.methods[0].code.as_mut().unwrap();
        match variant {
            0 => code.instructions[16] = 0x000e,
            1 => code.instructions[16] = 0xfa28,
            2 => {
                code.tries = 1;
                code.try_regions.push(DexTryRegion {
                    start: 10,
                    end: 15,
                    catches: Arc::from([(None, 15)]),
                });
            }
            3 => {
                code.instructions.truncate(10);
                code.instructions.extend([0x1071, 2, 2, 0x0012, 0x0027]);
            }
            _ => unreachable!(),
        }
        let plans = proof::select(code);
        if variant < 3 {
            assert!(!plans.iter().any(|p| p.start == 10), "variant{variant}");
        }
        assert!(
            native_java::render_method("sample.CtorTail", &c, &c.methods[0]).is_err(),
            "variant{variant}"
        );
    }
}
#[test]
#[ignore = "root runs Java25 serially with RDX_CONSTRUCTOR_TAIL_JVM_DIR"]
fn jvm_detached_throw_preserves_error_and_super_effect_order() {
    let c = fixture();
    let method = native_java::render_method("sample.CtorTail", &c, &c.methods[0])
        .unwrap()
        .source;
    let base = std::path::PathBuf::from(
        std::env::var_os("RDX_CONSTRUCTOR_TAIL_JVM_DIR").expect("RDX_CONSTRUCTOR_TAIL_JVM_DIR"),
    );
    std::fs::create_dir_all(base.join("sample")).unwrap();
    let source = format!(
        r#"package sample;
class Hooks {{ static String trace="";static boolean fail;static final RuntimeException marker=new RuntimeException();
static void error(String s) {{trace+="error:"+s+";";if(fail)throw marker;}}static void consume(Object o) {{throw new AssertionError();}} }}
class Base {{Base() {{Hooks.trace+="base;";}}}}
public class CtorTail extends Base {{ {method}
public static void main(String[] a) {{int cases=0;for(int mask:new int[]{{0,1,-1,Integer.MAX_VALUE}})for(boolean fail:new boolean[]{{false,true}}) {{
Hooks.trace="";Hooks.fail=fail;Throwable caught=null;try {{new CtorTail(mask);}}catch(Throwable t){{caught=t;}}
if(mask==0){{if(caught!=null||!Hooks.trace.equals("base;"))throw new AssertionError();}}
else {{if(!Hooks.trace.equals("error:boom;")||(fail?caught!=Hooks.marker:!(caught instanceof NullPointerException)))throw new AssertionError(Hooks.trace);}}
cases++;}}System.out.print(cases);}}}}"#
    );
    std::fs::write(base.join("sample/CtorTail.java"), source).unwrap();
    for (program, args) in [
        ("javac", vec!["sample/CtorTail.java"]),
        ("java", vec!["-cp", ".", "sample.CtorTail"]),
    ] {
        let result = Command::new(program)
            .args(args)
            .current_dir(&base)
            .output()
            .unwrap();
        std::fs::write(base.join(format!("{program}.log")), &result.stderr).unwrap();
        assert!(
            result.status.success(),
            "{program}:{}",
            String::from_utf8_lossy(&result.stderr)
        );
        if program == "java" {
            assert_eq!(result.stdout, b"8");
        }
    }
}
#[test]
#[ignore = "root runs Java25 serially with RDX_CONSTRUCTOR_TAIL_JVM_DIR"]
fn jvm_unrelated_handler_keeps_pre_super_error_and_post_super_throw_identity() {
    let c = unrelated_post_init_handler_fixture();
    let method = native_java::render_method("sample.CtorTail", &c, &c.methods[0])
        .unwrap()
        .source;
    let base = std::path::PathBuf::from(
        std::env::var_os("RDX_CONSTRUCTOR_TAIL_JVM_DIR").expect("RDX_CONSTRUCTOR_TAIL_JVM_DIR"),
    )
    .join("unrelated-handler");
    std::fs::create_dir_all(base.join("sample")).unwrap();
    let source = format!(
        r#"package sample;
class Hooks {{ static String trace="";static boolean fail;static final RuntimeException marker=new RuntimeException();
static void error(String s) {{trace+="error:"+s+";";if(fail)throw marker;}}
static void consume(Object o) {{if(!(o instanceof CtorTail))throw new AssertionError();trace+="consume;";if(fail)throw marker;}} }}
class Base {{Base() {{Hooks.trace+="base;";}}}}
public class CtorTail extends Base {{ {method}
public static void main(String[] a) {{int cases=0;for(int mask:new int[]{{0,1,-1,Integer.MAX_VALUE}})for(boolean fail:new boolean[]{{false,true}}) {{
Hooks.trace="";Hooks.fail=fail;Throwable caught=null;try {{new CtorTail(mask);}}catch(Throwable t){{caught=t;}}
if(mask==0){{if(!Hooks.trace.equals("base;consume;")||(fail?caught!=Hooks.marker:caught!=null))throw new AssertionError(Hooks.trace);}}
else {{if(!Hooks.trace.equals("error:boom;")||(fail?caught!=Hooks.marker:!(caught instanceof NullPointerException)))throw new AssertionError(Hooks.trace);}}
cases++;}}System.out.print(cases);}}}}"#
    );
    std::fs::write(base.join("sample/CtorTail.java"), source).unwrap();
    for (program, args) in [
        ("javac", vec!["sample/CtorTail.java"]),
        ("java", vec!["-cp", ".", "sample.CtorTail"]),
    ] {
        let result = Command::new(program)
            .args(args)
            .current_dir(&base)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{program}:{}",
            String::from_utf8_lossy(&result.stderr)
        );
        if program == "java" {
            assert_eq!(result.stdout, b"8");
        }
    }
}

#[test]
#[ignore = "root runs pinned Airbnb APK serially"]
fn airbnb_constructor_terminal_receiver_reuse_renders() {
    use rdx::{engine::DecompilerEngine, native_engine::NativeDexEngine};
    let path = std::env::var_os("RDX_TEST_APK").expect("RDX_TEST_APK");
    let mut engine = NativeDexEngine::default();
    engine.open(std::path::Path::new(&path)).unwrap();
    for name in [
        "com.airbnb.android.feat.aircover.claims.screens.datepicker.b",
        "com.airbnb.android.feat.aircover.claims.screens.selectoption.c",
        "com.airbnb.android.feat.cancellation.stricttofirm.a",
        "com.airbnb.android.feat.groceryintegrations.webview.uber.UberWebViewViewModel",
        "com.airbnb.android.lib.trust.cdui.d",
    ] {
        let c = engine.class(name).unwrap();
        let m = c
            .methods
            .iter()
            .find(|m| m.name.as_ref() == "<init>")
            .unwrap();
        let out = native_java::render_method(name, c, m)
            .unwrap_or_else(|error| panic!("{name}: {error:#}"));
        assert!(out.source.contains("super("), "{}", out.source);
        assert!(!out.source.contains("masked"), "{}", out.source);
    }
}
