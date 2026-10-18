use super::{inherited_override_exception, interface_default_bridge, method};
use crate::{
    native_cfg::EdgeKind,
    native_dex::{DexClass, DexMethod},
    native_ir::{PoolKind, ValueKind},
    native_method::MethodAnalysis,
    native_ssa::DefinitionKind,
    native_types::TypeResolution,
};
use anyhow::{Context, Result, ensure};
use std::{collections::HashSet, sync::Arc};

const THROWABLE: &str = "Ljava/lang/Throwable;";
const RUNTIME_EXCEPTION: &str = "Ljava/lang/RuntimeException;";
const ERROR: &str = "Ljava/lang/Error;";

pub(super) struct Proof {
    pub authorization: method::SameObjectRethrowAuthorization,
    pub root_header: Vec<String>,
}

pub(super) fn prove(
    class_name: &str,
    class: &DexClass,
    root: &DexMethod,
    failure: &method::PreciseRethrowFailure,
) -> Result<Proof> {
    let first = prove_first_shape(class_name, class, root, failure);
    if first.is_ok() {
        return first;
    }
    let closed = prove_closed_catches(class_name, class, root, failure);
    if std::env::var_os("RDX_RETHROW_DIAG").is_some() && closed.is_err() {
        eprintln!(
            "RDX_RETHROW_DIAG {}.{} first={} closed={:#}",
            class_name,
            root.name,
            first.err().unwrap(),
            closed.as_ref().err().unwrap()
        );
    }
    closed
}

