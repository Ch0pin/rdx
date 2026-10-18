pub use rdx::{native_cfg, native_dominators, native_ir, native_ssa};
#[allow(dead_code)]
#[path = "../src/native_java/forward_iteration_entry.rs"]
mod forward_iteration_entry;
#[path = "../src/native_java/parent_continue.rs"]
mod plan;
use rdx::{
    native_dex::{self, DexClass, DexSymbols},
    native_java,
};
use std::{fs, process::Command, sync::Arc};
fn sole_plan(code: &native_dex::DexCode) -> Option<plan::Plan> {
    let plans = plan::select_all(code)?;
    (plans.len() == 1).then(|| plans[0].clone())
}
fn fixture() -> DexClass {
    variant(0)
}
fn variant(kind: usize) -> DexClass {
    let wide = kind == 3 || kind == 4;
    let mut c = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    c.descriptor = "Lsample/Entry;".into();
    c.methods.retain(|m| m.name.as_ref() == "answer");
    c.symbols = Arc::new(DexSymbols {
        types: vec![
            "Lsample/Effects;".into(),
            "[I".into(),
            "Lsample/Box;".into(),
        ],
        strings: vec!["parent".into(), "child".into(), "<init>".into()],
        protos: vec![("V".into(), vec!["I".into()]), ("V".into(), vec![])],
        methods: vec![(0, 0, 0), (0, 0, 1), (2, 1, 2)],
        ..Default::default()
    });
    let m = &mut c.methods[0];
    m.name = "run".into();
    m.access_flags = 9;
    m.return_type = if wide { "J" } else { "I" }.into();
    m.parameters = vec!["I".into(), "I".into(), if wide { "J" } else { "I" }.into()];
    m.declaring_type = c.descriptor.clone();
    let code = m.code.as_mut().unwrap();
    code.registers = if wide { 11 } else { 7 };
    code.ins = if wide { 4 } else { 3 };
    code.outs = 1;
    let mut w = vec![
        0x0012,
        if wide { 0x9204 } else { 0x6201 },
        if wide { 0x7423 } else { 0x4323 },
        1,
    ];
    let parent = w.len();
    w.extend([0x1071, 0, 0]);
    let parent_guard = w.len();
    w.extend([
        if wide { 0x7035 } else { 0x4035 },
        0,
        0x00d8,
        0x0100,
        0x0112,
    ]);
    let child = w.len();
    w.extend([
        if wide { 0x8135 } else { 0x5135 },
        (parent as isize - child as isize) as i16 as u16,
    ]);
    w.extend([
        if wide { 0x0422 } else { 0x0322 },
        2,
        0x1070,
        2,
        if wide { 4 } else { 3 },
        0x1071,
        1,
        1,
    ]);
    if kind == 1 {
        w.extend([0x2301, 0x6201, 0x3601]);
    }
    if kind == 4 {
        w.extend([0x2504, 0x9204, 0x5904]);
    }
    if kind == 2 {
        w.extend([0x04d8, 0xff04, 0x00d8, 0x0100, 0x05d8, 0xff05]);
    }
    if kind == 5 {
        w.extend([0x0639, 3, 0x020f]);
    }
    w.extend(if wide {
        vec![0x029b, 0x0902]
    } else {
        vec![0x0290, 0x0002]
    });
    w.extend([0x01d8, 0x0101]);
    let latch = w.len();
    w.push(((child as isize - latch as isize) as i8 as u8 as u16) << 8 | 0x28);
    let exit = w.len();
    w[parent_guard + 1] = (exit - parent_guard) as u16;
    w.push(if wide { 0x0210 } else { 0x020f });
    code.instructions = w;
    c
}
fn sequential_fixture(kind: usize) -> DexClass {
    let mut class = variant(kind);
    let code = class.methods[0].code.as_mut().unwrap();
    let original = code.instructions.clone();
    let mut words = original[..original.len() - 1].to_vec();
    words.extend(if kind == 3 || kind == 4 {
        vec![0x2904, 0x07d8, 0xff07, 0x08d8, 0x0108, 0x1071, 0, 7]
    } else {
        vec![0x2601, 0x04d8, 0xff04, 0x05d8, 0x0105, 0x1071, 0, 2]
    });
    words.extend(original);
    code.instructions = words;
    class
}
fn independent_loop_fixture(kind: usize) -> DexClass {
    let mut class = fixture();
    let code = class.methods[0].code.as_mut().unwrap();
    let mut original = code.instructions.clone();
    original.pop();
    let ordinary = [0x0312, 0x4335, 8, 0x1071, 0, 3, 0x03d8, 0x0103, 0xf928];
    let mut words = Vec::new();
    if kind != 1 {
        words.extend(ordinary);
    }
    words.extend(original);
    if kind != 0 {
        words.extend(ordinary);
    }
    words.push(0x020f);
    code.instructions = words;
    class
}
fn sibling_loop_fixture(count: usize) -> DexClass {
    let mut class = fixture();
    let code = class.methods[0].code.as_mut().unwrap();
    for _ in 0..count {
        let plan = sole_plan(code).unwrap();
        // Insert before the first sibling's initialization. Inserting at the
        // child's original header would relocate the previous sibling exit
        // past the new code and leave that new sibling unreachable.
        let at = plan.siblings.first().map_or(plan.child, |s| s.header - 1);
        insert_words(
            code,
            at,
            &[
                0x0312, // sibling counter = 0
                0x5335, 10, // header: counter >= child limit -> sibling exit
                0x1071, 1, 3, // effect(counter)
                0x0290, 0x0302, // result += counter
                0x03d8, 0x0103, // counter++
                0xf728, // continue sibling
            ],
        );
    }
    class
}
fn success_tail_fixture(siblings: usize) -> DexClass {
    let mut class = sibling_loop_fixture(siblings);
    let code = class.methods[0].code.as_mut().unwrap();
    let p = sole_plan(code).unwrap();
    let last = p.child_latch + 7;
    code.instructions.splice(
        p.child_latch..p.child_latch + 1,
        [
            0x5134,
            (p.child as isize - p.child_latch as isize) as i16 as u16,
            0x1071,
            0,
            1, // successful child completion observes its actual counter
            0x0290,
            0x0102, // result += child counter
            (((p.parent as isize - last as isize) as i8 as u8 as u16) << 8) | 0x28,
        ],
    );
    code.instructions[p.parent_guard + 1] += 7;
    class
}

