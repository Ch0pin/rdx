use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::sync::Arc;
fn class(name: &str, symbols: Arc<DexSymbols>, methods: Vec<DexMethod>) -> DexClass {
    DexClass {
        symbols,
        descriptor: name.into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 0x11,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods,
    }
}
fn method(owner: &str, name: &str, words: Vec<u16>, regs: u16) -> DexMethod {
    DexMethod {
        declaring_type: owner.into(),
        name: name.into(),
        return_type: "V".into(),
        parameters: vec![],
        thrown_types: vec![],
        access_flags: 9,
        code: Some(DexCode {
            registers: regs,
            ins: 0,
            outs: 0,
            tries: 0,
            try_regions: vec![],
            instructions: words,
            offset: 0,
        }),
    }
}
fn fixture(variant: u8) -> (DexClass, DexClass, Arc<DexSymbols>) {
    let symbols = Arc::new(DexSymbols {
        types: vec![
            "Lsample/Agent;".into(),
            "Lsample/Api;".into(),
            "Lsample/Fault;".into(),
            "Lsample/Marker;".into(),
        ],
        strings: vec![
            if variant == 12 { "close" } else { "delegate" }.into(),
            "noop".into(),
            "tick".into(),
            "<init>".into(),
        ],
        protos: vec![("V".into(), vec![])],
        methods: vec![
            (
                1,
                if variant == 10 { 9 } else { 0 },
                if variant == 11 { 99 } else { 1 },
            ),
            (0, 0, 0),
            (2, 0, 2),
            (3, 0, 3),
        ],
        ..Default::default()
    });
    let mut target = method("Lsample/Api;", "noop", vec![0x000e], 0);
    if variant == 1 {
        target.thrown_types = vec!["Ljava/io/IOException;".into()];
    }
    if variant == 2 {
        target.access_flags = 1;
        target.code.as_mut().unwrap().registers = 1;
        target.code.as_mut().unwrap().ins = 1;
    }
    if variant == 3 {
        target.parameters = vec!["Ljava/lang/Object;".into()];
        target.code.as_mut().unwrap().registers = 1;
        target.code.as_mut().unwrap().ins = 1;
    }
    if variant == 6 {
        target.code.as_mut().unwrap().instructions = vec![0x0071, 2, 0, 0x000e];
    }
    let mut delegate = method("Lsample/Agent;", "delegate", vec![0x0071, 0, 0, 0x000e], 0);
    if variant == 13 {
        delegate.code.as_mut().unwrap().registers = 1;
        delegate.code.as_mut().unwrap().instructions = vec![0x0071, 0, 0, 0x0012, 0x0027];
    }
    if variant == 4 {
        delegate.code.as_mut().unwrap().instructions = vec![0x0071, 0];
    }
    if variant == 6 {
        let code = delegate.code.as_mut().unwrap();
        code.registers = 1;
        code.outs = 1;
        code.instructions = vec![0x0022, 3, 0x1070, 3, 0, 0x0071, 0, 0, 0x000e];
    }
    if variant == 9 {
        delegate.code.as_mut().unwrap().instructions[1] = 65535;
    }
    if variant == 12 {
        delegate.name = "close".into();
        delegate.access_flags = 1;
        let code = delegate.code.as_mut().unwrap();
        code.registers = 1;
        code.ins = 1;
    }
    if variant == 5 {
        delegate
            .code
            .as_mut()
            .unwrap()
            .instructions
            .extend([0x0100, 0, 0, 0]);
    }
    let mut run = method(
        "Lsample/Calls;",
        "run",
        vec![0x0071, 1, 0, 0x000e, 0x000d, 0x0027],
        1,
    );
    if variant == 12 {
        run.parameters = vec!["Lsample/Agent;".into()];
        let code = run.code.as_mut().unwrap();
        code.ins = 1;
        code.outs = 1;
        code.instructions[0] = 0x106e;
    }
    run.code.as_mut().unwrap().tries = 1;
    run.code.as_mut().unwrap().try_regions = vec![DexTryRegion {
        start: 0,
        end: 3,
        catches: vec![(None, 4)].into(),
    }];
    let mut agent = class("Lsample/Agent;", symbols.clone(), vec![delegate]);
    let caller = class("Lsample/Calls;", symbols.clone(), vec![run]);
    let api = class("Lsample/Api;", symbols.clone(), vec![target]);
    let mut api = api;
    if variant == 8 {
        let mut other = method("Lsample/Api;", "noop", vec![0x0012, 0x0011], 1);
        other.return_type = "Ljava/lang/Object;".into();
        api.methods.push(other);
    }
    if variant == 12 {
        agent.superclass = Some("Ljava/io/InputStream;".into());
    }
    let mut ctor = method("Lsample/Marker;", "<init>", vec![], 1);
    ctor.access_flags = 1;
    ctor.code = None;
    let marker = class("Lsample/Marker;", symbols.clone(), vec![ctor]);
    let duplicate = class(
        "Lsample/Api;",
        symbols.clone(),
        vec![method("Lsample/Api;", "noop", vec![0x000e], 0)],
    );
    let mut classes = vec![&agent, &caller, &api, &marker];
    if variant == 7 {
        classes.push(&duplicate);
    }
    let hierarchy = TypeHierarchy::from_classes(classes).unwrap();
    symbols.hierarchy.set(Arc::new(hierarchy)).unwrap();
    (agent, caller, symbols)
}
#[test]
fn exact_loaded_empty_static_call_proves_no_new_header() {
    let (agent, caller, symbols) = fixture(0);
    let h = symbols.hierarchy.get().unwrap();
    assert!(
        h.call_checked_effects(&agent.descriptor, "delegate", &[], "V", true)
            .unwrap()
            .is_empty()
    );
    let declaration = native_java::render_method("sample.Agent", &agent, &agent.methods[0])
        .unwrap()
        .source;
    assert!(!declaration.contains(" throws "));
    let caller = native_java::render_method("sample.Calls", &caller, &caller.methods[0])
        .unwrap()
        .source;
    assert!(caller.contains("throw e"));
}
#[test]
fn checked_static_target_keeps_potential_generated_header() {
    let (agent, caller, symbols) = fixture(1);
    assert_eq!(
        symbols
            .hierarchy
            .get()
            .unwrap()
            .call_checked_effects(&agent.descriptor, "delegate", &[], "V", true)
            .unwrap(),
        vec![Arc::<str>::from("Ljava/lang/Throwable;")]
    );
    let source = native_java::render_method("sample.Agent", &agent, &agent.methods[0])
        .unwrap()
        .source;
    assert!(source.contains("throws java.io.IOException"));
    let caller_source = native_java::render_method("sample.Calls", &caller, &caller.methods[0])
        .unwrap()
        .source;
    assert!(
        caller_source.contains("throw _rdx_caught_throw_"),
        "{caller_source}"
    );
    assert!(
        !caller_source
            .lines()
            .find(|line| line.contains(" run("))
            .unwrap()
            .contains(" throws "),
        "{caller_source}"
    );
}
#[test]
fn static_prototype_and_truncation_uncertainty_stays_unknown() {
    for variant in 2..=5 {
        let (agent, _, symbols) = fixture(variant);
        assert_eq!(
            symbols
                .hierarchy
                .get()
                .unwrap()
                .call_checked_effects(&agent.descriptor, "delegate", &[], "V", true)
                .unwrap(),
            vec![Arc::<str>::from("Ljava/lang/Throwable;")]
        );
    }
}

