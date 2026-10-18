use rdx::{
    native_dex::{DexClass, DexCode, DexField, DexMethod, DexSymbols, DexTryRegion},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::sync::Arc;

// Structural a4.g shape: static lock/counter at PCs 0..9, Object initializer
// at PC 10, ten blank-final stores, return at 33, cleanup at PCs 34..36.
fn fixture() -> DexClass {
    let owner: Arc<str> = "Lsample/Prefix;".into();
    let descriptions = [
        ("Lock", "Ljava/lang/Object;", true),
        ("counter", "I", true),
        ("id", "I", false),
        ("name", "Ljava/lang/String;", false),
        ("f1", "F", false),
        ("f2", "F", false),
        ("f3", "F", false),
        ("f4", "F", false),
        ("peer", "Ljava/lang/Object;", false),
        ("wide", "J", false),
        ("number", "I", false),
        ("flag", "Z", false),
    ];
    let mut symbols = DexSymbols {
        types: vec![
            "Ljava/lang/Object;".into(),
            owner.clone(),
            "I".into(),
            "Ljava/lang/String;".into(),
            "F".into(),
            "J".into(),
            "Z".into(),
        ],
        strings: vec!["<init>".into()],
        protos: vec![("V".into(), vec![])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    };
    let fields = descriptions
        .into_iter()
        .map(|(name, ty, is_static)| {
            let field_type = symbols.types.iter().position(|t| t.as_ref() == ty).unwrap() as u16;
            let field_name = symbols.strings.len() as u32;
            symbols.strings.push(name.into());
            symbols.fields.push((1, field_type, field_name));
            DexField {
                declaring_type: owner.clone(),
                name: name.into(),
                field_type: ty.into(),
                access_flags: if is_static { 9 } else { 17 },
                is_static,
            }
        })
        .collect();
    let words = vec![
        0x0062, 0, 0x001d, 0x0160, 1, 0x01d8, 0x0101, 0x0167, 1, 0x001e, 0x1070, 0, 3, 0x3159, 2,
        0x345b, 3, 0x3559, 4, 0x3659, 5, 0x3759, 6, 0x3859, 7, 0x395b, 8, 0x3a5a, 9, 0x3c59, 10,
        0x3d5c, 11, 0x000e, 0x020d, 0x001e, 0x0227,
    ];
    DexClass {
        descriptor: owner.clone(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields,
        methods: vec![DexMethod {
            declaring_type: owner,
            name: "<init>".into(),
            return_type: "V".into(),
            parameters: vec![
                "Ljava/lang/String;".into(),
                "F".into(),
                "F".into(),
                "F".into(),
                "F".into(),
                "Ljava/lang/Object;".into(),
                "J".into(),
                "I".into(),
                "Z".into(),
            ],
            thrown_types: vec![],
            access_flags: 0x10001,
            code: Some(DexCode {
                registers: 14,
                ins: 11,
                outs: 1,
                tries: 1,
                instructions: words,
                offset: 0,
                try_regions: vec![DexTryRegion {
                    start: 3,
                    end: 9,
                    catches: vec![(None, 34)].into(),
                }],
            }),
        }],
        symbols: Arc::new(symbols),
    }
}
fn render(class: &DexClass) -> anyhow::Result<rdx::engine::DecompiledCode> {
    let hierarchy = Arc::new(TypeHierarchy::from_classes([class])?);
    class.symbols.hierarchy.set(hierarchy).unwrap();
    native_java::render_method("sample.Prefix", class, &class.methods[0])
}
#[test]
fn terminal_monitor_prefix_precedes_exact_initializer_and_final_writes() {
    let class = fixture();
    let code = render(&class).unwrap();
    let source = &code.source;
    let monitor = source.find("synchronized (").unwrap();
    let initializer = source.find("super();").unwrap();
    assert!(monitor < initializer, "{source}");
    assert!(source.contains("sample.Prefix.counter"), "{source}");
    for name in [
        "id", "name", "f1", "f2", "f3", "f4", "peer", "wide", "number", "flag",
    ] {
        let store = source.find(&format!("this.{name} = ")).unwrap();
        assert!(store > initializer, "{source}");
        assert_eq!(source.matches(&format!("this.{name} = ")).count(), 1);
    }
    assert!(
        code.links
            .iter()
            .any(|link| link.label == "java.lang.Object.<init>()V")
    );
}
// Replace one instruction with a fully encoded instruction, translating the
// saved protection/cleanup offsets. The base fixture has no branch operands.
fn replace(class: &mut DexClass, start: usize, width: usize, words: &[u16]) {
    let code = class.methods[0].code.as_mut().unwrap();
    let delta = words.len() as i32 - width as i32;
    code.instructions
        .splice(start..start + width, words.iter().copied());
    for region in &mut code.try_regions {
        if region.start as usize > start {
            region.start = (region.start as i32 + delta) as u32;
        }
        if region.end as usize > start {
            region.end = (region.end as i32 + delta) as u32;
        }
        let catches: Vec<_> = region
            .catches
            .iter()
            .map(|(ty, pc)| {
                (
                    ty.clone(),
                    if *pc as usize > start {
                        (*pc as i32 + delta) as u32
                    } else {
                        *pc
                    },
                )
            })
            .collect();
        region.catches = catches.into();
    }
}
fn helper_call(class: &mut DexClass, passes_this: bool) {
    let symbols = Arc::get_mut(&mut class.symbols).unwrap();
    let proto = symbols.protos.len() as u16;
    let name = symbols.strings.len() as u32;
    symbols.protos.push((
        "V".into(),
        if passes_this {
            vec!["Ljava/lang/Object;".into()]
        } else {
            vec![]
        },
    ));
    symbols.strings.push("helper".into());
    symbols.methods.push((1, proto, name));
    class.methods.push(DexMethod {
        declaring_type: class.descriptor.clone(),
        name: "helper".into(),
        return_type: "V".into(),
        parameters: if passes_this {
            vec!["Ljava/lang/Object;".into()]
        } else {
            vec![]
        },
        thrown_types: vec![],
        access_flags: 9,
        code: Some(DexCode {
            registers: u16::from(passes_this),
            ins: u16::from(passes_this),
            outs: 0,
            tries: 0,
            try_regions: vec![],
            instructions: vec![0x000e],
            offset: 0,
        }),
    });
    replace(
        class,
        5,
        2,
        &[
            if passes_this { 0x1071 } else { 0x0071 },
            1,
            if passes_this { 3 } else { 0 },
        ],
    );
}
#[test]
fn handler_identity_lock_ownership_and_alternate_entries_decline() {
    for variant in 0..7 {
        let mut class = fixture();
        if variant == 2 {
            class.methods[0].parameters[5] = "Ljava/lang/Throwable;".into();
        }
        let words = &mut class.methods[0].code.as_mut().unwrap().instructions;
        match variant {
            0 => words[34] = 0x000e,
            1 => {
                words[34] = 0x0029;
                words[35] = (-24i16) as u16;
            }
            2 => words[36] = 0x0927, // different, well-typed Throwable parameter
            3 => words[35] = 0x091e, // different reference lock parameter
            4 => words[9] = 0x091e,
            5 => {
                words[13] = 0x0029;
                words[14] = 21;
            }
            6 => {
                words[13] = 0x0029;
                words[14] = 22;
            }
            _ => unreachable!(),
        }
        assert!(render(&class).is_err(), "handler variant {variant}");
    }
}
fn captured_counter_fixture() -> DexClass {
    let mut class = fixture();
    let words = &mut class.methods[0].code.as_mut().unwrap().instructions;
    words[5] = 0x02d8; // Preserve old counter in v1; increment v2.
    words[7] = 0x0267;
    words[13..33].rotate_left(2); // Exact real order: id is the last final store.
    words[34] = 0x030d; // Terminal handler replaces incoming this with exception.
    words[36] = 0x0327;
    class
}
#[test]
fn terminal_cleanup_can_reuse_receiver_register_for_caught_exception() {
    let class = captured_counter_fixture();
    let code = render(&class).unwrap();
    assert!(code.source.contains("synchronized ("), "{}", code.source);
    assert!(code.source.find("super();").unwrap() < code.source.find("this.id = ").unwrap());
}

#[test]
fn prefix_never_reads_overwrites_or_aliases_incoming_this() {
    for variant in 0..11 {
        let mut class = fixture();
        match variant {
            0 => replace(&mut class, 5, 2, &[0x0208, 3]), // reference alias of this
            1 => replace(&mut class, 5, 2, &[0x0369, 0]), // static escape of this
            2 => replace(&mut class, 5, 2, &[0x3159, 2]), // instance store through this
            3 => replace(&mut class, 5, 2, &[0x3152, 2]), // instance read through this
            4 => class.methods[0].code.as_mut().unwrap().instructions[0] = 0x0362,
            5 => {
                class.methods[0].parameters[5] = "Ljava/lang/Throwable;".into();
                let words = &mut class.methods[0].code.as_mut().unwrap().instructions;
                words[34] = 0x030d;
                words[36] = 0x0927; // well-typed parameter differs from caught exception
            }
            6 => helper_call(&mut class, true), // static call passes this
            7 => {
                class.methods[0].parameters[5] = "[Ljava/lang/Object;".into();
                replace(&mut class, 5, 2, &[0x034d, 0x0109]); // aput-object this, parameter-array, counter
            }
            8 => replace(&mut class, 5, 2, &[0x000e]), // terminal normal path before initialization
            9 => replace(&mut class, 5, 2, &[0xa204]), // wide write v2/v3 overlaps incoming this
            10 => replace(&mut class, 5, 2, &[0x2104]), // wide read v2/v3 overlaps incoming this
            _ => unreachable!(),
        }
        assert!(render(&class).is_err(), "receiver variant {variant}");
    }
}
#[test]
fn prefix_protection_control_flow_allocation_and_budget_decline() {
    for variant in 0..10 {
        let mut class = fixture();
        match variant {
            0 => class.methods[0].code.as_mut().unwrap().try_regions[0].end = 13,
            1 => class.methods[0].code.as_mut().unwrap().try_regions[0].start = 2,
            2 => class.methods[0].code.as_mut().unwrap().try_regions[0].start = 5,
            3 => {
                let code = class.methods[0].code.as_mut().unwrap();
                code.tries = 2;
                code.try_regions.push(DexTryRegion {
                    start: 3,
                    end: 9,
                    catches: vec![(None, 34)].into(),
                });
            }
            4 => replace(&mut class, 5, 2, &[0x0029, 5]),
            5 => replace(&mut class, 5, 2, &[0x0029, 0]),
            6 => replace(&mut class, 5, 2, &[0x0222, 0]), // well-encoded unused allocation
            7 => helper_call(&mut class, false),
            8 => class.methods[0].code.as_mut().unwrap().registers = 257,
            9 => class.methods[0]
                .code
                .as_mut()
                .unwrap()
                .instructions
                .extend(std::iter::repeat_n(0, 4096)),
            _ => unreachable!(),
        }
        assert!(render(&class).is_err(), "shape variant {variant}");
    }
}
#[test]
fn terminal_prefix_preserves_later_final_assignment_and_exact_owner_guards() {
    for variant in 0..4 {
        let mut class = fixture();
        match variant {
            0 => class.methods[0].code.as_mut().unwrap().instructions[31] = 0x3d5c,
            1 => class.methods[0].code.as_mut().unwrap().instructions[32] = 10, // wrong final field type
            2 => class.superclass = Some("Ljava/lang/RuntimeException;".into()),
            3 => class.fields[2].is_static = true,
            _ => unreachable!(),
        }
        if variant == 0 {
            // Repeat number assignment instead of assigning flag; successful
            // return must still prove each blank-final exactly once.
            class.methods[0].code.as_mut().unwrap().instructions[29] = 0x3c59;
            class.methods[0].code.as_mut().unwrap().instructions[30] = 2;
        }
        assert!(render(&class).is_err(), "suffix variant {variant}");
    }
}

#[test]
fn prefix_static_declarations_require_exact_access_type_and_writability() {
    // Instruction boundaries and field references remain complete. These are
    // declaration/opcode authority controls, rather than truncated-word tests.
    for variant in 0..11 {
        let mut class = fixture();
        match variant {
            0 => class.fields[1].access_flags |= 0x10, // final static write
            1 => {
                class.fields[1].is_static = false;
                class.fields[1].access_flags &= !8;
            }
            2 => class.fields[1].is_static = false, // static metadata mismatch
            3 => class.fields[1].access_flags &= !8,
            4 => class.fields[1].field_type = "J".into(),
            5 => Arc::get_mut(&mut class.symbols).unwrap().fields[1].0 = 0, // external owner
            6 => {
                class.fields.remove(1);
            }
            7 => class.fields.push(DexField {
                declaring_type: class.descriptor.clone(),
                name: "counter".into(),
                field_type: "I".into(),
                access_flags: 9,
                is_static: true,
            }),
            8 => class.fields[1].access_flags = 11, // multiple visibility bits
            9 => class.fields[0].field_type = "Ljava/lang/String;".into(),
            10 => class.methods[0].code.as_mut().unwrap().instructions[3] = 0x0161,
            _ => unreachable!(),
        }
        assert!(render(&class).is_err(), "static metadata variant {variant}");
    }
}
#[test]
fn own_static_access_accepts_each_valid_visibility_and_final_lock_read() {
    for visibility in [0, 1, 2, 4] {
        let mut class = fixture();
        class.fields[0].access_flags = 8 | 0x10 | visibility;
        class.fields[1].access_flags = 8 | visibility;
        let code = render(&class).unwrap();
        assert!(code.source.contains("synchronized ("), "{}", code.source);
    }
}

#[test]
#[ignore = "requires RDX_JAVA25_HOME; root runs exact emitted source serially"]
fn terminal_monitor_prefix_jvm_state_and_failure_release() {
    let normal = fixture();
    let normal_source = render(&normal).unwrap().source;
    let mut captured = captured_counter_fixture();
    captured.descriptor = "Lsample/CapturedPrefix;".into();
    captured.methods[0].declaring_type = captured.descriptor.clone();
    for field in &mut captured.fields {
        field.declaring_type = captured.descriptor.clone();
    }
    Arc::get_mut(&mut captured.symbols).unwrap().types[1] = captured.descriptor.clone();
    let hierarchy = Arc::new(TypeHierarchy::from_classes([&captured]).unwrap());
    captured.symbols.hierarchy.set(hierarchy).unwrap();
    let captured_source =
        native_java::render_method("sample.CapturedPrefix", &captured, &captured.methods[0])
            .unwrap()
            .source;
    let mut fault = fixture();
    replace(&mut fault, 5, 2, &[0xc1b3]); // div-int/2addr counter, p7
    let code = fault.methods[0].code.as_mut().unwrap();
    let handler = code.instructions.len() - 3;
    code.instructions[handler] = 0x030d;
    code.instructions[handler + 2] = 0x0327;
    fault.descriptor = "Lsample/FaultPrefix;".into();
    fault.methods[0].declaring_type = fault.descriptor.clone();
    for field in &mut fault.fields {
        field.declaring_type = fault.descriptor.clone();
    }
    Arc::get_mut(&mut fault.symbols).unwrap().types[1] = fault.descriptor.clone();
    let hierarchy = Arc::new(TypeHierarchy::from_classes([&fault]).unwrap());
    fault.symbols.hierarchy.set(hierarchy).unwrap();
    let fault_source = native_java::render_method("sample.FaultPrefix", &fault, &fault.methods[0])
        .unwrap()
        .source;
    let directory = std::env::temp_dir().join(format!("rdx-monitor-prefix-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("normal.java.txt"), normal_source).unwrap();
    std::fs::write(directory.join("captured.java.txt"), captured_source).unwrap();
    std::fs::write(directory.join("fault.java.txt"), fault_source).unwrap();
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/constructor_monitor_prefix_jvm.py");
    let output = std::process::Command::new("python3")
        .arg(script)
        .arg(&directory)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    println!("{}", String::from_utf8_lossy(&output.stdout));
    println!("evidence {}", directory.display());
}