#[test]
fn conditional_child_feedback_preserves_distinct_guard_and_success_frames() {
    for siblings in 0..=2 {
        let class = success_tail_fixture(siblings);
        let code = class.methods[0].code.as_ref().unwrap();
        let p = sole_plan(code).unwrap();
        assert_eq!(p.success_tail.len(), 3);
        assert_eq!(p.siblings.len(), siblings);
        assert!(!p.parent_success_phi_inputs.is_empty());
        assert!(plan::validate(code, &p));
        native_java::render_method("sample.Entry", &class, &class.methods[0]).unwrap();
    }
}

#[test]
fn success_tail_cross_entry_reentry_and_terminal_ownership_decline() {
    for shape in 0..3 {
        let mut class = success_tail_fixture(0);
        let code = class.methods[0].code.as_mut().unwrap();
        let p = sole_plan(code).unwrap();
        let last = *p.success_tail.last().unwrap();
        match shape {
            0 => code.instructions[p.child_guard + 1] = (p.success_tail[0] - p.child_guard) as u16,
            1 => {
                code.instructions[last] =
                    (((p.parent_guard as isize - last as isize) as i8 as u8 as u16) << 8) | 0x28
            }
            _ => code.instructions[last] = 0x020f,
        }
        assert!(plan::select_all(code).is_none(), "shape {shape}");
    }
}

#[test]
fn preceding_siblings_keep_independent_canonical_ownership() {
    for count in 1..=2 {
        let class = sibling_loop_fixture(count);
        let code = class.methods[0].code.as_ref().unwrap();
        let plan = sole_plan(code).unwrap();
        assert_eq!(plan.siblings.len(), count);
        assert!(plan::validate(code, &plan));
        for sibling in &plan.siblings {
            assert!(sibling.header < sibling.latch && sibling.exit <= plan.child);
            assert!(
                sibling
                    .nodes
                    .iter()
                    .all(|pc| !plan.child_nodes.contains(pc))
            );
        }
        native_java::render_method("sample.Entry", &class, &class.methods[0]).unwrap();
    }
}

