use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_java,
};
use std::sync::Arc;

fn fixture(words: Vec<u16>, ranges: &[(u32, u32, u32)]) -> DexClass {
    DexClass {
        descriptor: "Lsample/Monitor;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols {
            strings: vec!["effect".into()],
            types: vec!["Lsample/Monitor;".into()],
            protos: vec![("I".into(), vec![])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: "Lsample/Monitor;".into(),
            name: "test".into(),
            return_type: "I".into(),
            parameters: vec!["Ljava/lang/Object;".into()],
            thrown_types: vec![],
            access_flags: 9,
            code: Some(DexCode {
                registers: 3,
                ins: 1,
                outs: 0,
                tries: ranges.len() as u16,
                try_regions: ranges
                    .iter()
                    .map(|&(start, end, handler)| DexTryRegion {
                        start,
                        end,
                        catches: vec![(None, handler)].into(),
                    })
                    .collect(),
                instructions: words,
                offset: 0,
            }),
        }],
    }
}
fn detached() -> DexClass {
    fixture(
        vec![
            0x021d, 0x0071, 0, 0, 0x000a, 0x021e, 0x000f, 0x010d, 0x021e, 0x0127,
        ],
        &[(1, 6, 7), (8, 9, 7)],
    )
}
fn render(class: &DexClass) -> anyhow::Result<rdx::engine::DecompiledCode> {
    native_java::render_method("sample.Monitor", class, &class.methods[0])
}
#[test]
fn detached_release_keeps_result_in_scope_and_call_inside_monitor() {
    let code = render(&detached()).unwrap();
    let begin = code.source.find("synchronized (p0)").unwrap();
    let call = code.source.find("sample.Monitor.effect()").unwrap();
    let end = code.source[call..].find('}').unwrap() + call;
    assert!(begin < call && call < end, "{}", code.source);
    assert_eq!(code.source.matches(".effect()").count(), 1);
    assert!(code.source[end..].contains("return "), "{}", code.source);
    assert!(
        code.links
            .iter()
            .any(|link| link.label == "sample.Monitor.effect()I")
    );
    for link in &code.links {
        assert!(link.start < link.end && link.end <= code.source.chars().count());
    }
}
#[test]
fn interleaved_cleanup_and_release_gotos_render() {
    let class = fixture(
        vec![
            0x021d, 0x0071, 0, 0, 0x000a, 0x0328, 0x010d, 0x0328, 0x021e, 0x0328, 0x021e, 0x0127,
            0x000f,
        ],
        &[(1, 11, 6)],
    );
    assert!(render(&class).unwrap().source.contains("synchronized (p0)"));
}
#[test]
fn malformed_monitor_shapes_fail_closed() {
    for (pc, word) in [
        (5, 0x011e),
        (8, 0x001e),
        (9, 0x0027),
        (7, 0x020d),
        (4, 0x020a),
        (5, 0x000f),
        (5, 0x021d),
    ] {
        let mut class = detached();
        class.methods[0].code.as_mut().unwrap().instructions[pc] = word;
        assert!(
            render(&class).is_err(),
            "accepted malformed monitor at {pc}"
        );
    }
    let mut class = detached();
    class.methods[0].code.as_mut().unwrap().try_regions[0].end = 2;
    assert!(render(&class).is_err());
    let mut class = detached();
    class.methods[0].code.as_mut().unwrap().try_regions[0].catches =
        vec![(Some("Ljava/lang/Exception;".into()), 7)].into();
    assert!(render(&class).is_err());
}
#[test]
fn external_jump_into_protected_body_is_rejected() {
    // An alternate entry before acquisition reaches the protected call directly.
    let class = fixture(
        vec![
            0x0238, 3, 0x021d, 0x0071, 0, 0, 0x000a, 0x021e, 0x000f, 0x010d, 0x021e, 0x0127,
        ],
        &[(3, 8, 9)],
    );
    assert!(render(&class).is_err());
}

