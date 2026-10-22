//! Elide an unobservable overwritten primitive final store in a constructor
//! prefix. Original DEX PCs stay intact; the ordinary constructor proof must
//! still validate every remaining final assignment and continuation.
use super::*;
use crate::{native_cfg::EdgeKind, native_constructors::ConstructorOrigin, native_ir::PoolKind};
use std::collections::{BTreeMap, BTreeSet};

/// Repeated final assignments require the full constructor flow proof even
/// when every catch is typed. A failed elision certificate never authorizes
/// emitting two Java assignments to a blank final.
pub(super) fn requires_final_assignment_proof(class: &DexClass, method: &DexMethod) -> bool {
    if method.name.as_ref() != "<init>" {
        return false;
    }
    let finals: BTreeSet<_> = class
        .fields
        .iter()
        .filter(|field| field.access_flags & 0x10 != 0 && !field.is_static)
        .map(|field| (field.name.as_ref(), field.field_type.as_ref()))
        .collect();
    if finals.is_empty() {
        return false;
    }
    let Some(code) = &method.code else {
        return false;
    };
    if code.instructions.len() > 65_536 {
        return true;
    }
    let mut writes: BTreeMap<_, Vec<usize>> = BTreeMap::new();
    let mut possible_cycle = code.try_regions.iter().any(|region| {
        region
            .catches
            .iter()
            .any(|(_, target)| *target < region.end)
    });
    let mut pc = 0;
    while pc < code.instructions.len() {
        // Width-aware detection needs neither a second decoded operand graph
        // nor any copied code. The ordinary renderer still validates operands.
        let Ok((width, payload)) = crate::native_cfg::instruction_width(&code.instructions, pc)
        else {
            return true;
        };
        let current = pc;
        pc += width;
        if payload {
            continue;
        }
        let opcode = code.instructions[current] as u8;
        possible_cycle |= match opcode {
            0x28 => (code.instructions[current] >> 8) as u8 as i8 <= 0,
            0x29 | 0x32..=0x3d => code.instructions[current + 1] as i16 <= 0,
            0x2a => {
                let delta = u32::from(code.instructions[current + 1])
                    | (u32::from(code.instructions[current + 2]) << 16);
                delta as i32 <= 0
            }
            // Switch payloads may contain backward targets. Let the canonical
            // CFG decide whether any can revisit an own-final store.
            0x2b | 0x2c => true,
            _ => false,
        };
        if !matches!(opcode, 0x59..=0x5f) {
            continue;
        }
        let Some(&index) = code.instructions.get(current + 1) else {
            return true;
        };
        let Some(&(owner, descriptor, name)) = class.symbols.fields.get(index as usize) else {
            return true;
        };
        let (Some(owner), Some(descriptor), Some(name)) = (
            class.symbols.types.get(owner as usize),
            class.symbols.types.get(descriptor as usize),
            class.symbols.strings.get(name as usize),
        ) else {
            return true;
        };
        if owner == &class.descriptor && finals.contains(&(name.as_str(), descriptor.as_ref())) {
            writes
                .entry((name.as_str(), descriptor.as_ref()))
                .or_default()
                .push(current);
        }
    }
    if writes.is_empty() || (!possible_cycle && writes.values().all(|sites| sites.len() == 1)) {
        return false;
    }
    // A switch can contain several mutually exclusive stores to one final.
    // They need no new authority when its existing constructor-prefix proof
    // already preserves the exact arms. Only a path between stores requires
    // the stronger final-assignment flow check. Keep single-site groups too:
    // a cycle that revisits one store also assigns its final more than once.
    let Ok(cfg) = crate::native_cfg::ControlFlowGraph::build(code) else {
        return true;
    };
    if cfg.blocks.len() > 16_384 {
        return true;
    }
    let mut work = 262_144usize;
    for sites in writes.values() {
        let sites: BTreeSet<_> = sites.iter().copied().collect();
        for &source in &sites {
            let Some(&root) = cfg.block_at.get(&source) else {
                return true;
            };
            let block = &cfg.blocks[root];
            if block.instructions.len() > work {
                return true;
            }
            work -= block.instructions.len();
            let Some(position) = block.instructions.iter().position(|pc| *pc == source) else {
                return true;
            };
            if block.instructions[position + 1..]
                .iter()
                .any(|pc| sites.contains(pc))
            {
                return true;
            }
            let mut pending: Vec<_> = block.successors.iter().map(|edge| edge.target).collect();
            let mut visited = BTreeSet::new();
            while let Some(next) = pending.pop() {
                if work == 0 {
                    return true;
                }
                work -= 1;
                if !visited.insert(next) {
                    continue;
                }
                let block = &cfg.blocks[next];
                if block.instructions.len() > work {
                    return true;
                }
                work -= block.instructions.len();
                if block.instructions.iter().any(|pc| sites.contains(pc)) {
                    return true;
                }
                pending.extend(block.successors.iter().map(|edge| edge.target));
            }
        }
    }
    false
}