#[test]
fn same_owner_checked_parameter_helper_remains_a_header_trigger() {
    let symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/Agent;".into()],
        strings: vec!["delegate".into(), "raise".into()],
        protos: vec![
            ("V".into(), vec![]),
            ("V".into(), vec!["Ljava/io/IOException;".into()]),
        ],
        methods: vec![(0, 1, 1), (0, 0, 0)],
        ..Default::default()
    });
    let delegate = method(
        "Lsample/Agent;",
        "delegate",
        vec![0x0012, 0x1071, 0, 0, 0x000e],
        1,
    );
    let mut helper = method("Lsample/Agent;", "raise", vec![0x0027], 1);
    helper.access_flags = 10;
    helper.parameters = vec!["Ljava/io/IOException;".into()];
    helper.code.as_mut().unwrap().ins = 1;
    let agent = class("Lsample/Agent;", symbols.clone(), vec![delegate, helper]);
    let mut run = method(
        "Lsample/Calls;",
        "run",
        vec![0x0071, 1, 0, 0x000e, 0x000d, 0x0027],
        1,
    );
    run.code.as_mut().unwrap().tries = 1;
    run.code.as_mut().unwrap().try_regions = vec![DexTryRegion {
        start: 0,
        end: 3,
        catches: vec![(None, 4)].into(),
    }];
    let caller = class("Lsample/Calls;", symbols.clone(), vec![run]);
    let h = TypeHierarchy::from_classes([&agent, &caller]).unwrap();
    assert_eq!(
        h.call_checked_effects(&agent.descriptor, "delegate", &[], "V", true)
            .unwrap(),
        vec![Arc::<str>::from("Ljava/lang/Throwable;")]
    );
    symbols.hierarchy.set(Arc::new(h)).unwrap();
    let source = native_java::render_method("sample.Agent", &agent, &agent.methods[0])
        .unwrap()
        .source;
    assert!(source.contains("throws java.io.IOException"));
    let helper = native_java::render_method("sample.Agent", &agent, &agent.methods[1])
        .unwrap()
        .source;
    assert!(helper.contains("throws java.io.IOException"));
}