// Independent bounded evaluators for these monitor fixtures. This is a semantic
// regression for the supported shape, not Android/JVM execution or a general VM.
fn dex_trace(class: &DexClass, null_lock: bool, throws: bool) -> Vec<String> {
    let code = class.methods[0].code.as_ref().unwrap();
    let mut pc = 0;
    let mut held = false;
    let mut trace = vec![];
    let mut result = 0;
    for _ in 0..100 {
        let instruction_pc = pc;
        let op = code.instructions[pc] as u8;
        let mut error = None;
        match op {
            0x1d => {
                trace.push("enter".into());
                if null_lock {
                    error = Some("NPE");
                } else {
                    held = true;
                }
                pc += 1;
            }
            0x1e => {
                assert!(held);
                held = false;
                trace.push("release".into());
                pc += 1;
            }
            0x71 => {
                trace.push("effect".into());
                if throws {
                    error = Some("effect-error");
                } else {
                    result = 7;
                }
                pc += 3;
            }
            0x0a | 0x0d => pc += 1,
            0x28 => pc = (pc as isize + (code.instructions[pc] >> 8) as i8 as isize) as usize,
            0x0f => {
                assert!(!held);
                trace.push(format!("return:{result}"));
                return trace;
            }
            0x27 => error = Some("effect-error"),
            _ => panic!("unsupported evaluator opcode {op:x}"),
        }
        if let Some(error) = error {
            if let Some(region) = code.try_regions.iter().find(|region| {
                region.start as usize <= instruction_pc && instruction_pc < region.end as usize
            }) {
                pc = region.catches[0].1 as usize;
            } else {
                assert!(!held);
                trace.push(format!("throw:{error}"));
                return trace;
            }
        }
    }
    panic!("fixture evaluator budget");
}
fn java_trace(source: &str, null_lock: bool, throws: bool) -> Vec<String> {
    use std::collections::HashMap;
    let mut values = HashMap::<String, i32>::new();
    let mut held = false;
    let mut trace = vec![];
    for line in source.lines().map(str::trim) {
        if line.starts_with("public static ") || line.is_empty() {
            continue;
        }
        if line == "synchronized (p0) {" {
            trace.push("enter".into());
            if null_lock {
                trace.push("throw:NPE".into());
                return trace;
            }
            held = true;
            continue;
        }
        if line == "{" {
            continue;
        } // negative control: ordinary block
        if line == "}" {
            if held {
                held = false;
                trace.push("release".into());
            }
            continue;
        }
        if let Some(value) = line
            .strip_prefix("return ")
            .and_then(|s| s.strip_suffix(';'))
        {
            assert!(!held);
            trace.push(format!("return:{}", values[value]));
            return trace;
        }
        if let Some((left, right)) = line.trim_end_matches(';').split_once(" = ") {
            let name = left.strip_prefix("int ").unwrap_or(left);
            let value = if right == "sample.Monitor.effect()" {
                trace.push("effect".into());
                if throws {
                    if held {
                        trace.push("release".into());
                    }
                    trace.push("throw:effect-error".into());
                    return trace;
                }
                7
            } else {
                *values.get(right).expect("unsupported Java expression")
            };
            values.insert(name.to_string(), value);
            continue;
        }
        assert!(
            line.starts_with("int ") && line.ends_with(';'),
            "unsupported Java line: {line}"
        );
    }
    panic!("missing Java return");
}
#[test]
fn monitor_semantics_normal_null_and_exception_paths_match() {
    for class in [
        detached(),
        fixture(
            vec![
                0x021d, 0x0071, 0, 0, 0x000a, 0x0328, 0x010d, 0x0328, 0x021e, 0x0328, 0x021e,
                0x0127, 0x000f,
            ],
            &[(1, 11, 6)],
        ),
    ] {
        let source = render(&class).unwrap().source;
        for (null_lock, throws) in [(false, false), (true, false), (false, true)] {
            let expected = dex_trace(&class, null_lock, throws);
            assert_eq!(java_trace(&source, null_lock, throws), expected, "{source}");
            let missing_monitor = source.replace("synchronized (p0) {", "{");
            assert_ne!(java_trace(&missing_monitor, null_lock, throws), expected);
        }
    }
}

#[test]
#[ignore = "Requires RDX_TEST_APK set to Expedia"]
fn hilt_component_manager_monitor_region() {
    use rdx::{engine::DecompilerEngine, native_engine::NativeDexEngine};
    let path = std::env::var_os("RDX_TEST_APK").unwrap();
    let mut engine = NativeDexEngine::default();
    engine.open(std::path::Path::new(&path)).unwrap();
    let name =
        "com.expedia.bookings.loyalty.nondismissiblebanner.Hilt_NonDismissibleBannerActivity";
    let class = engine.class(name).unwrap();
    let method = class
        .methods
        .iter()
        .find(|m| m.name.as_ref() == "componentManager" && m.return_type.as_ref() == "Lcs5/a;")
        .unwrap();
    let code = native_java::render_method(name, class, method).unwrap();
    assert_eq!(code.source.matches("synchronized (").count(), 1);
    assert_eq!(code.source.matches(".createComponentManager()").count(), 1);
    std::fs::write("target/validation/hilt-componentManager.java", code.source).unwrap();
}

#[test]
fn unprotected_monitor_instructions_return_errors_without_panicking() {
    for opcode in [0x021d, 0x021e] {
        let class = fixture(vec![opcode, 0x0012, 0x000f], &[]);
        let error = render(&class).unwrap_err();
        assert!(
            error.to_string().contains("monitor instruction"),
            "{error:#}"
        );
    }
}

#[test]
fn nonthrowing_tail_outside_try_preserves_release_on_every_path() {
    let mut class = detached();
    class.methods[0].code.as_mut().unwrap().try_regions[0].end = 4;
    let java = render(&class).unwrap();
    for (null, throws) in [(false, false), (true, false), (false, true)] {
        assert_eq!(
            dex_trace(&class, null, throws),
            java_trace(&java.source, null, throws)
        );
    }
}

#[test]
fn synchronized_loop_allows_unprotected_branch_but_not_unprotected_call() {
    let mut class = fixture(
        vec![
            0x021d, 0x2012, 0x0071, 0, 0, 0x00d8, 0xff00, 0x0039, 0xfffb, 0x021e, 0x7012, 0x000f,
            0x010d, 0x021e, 0x0127,
        ],
        &[(1, 7, 12)],
    );
    let java = render(&class).unwrap().source;
    assert!(java.contains("synchronized (p0)"), "{java}");
    assert!(java.contains("while ("), "{java}");
    assert_eq!(java.matches("sample.Monitor.effect()").count(), 1);
    class.methods[0].code.as_mut().unwrap().try_regions[0].end = 2;
    assert!(
        render(&class)
            .unwrap_err()
            .to_string()
            .contains("throwing monitor body")
    );
}