#[test]
fn sibling_cross_entry_parent_escape_and_noncanonical_feedback_decline() {
    for shape in 0..3 {
        let mut class = sibling_loop_fixture(2);
        let code = class.methods[0].code.as_mut().unwrap();
        let p = sole_plan(code).unwrap();
        let sibling = &p.siblings[0];
        match shape {
            0 => {
                code.instructions[p.parent_guard + 1] = (sibling.header + 2 - p.parent_guard) as u16
            }
            1 => {
                code.instructions[sibling.header + 1] =
                    (p.parent as isize - sibling.header as isize) as i16 as u16
            }
            _ => {
                code.instructions[sibling.latch] =
                    ((((p.child + 2) as isize - sibling.latch as isize) as i8 as u8 as u16) << 8)
                        | 0x28
            }
        }
        assert!(plan::select_all(code).is_none(), "shape {shape}");
    }
}
fn insert_nops(code: &mut native_dex::DexCode, at: usize, count: usize) {
    let decoded = native_ir::DecodedMethod::decode(code).unwrap();
    let mut words = code.instructions[..at].to_vec();
    words.extend(std::iter::repeat_n(0, count));
    words.extend_from_slice(&code.instructions[at..]);
    for instruction in decoded.instructions {
        let Some(target) = instruction.branch_target else {
            continue;
        };
        let from = instruction.pc + usize::from(instruction.pc >= at) * count;
        let to = target + usize::from(target >= at) * count;
        let delta = to as isize - from as isize;
        match instruction.opcode {
            0x28 => {
                assert!((-128..=127).contains(&delta));
                words[from] = ((delta as i8 as u8 as u16) << 8) | 0x28;
            }
            0x32..=0x3d => words[from + 1] = delta as i16 as u16,
            opcode => panic!("unexpected branch {opcode}"),
        }
    }
    code.instructions = words;
}
fn insert_words(code: &mut native_dex::DexCode, at: usize, extra: &[u16]) {
    let decoded = native_ir::DecodedMethod::decode(code).unwrap();
    let mut words = code.instructions[..at].to_vec();
    words.extend_from_slice(extra);
    words.extend_from_slice(&code.instructions[at..]);
    for instruction in decoded.instructions {
        let Some(target) = instruction.branch_target else {
            continue;
        };
        let from = instruction.pc + usize::from(instruction.pc >= at) * extra.len();
        let to = target + usize::from(target >= at) * extra.len();
        let delta = to as isize - from as isize;
        match instruction.opcode {
            0x28 => {
                assert!((-128..=127).contains(&delta));
                words[from] = ((delta as i8 as u8 as u16) << 8) | 0x28;
            }
            0x32..=0x3d => words[from + 1] = delta as i16 as u16,
            opcode => panic!("unexpected branch {opcode}"),
        }
    }
    code.instructions = words;
}
fn two_feedback_fixture(mode: usize) -> DexClass {
    let mut class = header_fixture(mode);
    let code = class.methods[0].code.as_mut().unwrap();
    let old = sole_plan(code).unwrap();
    let mut extra = vec![
        0x1033,
        (old.child as isize - old.child_latch as isize) as i16 as u16,
        0x1071,
        0,
        0,
    ];
    extra.extend(if mode == 3 {
        vec![0x029b, 0x0902]
    } else {
        vec![0x02d8, 0x0702]
    });
    insert_words(code, old.child_latch, &extra);
    class
}
fn reference_feedback_fixture() -> DexClass {
    let mut class = two_feedback_fixture(0);
    class.methods[0].parameters[2] = "Ljava/lang/Object;".into();
    class.methods[0].return_type = "Ljava/lang/Object;".into();
    let code = class.methods[0].code.as_mut().unwrap();
    code.instructions[1] = 0x6207;
    let body = code
        .instructions
        .windows(2)
        .position(|words| words == [0x0290, 0x0002])
        .unwrap();
    code.instructions[body] = 0x6207;
    code.instructions[body + 1] = 0;
    let fallthrough = code
        .instructions
        .windows(2)
        .position(|words| words == [0x02d8, 0x0702])
        .unwrap();
    code.instructions[fallthrough] = 0x3207;
    code.instructions[fallthrough + 1] = 0;
    *code.instructions.last_mut().unwrap() = 0x0211;
    class
}
#[test]
fn both_feedback_edges_keep_actual_phi_inputs_successors_and_fault_order() {
    for mode in 0..6 {
        let class = if mode < 5 {
            two_feedback_fixture(mode)
        } else {
            reference_feedback_fixture()
        };
        let code = class.methods[0].code.as_ref().unwrap();
        let plan = sole_plan(code).unwrap();
        assert_eq!(plan.child_feedback.len(), 2);
        assert_eq!(plan.child_feedback[0].successors.len(), 2);
        assert!(plan.child_feedback[0].successors.contains(&plan.child));
        assert!(
            plan.child_feedback[0]
                .successors
                .contains(&(plan.child_feedback[0].from + 2))
        );
        assert_eq!(plan.child_feedback[1].successors, vec![plan.child]);
        assert!(plan::validate(code, &plan));
        native_java::render_method("sample.Entry", &class, &class.methods[0]).unwrap();
        for poison in 0..4 {
            let mut bad = plan.clone();
            match poison {
                0 => bad.child_feedback[0].successors.reverse(),
                1 => bad.child_feedback[0].block += 1,
                2 => bad.child_feedback[0].phi_inputs.clear(),
                3 => bad.child_feedback.swap(0, 1),
                _ => unreachable!(),
            }
            assert!(
                !plan::validate(code, &bad),
                "metadata mode{mode} poison{poison}"
            );
        }
    }
}
#[test]
fn narrow_overwrite_cannot_carry_stale_wide_second_word() {
    let original = two_feedback_fixture(3);
    let positions: Vec<_> = original.methods[0]
        .code
        .as_ref()
        .unwrap()
        .instructions
        .windows(2)
        .enumerate()
        .filter(|(_, words)| *words == [0x029b, 0x0902])
        .map(|(pc, _)| pc)
        .collect();
    assert_eq!(positions.len(), 2);
    for pc in positions {
        let mut class = two_feedback_fixture(3);
        let code = class.methods[0].code.as_mut().unwrap();
        code.instructions[pc] = 0x0201;
        code.instructions[pc + 1] = 0;
        assert!(plan::select_all(code).is_none());
        assert!(native_java::render_method("sample.Entry", &class, &class.methods[0]).is_err());
    }
}