fn prove_first_shape(
    class_name: &str,
    class: &DexClass,
    root: &DexMethod,
    failure: &method::PreciseRethrowFailure,
) -> Result<Proof> {
    let code = root.code.as_ref().context("same-object root has no code")?;
    ensure!(
        !matches!(root.name.as_ref(), "<init>" | "<clinit>")
            && failure.effect == THROWABLE
            && code.instructions.len() <= 4_096
            && code.registers <= 256
            && code.try_regions.len() == 1
            && code.tries == 1,
        "same-object root is outside the first shape"
    );
    ensure!(
        root.declaring_type == class.descriptor
            && failure.site.declaring_type == root.declaring_type
            && failure.site.method_name == root.name
            && failure.site.parameters == root.parameters
            && failure.site.return_type == root.return_type
            && failure.site.static_method == (root.access_flags & 8 != 0)
            && failure.site.code_offset == code.offset,
        "same-object retry caller identity differs"
    );

    let region = &code.try_regions[0];
    ensure!(
        region.catches.len() == 1
            && region.catches[0].0.is_none()
            && usize::try_from(region.catches[0].1).ok() == Some(failure.site.handler_pc),
        "same-object retry requires one original catchall"
    );

    let analysis = MethodAnalysis::build(class, root)?;
    let ir = analysis.instructions();
    ensure!(
        ir.instructions.iter().all(|instruction| {
            !matches!(instruction.opcode, 0x1d..=0x1e | 0x2b..=0x2c)
                && instruction.payload_target.is_none()
                && !unsupported_extended_invoke(instruction.opcode)
                && !writes_external_state(instruction.opcode)
        }),
        "same-object root contains unsupported control, dispatch, or write"
    );
    ensure!(
        ir.instructions
            .iter()
            .filter(|instruction| matches!(instruction.opcode, 0x6e..=0x72 | 0x74..=0x78))
            .count()
            <= 64,
        "same-object call budget exceeded"
    );
    let handler = instruction(ir, failure.site.handler_pc)?;
    ensure!(
        handler.opcode == 0x0d
            && handler.width == 1
            && handler.writes.len() == 1
            && handler.writes[0].register == failure.site.caught_register
            && handler.writes[0].kind == ValueKind::Reference,
        "same-object handler does not begin with the original move-exception"
    );
    let trampoline_pc = handler.pc + handler.width;
    let trampoline = instruction(ir, trampoline_pc)?;
    ensure!(
        matches!(trampoline.opcode, 0x28..=0x2a)
            && trampoline
                .branch_target
                .is_some_and(|target| target > trampoline_pc),
        "same-object handler requires one forward trampoline"
    );
    let cleanup_pc = trampoline.branch_target.unwrap();

    let cfg = analysis.blocks();
    let handler_block_index = *cfg
        .block_at
        .get(&handler.pc)
        .context("same-object handler block missing")?;
    let cleanup_block_index = *cfg
        .block_at
        .get(&cleanup_pc)
        .context("same-object cleanup block missing")?;
    let handler_block = &cfg.blocks[handler_block_index];
    let cleanup_block = &cfg.blocks[cleanup_block_index];
    ensure!(
        exception_only_handler_entry(cfg, handler_block_index, handler.pc)
            && original_ir_has_no_normal_handler_entry(ir, handler.pc)
            && handler_block.instructions == [handler.pc, trampoline.pc]
            && handler_block.successors.len() == 1
            && handler_block.successors[0].kind == EdgeKind::Normal
            && handler_block.successors[0].target == cleanup_block_index,
        "same-object handler has normal entry or its trampoline differs"
    );
    let cleanup_predecessors = cfg
        .blocks
        .iter()
        .flat_map(|block| &block.successors)
        .filter(|edge| edge.target == cleanup_block_index)
        .count();
    ensure!(
        cleanup_predecessors == 1
            && !cleanup_block.instructions.is_empty()
            && cleanup_block.instructions.len() <= 64
            && cleanup_block.successors.is_empty()
            && cleanup_block.instructions.last().copied() == Some(failure.site.throw_pc),
        "same-object cleanup has alternate entry or exit"
    );
    for &pc in &cleanup_block.instructions {
        let operation = instruction(ir, pc)?;
        ensure!(
            (pc == failure.site.throw_pc && operation.opcode == 0x27)
                || (pc != failure.site.throw_pc
                    && !matches!(
                        operation.opcode,
                        0x0d..=0x11 | 0x1d..=0x1e | 0x27..=0x3d
                    )),
            "same-object cleanup is not branch-free"
        );
        ensure!(
            !code
                .try_regions
                .iter()
                .any(|protected| { protected.start as usize <= pc && pc < protected.end as usize }),
            "same-object cleanup remains protected"
        );
    }

    let ssa = analysis.ssa();
    let caught_write = ssa
        .instructions
        .iter()
        .find(|instruction| instruction.pc == handler.pc)
        .and_then(|instruction| instruction.writes.first())
        .context("same-object caught SSA write missing")?;
    ensure!(
        caught_write.register == failure.site.caught_register
            && caught_write.kind == ValueKind::Reference
            && caught_write.words.len() == 1,
        "same-object caught SSA write differs"
    );
    let caught_word = caught_write.words[0];
    ensure!(
        ssa.definitions.get(caught_word).is_some_and(|definition| {
            definition.register == failure.site.caught_register
                && matches!(
                    definition.kind,
                    DefinitionKind::Instruction { pc, word: 0, .. } if pc == handler.pc
                )
        }),
        "same-object caught SSA definition differs"
    );
    let throw_read = ssa
        .instructions
        .iter()
        .find(|instruction| instruction.pc == failure.site.throw_pc)
        .and_then(|instruction| instruction.reads.first())
        .context("same-object terminal throw read missing")?;
    ensure!(
        exact_terminal_caught_use(
            &ssa.instructions,
            &ssa.phis,
            caught_word,
            failure.site.throw_pc,
            throw_read,
        ),
        "same-object caught value is copied, merged, or observed"
    );

    ensure!(
        ir.instructions
            .iter()
            .filter(|instruction| instruction.opcode == 0x27)
            .all(|instruction| instruction.pc == failure.site.throw_pc),
        "same-object root has another explicit throw"
    );

    let hierarchy = class
        .symbols
        .hierarchy
        .get()
        .context("same-object retry requires hierarchy")?;
    let inferred = analysis.infer_types()?;
    ensure!(
        inferred.wide_pair_issues == 0,
        "same-object type inference has wide-pair uncertainty"
    );
    ensure!(
        class.fields.len() <= 256,
        "same-object field budget exceeded"
    );
    for operation in &ir.instructions {
        if !matches!(operation.opcode, 0x52..=0x58 | 0x60..=0x66) {
            continue;
        }
        let reference = operation
            .reference
            .as_ref()
            .filter(|reference| reference.kind == PoolKind::Field)
            .context("field read reference missing")?;
        let &(owner_index, type_index, name_index) = class
            .symbols
            .fields
            .get(reference.index as usize)
            .context("field read index missing")?;
        let owner = class
            .symbols
            .types
            .get(owner_index as usize)
            .context("field read owner missing")?;
        let ty = class
            .symbols
            .types
            .get(type_index as usize)
            .context("field read type missing")?;
        let name = class
            .symbols
            .strings
            .get(name_index as usize)
            .context("field read name missing")?;
        let static_read = operation.opcode >= 0x60;
        let mut declarations = class.fields.iter().filter(|field| {
            field.declaring_type.as_ref() == owner.as_ref() && field.name.as_ref() == name.as_str()
        });
        let field = declarations
            .next()
            .context("exact own field declaration missing")?;
        ensure!(
            declarations.next().is_none()
                && owner.as_ref() == class.descriptor.as_ref()
                && field.is_static == static_read
                && field.is_static == (field.access_flags & 8 != 0)
                && field.field_type.as_ref() == ty.as_ref()
                && field.access_flags & !0x10df == 0
                && field.access_flags & 0x50 != 0x50
                && matches!(field.access_flags & 7, 0 | 1 | 2 | 4)
                && field_opcode_matches_type(operation.opcode, ty),
            "field read declaration, staticness, access, or type differs"
        );
        if !static_read {
            let receiver = ssa
                .instructions
                .iter()
                .find(|instruction| instruction.pc == operation.pc)
                .and_then(|instruction| instruction.reads.first())
                .context("instance field receiver SSA missing")?;
            ensure!(
                receiver.kind == ValueKind::Reference
                    && receiver.words.len() == 1
                    && inferred.values.get(receiver.words[0]).is_some_and(|value| {
                        matches!(&value.resolution, TypeResolution::Resolved(resolved) if resolved == owner)
                            && !value.bounds_truncated
                            && !value.assignment_bounds_truncated
                    }),
                "instance field receiver does not retain the declared owner type"
            );
        }
    }
    let mut declarations = 0usize;
    let mut dependency_words = 0usize;
    let root_header = root_header(class, root);
    for operation in &ir.instructions {
        if !matches!(operation.opcode, 0x6e | 0x71 | 0x74 | 0x77) {
            ensure!(
                !matches!(operation.opcode, 0x6f..=0x70 | 0x72 | 0x75..=0x76 | 0x78),
                "same-object retry does not resolve this invoke dispatch"
            );
            continue;
        }
        declarations = declarations
            .checked_add(1)
            .context("declaration budget overflow")?;
        ensure!(declarations <= 64, "declaration budget exceeded");
        let reference = operation
            .reference
            .as_ref()
            .filter(|reference| reference.kind == PoolKind::Method)
            .context("invoke method reference missing")?;
        let &(owner_index, proto_index, name_index) = class
            .symbols
            .methods
            .get(reference.index as usize)
            .context("invoke method index missing")?;
        let owner = class
            .symbols
            .types
            .get(owner_index as usize)
            .context("invoke owner missing")?;
        let (ret, args) = class
            .symbols
            .protos
            .get(proto_index as usize)
            .context("invoke prototype missing")?;
        let name = class
            .symbols
            .strings
            .get(name_index as usize)
            .context("invoke name missing")?;
        ensure!(
            !matches!(name.as_str(), "<init>" | "<clinit>"),
            "constructors are outside same-object retry"
        );
        let static_call = matches!(operation.opcode, 0x71 | 0x77);
        if !static_call {
            let receiver = analysis
                .calls()
                .calls
                .iter()
                .find(|call| call.pc == operation.pc)
                .and_then(|call| call.receiver.as_ref())
                .context("invoke receiver SSA missing")?;
            ensure!(
                receiver.words.len() == 1
                    && receiver.words.first().and_then(|word| inferred.values.get(*word)).is_some_and(
                        |value| matches!(&value.resolution, TypeResolution::Resolved(ty) if ty == owner)
                            && !value.bounds_truncated
                            && !value.assignment_bounds_truncated
                    ),
                "invoke receiver does not retain the declared owner type"
            );
        }
        let thrown = if owner == &class.descriptor {
            established_same_class_header(
                class_name,
                class,
                name,
                args,
                ret,
                static_call,
                &mut dependency_words,
            )?
        } else {
            let declaration = hierarchy
                .exact_platform_source_declaration(
                    &root.declaring_type,
                    owner,
                    name,
                    args,
                    ret,
                    static_call,
                )
                .context("exact platform source declaration unavailable")?;
            ensure!(
                declaration.owner_access_flags & 1 != 0 && declaration.member_access_flags & 1 != 0,
                "platform source declaration is not publicly accessible"
            );
            declaration.thrown_types
        };
        for ty in thrown {
            ensure_source_throw_is_covered(class, &root_header, &ty)?;
        }
    }

    Ok(Proof {
        authorization: method::SameObjectRethrowAuthorization {
            site: failure.site.clone(),
            additional_sites: Vec::new(),
        },
        root_header,
    })
}

