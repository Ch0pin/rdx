//! G06: abrupt error guards retain effects inside nested reader-style loops.
use rdx::{
    native_dex::{self, DexSymbols},
    native_java,
};
use std::{path::Path, sync::Arc};

fn fixture() -> native_dex::DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class.methods.retain(|m| m.name.as_ref() == "answer");
    class.symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/Effects;".into()],
        strings: vec!["touch".into()],
        protos: vec![("V".into(), vec![])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    let method = &mut class.methods[0];
    method.name = "run".into();
    method.parameters = vec![
        "I".into(),
        "I".into(),
        "Ljava/lang/RuntimeException;".into(),
    ];
    method.access_flags = 9;
    let code = method.code.as_mut().unwrap();
    code.registers = 5;
    code.ins = 3;
    code.outs = 0;
    code.instructions = vec![
        0x0012, 0x2035, 13, 0x0112, 0x0338, 11, 0x3135, 5, 0x01d8, 0x0101, 0xfa28, 0x00d8, 0x0100,
        0xf428, 0x000f, 0x0071, 0, 0, 0x0427,
    ];
    class
}

fn guarded_continue_fixture() -> native_dex::DexClass {
    let mut class = fixture();
    let symbols = Arc::get_mut(&mut class.symbols).unwrap();
    symbols.strings.push("step".into());
    symbols.methods.push((0, 0, 1));
    let method = &mut class.methods[0];
    method.name = "runSuffix".into();
    // The first inner iteration jumps past its physical latch to a guarded,
    // effectful suffix, then returns to the common goto before that suffix.
    method.code.as_mut().unwrap().instructions = vec![
        0x0012, 0x2035, 23, 0x0112, 0x0338, 21, 0x3135, 15, 0x0138, 5, 0x01d8, 0x0101, 0xf828,
        0x0338, 12, 0x0071, 1, 0, 0x01d8, 0x0101, 0xf828, 0x00d8, 0x0100, 0xea28, 0x000f, 0x0071,
        0, 0, 0x0427,
    ];
    class
}

#[test]
fn guarded_continue_suffix_retains_effect_and_original_error_branch() {
    let class = guarded_continue_fixture();
    let code = native_java::render_method("sample.Hello", &class, &class.methods[0]).unwrap();
    assert_eq!(
        code.source.matches("sample.Effects.step()").count(),
        1,
        "{}",
        code.source
    );
    assert_eq!(
        code.source.matches("while (true)").count(),
        2,
        "{}",
        code.source
    );
}

#[test]
fn nested_terminal_guard_preserves_call_and_throw_in_body() {
    let class = fixture();
    let code = native_java::render_method("sample.Hello", &class, &class.methods[0]).unwrap();
    assert_eq!(
        code.source.matches("while (true)").count(),
        2,
        "{}",
        code.source
    );
    assert_eq!(
        code.source.matches("sample.Effects.touch()").count(),
        1,
        "{}",
        code.source
    );
    assert_eq!(code.source.matches("throw ").count(), 1, "{}", code.source);
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn terminal_guard_loops_jvm_preserve_zero_iterations_effects_and_exception_identity() {
    let class = fixture();
    let code = native_java::render_method("sample.Hello", &class, &class.methods[0]).unwrap();
    let suffix = guarded_continue_fixture();
    let suffix = native_java::render_method("sample.Hello", &suffix, &suffix.methods[0]).unwrap();
    let source = format!(
        r#"public class TerminalGuards {{
        static class Effects {{static int calls,steps;static boolean fail;static RuntimeException error=new RuntimeException();
        static void step(){{steps++;}}
        static void touch(){{calls++;if(fail)throw error;}}}}
        {}
        public static void main(String[] args) {{
            RuntimeException original=new RuntimeException();
            for(int outer:new int[]{{-1,0,1,3}}) for(int inner:new int[]{{0,1,4}}) for(boolean failure:new boolean[]{{false,true}}) for(boolean suffix:new boolean[]{{false,true}}) {{
                Effects.calls=Effects.steps=0;Effects.fail=failure;
                boolean abrupt=outer>0&&inner==0;
                try {{int result=suffix?runSuffix(outer,inner,original):run(outer,inner,original);if(abrupt||result!=Math.max(0,outer))throw new AssertionError("result");}}
                catch(RuntimeException ex) {{if(!abrupt||ex!=(failure?Effects.error:original))throw new AssertionError("identity",ex);}}
                if(Effects.calls!=(abrupt?1:0))throw new AssertionError("effects");
                if(Effects.steps!=((suffix&&inner>0)?Math.max(0,outer):0))throw new AssertionError("suffix effects");
            }}
        }}
    }}"#,
        format!("{}\n{}", code.source, suffix.source).replace("sample.Effects", "Effects")
    );
    let dir = std::env::temp_dir().join(format!("rdx-terminal-guards-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("TerminalGuards.java"), &source).unwrap();
    for (program, arg) in [("javac", "TerminalGuards.java"), ("java", "TerminalGuards")] {
        let output = std::process::Command::new(program)
            .arg(arg)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{program}: {}\n{source}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore = "requires exact Airbnb APK via RDX_TEST_APK"]
fn airbnb_a4z0_exact_reader_methods_reconstruct() {
    use rdx::{engine::DecompilerEngine, native_engine::NativeDexEngine};
    let apk = std::env::var("RDX_TEST_APK").expect("RDX_TEST_APK");
    let mut engine = NativeDexEngine::default();
    engine.open(Path::new(&apk)).unwrap();
    let class = engine.class("a4z0").expect("a4z0");
    for name in ["ǃ", "invoke"] {
        let method = class
            .methods
            .iter()
            .find(|m| {
                m.name.as_ref() == name
                    && m.parameters
                        .iter()
                        .map(AsRef::as_ref)
                        .eq(["Ljava/lang/Object;", "Ljava/lang/Object;"])
                    && m.return_type.as_ref() == "Ljava/lang/Object;"
            })
            .expect("exact Object,Object method");
        native_java::render_method("a4z0", class, method)
            .unwrap_or_else(|error| panic!("a4z0.{name}: {error:#}"));
    }
}