#[test]
fn third_feedback_and_cross_entry_protected_undefined_inputs_reject() {
    for mode in 0..4 {
        let mut class = two_feedback_fixture(0);
        let code = class.methods[0].code.as_mut().unwrap();
        let old = sole_plan(code).unwrap();
        match mode {
            0 => insert_words(
                code,
                old.child_latch,
                &[
                    0x1033,
                    (old.child as isize - old.child_latch as isize) as i16 as u16,
                ],
            ),
            1 => {
                code.instructions[old.parent_guard + 1] =
                    (old.child_feedback[0].from - old.parent_guard) as u16;
            }
            2 => {
                code.tries = 1;
            }
            3 => {
                code.instructions[1] = 0x7201;
            }
            _ => unreachable!(),
        }
        assert!(plan::select_all(code).is_none(), "negative mode{mode}");
    }
}
#[test]
fn aggregate_pair_ownership_256_is_an_exact_boundary() {
    let mut class = sequential_fixture(0);
    let code = class.methods[0].code.as_mut().unwrap();
    let plans = plan::select_all(code).unwrap();
    let owned: usize = plans.iter().map(|p| p.parent_nodes.len()).sum();
    let padding = 256 - owned;
    let first = padding / 2;
    insert_nops(code, plans[0].child, first);
    insert_nops(code, plans[1].child + first, padding - first);
    let accepted = plan::select_all(code).expect("256 exactly owned instructions");
    assert_eq!(
        accepted.iter().map(|p| p.parent_nodes.len()).sum::<usize>(),
        256
    );
    native_java::render_method("sample.Entry", &class, &class.methods[0]).unwrap();
    let code = class.methods[0].code.as_mut().unwrap();
    insert_nops(code, accepted[0].child, 1);
    assert!(plan::select_all(code).is_none());
}
#[test]
fn ordinary_loops_and_pair_boundaries_remain_owned() {
    for kind in 0..3 {
        let class = independent_loop_fixture(kind);
        let code = class.methods[0].code.as_ref().unwrap();
        let plans = plan::select_all(code).unwrap();
        assert_eq!(plans.len(), 1);
        assert!(plan::validate_all(code, &plans));
        native_java::render_method("sample.Entry", &class, &class.methods[0]).unwrap();
    }
    for kind in 0..3 {
        let mut class = sequential_fixture(0);
        let code = class.methods[0].code.as_mut().unwrap();
        let plans = plan::select_all(code).unwrap();
        if kind == 0 {
            let outside = fixture().methods[0]
                .code
                .as_ref()
                .unwrap()
                .instructions
                .len()
                - 1;
            code.instructions[outside] =
                (((plans[0].child as isize - outside as isize) as i8 as u8 as u16) << 8) | 0x28;
        } else if kind == 1 {
            let guard = plans[1].child_guard;
            code.instructions[guard + 1] =
                (plans[0].parent as isize - guard as isize) as i16 as u16;
        } else {
            code.instructions.pop();
            code.instructions.extend(
                fixture().methods[0]
                    .code
                    .as_ref()
                    .unwrap()
                    .instructions
                    .clone(),
            );
        }
        assert!(
            plan::select_all(code).is_none(),
            "cross-entry/overlap/budget kind{kind}"
        );
    }
}
#[test]
fn sequential_disjoint_pairs_preserve_external_frames_and_raw_edges() {
    for kind in [0, 1, 2, 3, 4] {
        let class = sequential_fixture(kind);
        let code = class.methods[0].code.as_ref().unwrap();
        let plans = plan::select_all(code).expect("two disjoint canonical pairs");
        assert_eq!(plans.len(), 2);
        assert!(plans[0].bound < plans[1].parent);
        assert!(plan::validate_all(code, &plans));
        for p in &plans {
            assert!(plan::validate(code, p));
        }
        let rendered =
            native_java::render_method("sample.Entry", &class, &class.methods[0]).unwrap();
        assert_eq!(rendered.source.matches("while (true)").count(), 4);
        for target in [
            "sample.Effects.parent(I)V",
            "sample.Effects.child(I)V",
            "sample.Box.<init>()V",
        ] {
            assert!(format!("{:?}", rendered.links).contains(target));
        }
        let mut changed = plans.clone();
        changed.swap(0, 1);
        assert!(!plan::validate_all(code, &changed));
    }
}
#[test]
fn original_parent_child_frames_render() {
    let c = fixture();
    let code = c.methods[0].code.as_ref().unwrap();
    let p = sole_plan(code).unwrap();
    assert!(plan::validate(code, &p));
    let s = native_java::render_method("sample.Entry", &c, &c.methods[0]).unwrap();
    eprintln!("{}", s.source);
    assert_eq!(s.source.matches("while (true)").count(), 2);
    assert!(s.source.contains("continue;"));
    let links = format!("{:?}", s.links);
    for target in [
        "sample.Effects.parent(I)V",
        "sample.Effects.child(I)V",
        "sample.Box.<init>()V",
    ] {
        assert!(links.contains(target), "{target}: {links}");
    }
}