// All normal source operations remain unchanged. This wider retry proves the
// Java declaration obligation independently of cleanup layout: each authorized
// throw is the same original catch variable, and every invocation has an exact
// source header covered by the unchanged root declaration.
fn prove_closed_catches(
    class_name: &str,
    class: &DexClass,
    root: &DexMethod,
    failure: &method::PreciseRethrowFailure,
) -> Result<Proof> {
    let code = root.code.as_ref().context("closed-catch code missing")?;
    ensure!(
        failure.effect == THROWABLE
            && !matches!(root.name.as_ref(), "<init>" | "<clinit>")
            && root.declaring_type == class.descriptor
            && code.instructions.len() <= 4096
            && code.registers <= 256
            && (1..=16).contains(&code.try_regions.len())
            && usize::from(code.tries) == code.try_regions.len()
            && super::source_header_authority(class),
        "closed-catch root is outside bounded source shape"
    );
    let analysis = MethodAnalysis::build(class, root)?;
    let ir = analysis.instructions();
    let cfg = analysis.blocks();
    let ssa = analysis.ssa();
    ensure!(cfg.blocks.len()<=256 && ir.instructions.iter().all(|i|!matches!(i.opcode,0x1d|0x1e)&&!unsupported_extended_invoke(i.opcode)),"closed-catch unsupported monitor/dispatch");
    // Normal cycles require their own handler/loop lifetime proof.
    let mut incoming = vec![0usize; cfg.blocks.len()];
    for block in &cfg.blocks {
        for edge in &block.successors {
            if edge.kind == EdgeKind::Normal {
                incoming[edge.target] += 1;
            }
        }
    }
    let mut ready: Vec<_> = incoming
        .iter()
        .enumerate()
        .filter_map(|(i, n)| (*n == 0).then_some(i))
        .collect();
    let mut visited = 0;
    while let Some(block) = ready.pop() {
        visited += 1;
        for edge in &cfg.blocks[block].successors {
            if edge.kind == EdgeKind::Normal {
                incoming[edge.target] -= 1;
                if incoming[edge.target] == 0 {
                    ready.push(edge.target);
                }
            }
        }
    }
    ensure!(visited == cfg.blocks.len(), "closed-catch normal cycle");
    let hierarchy = class
        .symbols
        .hierarchy
        .get()
        .context("closed-catch hierarchy missing")?;
    let root_header = root_header(class, root);
    let mut dependency_words = 0;
    let mut calls = 0;
    let mut loaded_external = false;
    for operation in &ir.instructions {
        if !matches!(operation.opcode,0x6e..=0x72|0x74..=0x78) {
            continue;
        }
        calls += 1;
        ensure!(calls <= 128, "closed-catch call budget");
        let reference = operation
            .reference
            .as_ref()
            .filter(|r| r.kind == PoolKind::Method)
            .context("closed-catch method ref")?;
        let &(owner_i, proto_i, name_i) = class
            .symbols
            .methods
            .get(reference.index as usize)
            .context("closed-catch method pool")?;
        let owner = class
            .symbols
            .types
            .get(owner_i as usize)
            .context("closed-catch owner")?;
        let name = class
            .symbols
            .strings
            .get(name_i as usize)
            .context("closed-catch name")?;
        let (ret, args) = class
            .symbols
            .protos
            .get(proto_i as usize)
            .context("closed-catch prototype")?;
        let static_call = matches!(operation.opcode, 0x71 | 0x77);
        let thrown = if owner == &class.descriptor && !name.starts_with('<') {
            established_same_class_header(
                class_name,
                class,
                name,
                args,
                ret,
                static_call,
                &mut dependency_words,
            )?
        } else {
            let declaration = hierarchy
                .exact_rethrow_source_declaration(
                    &root.declaring_type,
                    owner,
                    name,
                    args,
                    ret,
                    operation.opcode,
                )
                .with_context(|| {
                    format!(
                        "closed-catch exact source header unavailable at {:04x}: {}.{}({}){}",
                        operation.pc,
                        owner,
                        name,
                        args.iter().map(AsRef::as_ref).collect::<String>(),
                        ret
                    )
                })?;
            loaded_external |= class
                .symbols
                .hierarchy
                .get()
                .is_some_and(|h| h.loaded_source_owner(owner));
            declaration.thrown_types
        };
        for ty in thrown {
            ensure_source_throw_is_covered(class, &root_header, &ty)?;
        }
    }
    ensure!(
        loaded_external || code.try_regions.len() > 1,
        "closed-catch expansion needs loaded declaration or nested original handlers"
    );
    let mut handlers = std::collections::HashMap::new();
    for region in &code.try_regions {
        for (caught, target) in region.catches.iter() {
            if caught.as_ref().is_some_and(|t| t.as_ref() != THROWABLE) {
                continue;
            }
            let pc = *target as usize;
            let ins = instruction(ir, pc)?;
            ensure!(
                ins.opcode == 0x0d && ins.writes.len() == 1,
                "closed-catch move-exception differs"
            );
            let block = *cfg
                .block_at
                .get(&pc)
                .context("closed-catch handler block")?;
            ensure!(
                exception_only_handler_entry(cfg, block, pc)
                    && original_ir_has_no_normal_handler_entry_with_switch(ir, pc),
                "closed-catch normal handler entry"
            );
            let write = ssa
                .instructions
                .iter()
                .find(|i| i.pc == pc)
                .and_then(|i| i.writes.first())
                .context("closed-catch SSA handler")?;
            ensure!(write.words.len() == 1, "closed-catch wide handler");
            handlers.insert(write.words[0], (pc, write.register));
        }
    }
    fn origin(
        id: usize,
        analysis: &MethodAnalysis<'_>,
        handlers: &std::collections::HashMap<usize, (usize, u16)>,
        active: &mut HashSet<usize>,
        budget: &mut usize,
    ) -> Option<(usize, u16)> {
        *budget = budget.checked_sub(1)?;
        if let Some(found) = handlers.get(&id) {
            return Some(*found);
        }
        if !active.insert(id) {
            return None;
        }
        let ssa = analysis.ssa();
        let result = match &ssa.definitions.get(id)?.kind {
            DefinitionKind::Instruction { pc, .. } => {
                let ins = analysis
                    .instructions()
                    .instructions
                    .iter()
                    .find(|i| i.pc == *pc)?;
                if !matches!(ins.opcode, 0x07..=0x09) {
                    return None;
                }
                let source = ssa
                    .instructions
                    .iter()
                    .find(|i| i.pc == *pc)?
                    .reads
                    .first()?;
                if source.words.len() != 1 {
                    return None;
                }
                origin(source.words[0], analysis, handlers, active, budget)
            }
            DefinitionKind::Phi { .. } => {
                let phi = ssa.phis.iter().find(|p| p.result == id)?;
                let mut found = None;
                if phi.incoming.is_empty() {
                    return None;
                }
                for (_, input) in &phi.incoming {
                    let next = origin(*input, analysis, handlers, active, budget)?;
                    if found.is_some_and(|old| old != next) {
                        return None;
                    }
                    found = Some(next);
                }
                found
            }
            _ => None,
        };
        active.remove(&id);
        result
    }
    let mut sites = Vec::new();
    let mut budget = 100_000;
    for ins in ir.instructions.iter().filter(|i| i.opcode == 0x27) {
        if code.try_regions.iter().any(|region| {
            region.start as usize <= ins.pc
                && ins.pc < region.end as usize
                && region
                    .catches
                    .iter()
                    .any(|(ty, _)| ty.as_ref().is_none_or(|ty| ty.as_ref() == THROWABLE))
        }) {
            continue;
        }
        let read = ssa
            .instructions
            .iter()
            .find(|i| i.pc == ins.pc)
            .and_then(|i| i.reads.first())
            .context("closed-catch throw read")?;
        if read.words.len() != 1 {
            continue;
        }
        let Some((handler_pc, caught_register)) = origin(
            read.words[0],
            &analysis,
            &handlers,
            &mut HashSet::new(),
            &mut budget,
        ) else {
            continue;
        };
        sites.push(method::PreciseRethrowSite {
            declaring_type: root.declaring_type.clone(),
            method_name: root.name.clone(),
            parameters: root.parameters.clone(),
            return_type: root.return_type.clone(),
            static_method: root.access_flags & 8 != 0,
            code_offset: code.offset,
            handler_pc,
            caught_register,
            throw_pc: ins.pc,
        });
    }
    ensure!(
        sites.contains(&failure.site),
        "closed-catch failure identity is unproved"
    );
    sites.retain(|site| site != &failure.site);
    Ok(Proof {
        authorization: method::SameObjectRethrowAuthorization {
            site: failure.site.clone(),
            additional_sites: sites,
        },
        root_header,
    })
}

