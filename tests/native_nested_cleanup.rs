use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_java,
};
use std::sync::Arc;
fn fixture() -> DexClass {
    let owner: Arc<str> = "Lsample/Cleanup;".into();
    let ty: Arc<str> = "Ljava/lang/RuntimeException;".into();
    DexClass {
        descriptor: owner.clone(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols {
            types: vec![owner.clone()],
            strings: vec![
                "obtain".into(),
                "work".into(),
                "release".into(),
                "log".into(),
            ],
            protos: vec![
                ("Ljava/lang/Object;".into(), vec![]),
                ("Z".into(), vec!["Ljava/lang/Object;".into()]),
                ("V".into(), vec!["Ljava/lang/Object;".into()]),
                ("V".into(), vec![ty.clone()]),
            ],
            methods: vec![(0, 0, 0), (0, 1, 1), (0, 2, 2), (0, 3, 3)],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: owner,
            name: "test".into(),
            return_type: "V".into(),
            parameters: vec![ty.clone()],
            thrown_types: vec![],
            access_flags: 9,
            code: Some(DexCode {
                registers: 3,
                ins: 1,
                outs: 1,
                tries: 5,
                offset: 0,
                instructions: vec![
                    0x0071, 0, 0, 0x010c, 0x1071, 1, 1, 0x000a, 0x0038, 6, 0x1071, 2, 1, 0x000e,
                    0x0227, 0x000d, 0x1071, 2, 1, 0x0027, 0x000d, 0x1071, 3, 0, 0x000e,
                ],
                try_regions: [
                    (0, 4, Some(ty.clone()), 20),
                    (4, 8, None, 15),
                    (10, 13, Some(ty.clone()), 20),
                    (14, 15, None, 15),
                    (16, 20, Some(ty), 20),
                ]
                .into_iter()
                .map(|(start, end, ty, h)| DexTryRegion {
                    start,
                    end,
                    catches: vec![(ty, h)].into(),
                })
                .collect(),
            }),
        }],
    }
}
fn render(c: &DexClass) -> anyhow::Result<rdx::engine::DecompiledCode> {
    native_java::render_method("sample.Cleanup", c, &c.methods[0])
}
#[test]
fn duplicate_cleanup_becomes_one_finally_with_original_links() {
    let code = render(&fixture()).unwrap();
    assert_eq!(
        code.source.matches("finally {").count(),
        1,
        "{}",
        code.source
    );
    assert_eq!(
        code.source.matches("sample.Cleanup.release(").count(),
        1,
        "{}",
        code.source
    );
    assert!(code.source.contains("catch (java.lang.RuntimeException"));
    for (label, token) in [
        ("sample.Cleanup.obtain()Ljava/lang/Object;", "obtain"),
        ("sample.Cleanup.work(Ljava/lang/Object;)Z", "work"),
        ("sample.Cleanup.release(Ljava/lang/Object;)V", "release"),
    ] {
        let links: Vec<_> = code.links.iter().filter(|l| l.label == label).collect();
        assert_eq!(links.len(), 1);
        assert_eq!(
            code.source
                .chars()
                .skip(links[0].start)
                .take(links[0].end - links[0].start)
                .collect::<String>(),
            token
        );
    }
}
#[test]
fn mismatched_cleanup_inputs_dispatch_and_bypass_returns_are_rejected() {
    for (index, value) in [(18, 2), (9, 5), (7, 0x010a), (19, 0x0227), (17, 3)] {
        let mut c = fixture();
        c.methods[0].code.as_mut().unwrap().instructions[index] = value;
        assert!(render(&c).is_err(), "accepted mutation {index}");
    }
    let mut c = fixture();
    c.methods[0].code.as_mut().unwrap().try_regions[1].start = 7;
    assert!(render(&c).is_err());
    let mut c = fixture();
    c.methods[0].code.as_mut().unwrap().try_regions[2].end = 12;
    assert!(render(&c).is_err(), "accepted split invocation boundary");
}
// Independent bounded execution of the original DEX fixture. Runtime vs other
// failures distinguish the outer typed catch from the inner catch-all.
fn dex_trace(success: bool, fault: Option<(u16, bool)>, release_fault: bool) -> Vec<&'static str> {
    let c = fixture();
    let code = c.methods[0].code.as_ref().unwrap();
    let mut pc = 0;
    let mut caught = true;
    let mut trace = vec![];
    for _ in 0..64 {
        let at = pc;
        let mut thrown = None;
        match code.instructions[pc] as u8 {
            0x71 => {
                let method = code.instructions[pc + 1];
                trace.push(["obtain", "work", "release", "log"][method as usize]);
                if method == 2 && release_fault {
                    thrown = Some(true);
                } else if let Some((id, runtime)) = fault
                    && id == method
                {
                    thrown = Some(runtime);
                }
                pc += 3;
            }
            0x0a | 0x0c => pc += 1,
            0x0d => pc += 1,
            0x38 => {
                pc = if success {
                    pc + 2
                } else {
                    (pc as isize + code.instructions[pc + 1] as i16 as isize) as usize
                }
            }
            0x27 => {
                thrown = Some(if code.instructions[pc] >> 8 == 2 {
                    true
                } else {
                    caught
                })
            }
            0x0e => {
                trace.push("return");
                return trace;
            }
            _ => panic!("fixture opcode"),
        }
        if let Some(runtime) = thrown {
            if let Some(region) = code.try_regions.iter().find(|r| {
                r.start as usize <= at
                    && at < (r.end as usize)
                    && (r.catches[0].0.is_none() || runtime)
            }) {
                caught = runtime;
                pc = region.catches[0].1 as usize;
            } else {
                trace.push(if runtime { "runtime" } else { "other" });
                return trace;
            }
        }
    }
    panic!("fixture loop");
}
fn structured_trace(
    source: &str,
    success: bool,
    fault: Option<(u16, bool)>,
    release_fault: bool,
) -> Vec<&'static str> {
    assert!(source.contains("finally {"));
    assert!(source.contains("catch (java.lang.RuntimeException"));
    let mut trace = vec!["obtain"];
    let mut failure = if let Some((0, runtime)) = fault {
        Some(runtime)
    } else {
        trace.push("work");
        let error = if let Some((1, runtime)) = fault {
            Some(runtime)
        } else if success {
            None
        } else {
            Some(true)
        };
        trace.push("release");
        if release_fault { Some(true) } else { error }
    };
    if failure == Some(true) {
        trace.push("log");
        failure = if let Some((3, runtime)) = fault {
            Some(runtime)
        } else {
            None
        };
    }
    trace.push(match failure {
        None => "return",
        Some(true) => "runtime",
        Some(false) => "other",
    });
    trace
}
#[test]
fn cleanup_normal_exception_and_cleanup_failure_traces_match() {
    let source = render(&fixture()).unwrap().source;
    for success in [false, true] {
        for release_fault in [false, true] {
            for fault in [
                None,
                Some((0, true)),
                Some((0, false)),
                Some((1, true)),
                Some((1, false)),
                Some((3, true)),
                Some((3, false)),
            ] {
                assert_eq!(
                    dex_trace(success, fault, release_fault),
                    structured_trace(&source, success, fault, release_fault)
                );
            }
        }
    }
}