#[test]
fn changed_guard_parallel_and_wide_frames_are_proved() {
    for kind in 0..6 {
        let c = variant(kind);
        let p = sole_plan(c.methods[0].code.as_ref().unwrap())
            .unwrap_or_else(|| panic!("raw kind{kind}"));
        assert!(plan::validate(c.methods[0].code.as_ref().unwrap(), &p));
        let result = native_java::render_method("sample.Entry", &c, &c.methods[0])
            .unwrap_or_else(|e| panic!("kind{kind}: {e}"));
        assert!(
            result.source.contains("continue;"),
            "kind{kind}: {}",
            result.source
        );
    }
}
#[test]
fn foreign_extra_exit_exception_undefined_and_plan_mutations_refuse() {
    let c = fixture();
    let code = c.methods[0].code.as_ref().unwrap();
    let p = sole_plan(code).unwrap();
    for kind in 0..8 {
        let mut bad = p.clone();
        match kind {
            0 => {
                bad.parent_nodes.pop();
            }
            1 => {
                bad.child_nodes.pop();
            }
            2 => {
                bad.edges.pop();
            }
            3 => {
                bad.widths.pop();
            }
            4 => {
                bad.targets.pop();
            }
            5 => {
                bad.words.pop();
            }
            6 => {
                bad.parent_phi_inputs.push((0, usize::MAX));
            }
            7 => {
                bad.bound -= 1;
            }
            _ => unreachable!(),
        };
        assert!(!plan::validate(code, &bad), "plan mutation{kind}");
    }
    for kind in 0..6 {
        let mut c = fixture();
        let code = c.methods[0].code.as_mut().unwrap();
        match kind {
            0 => {
                code.tries = 1;
                code.try_regions.push(native_dex::DexTryRegion {
                    start: 14,
                    end: 19,
                    catches: vec![(None, 27)].into(),
                });
            }
            1 => {
                code.instructions[13] = 15;
            } // child sole exit changes to method return
            2 => {
                code.instructions[11] = 0x0412;
            } // undefined child counter ancestry
            3 => {
                code.instructions[26] = 0xef28;
            } // different inner feedback target
            4 => {
                code.instructions[7] = 0x0032;
                code.instructions[8] = 15;
            } // foreign parent-guard entry into child body
            5 => {
                code.instructions[17] = 0;
            } // completed allocation loses canonical constructor
            _ => unreachable!(),
        }
        if kind == 5 {
            assert!(native_java::render_method("sample.Entry", &c, &c.methods[0]).is_err());
        } else {
            assert!(sole_plan(code).is_none(), "mutation{kind}");
        }
    }
}