fn original_ir_has_no_normal_handler_entry_with_switch(
    ir: &crate::native_ir::DecodedMethod,
    pc: usize,
) -> bool {
    // Switch targets are checked by the canonical CFG incoming-edge proof;
    // explicit branches/fallthrough here also retain original instruction PCs.
    ir.instructions.iter().all(|ins| {
        ins.branch_target != Some(pc)
            && (matches!(ins.opcode,0x0e..=0x11|0x27..=0x2a) || ins.pc + ins.width != pc)
    })
}

fn exception_only_handler_entry(
    cfg: &crate::native_cfg::ControlFlowGraph,
    target: usize,
    handler_pc: usize,
) -> bool {
    handler_pc != 0
        && cfg.blocks.iter().all(|block| {
            block
                .successors
                .iter()
                .all(|edge| edge.target != target || edge.kind != EdgeKind::Normal)
        })
}

fn original_ir_has_no_normal_handler_entry(
    ir: &crate::native_ir::DecodedMethod,
    handler_pc: usize,
) -> bool {
    handler_pc != 0
        && ir.instructions.iter().all(|instruction| {
            let explicit_branch_is_safe = instruction.branch_target != Some(handler_pc);
            // Switch case targets are validated by the decoder but are retained
            // in payload words rather than Instruction. The first proof already
            // excludes switches, and this original-IR guard keeps that boundary.
            let switch_is_absent = !matches!(instruction.opcode, 0x2b..=0x2c);
            let falls_through = !matches!(instruction.opcode, 0x0e..=0x11 | 0x27..=0x2a);
            let fallthrough_is_safe =
                !falls_through || instruction.pc.checked_add(instruction.width) != Some(handler_pc);
            explicit_branch_is_safe && switch_is_absent && fallthrough_is_safe
        })
}

fn instruction(
    ir: &crate::native_ir::DecodedMethod,
    pc: usize,
) -> Result<&crate::native_ir::Instruction> {
    ir.instructions
        .iter()
        .find(|instruction| instruction.pc == pc)
        .context("instruction PC is not an original boundary")
}

fn writes_external_state(opcode: u8) -> bool {
    matches!(opcode, 0x26 | 0x4b..=0x51 | 0x59..=0x5f | 0x67..=0x6d)
}

fn unsupported_extended_invoke(opcode: u8) -> bool {
    matches!(opcode, 0xfa..=0xfd)
}

fn field_opcode_matches_type(opcode: u8, ty: &str) -> bool {
    let family = if opcode >= 0x60 {
        opcode - 0x60
    } else {
        opcode - 0x52
    };
    match family {
        0 => matches!(ty, "I" | "F"),
        1 => matches!(ty, "J" | "D"),
        2 => ty.starts_with('L') || ty.starts_with('['),
        3 => ty == "Z",
        4 => ty == "B",
        5 => ty == "C",
        6 => ty == "S",
        _ => false,
    }
}

fn exact_terminal_caught_use(
    instructions: &[crate::native_ssa::SsaInstruction],
    phis: &[crate::native_ssa::Phi],
    caught_word: usize,
    throw_pc: usize,
    throw_read: &crate::native_ssa::SsaOperand,
) -> bool {
    throw_read.kind == ValueKind::Reference
        && throw_read.words.as_slice() == [caught_word]
        && instructions.iter().all(|instruction| {
            instruction.pc == throw_pc
                || instruction
                    .reads
                    .iter()
                    .all(|operand| !operand.words.contains(&caught_word))
        })
        && phis.iter().all(|phi| {
            phi.result != caught_word && phi.incoming.iter().all(|(_, word)| *word != caught_word)
        })
}

fn root_header(class: &DexClass, method: &DexMethod) -> Vec<String> {
    let mut header: Vec<String> = method
        .thrown_types
        .iter()
        .map(ToString::to_string)
        .chain(
            inherited_override_exception(class, method)
                .into_iter()
                .map(ToString::to_string),
        )
        .collect();
    let mut seen = HashSet::new();
    header.retain(|ty| seen.insert(ty.clone()));
    header
}

fn established_same_class_header(
    class_name: &str,
    class: &DexClass,
    name: &str,
    args: &[Arc<str>],
    ret: &str,
    static_call: bool,
    dependency_words: &mut usize,
) -> Result<Vec<Arc<str>>> {
    let mut candidates = class.methods.iter().filter(|candidate| {
        candidate.declaring_type == class.descriptor
            && candidate.name.as_ref() == name
            && candidate.parameters == args
    });
    let target = candidates
        .next()
        .context("same-class selected declaration missing")?;
    ensure!(
        candidates.next().is_none()
            && target.return_type.as_ref() == ret
            && (target.access_flags & 8 != 0) == static_call
            && target.access_flags & !0x31dff == 0
            && matches!(target.access_flags & 7, 0 | 1 | 2 | 4)
            && class.access_flags & !0x17631 == 0
            && matches!(class.access_flags & 7, 0 | 1 | 2 | 4),
        "same-class selected declaration is ambiguous or malformed"
    );
    let declaration_only = target.access_flags & (0x400 | 0x100) != 0;
    if declaration_only {
        ensure!(
            target.code.is_none(),
            "same-class declaration-only source has a body"
        );
    } else {
        // The full baseline render and the retained body reconstruction are
        // separate bounded passes. Reserve both before entering either pass.
        let words = target
            .code
            .as_ref()
            .context("concrete selected declaration has no body")?
            .instructions
            .len();
        let charged_words = words
            .checked_mul(2)
            .context("dependency word budget overflow")?;
        *dependency_words = dependency_words
            .checked_add(charged_words)
            .context("dependency word budget overflow")?;
        ensure!(
            *dependency_words <= 16_384,
            "dependency word budget exceeded"
        );
    }
    // Establish the complete source declaration and body using the ordinary
    // renderer with this retry disabled. This covers flags, varargs, types,
    // annotations and the existing source header without recursive authority.
    super::render_method_without_same_object_retry(class_name, class, target)?;
    let mut header = target.thrown_types.clone();
    header.extend(
        inherited_override_exception(class, target)
            .into_iter()
            .map(Arc::from),
    );
    if !declaration_only {
        let body = match interface_default_bridge::reconstruct(class_name, class, target)? {
            Some(body) => body,
            None => method::reconstruct(class_name, class, target)?,
        };
        header.extend(body.inferred_throws.into_iter().map(Arc::from));
    }
    let mut seen = HashSet::new();
    header.retain(|ty| seen.insert(ty.clone()));
    Ok(header)
}