#[test]
#[ignore = "requires javac and java"]
fn complete_header_sources_compile_and_preserve_unchecked_fault_identity() {
    let (agent, caller, symbols) = fixture(6);
    let api = class(
        "Lsample/Api;",
        symbols,
        vec![method(
            "Lsample/Api;",
            "noop",
            vec![0x0071, 2, 0, 0x000e],
            0,
        )],
    );
    let dir = std::env::temp_dir().join(format!("rdx-complete-header-{}", std::process::id()));
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(dir.clone());
    std::fs::create_dir_all(dir.join("sample")).unwrap();
    for (name, class) in [("Agent", &agent), ("Calls", &caller), ("Api", &api)] {
        let source =
            native_java::render_method(&format!("sample.{name}"), class, &class.methods[0])
                .unwrap()
                .source;
        std::fs::write(
            dir.join(format!("sample/{name}.java")),
            format!("package sample; public class {name} {{\n{source}}}"),
        )
        .unwrap();
    }
    std::fs::write(dir.join("sample/Marker.java"),"package sample; public class Marker { public Marker() { if(Fault.constructor) Fault.tick(); } }").unwrap();
    std::fs::write(dir.join("sample/Fault.java"),"package sample; public class Fault { static Throwable error; static boolean constructor; public static void tick() { if(error instanceof RuntimeException) throw (RuntimeException) error; if(error instanceof Error) throw (Error) error; } }").unwrap();
    std::fs::write(dir.join("sample/Main.java"),"package sample; public class Main { public static void main(String[] args) { Calls.run(); for(boolean constructor:new boolean[]{false,true}) { Fault.constructor=constructor; for(Throwable e:new Throwable[]{new RuntimeException(),new AssertionError()}) { Fault.error=e; try { Calls.run(); throw new AssertionError(); } catch(Throwable actual) { if(actual!=e) throw new AssertionError(); } } } System.out.println(\"identity-pass\"); } }").unwrap();
    let mut cmd = std::process::Command::new("javac");
    cmd.arg("-d").arg(&dir);
    for name in ["Agent", "Calls", "Api", "Fault", "Marker", "Main"] {
        cmd.arg(dir.join(format!("sample/{name}.java")));
    }
    let result = cmd.output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let result = std::process::Command::new("java")
        .arg("-cp")
        .arg(&dir)
        .arg("sample.Main")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stdout).contains("identity-pass"));
}

#[test]
fn duplicate_owner_method_and_malformed_symbol_contracts_stay_unknown() {
    for variant in 7..=11 {
        let (agent, _, symbols) = fixture(variant);
        assert_eq!(
            symbols
                .hierarchy
                .get()
                .unwrap()
                .call_checked_effects(&agent.descriptor, "delegate", &[], "V", true)
                .unwrap(),
            vec![Arc::<str>::from("Ljava/lang/Throwable;")],
            "variant={variant}"
        );
    }
}
#[test]
fn inherited_checked_header_survives_complete_empty_body_header_proof() {
    let (agent, _, symbols) = fixture(12);
    assert_eq!(
        symbols
            .hierarchy
            .get()
            .unwrap()
            .call_checked_effects(&agent.descriptor, "close", &[], "V", false)
            .unwrap(),
        vec![Arc::<str>::from("Ljava/io/IOException;")]
    );
    let source = native_java::render_method("sample.Agent", &agent, &agent.methods[0])
        .unwrap()
        .source;
    assert!(source.contains("throws java.io.IOException"));
}

#[test]
fn explicit_null_throw_keeps_a_closed_generated_header() {
    let (agent, caller, symbols) = fixture(13);
    assert_eq!(
        symbols.hierarchy.get().unwrap().call_checked_effects(
            &agent.descriptor,
            "delegate",
            &[],
            "V",
            true
        ),
        Some(vec![])
    );
    let source = native_java::render_method("sample.Agent", &agent, &agent.methods[0])
        .unwrap()
        .source;
    assert!(source.contains("throw null;"), "{source}");
    assert!(!source.contains(" throws "), "{source}");
    assert!(native_java::render_method("sample.Calls", &caller, &caller.methods[0]).is_ok());
}
