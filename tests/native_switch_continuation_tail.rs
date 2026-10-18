pub use rdx::{native_cfg, native_dex, native_dominators, native_ir, native_ssa};
use rdx::{native_dex::DexSymbols, native_java};
use std::sync::Arc;
#[path = "../src/native_java/switch_continuation_tail.rs"]
mod proof;

fn fixture() -> native_dex::DexClass {
    let mut c = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    c.descriptor = "Lsample/SwitchContinuation;".into();
    c.methods.retain(|m| m.name.as_ref() == "answer");
    c.symbols = Arc::new(DexSymbols {
        types: vec![c.descriptor.clone()],
        strings: vec!["step".into()],
        protos: vec![("I".into(), vec!["I".into()])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    let m = &mut c.methods[0];
    m.declaring_type = c.descriptor.clone();
    m.name = "run".into();
    m.access_flags = 9;
    m.parameters = vec!["I".into(), "I".into()];
    m.return_type = "I".into();
    let code = m.code.as_mut().unwrap();
    code.registers = 6;
    code.ins = 2;
    code.outs = 1;
    code.tries = 0;
    code.try_regions.clear();
    code.instructions = vec![
        0x0012, 0x0112, // accumulator and counter
        0x4135, 28, // header2: counter >= count -> exit30
        0x0390, 0x0105, // selector = mode + counter
        0x1071, 0, 3, 0x030a, // step(selector) -> selector
        0x032b, 22, 0,      // packed switch10 -> payload32
        0x0228, // default13 -> first prefix15
        0x20b0, // second prefix14: accumulator += temporary
        0x00d8, 0x0100, // first prefix15: accumulator++
        0x01d8, 0x0101, // counter++
        0xef28, // direct latch19 -> header2
        0x1071, 0, 1, 0x020a, 0xf628, // late case20: step(counter), goto14
        0x1071, 0, 0, 0x020a, 0xf228, // late case25: step(accumulator), goto15
        0x000f, 0, // return30; payload alignment
        0x0100, 2, 0, 0, 10, 0, 15, 0,
    ];
    c
}

fn late_only_literal_fixture() -> native_dex::DexClass {
    let mut c = fixture();
    let code = c.methods[0].code.as_mut().unwrap();
    // The accumulator starts as literal zero and changes only on a late case
    // path. A walk ending at the direct latch incorrectly calls it invariant.
    code.instructions[15] = 0x0001;
    code.instructions[16] = 0;
    c
}

fn separate_exit_fixture() -> native_dex::DexClass {
    let mut c = fixture();
    c.symbols = Arc::new(DexSymbols {
        types: vec![c.descriptor.clone(), "Ljava/lang/Integer;".into()],
        strings: vec![
            "step".into(),
            "valueOf".into(),
            "intValue".into(),
            "exit".into(),
        ],
        protos: vec![
            ("I".into(), vec!["I".into()]),
            ("Ljava/lang/Integer;".into(), vec!["I".into()]),
            ("I".into(), vec![]),
        ],
        methods: vec![(0, 0, 0), (1, 1, 1), (1, 2, 2)],
        ..Default::default()
    });
    let m = &mut c.methods[0];
    m.return_type = "Ljava/lang/String;".into();
    let code = m.code.as_mut().unwrap();
    code.registers = 7;
    code.instructions = vec![
        0x0012, 0x1071, 1, 0, 0x000c, 0x0112, // Integer.valueOf(0), counter0
        0x106e, 2, 0, 0x030a, // header6: Integer.intValue()
        0x1071, 0, 3, 0x030a, // step(carried value), retains header effect order
        0x0207, 0x001a, 3, // preserve Integer v0 in v2, then String v0
        0x5135, 24, // guard17 -> exit41
        0x1071, 0, 6, 0x030a, // step(mode)
        0x032b, 21, 0, // switch23 -> payload44
        0x0128, 0x2007, // default26 -> prefix27 restores Integer
        0x01d8, 0x0101, 0xe828, // increment28; direct latch30 -> header6
        0x1071, 1, 1, 0x000c, 0xf928, // late31: box counter, goto28
        0x1071, 1, 5, 0x000c, 0xf428, // late36: box count, goto28
        0x0128, 0x0011, 0, // exit41: goto, deferred String read42, payload alignment
        0x0100, 2, 0, 0, 8, 0, 13, 0,
    ];
    c
}

fn truncated_null_fixture() -> native_dex::DexClass {
    let mut c = separate_exit_fixture();
    let symbols = Arc::get_mut(&mut c.symbols).unwrap();
    let observe_name = symbols.strings.len() as u32;
    symbols.strings.push("observe".into());
    let observe_proto = symbols.protos.len() as u16;
    symbols
        .protos
        .push(("I".into(), vec!["Ljava/lang/Integer;".into()]));
    let observe_method = symbols.methods.len() as u16;
    symbols.methods.push((0, observe_proto, observe_name));
    let mut prefix = Vec::new();
    for (index, ty) in [
        "String",
        "Integer",
        "Long",
        "Double",
        "Float",
        "Short",
        "Byte",
        "Boolean",
        "Character",
    ]
    .iter()
    .enumerate()
    {
        let proto = symbols.protos.len() as u16;
        let name = symbols.strings.len() as u32;
        symbols.strings.push(format!("null{index}"));
        symbols
            .protos
            .push(("V".into(), vec![format!("Ljava/lang/{ty};").into()]));
        let method = symbols.methods.len() as u16;
        symbols.methods.push((0, proto, name));
        prefix.extend([0x1071, method, 4]);
    }
    prefix.push(0); // retain aligned switch payload after prefix insertion
    let code = c.methods[0].code.as_mut().unwrap();
    code.instructions[0] = 0x0412; // same zero used numerically and by nine reference sinks
    code.instructions[3] = 4; // Integer.valueOf(v4)
    code.instructions[19..23].copy_from_slice(&[0x0390, 0x0106, 0, 0]); // selector=mode+counter, mixed paths
    code.instructions[26] = 0x4307; // default: read nullable carried v4 via move-object
    code.instructions[35] = 0x0407; // late case: v4 = boxed Integer; fall into sibling
    code.instructions
        .splice(27..27, [0x1071, observe_method, 4, 0x030a]);
    code.instructions[18] = 28; // guard -> shifted exit
    code.instructions[24] = 25; // switch -> shifted payload
    code.instructions[34] = 0xe428; // shifted latch -> original header
    code.instructions[52] = 12; // shifted first late case
    code.instructions[54] = 17; // shifted second late case
    code.instructions.splice(6..6, prefix);
    c
}

#[test]
fn shared_zero_with_truncated_use_bounds_preserves_nullable_carried_reference() {
    let c = truncated_null_fixture();
    let analysis = rdx::native_method::MethodAnalysis::build(&c, &c.methods[0]).unwrap();
    let types = analysis.infer_types().unwrap();
    let phi = analysis
        .ssa()
        .phis
        .iter()
        .find(|p| p.register == 4 && analysis.blocks().blocks[p.block].start == 34)
        .unwrap();
    let inferred = &types.values[phi.result];
    assert!(inferred.bounds_truncated, "{inferred:?}");
    assert!(!inferred.assignment_bounds_truncated, "{inferred:?}");
    assert!(
        inferred
            .assignment
            .contains(&rdx::native_types::AssignmentBound::Literal {
                bits: 0,
                wide: false
            })
    );
    let source = native_java::render_method("sample.SwitchContinuation", &c, &c.methods[0])
        .unwrap()
        .source;
    assert!(source.contains("Integer"), "{source}");
    let mut nonzero = truncated_null_fixture();
    nonzero.methods[0].code.as_mut().unwrap().instructions[0] = 0x1412;
    assert!(
        native_java::render_method("sample.SwitchContinuation", &nonzero, &nonzero.methods[0])
            .is_err()
    );
    let mut primitive = truncated_null_fixture();
    primitive.methods[0].code.as_mut().unwrap().instructions[54] = 0x4381; // int-to-long from nullable v4
    assert!(
        native_java::render_method(
            "sample.SwitchContinuation",
            &primitive,
            &primitive.methods[0]
        )
        .is_err()
    );
}

#[test]
fn carried_integer_and_deferred_string_exit_have_separate_locals() {
    let c = separate_exit_fixture();
    let p = proof::select(c.methods[0].code.as_ref().unwrap()).unwrap();
    assert_eq!((p.header, p.guard, p.latch, p.exit), (6, 17, 30, 41));
    let rendered =
        native_java::render_method("sample.SwitchContinuation", &c, &c.methods[0]).unwrap();
    assert!(rendered.source.contains("Integer"), "{}", rendered.source);
    assert!(rendered.source.contains("String"), "{}", rendered.source);
    assert_eq!(
        rendered
            .source
            .matches("sample.SwitchContinuation.step(")
            .count(),
        2,
        "{}",
        rendered.source
    );
    let mut changed = separate_exit_fixture();
    // A body normal exit bypasses the header's String definition.
    changed.methods[0].code.as_mut().unwrap().instructions[35] = 0x0628;
    assert!(proof::select(changed.methods[0].code.as_ref().unwrap()).is_none());
}

#[test]
fn literal_written_only_through_late_case_gets_a_carried_local() {
    let c = late_only_literal_fixture();
    assert!(proof::select(c.methods[0].code.as_ref().unwrap()).is_some());
    let rendered =
        native_java::render_method("sample.SwitchContinuation", &c, &c.methods[0]).unwrap();
    assert!(
        !rendered
            .source
            .lines()
            .any(|line| line.trim_start().starts_with("0 =")),
        "{}",
        rendered.source
    );
    assert!(
        rendered.source.contains("while (true)"),
        "{}",
        rendered.source
    );
}

#[test]
fn late_switch_cases_keep_shared_copy_prefix_and_feedback() {
    let c = fixture();
    let p = proof::select(c.methods[0].code.as_ref().unwrap()).unwrap();
    assert_eq!(
        (p.header, p.guard, p.latch, p.exit, p.switch),
        (2, 2, 19, 30, 10)
    );
    assert!(p.edges.contains(&(24, 14)) && p.edges.contains(&(29, 15)));
    let rendered =
        native_java::render_method("sample.SwitchContinuation", &c, &c.methods[0]).unwrap();
    assert_eq!(
        rendered.source.matches("while (true)").count(),
        1,
        "{}",
        rendered.source
    );
    assert_eq!(
        rendered.source.matches("switch (").count(),
        1,
        "{}",
        rendered.source
    );
    assert_eq!(
        rendered
            .source
            .matches("sample.SwitchContinuation.step(")
            .count(),
        3,
        "{}",
        rendered.source
    );
    assert!(
        rendered.source.matches("continue;").count() >= 3,
        "{}",
        rendered.source
    );
    assert!(
        rendered
            .links
            .iter()
            .any(|l| l.label == "sample.SwitchContinuation.step(I)I")
    );
}

#[test]
fn unowned_entry_hidden_cycle_monitor_and_payload_mutations_decline() {
    for mutation in 0..4 {
        let mut c = fixture();
        let code = c.methods[0].code.as_mut().unwrap();
        match mutation {
            0 => code.instructions[0] = 0x1428,  // outside -> late case
            1 => code.instructions[24] = 0xfc28, // hidden case cycle ->18
            2 => code.instructions[14] = 0x001d, // monitor-enter
            3 => code.instructions[36] = 11,     // switch target middle of invoke
            _ => unreachable!(),
        }
        assert!(proof::select(code).is_none(), "mutation {mutation}");
        assert!(
            native_java::render_method("sample.SwitchContinuation", &c, &c.methods[0]).is_err(),
            "mutation {mutation}"
        );
    }
}

#[test]
#[ignore = "root runs serially with javac/java and RDX_SWITCH_JVM_DIR"]
fn jvm_late_switch_cases_preserve_original_dex_trace() {
    let base = std::path::PathBuf::from(
        std::env::var_os("RDX_SWITCH_JVM_DIR").expect("RDX_SWITCH_JVM_DIR"),
    );
    for (base, c) in [
        (base.clone(), fixture()),
        (base.join("late-only-literal"), late_only_literal_fixture()),
    ] {
        let code = c.methods[0].code.as_ref().unwrap();
        std::fs::create_dir_all(&base).unwrap();
        let rendered =
            native_java::render_method("sample.SwitchContinuation", &c, &c.methods[0]).unwrap();
        std::fs::write(base.join("method.java"), rendered.source).unwrap();
        let raw = serde_json::json!({"class":c.descriptor.as_ref(),"words":code.instructions,"registers":code.registers,"ins":code.ins,
        "method_pool":[{"owner":c.descriptor.as_ref(),"name":"step","parameters":["I"],"return":"I"}],
        "header":2,"switch":10,"prefix_first":15,"prefix_second":14,"latch":19,"late_cases":[20,25]});
        std::fs::write(
            base.join("raw.json"),
            serde_json::to_vec_pretty(&raw).unwrap(),
        )
        .unwrap();
        let status = std::process::Command::new("python3")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/switch_continuation_tail_jvm.py"
            ))
            .arg(&base)
            .status()
            .unwrap();
        assert!(status.success());
    }
}

#[test]
#[ignore = "root runs serially with javac/java and RDX_SWITCH_JVM_DIR"]
fn jvm_separate_exit_frame_preserves_header_effects_and_deferred_read() {
    let c = separate_exit_fixture();
    let code = c.methods[0].code.as_ref().unwrap();
    let base = std::path::PathBuf::from(
        std::env::var_os("RDX_SWITCH_JVM_DIR").expect("RDX_SWITCH_JVM_DIR"),
    )
    .join("separate-exit");
    std::fs::create_dir_all(&base).unwrap();
    let rendered =
        native_java::render_method("sample.SwitchContinuation", &c, &c.methods[0]).unwrap();
    std::fs::write(base.join("method.java"), rendered.source).unwrap();
    let raw = serde_json::json!({"class":c.descriptor.as_ref(),"words":code.instructions,"registers":code.registers,"ins":code.ins,
        "strings":["step","valueOf","intValue","exit"],
        "method_pool":[{"owner":c.descriptor.as_ref(),"name":"step","parameters":["I"],"return":"I"},
        {"owner":"Ljava/lang/Integer;","name":"valueOf","parameters":["I"],"return":"Ljava/lang/Integer;"},
        {"owner":"Ljava/lang/Integer;","name":"intValue","parameters":[],"return":"I"}],
        "header":6,"switch":23,"prefix_first":27,"prefix_second":28,"latch":30,"late_cases":[31,36],"exit":41,"deferred_return":42});
    std::fs::write(
        base.join("raw.json"),
        serde_json::to_vec_pretty(&raw).unwrap(),
    )
    .unwrap();
    let status = std::process::Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/switch_continuation_exit_jvm.py"
        ))
        .arg(&base)
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
#[ignore = "root runs serially with javac/java and RDX_SWITCH_JVM_DIR"]
fn jvm_truncated_use_bounds_preserve_nullable_carried_reference() {
    let c = truncated_null_fixture();
    let code = c.methods[0].code.as_ref().unwrap();
    let base = std::path::PathBuf::from(
        std::env::var_os("RDX_SWITCH_JVM_DIR").expect("RDX_SWITCH_JVM_DIR"),
    )
    .join("truncated-null");
    std::fs::create_dir_all(&base).unwrap();
    let rendered =
        native_java::render_method("sample.SwitchContinuation", &c, &c.methods[0]).unwrap();
    std::fs::write(base.join("method.java"), rendered.source).unwrap();
    let pool:Vec<_> = c.symbols.methods.iter().map(|(owner,proto,name)| {
        let (ret,params)=&c.symbols.protos[*proto as usize];
        serde_json::json!({"owner":c.symbols.types[*owner as usize].as_ref(),"name":c.symbols.strings[*name as usize].as_str(),
            "parameters":params.iter().map(|p|p.as_ref()).collect::<Vec<_>>(),"return":ret.as_ref()})
    }).collect();
    let raw = serde_json::json!({"class":c.descriptor.as_ref(),"words":code.instructions,"registers":code.registers,"ins":code.ins,
        "strings":c.symbols.strings.iter().map(|s|s.as_str()).collect::<Vec<_>>(),"method_pool":pool,
        "header":34,"switch":51,"prefix_first":59,"prefix_second":60,"latch":62,"late_cases":[63,68],"exit":73,"deferred_return":74});
    std::fs::write(
        base.join("raw.json"),
        serde_json::to_vec_pretty(&raw).unwrap(),
    )
    .unwrap();
    let status = std::process::Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/switch_continuation_null_jvm.py"
        ))
        .arg(&base)
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
#[ignore = "requires pinned Airbnb RDX_TEST_APK; root runs serially"]
fn airbnb_four_exact_from_json_methods_and_raw_links_render() {
    use rdx::engine::{DecompilerEngine, NativeEngine};
    let path = std::env::var_os("RDX_TEST_APK").expect("RDX_TEST_APK");
    let mut engine = NativeEngine::start().unwrap();
    engine.open(std::path::Path::new(&path)).unwrap();
    for name in [
        "com.airbnb.android.args.fov.models.ChinaGovIdUserInputScreenJsonAdapter",
        "com.airbnb.android.args.fov.models.ConfirmLegalNameScreenJsonAdapter",
        "com.airbnb.android.args.fov.models.FormScreenJsonAdapter",
        "com.airbnb.android.base.codetoggle.mobileconfig.MobileConfigResponseJsonAdapter",
    ] {
        let out = engine.decompile_with_metadata(name).unwrap();
        assert!(
            !out.source.contains(&format!(
                ".method {name}.fromJson(Lnag0;)Ljava/lang/Object;"
            )),
            "{}",
            out.source
        );
        assert!(
            out.source.contains("while (true)") && out.source.contains("switch ("),
            "{}",
            out.source
        );
        let link = out
            .links
            .iter()
            .find(|l| {
                l.label.contains(".fromJson(Lnag0;)Ljava/lang/Object;")
                    && !l.label.starts_with(name)
            })
            .expect("retained original invoke link");
        let target = engine
            .resolve_usage_target(name, link.start, &out.source_hash)
            .unwrap();
        assert_eq!(target.id, link.label);
    }
}