fn ensure_source_throw_is_covered(
    class: &DexClass,
    root_header: &[String],
    ty: &str,
) -> Result<()> {
    method::validate_exception_type(class, ty)?;
    let hierarchy = class.symbols.hierarchy.get().context("hierarchy missing")?;
    ensure!(
        hierarchy.assignable(ty, THROWABLE) == crate::native_hierarchy::Relation::Proven,
        "source declaration is not a proven Throwable"
    );
    if hierarchy.assignable(ty, RUNTIME_EXCEPTION) == crate::native_hierarchy::Relation::Proven
        || hierarchy.assignable(ty, ERROR) == crate::native_hierarchy::Relation::Proven
    {
        return Ok(());
    }
    ensure!(
        root_header.iter().any(|declared| {
            hierarchy.assignable(ty, declared) == crate::native_hierarchy::Relation::Proven
        }),
        "source checked declaration exceeds unchanged root header"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::{
        method as renderer, render_method_without_caught_source_retry as render_method,
    };
    use super::{
        established_same_class_header, exact_terminal_caught_use, exception_only_handler_entry,
        original_ir_has_no_normal_handler_entry, prove, prove_first_shape,
        unsupported_extended_invoke, writes_external_state,
    };
    use crate::{
        native_cfg::ControlFlowGraph,
        native_dex::{DexClass, DexCode, DexField, DexMethod, DexSymbols, DexTryRegion, DexValue},
        native_hierarchy::{TypeHierarchy, platform_source_accessible_for_test},
        native_ir::{DecodedMethod, ValueKind},
        native_ssa::{Phi, SsaInstruction, SsaOperand},
    };
    use std::sync::Arc;

    // Each argument mirrors an independent DEX fixture field used by malformed controls.
    #[allow(clippy::too_many_arguments)]
    fn method(
        owner: &Arc<str>,
        name: &str,
        flags: u32,
        parameters: Vec<Arc<str>>,
        registers: u16,
        ins: u16,
        outs: u16,
        words: Vec<u16>,
    ) -> DexMethod {
        DexMethod {
            declaring_type: owner.clone(),
            name: name.into(),
            return_type: "V".into(),
            parameters,
            thrown_types: vec![],
            access_flags: flags,
            code: Some(DexCode {
                registers,
                ins,
                outs,
                tries: 0,
                try_regions: vec![],
                instructions: words,
                offset: 1,
            }),
        }
    }

    fn fixture(copied_catch: bool, checked_callee: bool) -> DexClass {
        let owner: Arc<str> = "Lsample/Example;".into();
        let symbols = Arc::new(DexSymbols {
            strings: vec!["f".into(), "clean".into(), "<init>".into()],
            types: vec![owner.clone(), "Ljava/lang/RuntimeException;".into()],
            protos: vec![("V".into(), vec![])],
            methods: vec![(0, 0, 0), (0, 0, 1), (1, 0, 2)],
            ..Default::default()
        });
        // A trivial throw-null body has a closed header. A non-null allocation
        // remains conservative in the index, while exact source rendering proves
        // the RuntimeException throw needs no checked declaration.
        let mut target = method(
            &owner,
            "f",
            0x11,
            vec![],
            1,
            1,
            1,
            vec![0x0022, 1, 0x1070, 2, 0, 0x0027],
        );
        if checked_callee {
            target.thrown_types.push("Ljava/io/IOException;".into());
        }
        let cleanup = method(&owner, "clean", 0x9, vec![], 0, 0, 0, vec![0x000e]);
        let (registers, handler_tail) = if copied_catch {
            (3, vec![0x0107, 0x0071, 1, 0, 0x0127])
        } else {
            (2, vec![0x0071, 1, 0, 0x0027])
        };
        let receiver_word = if copied_catch { 2 } else { 1 };
        let mut words = vec![0x106e, 0, receiver_word, 0x0328, 0x000d, 0x0228, 0x000e];
        words.extend(handler_tail);
        let mut root = method(
            &owner,
            "root",
            0x9,
            vec![owner.clone()],
            registers,
            1,
            1,
            words,
        );
        root.code.as_mut().unwrap().offset = 2;
        root.code.as_mut().unwrap().tries = 1;
        root.code.as_mut().unwrap().try_regions.push(DexTryRegion {
            start: 0,
            end: 3,
            catches: vec![(None, 4)].into(),
        });
        let class = DexClass {
            symbols,
            descriptor: owner,
            superclass: Some("Ljava/lang/Object;".into()),
            interfaces: vec![],
            access_flags: 0x11,
            annotations_offset: 0,
            static_values_offset: 0,
            static_values: vec![],
            fields: vec![],
            methods: vec![target, cleanup, root],
        };
        let hierarchy = Arc::new(TypeHierarchy::from_classes([&class]).unwrap());
        class.symbols.hierarchy.set(hierarchy).unwrap();
        class
    }

    fn baseline_failure(class: &DexClass) -> renderer::PreciseRethrowFailure {
        let root = &class.methods[2];
        let error = match renderer::reconstruct("sample.Example", class, root) {
            Ok(_) => panic!("baseline unexpectedly accepted same-object fixture"),
            Err(error) => error,
        };
        error
            .downcast_ref::<renderer::PreciseRethrowFailure>()
            .unwrap()
            .clone()
    }

    fn add_int_field(class: &mut DexClass, access_flags: u32, is_static: bool) {
        let symbols = Arc::get_mut(&mut class.symbols).unwrap();
        let type_index = u16::try_from(symbols.types.len()).unwrap();
        let name_index = u32::try_from(symbols.strings.len()).unwrap();
        symbols.types.push("I".into());
        symbols.strings.push("x".into());
        symbols.fields.push((0, type_index, name_index));
        class.fields.push(DexField {
            declaring_type: class.descriptor.clone(),
            name: "x".into(),
            field_type: "I".into(),
            access_flags,
            is_static,
        });
    }

    #[test]
    fn fixture_has_uncertain_index_effect_and_exact_empty_source_header() {
        let class = fixture(false, false);
        let effects = class
            .symbols
            .hierarchy
            .get()
            .unwrap()
            .call_checked_effects(&class.descriptor, "f", &[], "V", false)
            .unwrap();
        assert_eq!(
            effects.iter().map(AsRef::as_ref).collect::<Vec<_>>(),
            ["Ljava/lang/Throwable;"]
        );
        let failure = baseline_failure(&class);
        assert_eq!(failure.effect, "Ljava/lang/Throwable;");
        let mut work = 0;
        let header = established_same_class_header(
            "sample.Example",
            &class,
            "f",
            &[],
            "V",
            false,
            &mut work,
        )
        .unwrap();
        assert!(header.is_empty());
        assert!(prove("sample.Example", &class, &class.methods[2], &failure).is_ok());
    }

    #[test]
    fn exact_same_object_cleanup_rethrow_keeps_header_empty() {
        let class = fixture(false, false);
        let rendered = render_method("sample.Example", &class, &class.methods[2]).unwrap();
        assert!(rendered.source.contains("catch (java.lang.Throwable"));
        assert!(rendered.source.contains("clean();"));
        assert!(rendered.source.contains("throw e"));
        assert!(!rendered.source.lines().next().unwrap().contains(" throws "));
    }

    #[test]
    fn authorization_consumes_one_exact_throwable_effect() {
        let class = fixture(false, false);
        let failure = baseline_failure(&class);
        let authorization = renderer::SameObjectRethrowAuthorization {
            site: failure.site.clone(),
            additional_sites: Vec::new(),
        };
        let effects = renderer::reconstruct_same_object_rethrow_effects_for_test(
            "sample.Example",
            &class,
            &class.methods[2],
            &authorization,
        )
        .unwrap();
        assert!(effects.contains("Ljava/lang/Throwable;"));
        assert!(renderer::same_object_rethrow_evidence_for_test(
            &authorization,
            std::slice::from_ref(&failure.site),
            &["Ljava/lang/Throwable;"],
        ));
        assert!(!renderer::same_object_rethrow_evidence_for_test(
            &authorization,
            &[],
            &["Ljava/lang/Throwable;"],
        ));
        assert!(!renderer::same_object_rethrow_evidence_for_test(
            &authorization,
            &[failure.site.clone(), failure.site.clone()],
            &["Ljava/lang/Throwable;"],
        ));
        assert!(!renderer::same_object_rethrow_evidence_for_test(
            &authorization,
            std::slice::from_ref(&failure.site),
            &[],
        ));
    }

    #[test]
    fn copied_caught_value_has_no_authority() {
        let class = fixture(true, false);
        assert!(render_method("sample.Example", &class, &class.methods[2]).is_err());
    }

    #[test]
    fn checked_source_header_cannot_mutate_root_header() {
        let class = fixture(false, true);
        assert!(render_method("sample.Example", &class, &class.methods[2]).is_err());
    }

    #[test]
    fn covered_checked_source_header_is_preserved_exactly() {
        let mut class = fixture(false, true);
        class.methods[2]
            .thrown_types
            .push("Ljava/io/IOException;".into());
        let rendered = render_method("sample.Example", &class, &class.methods[2]).unwrap();
        let declaration = rendered.source.lines().next().unwrap();
        assert!(declaration.contains(" throws java.io.IOException"));
        assert_eq!(declaration.matches("java.io.IOException").count(), 1);
    }

    #[test]
    fn authorization_is_bound_to_original_code_identity() {
        let class = fixture(false, false);
        let root = &class.methods[2];
        let baseline = match renderer::reconstruct("sample.Example", &class, root) {
            Ok(_) => panic!("baseline unexpectedly accepted same-object fixture"),
            Err(error) => error,
        };
        let failure = baseline
            .downcast_ref::<renderer::PreciseRethrowFailure>()
            .unwrap();
        let mut wrong = failure.site.clone();
        wrong.code_offset += 1;
        assert!(
            renderer::reconstruct_same_object_rethrow(
                "sample.Example",
                &class,
                root,
                &renderer::SameObjectRethrowAuthorization {
                    site: wrong,
                    additional_sites: Vec::new()
                },
            )
            .is_err()
        );

        for wrong_site in [
            renderer::PreciseRethrowSite {
                handler_pc: failure.site.handler_pc + 1,
                ..failure.site.clone()
            },
            renderer::PreciseRethrowSite {
                throw_pc: failure.site.throw_pc - 1,
                ..failure.site.clone()
            },
        ] {
            assert!(
                renderer::reconstruct_same_object_rethrow(
                    "sample.Example",
                    &class,
                    root,
                    &renderer::SameObjectRethrowAuthorization {
                        site: wrong_site,
                        additional_sites: Vec::new()
                    },
                )
                .is_err()
            );
        }
    }

    #[test]
    fn copied_reassigned_and_phi_caught_values_have_no_authority() {
        let caught = SsaOperand {
            register: 0,
            kind: ValueKind::Reference,
            words: vec![7],
        };
        let terminal = SsaInstruction {
            pc: 10,
            reads: vec![caught.clone()],
            writes: vec![],
        };
        assert!(exact_terminal_caught_use(
            std::slice::from_ref(&terminal),
            &[],
            7,
            10,
            &caught,
        ));
        let copied = SsaInstruction {
            pc: 9,
            reads: vec![caught.clone()],
            writes: vec![SsaOperand {
                register: 1,
                kind: ValueKind::Reference,
                words: vec![8],
            }],
        };
        assert!(!exact_terminal_caught_use(
            &[copied, terminal.clone()],
            &[],
            7,
            10,
            &caught,
        ));
        let reassigned = SsaOperand {
            register: 0,
            kind: ValueKind::Reference,
            words: vec![8],
        };
        assert!(!exact_terminal_caught_use(
            std::slice::from_ref(&terminal),
            &[],
            7,
            10,
            &reassigned,
        ));
        let phi = Phi {
            block: 1,
            register: 0,
            result: 8,
            incoming: vec![(Some(0), 7)],
        };
        assert!(!exact_terminal_caught_use(
            &[terminal],
            &[phi],
            7,
            10,
            &caught,
        ));
    }

    #[test]
    fn alternate_cleanup_entry_and_protected_cleanup_are_rejected() {
        let mut alternate = fixture(false, false);
        alternate.methods[2].code.as_mut().unwrap().instructions[3] = 0x0428;
        assert!(render_method("sample.Example", &alternate, &alternate.methods[2]).is_err());

        let mut overlap = fixture(false, false);
        overlap.methods[2].code.as_mut().unwrap().try_regions[0].end = 10;
        assert!(render_method("sample.Example", &overlap, &overlap.methods[2]).is_err());
    }

    #[test]
    fn normal_handler_predecessor_has_no_caught_identity_authority() {
        let class = fixture(false, false);
        let failure = baseline_failure(&class);
        let mut normal_entry = class;
        normal_entry.methods[2].code.as_mut().unwrap().instructions[3] = 0x0128;
        let code = normal_entry.methods[2].code.as_ref().unwrap();
        let ir = DecodedMethod::decode(code).unwrap();
        let cfg = ControlFlowGraph::build(code).unwrap();
        let handler = cfg.block_at[&4];
        assert!(!exception_only_handler_entry(&cfg, handler, 4));
        assert!(!original_ir_has_no_normal_handler_entry(&ir, 4));
        let error = match prove_first_shape(
            "sample.Example",
            &normal_entry,
            &normal_entry.methods[2],
            &failure,
        ) {
            Ok(_) => panic!("normal handler predecessor gained authority"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("same-object handler has normal entry"),
            "{error:#}"
        );
        assert!(
            prove(
                "sample.Example",
                &normal_entry,
                &normal_entry.methods[2],
                &failure
            )
            .is_err(),
            "general retry granted normal handler entry"
        );

        let mut method_entry = fixture(false, false);
        method_entry.methods[2].code.as_mut().unwrap().instructions = vec![
            0x000d, 0x0628, 0x106e, 0, 1, 0x0128, 0x000e, 0x0071, 1, 0, 0, 0x0027,
        ];
        method_entry.methods[2].code.as_mut().unwrap().try_regions[0] = DexTryRegion {
            start: 2,
            end: 5,
            catches: vec![(None, 0)].into(),
        };
        let mut entry_failure = failure;
        entry_failure.site.handler_pc = 0;
        entry_failure.site.throw_pc = 11;
        let code = method_entry.methods[2].code.as_ref().unwrap();
        let ir = DecodedMethod::decode(code).unwrap();
        let cfg = ControlFlowGraph::build(code).unwrap();
        let handler = cfg.block_at[&0];
        assert!(!exception_only_handler_entry(&cfg, handler, 0));
        assert!(!original_ir_has_no_normal_handler_entry(&ir, 0));
        let error = match prove_first_shape(
            "sample.Example",
            &method_entry,
            &method_entry.methods[2],
            &entry_failure,
        ) {
            Ok(_) => panic!("method-entry handler gained authority"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("same-object handler has normal entry"),
            "{error:#}"
        );
        assert!(
            prove(
                "sample.Example",
                &method_entry,
                &method_entry.methods[2],
                &entry_failure
            )
            .is_err(),
            "general retry granted method-entry handler authority"
        );
    }

    #[test]
    fn original_fallthrough_into_handler_has_no_authority() {
        let mut fallthrough = fixture(false, false);
        fallthrough.methods[2].code.as_mut().unwrap().instructions[3] = 0x0000;
        let ir = DecodedMethod::decode(fallthrough.methods[2].code.as_ref().unwrap()).unwrap();
        assert!(!original_ir_has_no_normal_handler_entry(&ir, 4));
    }

    #[test]
    fn dead_original_branch_to_handler_has_no_caught_identity_authority() {
        let class = fixture(false, false);
        let failure = baseline_failure(&class);
        let mut dead_entry = class;
        // PC 11 is unreachable after the terminal throw at PC 10. The signed
        // goto/8 delta -7 still targets the original handler at PC 4.
        dead_entry.methods[2]
            .code
            .as_mut()
            .unwrap()
            .instructions
            .push(0xf928);
        let code = dead_entry.methods[2].code.as_ref().unwrap();
        let ir = DecodedMethod::decode(code).unwrap();
        let cfg = ControlFlowGraph::build(code).unwrap();
        let handler = cfg.block_at[&4];
        assert!(exception_only_handler_entry(&cfg, handler, 4));
        assert!(!original_ir_has_no_normal_handler_entry(&ir, 4));
        let error = match prove_first_shape(
            "sample.Example",
            &dead_entry,
            &dead_entry.methods[2],
            &failure,
        ) {
            Ok(_) => panic!("dead original handler branch gained authority"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("same-object handler has normal entry"),
            "{error:#}"
        );
        assert!(
            prove(
                "sample.Example",
                &dead_entry,
                &dead_entry.methods[2],
                &failure
            )
            .is_err(),
            "general retry granted normal handler entry"
        );
    }

    #[test]
    fn malformed_dead_branch_targets_are_rejected_by_original_decode() {
        let mut outside = fixture(false, false);
        outside.methods[2]
            .code
            .as_mut()
            .unwrap()
            .instructions
            .push(0x7f28);
        assert!(
            DecodedMethod::decode(outside.methods[2].code.as_ref().unwrap())
                .unwrap_err()
                .to_string()
                .contains("target outside method")
        );

        let mut interior = fixture(false, false);
        interior.methods[2]
            .code
            .as_mut()
            .unwrap()
            .instructions
            .extend([0x0029, 0xfff6]);
        assert!(
            DecodedMethod::decode(interior.methods[2].code.as_ref().unwrap())
                .unwrap_err()
                .to_string()
                .contains("branch target is not an instruction boundary")
        );
    }

    #[test]
    fn extended_invokes_and_external_writes_are_outside_first_shape() {
        for opcode in 0xfa..=0xfd {
            assert!(unsupported_extended_invoke(opcode));
        }
        for opcode in [0x26, 0x4b, 0x51, 0x59, 0x5f, 0x67, 0x6d] {
            assert!(writes_external_state(opcode));
        }

        let mut class = fixture(false, false);
        add_int_field(&mut class, 0x19, true);
        class.static_values.push(DexValue::Int(0));
        class.methods[2].code.as_mut().unwrap().instructions[7..11]
            .copy_from_slice(&[0x1112, 0x0167, 0, 0x0027]);
        assert!(render_method("sample.Example", &class, &class.methods[2]).is_err());
    }

    #[test]
    fn field_reads_require_exact_own_declaration_and_staticness() {
        let mut instance = fixture(false, false);
        add_int_field(&mut instance, 0x1, false);
        instance.methods[2].code.as_mut().unwrap().instructions[7..11]
            .copy_from_slice(&[0x1152, 0, 0, 0x0027]);
        assert!(render_method("sample.Example", &instance, &instance.methods[2]).is_ok());

        let mut mismatched_static = fixture(false, false);
        add_int_field(&mut mismatched_static, 0x1, false);
        mismatched_static.methods[2]
            .code
            .as_mut()
            .unwrap()
            .instructions[7..11]
            .copy_from_slice(&[0x0160, 0, 0, 0x0027]);
        assert!(
            render_method(
                "sample.Example",
                &mismatched_static,
                &mismatched_static.methods[2]
            )
            .is_err()
        );

        let mut duplicate_name = fixture(false, false);
        add_int_field(&mut duplicate_name, 0x1, false);
        let symbols = Arc::get_mut(&mut duplicate_name.symbols).unwrap();
        let long_index = u16::try_from(symbols.types.len()).unwrap();
        symbols.types.push("J".into());
        let x_name =
            u32::try_from(symbols.strings.iter().position(|name| name == "x").unwrap()).unwrap();
        symbols.fields.push((0, long_index, x_name));
        duplicate_name.fields.push(DexField {
            declaring_type: duplicate_name.descriptor.clone(),
            name: "x".into(),
            field_type: "J".into(),
            access_flags: 0x1,
            is_static: false,
        });
        duplicate_name.methods[2]
            .code
            .as_mut()
            .unwrap()
            .instructions[7..11]
            .copy_from_slice(&[0x1152, 0, 0, 0x0027]);
        assert!(
            render_method(
                "sample.Example",
                &duplicate_name,
                &duplicate_name.methods[2]
            )
            .is_err()
        );

        let mut final_volatile = fixture(false, false);
        add_int_field(&mut final_volatile, 0x51, false);
        final_volatile.methods[2]
            .code
            .as_mut()
            .unwrap()
            .instructions[7..11]
            .copy_from_slice(&[0x1152, 0, 0, 0x0027]);
        assert!(
            render_method(
                "sample.Example",
                &final_volatile,
                &final_volatile.methods[2]
            )
            .is_err()
        );
    }

    #[test]
    fn declaration_only_body_and_ambiguous_source_header_are_rejected() {
        let mut native_body = fixture(false, false);
        native_body.methods[0].access_flags = 0x109;
        native_body.methods[2].code.as_mut().unwrap().instructions[0] = 0x0071;
        native_body.methods[2].code.as_mut().unwrap().instructions[2] = 0;
        assert!(render_method("sample.Example", &native_body, &native_body.methods[2]).is_err());
        native_body.methods[0].code = None;
        assert!(render_method("sample.Example", &native_body, &native_body.methods[2]).is_ok());

        let mut ambiguous = fixture(false, false);
        let mut duplicate = method(
            &ambiguous.descriptor,
            "f",
            0x11,
            vec![],
            1,
            1,
            0,
            vec![0x0012, 0x0027],
        );
        duplicate.return_type = "Ljava/lang/Object;".into();
        ambiguous.methods.push(duplicate);
        assert!(render_method("sample.Example", &ambiguous, &ambiguous.methods[2]).is_err());
    }

    #[test]
    fn concrete_source_header_requires_baseline_header_validation() {
        let legal = fixture(false, false);
        let mut words = 0;
        assert!(
            established_same_class_header(
                "sample.Example",
                &legal,
                "clean",
                &[],
                "V",
                true,
                &mut words,
            )
            .is_ok()
        );

        let mut malformed_varargs = fixture(false, false);
        malformed_varargs.methods[1].access_flags = 0x89;
        let mut words = 0;
        assert!(
            established_same_class_header(
                "sample.Example",
                &malformed_varargs,
                "clean",
                &[],
                "V",
                true,
                &mut words,
            )
            .is_err()
        );
        assert!(
            render_method(
                "sample.Example",
                &malformed_varargs,
                &malformed_varargs.methods[2]
            )
            .is_err()
        );

        let mut void_parameter = fixture(false, false);
        void_parameter.methods[1].parameters = vec!["V".into()];
        let args: Vec<Arc<str>> = vec!["V".into()];
        let mut words = 0;
        assert!(
            established_same_class_header(
                "sample.Example",
                &void_parameter,
                "clean",
                &args,
                "V",
                true,
                &mut words,
            )
            .is_err()
        );

        let mut oversized = fixture(false, false);
        oversized.methods[1].code.as_mut().unwrap().instructions = vec![0; 16_385];
        oversized.methods[1]
            .code
            .as_mut()
            .unwrap()
            .instructions
            .push(0x000e);
        assert!(render_method("sample.Example", &oversized, &oversized.methods[2]).is_err());
    }

    #[test]
    fn public_source_alternative_rejects_malformed_own_declarations_and_fields() {
        let mut native_body = fixture(false, false);
        native_body.methods[0].access_flags = 0x109;
        native_body.methods[2].code.as_mut().unwrap().instructions[0] = 0x0071;
        native_body.methods[2].code.as_mut().unwrap().instructions[2] = 0;
        assert!(
            super::super::render_method("sample.Example", &native_body, &native_body.methods[2])
                .is_err()
        );
        let mut varargs = fixture(false, false);
        varargs.methods[1].access_flags = 0x89;
        assert!(
            super::super::render_method("sample.Example", &varargs, &varargs.methods[2]).is_err()
        );
        let mut field = fixture(false, false);
        add_int_field(&mut field, 1, false);
        field.methods[2].code.as_mut().unwrap().instructions[7..11]
            .copy_from_slice(&[0x0160, 0, 0, 0x0027]);
        assert!(super::super::render_method("sample.Example", &field, &field.methods[2]).is_err());
    }

    #[test]
    fn public_source_alternative_rejects_ambiguous_members_and_wrong_dispatch() {
        let mut ambiguous = fixture(false, false);
        let mut duplicate = method(
            &ambiguous.descriptor,
            "f",
            0x11,
            vec![],
            1,
            1,
            0,
            vec![0x0012, 0x0027],
        );
        duplicate.return_type = "Ljava/lang/Object;".into();
        ambiguous.methods.push(duplicate);
        assert!(
            super::super::render_method("sample.Example", &ambiguous, &ambiguous.methods[2])
                .is_err()
        );
        let mut fields = fixture(false, false);
        add_int_field(&mut fields, 1, false);
        fields.fields.push(DexField {
            declaring_type: fields.descriptor.clone(),
            name: "x".into(),
            field_type: "J".into(),
            access_flags: 1,
            is_static: false,
        });
        fields.methods[2].code.as_mut().unwrap().instructions[7..11]
            .copy_from_slice(&[0x1152, 0, 0, 0x0027]);
        assert!(
            super::super::render_method("sample.Example", &fields, &fields.methods[2]).is_err()
        );
        let mut dispatch = fixture(false, false);
        dispatch.methods[2].code.as_mut().unwrap().instructions[0] = 0x0071;
        dispatch.methods[2].code.as_mut().unwrap().instructions[2] = 0;
        assert!(
            super::super::render_method("sample.Example", &dispatch, &dispatch.methods[2]).is_err()
        );
    }

    #[test]
    fn public_source_alternative_preserves_old_proof_refusal_and_caught_copy_identity() {
        for (copied, checked) in [(true, false), (false, true)] {
            let class = fixture(copied, checked);
            let failure = baseline_failure(&class);
            assert!(prove("sample.Example", &class, &class.methods[2], &failure).is_err());
            let source = super::super::render_method("sample.Example", &class, &class.methods[2])
                .unwrap()
                .source;
            assert!(source.contains("throw _rdx_caught_throw_"), "{source}");
            assert!(
                !source
                    .lines()
                    .find(|line| line.contains(" root("))
                    .unwrap()
                    .contains("throws")
            );
        }
    }

    #[test]
    fn inaccessible_platform_source_headers_have_no_authority() {
        assert!(!platform_source_accessible_for_test(
            "Lcaller/Root;",
            "Lsdk/Owner;",
            1,
            2,
        ));
        assert!(!platform_source_accessible_for_test(
            "Lcaller/Root;",
            "Lsdk/Owner;",
            0,
            1,
        ));
        assert!(platform_source_accessible_for_test(
            "Lsdk/Root;",
            "Lsdk/Owner;",
            0,
            0,
        ));
    }

    #[test]
    fn incomplete_platform_metadata_is_unknown() {
        let class = fixture(false, false);
        let hierarchy = class.symbols.hierarchy.get().unwrap();
        assert!(
            hierarchy
                .exact_platform_source_declaration(
                    &class.descriptor,
                    "Lmissing/Owner;",
                    "call",
                    &[],
                    "V",
                    false,
                )
                .is_none()
        );
    }
}