struct Temp(std::path::PathBuf);
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
#[ignore = "requires javac/java; emitted source compiled unchanged"]
fn exact_register_oracle_preserves_wide_swaps_guards_exits_and_faults() {
    for kind in 0..28 {
        let c = if kind < 6 {
            variant(kind)
        } else if kind < 11 {
            header_fixture(kind - 6)
        } else if kind < 16 {
            sequential_fixture(kind - 11)
        } else if kind < 19 {
            independent_loop_fixture(kind - 16)
        } else if kind < 24 {
            two_feedback_fixture(kind - 19)
        } else if kind == 24 {
            reference_feedback_fixture()
        } else if kind == 25 {
            sibling_loop_fixture(2)
        } else {
            success_tail_fixture(if kind == 26 { 0 } else { 2 })
        };
        let code = c.methods[0].code.as_ref().unwrap();
        let source = native_java::render_method("sample.Entry", &c, &c.methods[0])
            .unwrap()
            .source;
        let dir = Temp(
            std::env::temp_dir().join(format!("rdx-parent-continue-{}-{kind}", std::process::id())),
        );
        fs::create_dir_all(dir.0.join("sample")).unwrap();
        fs::write(
            dir.0.join("sample/Entry.java"),
            format!("package sample; public class Entry {{{source}}}"),
        )
        .unwrap();
        let words = code
            .instructions
            .iter()
            .map(u16::to_string)
            .collect::<Vec<_>>()
            .join(",");
        fs::write(
            dir.0.join("sample/Main.java"),
            (if kind == 24 {
                reference_harness()
            } else {
                HARNESS.to_owned()
            })
            .replace("@WORDS@", &words)
            .replace("@REGS@", &code.registers.to_string())
            .replace("@INS@", &code.ins.to_string())
            .replace(
                "@WIDE@",
                if kind == 3 || kind == 4 || kind == 9 || kind == 14 || kind == 15 || kind == 22 {
                    "true"
                } else {
                    "false"
                },
            ),
        )
        .unwrap();
        let home = std::env::var_os("RDX_JAVA25_HOME").map(std::path::PathBuf::from);
        let tool = |name: &str| {
            home.as_ref()
                .map(|p| p.join("bin").join(name))
                .unwrap_or_else(|| name.into())
        };
        let compile = Command::new(tool("javac"))
            .arg("-d")
            .arg(&dir.0)
            .arg(dir.0.join("sample/Entry.java"))
            .arg(dir.0.join("sample/Main.java"))
            .output()
            .unwrap();
        assert!(
            compile.status.success(),
            "kind{kind}: {}\n{source}",
            String::from_utf8_lossy(&compile.stderr)
        );
        let run = Command::new(tool("java"))
            .arg("-Xverify:all")
            .arg("-cp")
            .arg(&dir.0)
            .arg("sample.Main")
            .output()
            .unwrap();
        assert!(
            run.status.success(),
            "kind{kind}: {}\n{}\n{source}",
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
        eprintln!("kind{kind}: {}", String::from_utf8_lossy(&run.stdout));
    }
}
fn reference_harness() -> String {
    let mut harness = HARNESS
        .replace(
            "static long dex(int a,int b,int seed)",
            "static Object dex(int a,int b,Object seed)",
        )
        .replace(
            "class Box {Box(){Effects.effect(\"a\",0);}}",
            "class Box {final int id;Box(){id=Effects.calls;Effects.effect(\"a\",0);}}",
        )
        .replace("case 0x01:", "case 0x07:case 0x01:")
        .replace(
            "case 0x0f:case 0x10:",
            "case 0x11:return r[d];case 0x0f:case 0x10:",
        );
    let start = harness.find(" static String outcome(").unwrap();
    let end = harness.find(" public static void main(").unwrap();
    harness.replace_range(start..end, r#" static String outcome(boolean model,int a,int b,int seed,int fault){Effects.reset(fault);Object token=new Object();try{Object value=model?dex(a,b,token):Entry.run(a,b,token);return (value==token?"seed"+seed:"box"+((Box)value).id)+"|"+Effects.trace;}catch(Throwable t){return (t==Effects.FAULT?"identity":t.getClass().getName())+"|"+Effects.trace;}}
"#);
    harness
}
const HARNESS: &str = r#"package sample;
class Effects {static final RuntimeException FAULT=new RuntimeException();static StringBuilder trace;static int calls,fault;static void reset(int f){trace=new StringBuilder();calls=0;fault=f;}static void effect(String label,int v){trace.append(label).append(v).append(';');if(++calls==fault)throw FAULT;}static void parent(int v){effect("p",v);}static void child(int v){effect("c",v);}static boolean guard(int c,int limit,int parent){effect("h"+limit+","+parent+",",c);return c>=limit;}}
class Box {Box(){Effects.effect("a",0);}}
public class Main {
 static final int[] W={@WORDS@};static final boolean WIDE=@WIDE@;static final Object TAIL=new Object();
 static int integer(Object o){return ((Number)o).intValue();}
 static long dex(int a,int b,int seed){Object[] r=new Object[@REGS@];int entry=r.length-@INS@;r[entry]=a;r[entry+1]=b;if(WIDE){r[entry+2]=(long)seed;r[entry+3]=TAIL;}else r[entry+2]=seed;
 Object pending=null;for(int pc=0,work=0;;){if(++work>10000)throw new AssertionError("unbounded model");int word=W[pc],op=word&255,d=word>>>8;switch(op){
 case 0x00:pc++;break;
 case 0x12:r[(word>>>8)&15]=(int)((short)word>>12);pc++;break;
 case 0x01:r[(word>>>8)&15]=r[word>>>12];pc++;break;
 case 0x04:r[(word>>>8)&15]=r[word>>>12];r[((word>>>8)&15)+1]=TAIL;pc++;break;
 case 0x23:r[(word>>>8)&15]=new int[integer(r[word>>>12])];pc+=2;break;
 case 0x22:r[d]=new Object();pc+=2;break;
 case 0x70:r[W[pc+2]&15]=new Box();pc+=3;break;
 case 0x0a:r[d]=pending;pending=null;pc++;break;
 case 0x71:if(W[pc+1]==3){int args=W[pc+2];pending=Effects.guard(integer(r[args&15]),integer(r[(args>>>4)&15]),integer(r[(args>>>8)&15]))?1:0;}else if(W[pc+1]==0)Effects.parent(integer(r[W[pc+2]&15]));else Effects.child(integer(r[W[pc+2]&15]));pc+=3;break;
 case 0x33:pc+=integer(r[(word>>>8)&15])!=integer(r[word>>>12])?(short)W[pc+1]:2;break;
 case 0x34:pc+=integer(r[(word>>>8)&15])<integer(r[word>>>12])?(short)W[pc+1]:2;break;
 case 0x35:pc+=integer(r[(word>>>8)&15])>=integer(r[word>>>12])?(short)W[pc+1]:2;break;
 case 0x39:pc+=integer(r[d])!=0?(short)W[pc+1]:2;break;
 case 0x90:r[d]=integer(r[W[pc+1]&255])+integer(r[W[pc+1]>>>8]);pc+=2;break;
 case 0x9b:r[d]=((Number)r[W[pc+1]&255]).longValue()+((Number)r[W[pc+1]>>>8]).longValue();r[d+1]=TAIL;pc+=2;break;
 case 0xd8:r[d]=integer(r[W[pc+1]&255])+(byte)(W[pc+1]>>>8);pc+=2;break;
 case 0x28:pc+=(byte)(word>>>8);break;
 case 0x0f:case 0x10:return ((Number)r[d]).longValue();
 default:throw new AssertionError("opcode "+op+" pc"+pc);}}}
 static String outcome(boolean model,int a,int b,int seed,int fault){Effects.reset(fault);try{long value=model?dex(a,b,seed):Entry.run(a,b,seed);return value+"|"+Effects.trace;}catch(Throwable t){return (t==Effects.FAULT?"identity":t.getClass().getName())+"|"+Effects.trace;}}
 public static void main(String[] args){int count=0;for(int a=-1;a<=5;a++)for(int b=-1;b<=5;b++)for(int seed:new int[]{-4,0,1,7})for(int fault=0;fault<=30;fault++){String want=outcome(true,a,b,seed,fault),got=outcome(false,a,b,seed,fault);if(!want.equals(got))throw new AssertionError(a+","+b+","+seed+","+fault+" expected "+want+" got "+got);count++;}System.out.println(count+" exact DEX-register comparisons");}
}
"#;
#[test]
fn undefined_wide_tail_and_partial_wide_word_are_rejected() {
    let mut c = variant(3);
    // A narrow copy initializes only the low word of the subsequently read long.
    c.methods[0].code.as_mut().unwrap().instructions[1] = 0x9201;
    assert!(sole_plan(c.methods[0].code.as_ref().unwrap()).is_none());
    assert!(native_java::render_method("sample.Entry", &c, &c.methods[0]).is_err());
    let mut c = variant(4);
    // Copying a long from a tail word cannot authorize a complete wide frame.
    let code = c.methods[0].code.as_mut().unwrap();
    let pc = code
        .instructions
        .iter()
        .position(|word| *word == 0x2504)
        .unwrap();
    code.instructions[pc] = 0x3504;
    assert!(native_java::render_method("sample.Entry", &c, &c.methods[0]).is_err());
}

fn header_fixture(mode: usize) -> DexClass {
    let mut c = variant(match mode {
        0 => 0,
        1 => 2,
        2 => 5,
        3 => 4,
        4 => 0,
        _ => unreachable!(),
    });
    let mut symbols = DexSymbols {
        types: c.symbols.types.clone(),
        strings: c.symbols.strings.clone(),
        protos: c.symbols.protos.clone(),
        methods: c.symbols.methods.clone(),
        ..Default::default()
    };
    symbols.strings.push("guard".into());
    symbols
        .protos
        .push(("Z".into(), vec!["I".into(), "I".into(), "I".into()]));
    symbols.methods.push((0, 2, 3));
    c.symbols = Arc::new(symbols);
    let code = c.methods[0].code.as_mut().unwrap();
    let old = sole_plan(code).unwrap();
    code.instructions.splice(
        old.child..old.child + 2,
        [
            0x3071,
            3,
            if mode == 3 { 0x0081 } else { 0x0051 },
            if mode == 3 || mode == 4 {
                0x040a
            } else {
                0x030a
            },
            if mode == 3 || mode == 4 {
                0x0439
            } else {
                0x0339
            },
            (old.parent as isize - (old.child + 4) as isize) as i16 as u16,
        ],
    );
    code.instructions[old.parent_guard + 1] += 4;
    let latch = old.child_latch + 4;
    code.instructions[latch] =
        ((old.child as isize - latch as isize) as i8 as u8 as u16) << 8 | 0x28;
    c
}
#[test]
fn invoked_header_is_owned_with_actual_guard_frame_and_negative_states() {
    for mode in 0..5 {
        let c = header_fixture(mode);
        let code = c.methods[0].code.as_ref().unwrap();
        let p = sole_plan(code).unwrap();
        assert_eq!(p.child_guard, p.child + 4);
        assert!(plan::validate(code, &p));
        let body = native_java::render_method("sample.Entry", &c, &c.methods[0]).unwrap();
        assert!(body.source.contains("sample.Effects.guard"));
        assert!(format!("{:?}", body.links).contains("sample.Effects.guard(III)Z"));
        let mut bad = p.clone();
        bad.child_guard -= 1;
        assert!(!plan::validate(code, &bad));
    }
    for mode in 0..4 {
        let mut c = header_fixture(0);
        if mode == 1 {
            let mut symbols = DexSymbols {
                types: c.symbols.types.clone(),
                strings: c.symbols.strings.clone(),
                protos: c.symbols.protos.clone(),
                methods: c.symbols.methods.clone(),
                ..Default::default()
            };
            symbols.protos[2].0 = "V".into();
            c.symbols = Arc::new(symbols);
        }
        let code = c.methods[0].code.as_mut().unwrap();
        let p = sole_plan(code).unwrap();
        match mode {
            0 => code.instructions[p.child + 3] = 0x030c, // reference-result cannot bind narrow guard
            1 => {} // a canonical three-argument void callback cannot supply move-result
            2 => {
                code.instructions[p.parent_guard + 1] = (p.child + 3 - p.parent_guard) as u16;
            } // foreign result entry
            3 => {
                code.instructions[2] = 0x4223;
                code.instructions[p.child] = 0x2071;
                code.instructions[p.child + 2] = 0x0031;
            } // undefined child preheader register
            _ => unreachable!(),
        }
        if mode == 0 || mode == 2 || mode == 3 {
            assert!(sole_plan(code).is_none(), "mode{mode}");
        } else {
            assert!(native_java::render_method("sample.Entry", &c, &c.methods[0]).is_err());
        }
    }
}