pub(super) fn adjust(class: &DexClass, method: &DexMethod) -> Option<DexMethod> {
    let removed = prove(class, method).ok().flatten()?;
    let code = method.code.as_ref()?;
    let mut instructions = code.instructions.clone();
    for pc in removed {
        // Exact two-code-unit iput-int; two ordinary nops preserve all targets.
        instructions[pc] = 0;
        instructions[pc + 1] = 0;
    }
    Some(DexMethod {
        declaring_type: method.declaring_type.clone(),
        name: method.name.clone(),
        return_type: method.return_type.clone(),
        parameters: method.parameters.clone(),
        thrown_types: method.thrown_types.clone(),
        access_flags: method.access_flags,
        code: Some(crate::native_dex::DexCode {
            registers: code.registers,
            ins: code.ins,
            outs: code.outs,
            tries: code.tries,
            instructions,
            try_regions: code.try_regions.clone(),
            offset: code.offset,
        }),
    })
}

fn exact_field<'a>(
    class: &'a DexClass,
    instruction: &crate::native_ir::Instruction,
) -> Option<&'a crate::native_dex::DexField> {
    let pool = instruction.reference.as_ref()?;
    if pool.kind != PoolKind::Field {
        return None;
    }
    let &(owner, descriptor, name) = class.symbols.fields.get(pool.index as usize)?;
    let (owner, descriptor, name) = (
        class.symbols.types.get(owner as usize)?,
        class.symbols.types.get(descriptor as usize)?,
        class.symbols.strings.get(name as usize)?,
    );
    if owner != &class.descriptor {
        return None;
    }
    let mut declarations = class
        .fields
        .iter()
        .filter(|field| field.name.as_ref() == name.as_str());
    let field = declarations.next()?;
    if declarations.next().is_some()
        || field.declaring_type != class.descriptor
        || field.field_type != *descriptor
        || field.is_static
        || field.access_flags & (8 | 0x40) != 0
        || field.access_flags & !0x109f != 0
        || !matches!(field.access_flags & 7, 0 | 1 | 2 | 4)
    {
        return None;
    }
    Some(field)
}

