pub use rdx::{
    native_calls, native_cfg, native_dex, native_ir, native_method, native_ssa, native_types,
};
#[path = "../src/native_java/constructor_switch_prefix.rs"]
mod prefix;

use native_dex::{DexClass, DexCode, DexField, DexMethod, DexSymbols};
use rdx::native_java;
use std::sync::Arc;

fn fixture(sparse: bool, prefixes: [Vec<u16>; 2], locals: u16) -> DexClass {
    let this = locals;
    let object = locals + 1;
    let value = locals + 2;
    let selector = locals + 3;
    let tail = vec![
        0x005b | object << 8 | this << 12,
        1,
        0x0059 | value << 8 | this << 12,
        2,
        0x000e,
    ];
    let mut words = vec![
        0x0059 | selector << 8 | this << 12,
        0,
        if sparse { 0x002c } else { 0x002b } | selector << 8,
        0,
        0,
    ];
    let mut starts = Vec::new();
    for before in prefixes {
        starts.push(words.len());
        words.extend(before);
        words.extend([0x1070, 0, this]);
        words.extend(&tail);
    }
    if words.len() % 2 != 0 {
        words.push(0);
    }
    let payload = words.len();
    words[3] = (payload - 2) as u16;
    let target = (starts[1] - 2) as u16;
    if sparse {
        words.extend([0x0200, 2, 0xfff9, 0xffff, 17, 0, target, 0, target, 0]);
    } else {
        words.extend([0x0100, 2, 1, 0, target, 0, target, 0]);
    }
    let class = DexClass {
        descriptor: "Lsample/PrefixChild;".into(),
        superclass: Some("Lsample/Base;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: [("a", "I"), ("b", "Ljava/lang/Object;"), ("c", "I")]
            .into_iter()
            .map(|(name, ty)| DexField {
                declaring_type: "Lsample/PrefixChild;".into(),
                name: name.into(),
                field_type: ty.into(),
                access_flags: 0x11,
                is_static: false,
            })
            .collect(),
        symbols: Arc::new(DexSymbols {
            types: vec![
                "Lsample/Base;".into(),
                "Lsample/Validation;".into(),
                "Lsample/PrefixChild;".into(),
                "I".into(),
                "Ljava/lang/Object;".into(),
                "Ljava/lang/String;".into(),
            ],
            strings: vec![
                "<init>".into(),
                "check".into(),
                "a".into(),
                "b".into(),
                "value".into(),
                "c".into(),
                "replacement".into(),
                "touch".into(),
            ],
            protos: vec![
                ("V".into(), vec![]),
                (
                    "V".into(),
                    vec!["Ljava/lang/Object;".into(), "Ljava/lang/String;".into()],
                ),
                ("Ljava/lang/Object;".into(), vec![]),
                ("V".into(), vec!["I".into()]),
            ],
            methods: vec![(0, 0, 0), (1, 1, 1), (1, 2, 6), (1, 3, 7), (1, 0, 0)],
            fields: vec![(2, 3, 2), (2, 4, 3), (2, 3, 5)],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: "Lsample/PrefixChild;".into(),
            name: "<init>".into(),
            return_type: "V".into(),
            parameters: vec!["Ljava/lang/Object;".into(), "I".into(), "I".into()],
            thrown_types: vec![],
            access_flags: 0x10001,
            code: Some(DexCode {
                registers: 4 + locals,
                ins: 4,
                outs: 2,
                tries: 0,
                try_regions: vec![],
                instructions: words,
                offset: 0,
            }),
        }],
    };
    let hierarchy = rdx::native_hierarchy::TypeHierarchy::from_classes([&class]).unwrap();
    class.symbols.hierarchy.set(Arc::new(hierarchy)).unwrap();
    class
}

fn validation() -> Vec<u16> {
    vec![0x031a, 4, 0x2071, 1, 0x0031]
}

fn analyzed(class: &DexClass) {
    // Negative cases must be decoded and typed; malformed offsets cannot mask
    // a missing lifetime/type-domain guard.
    native_method::MethodAnalysis::build(class, &class.methods[0])
        .unwrap()
        .infer_types()
        .unwrap();
}

#[test]
fn plan_keeps_exact_ranges_and_only_live_suffix_words_for_both_payload_formats() {
    for sparse in [false, true] {
        let class = fixture(sparse, [vec![], validation()], 0);
        analyzed(&class);
        let plan = prefix::select(&class, &class.methods[0], 2).unwrap();
        assert_eq!(
            (
                plan.switch_pc,
                plan.selector_register,
                plan.receiver_register
            ),
            (2, 3, 0)
        );
        assert_eq!(plan.needed_tail_words, vec![0, 1, 2]);
        assert_eq!(plan.arms.len(), 2); // Two keys share a single case prefix.
        assert_eq!(
            (
                plan.arms[0].start,
                plan.arms[0].delegation_start,
                plan.arms[0].delegation_end,
                plan.arms[0].post_start,
                plan.arms[0].end
            ),
            (5, 5, 8, 8, 13)
        );
        assert_eq!(
            (
                plan.arms[1].start,
                plan.arms[1].delegation_start,
                plan.arms[1].delegation_end,
                plan.arms[1].post_start,
                plan.arms[1].end
            ),
            (13, 18, 21, 21, 26)
        );
        assert_eq!(plan.arms[1].prefix_written_words, vec![3]);
        assert_eq!(
            plan.cases,
            if sparse {
                vec![(-7, 13), (17, 13)]
            } else {
                vec![(1, 13), (2, 13)]
            }
        );
    }
}

#[test]
fn receiver_use_allocation_undefined_and_unequal_liveout_are_rejected_on_valid_ssa() {
    let cases = [
        // This receiver cannot be passed to an independent validation prefix.
        fixture(false, [vec![], vec![0x031a, 4, 0x2071, 1, 0x0030]], 0),
        // This physical word cannot be overwritten and then silently restored.
        fixture(false, [vec![], vec![0x0012]], 0),
        // Explicit allocation is outside this first proof tranche.
        fixture(false, [vec![], vec![0x0322, 1, 0x1070, 4, 3]], 0),
        // Local v0 is a real Undefined SSA root, while this is v1.
        fixture(false, [vec![], vec![0x1071, 3, 0]], 1),
        // Both suffixes are identical, but default v1 is a call result and
        // case v1 is the original parameter (the fc.o shape).
        fixture(false, [vec![0x0071, 2, 0, 0x010c], validation()], 0),
    ];
    for class in cases {
        analyzed(&class);
        assert!(prefix::select(&class, &class.methods[0], 2).is_none());
    }
}

#[test]
fn unequal_suffix_this_delegation_argument_delegation_and_extra_entries_are_rejected() {
    let mut unequal = fixture(false, [vec![], validation()], 0);
    unequal.methods[0].code.as_mut().unwrap().instructions[24] = 0; // c becomes a.
    analyzed(&unequal);
    assert!(prefix::select(&unequal, &unequal.methods[0], 2).is_none());

    let mut recursive = fixture(false, [vec![], validation()], 0);
    Arc::get_mut(&mut recursive.symbols).unwrap().methods[0].0 = 2; // this(), not parent.
    analyzed(&recursive);
    assert!(prefix::select(&recursive, &recursive.methods[0], 2).is_none());

    let mut args = fixture(false, [vec![], validation()], 0);
    Arc::get_mut(&mut args.symbols).unwrap().protos[0].1 = vec!["Ljava/lang/Object;".into()];
    for pc in [5, 18] {
        args.methods[0].code.as_mut().unwrap().instructions[pc..pc + 3]
            .copy_from_slice(&[0x2070, 0, 0x0010]);
    }
    analyzed(&args);
    assert!(prefix::select(&args, &args.methods[0], 2).is_none());

    let mut extra = fixture(false, [vec![], validation()], 0);
    // Replace the two-word prelude with an if-eqz to the case delegation.
    // Both targets are valid, and a separate edge bypasses its validation.
    extra.methods[0].code.as_mut().unwrap().instructions[..2].copy_from_slice(&[0x0338, 18]);
    analyzed(&extra);
    assert!(prefix::select(&extra, &extra.methods[0], 2).is_none());
}

#[test]
fn rendering_runs_validation_before_one_parent_and_one_suffix_and_keeps_source_links() {
    for sparse in [false, true] {
        let class = fixture(sparse, [vec![], validation()], 0);
        let rendered =
            native_java::render_method("sample.PrefixChild", &class, &class.methods[0]).unwrap();
        assert_eq!(
            rendered.source.matches("super();").count(),
            1,
            "{}",
            rendered.source
        );
        assert_eq!(rendered.source.matches("this.b =").count(), 1);
        assert_eq!(rendered.source.matches("this.c =").count(), 1);
        let validation = rendered.source.find("Validation.check(").unwrap();
        let delegation = rendered.source.find("super();").unwrap();
        assert!(rendered.source.find("switch (").unwrap() < validation);
        assert!(validation < delegation && delegation < rendered.source.find("this.b =").unwrap());
        assert!(rendered.source.find("this.a = p2;").unwrap() < validation);
        for label in [
            "sample.Base.<init>()V",
            "sample.Validation.check(Ljava/lang/Object;Ljava/lang/String;)V",
        ] {
            assert!(
                rendered.links.iter().any(|link| link.label == label),
                "missing {label}"
            );
        }
    }
}

/// Independent raw-word event evaluator: prefix dispatch and delegation order
/// are read from the DEX, not from the helper plan or emitted source.
fn dex_events(class: &DexClass, selector: i32, object: bool, fail: u8) -> (String, bool) {
    let words = &class.methods[0].code.as_ref().unwrap().instructions;
    let mut trace = format!("F{selector}");
    let mut pc = 0;
    let read_i32 = |at: usize| (u32::from(words[at]) | u32::from(words[at + 1]) << 16) as i32;
    loop {
        match words[pc] as u8 {
            0x59 => {
                if pc != 0 {
                    trace.push('C');
                }
                pc += 2;
            }
            0x5b => {
                trace.push('O');
                pc += 2;
            }
            0x2b | 0x2c => {
                let payload = (pc as i32 + read_i32(pc + 1)) as usize;
                let count = usize::from(words[payload + 1]);
                let packed = words[pc] as u8 == 0x2b;
                let mut target = pc + 3;
                for n in 0..count {
                    let key = if packed {
                        read_i32(payload + 2) + n as i32
                    } else {
                        read_i32(payload + 2 + n * 2)
                    };
                    if key == selector {
                        let offset = if packed {
                            payload + 4 + n * 2
                        } else {
                            payload + 2 + count * 2 + n * 2
                        };
                        target = (pc as i32 + read_i32(offset)) as usize;
                    }
                }
                pc = target;
            }
            0x1a => pc += 2,
            0x71 => {
                trace.push('V');
                if !object || fail == 1 {
                    return (trace, true);
                }
                pc += 3;
            }
            0x70 => {
                trace.push_str(&format!("B{selector}"));
                if fail == 2 {
                    return (trace, true);
                }
                pc += 3;
            }
            0x0e => return (trace, false),
            op => panic!("unsupported evaluator opcode {op:x} at {pc}"),
        }
    }
}

#[test]
#[ignore = "requires Java25 flexible constructor bodies via RDX_JAVA25_HOME"]
fn jvm_matches_raw_dex_dispatch_field_callback_suffix_and_throwing_prefix() {
    use std::{fs, process::Command};
    let home = std::env::var("RDX_JAVA25_HOME").expect("RDX_JAVA25_HOME");
    for sparse in [false, true] {
        let class = fixture(sparse, [vec![], validation()], 0);
        let rendered =
            native_java::render_method("sample.PrefixChild", &class, &class.methods[0]).unwrap();
        let mut checks = String::new();
        for selector in [i32::MIN, -7, -1, 0, 1, 2, 17, i32::MAX] {
            for object in [false, true] {
                for fail in 0..=2 {
                    let (trace, throws) = dex_events(&class, selector, object, fail);
                    checks.push_str(&format!(
                        "verify({selector},{object},{fail},\"{trace}\",{throws});\n"
                    ));
                }
            }
        }
        let dir = std::env::temp_dir().join(format!(
            "rdx-constructor-prefix-{}-{sparse}",
            std::process::id()
        ));
        fs::create_dir_all(dir.join("sample")).unwrap();
        let source = r#"
package sample;
class Validation {
    static String trace; static int fail;
    static final RuntimeException marker = new RuntimeException();
    static void check(Object value,String label) { trace += "V"; if(value==null||fail==1)throw marker; }
}
class Base {
    Base() { Validation.trace += "B"+((PrefixChild)this).a; if(Validation.fail==2)throw Validation.marker; }
}
public class PrefixChild extends Base {
    public final int a; public final Object b; public final int c;
METHOD
    static void verify(int selector,boolean present,int fail,String expected,boolean throwsExpected) {
        Object input=present?new Object():null; Validation.trace="F"+selector; Validation.fail=fail;
        try {
            PrefixChild value=new PrefixChild(input,-923,selector);
            if(throwsExpected||value.a!=selector||value.b!=input||value.c!=-923)throw new AssertionError();
            Validation.trace+="OC";
        } catch(RuntimeException actual) { if(!throwsExpected||actual!=Validation.marker)throw new AssertionError(); }
        if(!Validation.trace.equals(expected))throw new AssertionError(Validation.trace+" != "+expected);
    }
    public static void main(String[] args) { CHECKS }
}
"#;
        fs::write(
            dir.join("sample/PrefixChild.java"),
            source
                .replace("METHOD", &rendered.source)
                .replace("CHECKS", &checks),
        )
        .unwrap();
        for (program, args) in [
            ("javac", vec!["sample/PrefixChild.java"]),
            ("java", vec!["-cp", ".", "sample.PrefixChild"]),
        ] {
            let output = Command::new(format!("{home}/bin/{program}"))
                .args(args)
                .current_dir(&dir)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        fs::remove_dir_all(dir).unwrap();
    }
}

#[test]
fn identical_relative_fill_payloads_need_explicit_exclusion() {
    let mut class = fixture(false, [vec![], validation()], 0);
    class.methods[0].parameters[0] = "[I".into();
    let mut words = vec![
        0x0359, 0, 0x032b, 30, 0, 0x1070, 0, 0, 0x0126, 32, 0, 0x015b, 1, 0x0259, 2, 0x000e,
        0x031a, 4, 0x2071, 1, 0x0031, 0x1070, 0, 0, 0x0126, 32, 0, 0x015b, 1, 0x0259, 2, 0x000e,
        0x0100, 2, 1, 0, 14, 0, 14, 0, 0x0300, 4, 1, 0, 7, 0,
    ];
    words.resize(56, 0);
    words.extend([0x0300, 4, 1, 0, 9, 0]);
    class.methods[0].code.as_mut().unwrap().instructions = words;
    analyzed(&class);
    assert!(prefix::select(&class, &class.methods[0], 2).is_none());
}
#[test]
fn unresolved_receiver_compatibility_is_rejected_without_guessing() {
    let mut class = fixture(false, [vec![], validation()], 0);
    Arc::get_mut(&mut class.symbols).unwrap().hierarchy.take();
    analyzed(&class);
    let a = native_method::MethodAnalysis::build(&class, &class.methods[0]).unwrap();
    let types = a.infer_types().unwrap();
    assert!(matches!(
        types.values[0].resolution,
        native_types::TypeResolution::Unresolved(_)
    ));
    assert!(prefix::select(&class, &class.methods[0], 2).is_none());
}