fn prove(class: &DexClass, method: &DexMethod) -> Result<Option<Vec<usize>>> {
    let code = method.code.as_ref().context("dead final prefix code")?;
    if method.name.as_ref() != "<init>"
        || method.access_flags & 8 != 0
        || class.access_flags & 0x10 == 0
        || class.superclass.as_deref() != Some("Ljava/lang/Object;")
        || class.methods.iter().any(|candidate| {
            candidate.name.as_ref() == "finalize"
                && candidate.parameters.is_empty()
                && candidate.return_type.as_ref() == "V"
        })
        || !class
            .symbols
            .hierarchy
            .get()
            .is_some_and(|hierarchy| hierarchy.unshadowed_sdk_type("Ljava/lang/Object;"))
    {
        return Ok(None);
    }
    ensure!(
        code.instructions.len() <= 4096
            && code.registers <= 256
            && class.fields.len() <= 256
            && class.methods.len() <= 256
            && code.try_regions.len() <= 16
            && usize::from(code.tries) == code.try_regions.len(),
        "dead final prefix metadata budget"
    );
    let ir = crate::native_ir::DecodedMethod::decode(code)?;
    // Select only the first short, unprotected straight-line prefix. A field
    // read, escape or alternate entry is proved separately below, never guessed
    // from the raw register number.
    let prefix: Vec<_> = ir
        .instructions
        .iter()
        .take_while(|instruction| {
            instruction.pc < 128
                && instruction.branch_target.is_none()
                && instruction.payload_target.is_none()
                && !matches!(instruction.opcode, 0x0e..=0x11 | 0x1d | 0x1e | 0x27..=0x2c)
                && !code.try_regions.iter().any(|region| {
                    region.start as usize <= instruction.pc && instruction.pc < region.end as usize
                })
        })
        .collect();
    let mut stores = BTreeMap::<&str, Vec<usize>>::new();
    for instruction in &prefix {
        if instruction.opcode == 0x59
            && let Some(field) = exact_field(class, instruction)
            && field.field_type.as_ref() == "I"
            && field.access_flags & 0x10 != 0
        {
            stores.entry(&field.name).or_default().push(instruction.pc);
        }
    }
    stores.retain(|_, writes| writes.len() > 1);
    if stores.is_empty() {
        return Ok(None);
    }
    let last = stores.values().flatten().copied().max().unwrap();
    let prefix: Vec<_> = prefix
        .into_iter()
        .take_while(|instruction| instruction.pc <= last)
        .collect();
    ensure!(
        prefix
            .first()
            .is_some_and(|instruction| instruction.pc == 0),
        "dead final prefix entry"
    );
    ensure!(
        prefix
            .windows(2)
            .all(|pair| pair[0].pc + pair[0].width == pair[1].pc),
        "dead final prefix instruction gap"
    );
    ensure!(
        code.try_regions.iter().all(|region| {
            region
                .catches
                .iter()
                .all(|(_, handler)| *handler as usize > last)
        }),
        "dead final prefix handler entry"
    );
    let analysis = crate::native_method::MethodAnalysis::build(class, method)?;
    let constructors = analysis.constructors()?;
    ensure!(
        constructors.unresolved.is_empty(),
        "dead final prefix constructor identity"
    );
    let mut initializers = constructors.bindings.iter().filter(|binding| {
        matches!(
            binding.origin,
            ConstructorOrigin::This | ConstructorOrigin::Super
        )
    });
    let initializer = initializers
        .next()
        .context("dead final prefix initializer absent")?;
    ensure!(
        initializers.next().is_none()
            && initializer.origin == ConstructorOrigin::Super
            && initializer.invoked_owner.as_ref() == "Ljava/lang/Object;"
            && !initializer.owner_retarget_required
            && analysis
                .calls()
                .calls
                .iter()
                .any(|call| { call.pc == initializer.invoke_pc && call.arguments.is_empty() }),
        "dead final prefix requires exact Object initializer"
    );
    let first = stores.values().flatten().copied().min().unwrap();
    let init = prefix
        .iter()
        .find(|instruction| instruction.pc == initializer.invoke_pc)
        .context("dead final prefix initializer outside prefix")?;
    ensure!(
        matches!(init.opcode, 0x70 | 0x76) && init.pc + init.width <= first,
        "dead final prefix store precedes initialization"
    );
    // Even an unreachable switch arm or a later exception edge must not enter
    // the prefix after its initial store. Retain the original normal entry.
    let cfg = analysis.blocks();
    for block in &cfg.blocks {
        for edge in &block.successors {
            if cfg.blocks[edge.target].start <= last {
                let tail = block
                    .instructions
                    .last()
                    .context("dead final prefix predecessor")?;
                let instruction = ir
                    .instructions
                    .iter()
                    .find(|instruction| instruction.pc == *tail)
                    .context("dead final prefix edge instruction")?;
                ensure!(
                    edge.kind == EdgeKind::Normal
                        && instruction.pc <= last
                        && instruction.branch_target.is_none()
                        && instruction.payload_target.is_none()
                        && instruction.pc + instruction.width == cfg.blocks[edge.target].start,
                    "dead final prefix alternate entry"
                );
            }
        }
    }
    let ssa: BTreeMap<_, _> = analysis
        .ssa()
        .instructions
        .iter()
        .map(|instruction| (instruction.pc, instruction))
        .collect();
    let mut identity = BTreeMap::new();
    let mut proven_stores = BTreeSet::new();
    for instruction in prefix {
        let values = ssa
            .get(&instruction.pc)
            .context("dead final prefix unreachable instruction")?;
        let reads_this: Vec<_> = values
            .reads
            .iter()
            .enumerate()
            .filter_map(|(index, operand)| {
                operand
                    .words
                    .iter()
                    .any(|value| {
                        *identity
                            .entry(*value)
                            .or_insert_with(|| direct_this_copy_value(method, &analysis, *value))
                    })
                    .then_some(index)
            })
            .collect();
        if reads_this.is_empty() {
            continue;
        }
        if matches!(instruction.opcode, 0x07..=0x09) && reads_this == [0] {
            continue;
        }
        if instruction.pc == initializer.invoke_pc && reads_this == [0] {
            continue;
        }
        ensure!(
            matches!(instruction.opcode, 0x59..=0x5f)
                && reads_this == [0]
                && exact_field(class, instruction).is_some(),
            "dead final prefix observes or publishes this"
        );
        if stores
            .values()
            .any(|writes| writes.contains(&instruction.pc))
        {
            proven_stores.insert(instruction.pc);
        }
    }
    ensure!(
        stores
            .values()
            .flatten()
            .all(|pc| proven_stores.contains(pc)),
        "dead final prefix receiver differs"
    );
    Ok(Some(
        stores
            .values()
            .flat_map(|writes| writes[..writes.len() - 1].iter().copied())
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_hierarchy::TypeHierarchy;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/constructor_dead_final_prefix.rs"
    ));

    fn establish(class: &DexClass) {
        class
            .symbols
            .hierarchy
            .set(Arc::new(TypeHierarchy::from_classes([class]).unwrap()))
            .unwrap();
    }

    #[test]
    fn mutually_exclusive_final_stores_keep_the_existing_constructor_path() {
        let mut class = fixture();
        let code = class.methods[0].code.as_mut().unwrap();
        code.tries = 0;
        code.try_regions.clear();
        code.instructions = vec![
            0x1070, 0, 2, // Object initializer
            0x0338, 6, // if p0 == 0, second arm at 9
            0x1012, 0x2059, 0, 0x000e, // first arm writes and returns
            0x2012, 0x2059, 0, 0x000e, // second arm writes and returns
        ];
        assert!(!requires_final_assignment_proof(&class, &class.methods[0]));

        // Connecting those same two store sites makes both execute on one
        // path, so the ordinary switch-prefix authority is insufficient.
        class.methods[0].code.as_mut().unwrap().instructions[8] = 0x0128;
        assert!(requires_final_assignment_proof(&class, &class.methods[0]));
    }

    #[test]
    fn one_final_store_revisited_by_a_cycle_requires_full_assignment_proof() {
        let mut class = fixture();
        let code = class.methods[0].code.as_mut().unwrap();
        code.tries = 0;
        code.try_regions.clear();
        code.instructions = vec![
            0x1070, 0, 2, // Object initializer
            0x1012, 0x2059, 0, // one static own-final store site at 4
            0x0339, 0xfffe, // if p0 != 0, revisit store at 4
            0x000e,
        ];
        assert!(requires_final_assignment_proof(&class, &class.methods[0]));

        // An unrelated backedge does not revisit the completed final store.
        class.methods[0].code.as_mut().unwrap().instructions[7] = 0;
        assert!(!requires_final_assignment_proof(&class, &class.methods[0]));
    }

    #[test]
    fn certificate_rejects_observation_publication_and_finalizability_directly() {
        let class = fixture();
        establish(&class);
        assert!(requires_final_assignment_proof(&class, &class.methods[0]));
        assert!(adjust(&class, &class.methods[0]).is_some());
        for inserted in [
            vec![0x2152, 0, 0x1071, 5, 1],
            vec![0x1071, 4, 2],
            vec![0x2107, 0x1071, 4, 1],
        ] {
            let mut class = fixture();
            let code = class.methods[0].code.as_mut().unwrap();
            let count = inserted.len() as u32;
            code.instructions.splice(10..10, inserted);
            code.try_regions[0].start += count;
            code.try_regions[0].end += count;
            code.try_regions[0].catches =
                vec![(Some("Ljava/lang/RuntimeException;".into()), 16 + count)].into();
            establish(&class);
            assert!(adjust(&class, &class.methods[0]).is_none());
        }
        let mut class = fixture();
        class.access_flags &= !0x10;
        establish(&class);
        assert!(adjust(&class, &class.methods[0]).is_none());
        let mut class = fixture();
        class.fields[0].access_flags |= 0x40;
        establish(&class);
        assert!(adjust(&class, &class.methods[0]).is_none());
        let mut class = fixture();
        class.methods.push(DexMethod {
            declaring_type: class.descriptor.clone(),
            name: "finalize".into(),
            return_type: "V".into(),
            parameters: vec![],
            thrown_types: vec![],
            access_flags: 4,
            code: None,
        });
        establish(&class);
        assert!(adjust(&class, &class.methods[0]).is_none());
    }
}
