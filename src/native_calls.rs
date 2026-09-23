//! Signature-aware binding for DEX invokes and filled-array construction.
//!
//! Invoke and filled-array operand typing, including polymorphic effective
//! prototypes, is adapted from JADX's Apache-2.0 licensed `InsnDecoder` at
//! commit 28ff15e4ae69950aebea110a13e5ab895d234dfc.  Unlike JADX, this pass does
//! not resolve methods or infer runtime receiver types.  It retains exact pool
//! indices and descriptors and rejects call-site invokes because the native DEX
//! parser does not retain their metadata.

use crate::native_dex::{DexCode, DexSymbols};
use crate::native_ir::{DecodedMethod, Instruction, PoolKind};
use anyhow::{Context, Result, bail, ensure};
use std::collections::HashSet;
use std::sync::Arc;

const MAX_BOUND_ARGUMENTS: usize = 1_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CallKind {
    Virtual,
    Super,
    Direct,
    Static,
    Interface,
    Polymorphic,
    FilledNewArray,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedRegister {
    pub register: u16,
    pub descriptor: Arc<str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundResult {
    pub register: TypedRegister,
    pub move_pc: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallTarget {
    Method {
        method_index: u32,
        declaring_type: Arc<str>,
        name: Arc<str>,
        /// The effective invocation prototype.  For polymorphic invokes this is
        /// the secondary proto operand, not the referenced method's prototype.
        prototype_index: u16,
    },
    Array {
        type_index: u32,
        array_type: Arc<str>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundCall {
    pub pc: usize,
    pub kind: CallKind,
    pub target: CallTarget,
    pub receiver: Option<TypedRegister>,
    pub arguments: Vec<TypedRegister>,
    pub return_type: Arc<str>,
    pub result: Option<BoundResult>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BoundCalls {
    pub calls: Vec<BoundCall>,
}

impl BoundCalls {
    pub fn bind(code: &DexCode, ir: &DecodedMethod, symbols: &DexSymbols) -> Result<Self> {
        ensure!(
            code.instructions.len() <= 1_000_000,
            "DEX method exceeds call-binding budget"
        );
        let forbidden_result_entries = control_flow_entries(code, ir)?;
        let mut calls = Vec::new();
        let mut consumed_results = HashSet::new();
        let mut total_arguments = 0usize;

        for (position, insn) in ir.instructions.iter().enumerate() {
            if matches!(insn.opcode, 0xfc | 0xfd) {
                bail!(
                    "invoke-custom at {} is unsupported: call-site metadata is not retained",
                    insn.pc
                );
            }
            if !matches!(insn.opcode, 0x24 | 0x25 | 0x6e..=0x72 | 0x74..=0x78 | 0xfa | 0xfb) {
                continue;
            }

            let (kind, target, receiver, arguments, return_type) =
                if matches!(insn.opcode, 0x24 | 0x25) {
                    bind_filled_array(insn, symbols)?
                } else {
                    bind_invoke(insn, symbols)?
                };
            total_arguments = total_arguments
                .checked_add(arguments.len())
                .context("bound argument count overflow")?;
            ensure!(
                total_arguments <= MAX_BOUND_ARGUMENTS,
                "DEX method exceeds bound argument budget"
            );

            let result = bind_result(
                ir.instructions.get(position + 1),
                insn,
                &return_type,
                &forbidden_result_entries,
            )?;
            if let Some(result) = &result {
                consumed_results.insert(result.move_pc);
            }
            calls.push(BoundCall {
                pc: insn.pc,
                kind,
                target,
                receiver,
                arguments,
                return_type,
                result,
            });
        }

        for insn in &ir.instructions {
            if matches!(insn.opcode, 0x0a..=0x0c) && !consumed_results.contains(&insn.pc) {
                bail!("orphan move-result at {}", insn.pc);
            }
        }
        Ok(Self { calls })
    }
}

type Binding = (
    CallKind,
    CallTarget,
    Option<TypedRegister>,
    Vec<TypedRegister>,
    Arc<str>,
);

fn bind_invoke(insn: &Instruction, symbols: &DexSymbols) -> Result<Binding> {
    let reference = insn
        .reference
        .context("invoke is missing method reference")?;
    ensure!(
        reference.kind == PoolKind::Method,
        "invoke has non-method reference at {}",
        insn.pc
    );
    bind_invoke_words(
        insn.opcode,
        insn.pc,
        reference.index,
        insn.prototype,
        insn.reads.iter().map(|word| usize::from(word.register)),
        symbols,
    )
}

/// Bind one ordinary invoke without decoding or analyzing the entire method.
/// Used by the Java emitter and the full SSA pipeline through the same binder.
/// Result placement/control-flow validation remains the caller's responsibility.
pub fn bind_invocation(
    opcode: u8,
    pc: usize,
    method_index: u32,
    registers: &[usize],
    symbols: &DexSymbols,
) -> Result<BoundCall> {
    ensure!(
        matches!(opcode, 0x6e..=0x72 | 0x74..=0x78),
        "unsupported ordinary invoke opcode"
    );
    ensure!(registers.len() <= 255, "invoke operand budget exceeded");
    ensure!(
        registers
            .iter()
            .all(|&register| u16::try_from(register).is_ok()),
        "invoke register exceeds DEX range"
    );
    let (kind, target, receiver, arguments, return_type) = bind_invoke_words(
        opcode,
        pc,
        method_index,
        None,
        registers.iter().copied(),
        symbols,
    )?;
    Ok(BoundCall {
        pc,
        kind,
        target,
        receiver,
        arguments,
        return_type,
        result: None,
    })
}

fn bind_invoke_words(
    opcode: u8,
    pc: usize,
    method_index: u32,
    prototype: Option<u16>,
    mut words: impl ExactSizeIterator<Item = usize>,
    symbols: &DexSymbols,
) -> Result<Binding> {
    let &(class_idx, declared_proto_idx, name_idx) = symbols
        .methods
        .get(method_index as usize)
        .with_context(|| format!("method index {method_index} out of bounds at {}", pc))?;
    let declaring_type = symbols
        .types
        .get(class_idx as usize)
        .with_context(|| format!("declaring type index {class_idx} out of bounds at {}", pc))?
        .clone();
    validate_reference(&declaring_type)
        .with_context(|| format!("invalid declaring type at {}", pc))?;
    let name = symbols
        .strings
        .get(name_idx as usize)
        .with_context(|| format!("method name index {name_idx} out of bounds at {}", pc))?;
    ensure!(!name.is_empty(), "empty method name at {}", pc);

    // The method_id prototype remains required pool metadata even though a
    // polymorphic instruction uses its secondary prototype for argument/result
    // binding.
    let (declared_return, declared_parameters) = symbols
        .protos
        .get(declared_proto_idx as usize)
        .with_context(|| {
            format!(
                "declared prototype index {declared_proto_idx} out of bounds at {}",
                pc
            )
        })?;
    validate_descriptor(declared_return, true)
        .with_context(|| format!("invalid declared return descriptor at {}", pc))?;
    for parameter in declared_parameters {
        validate_descriptor(parameter, false)
            .with_context(|| format!("invalid declared parameter descriptor at {}", pc))?;
    }

    let polymorphic = matches!(opcode, 0xfa | 0xfb);
    let proto_idx = if polymorphic {
        prototype.context("polymorphic invoke is missing secondary prototype")?
    } else {
        ensure!(
            prototype.is_none(),
            "non-polymorphic invoke has secondary prototype at {}",
            pc
        );
        declared_proto_idx
    };
    let (return_type, parameters) = symbols
        .protos
        .get(proto_idx as usize)
        .with_context(|| format!("prototype index {proto_idx} out of bounds at {}", pc))?;
    validate_descriptor(return_type, true)
        .with_context(|| format!("invalid return descriptor at {}", pc))?;
    for parameter in parameters {
        validate_descriptor(parameter, false)
            .with_context(|| format!("invalid parameter descriptor at {}", pc))?;
    }

    let is_static = matches!(opcode, 0x71 | 0x77);
    let kind = match opcode {
        0x6e | 0x74 => CallKind::Virtual,
        0x6f | 0x75 => CallKind::Super,
        0x70 | 0x76 => CallKind::Direct,
        0x71 | 0x77 => CallKind::Static,
        0x72 | 0x78 => CallKind::Interface,
        0xfa | 0xfb => CallKind::Polymorphic,
        _ => unreachable!(),
    };

    let receiver = if is_static {
        None
    } else {
        let word = words
            .next()
            .with_context(|| format!("invoke receiver missing at {}", pc))?;
        Some(TypedRegister {
            register: u16::try_from(word).context("invoke register exceeds DEX range")?,
            descriptor: declaring_type.clone(),
        })
    };
    // Check against the already bounded physical operand list before reserving
    // from potentially hostile prototype metadata.  Wide parameters consume
    // two words, so this inexpensive count is only an upper bound.
    ensure!(
        parameters.len() <= words.len(),
        "too few invoke argument words at {}",
        pc
    );
    let mut arguments = Vec::with_capacity(parameters.len());
    for parameter in parameters {
        let first = words
            .next()
            .with_context(|| format!("too few invoke argument words at {}", pc))?;
        if descriptor_words(parameter) == 2 {
            let second = words
                .next()
                .with_context(|| format!("wide invoke argument is truncated at {}", pc))?;
            ensure!(
                first.checked_add(1) == Some(second),
                "wide invoke argument is not a contiguous register pair at {}",
                pc
            );
        }
        arguments.push(TypedRegister {
            register: u16::try_from(first).context("invoke register exceeds DEX range")?,
            descriptor: parameter.clone(),
        });
    }
    ensure!(
        words.next().is_none(),
        "too many invoke argument words at {}",
        pc
    );

    Ok((
        kind,
        CallTarget::Method {
            method_index,
            declaring_type,
            name: Arc::from(name.as_str()),
            prototype_index: proto_idx,
        },
        receiver,
        arguments,
        return_type.clone(),
    ))
}

fn bind_filled_array(insn: &Instruction, symbols: &DexSymbols) -> Result<Binding> {
    let reference = insn
        .reference
        .context("filled-new-array is missing type reference")?;
    ensure!(
        reference.kind == PoolKind::Type,
        "filled-new-array has non-type reference at {}",
        insn.pc
    );
    let array_type = symbols
        .types
        .get(reference.index as usize)
        .with_context(|| {
            format!(
                "array type index {} out of bounds at {}",
                reference.index, insn.pc
            )
        })?
        .clone();
    validate_descriptor(&array_type, false)
        .with_context(|| format!("invalid array descriptor at {}", insn.pc))?;
    let element = array_type
        .strip_prefix('[')
        .with_context(|| format!("filled-new-array type is not an array at {}", insn.pc))?;
    ensure!(
        element != "J" && element != "D",
        "filled-new-array cannot contain wide elements at {}",
        insn.pc
    );
    let element: Arc<str> = Arc::from(element);
    let arguments = insn
        .reads
        .iter()
        .map(|word| TypedRegister {
            register: word.register,
            descriptor: element.clone(),
        })
        .collect();
    Ok((
        CallKind::FilledNewArray,
        CallTarget::Array {
            type_index: reference.index,
            array_type: array_type.clone(),
        },
        None,
        arguments,
        array_type,
    ))
}

fn bind_result(
    next: Option<&Instruction>,
    producer: &Instruction,
    return_type: &Arc<str>,
    forbidden: &HashSet<usize>,
) -> Result<Option<BoundResult>> {
    let Some(next) = next.filter(|next| {
        next.pc == producer.pc + producer.width && matches!(next.opcode, 0x0a..=0x0c)
    }) else {
        return Ok(None);
    };
    ensure!(
        return_type.as_ref() != "V",
        "void call at {} is followed by move-result",
        producer.pc
    );
    ensure!(
        !forbidden.contains(&next.pc),
        "control flow enters move-result at {}",
        next.pc
    );
    let expected = result_opcode(return_type);
    ensure!(
        next.opcode == expected,
        "move-result opcode type mismatch at {}",
        next.pc
    );
    let destination = next
        .writes
        .first()
        .context("move-result has no destination")?;
    ensure!(
        next.writes.len() == 1,
        "move-result has multiple destinations at {}",
        next.pc
    );
    Ok(Some(BoundResult {
        register: TypedRegister {
            register: destination.register,
            descriptor: return_type.clone(),
        },
        move_pc: next.pc,
    }))
}

fn result_opcode(descriptor: &str) -> u8 {
    match descriptor.as_bytes()[0] {
        b'J' | b'D' => 0x0b,
        b'L' | b'[' => 0x0c,
        _ => 0x0a,
    }
}

fn descriptor_words(descriptor: &str) -> usize {
    usize::from(matches!(descriptor, "J" | "D")) + 1
}

fn validate_reference(descriptor: &str) -> Result<()> {
    validate_descriptor(descriptor, false)?;
    ensure!(
        descriptor.starts_with('L') || descriptor.starts_with('['),
        "method declaring type is not a reference descriptor"
    );
    Ok(())
}

fn validate_descriptor(descriptor: &str, allow_void: bool) -> Result<()> {
    ensure!(!descriptor.is_empty(), "empty descriptor");
    let bytes = descriptor.as_bytes();
    let mut arrays = 0usize;
    while bytes.get(arrays) == Some(&b'[') {
        arrays += 1;
    }
    ensure!(arrays <= 255, "array descriptor exceeds 255 dimensions");
    let tail = &descriptor[arrays..];
    if arrays != 0 {
        ensure!(tail != "V", "void array element descriptor");
    }
    let object_name_valid = tail
        .strip_prefix('L')
        .and_then(|name| name.strip_suffix(';'))
        .is_some_and(|name| {
            !name.is_empty()
                && name.split('/').all(|segment| !segment.is_empty())
                && !name.contains(['.', ';', '['])
        });
    let valid = matches!(tail, "Z" | "B" | "S" | "C" | "I" | "J" | "F" | "D")
        || (allow_void && arrays == 0 && tail == "V")
        || object_name_valid;
    ensure!(valid, "malformed descriptor {descriptor:?}");
    Ok(())
}

fn control_flow_entries(code: &DexCode, ir: &DecodedMethod) -> Result<HashSet<usize>> {
    let mut entries = HashSet::new();
    for insn in &ir.instructions {
        if let Some(target) = insn.branch_target {
            entries.insert(target);
        }
        if matches!(insn.opcode, 0x2b | 0x2c) {
            let payload = insn.payload_target.context("switch has no payload")?;
            let count = usize::from(
                *code
                    .instructions
                    .get(payload + 1)
                    .context("truncated switch payload")?,
            );
            let offsets = if insn.opcode == 0x2b {
                payload + 4
            } else {
                payload + 2 + count * 2
            };
            for index in 0..count {
                let pos = offsets
                    .checked_add(index * 2)
                    .context("switch payload overflow")?;
                let lo = u32::from(
                    *code
                        .instructions
                        .get(pos)
                        .context("truncated switch target")?,
                );
                let hi = u32::from(
                    *code
                        .instructions
                        .get(pos + 1)
                        .context("truncated switch target")?,
                );
                let delta = (lo | hi << 16) as i32;
                let target = insn.pc as i64 + i64::from(delta);
                ensure!(
                    target >= 0 && target < code.instructions.len() as i64,
                    "switch target outside method"
                );
                entries.insert(target as usize);
            }
        }
    }
    for region in &code.try_regions {
        for &(_, handler) in region.catches.iter() {
            entries.insert(handler as usize);
        }
    }
    Ok(entries)
}
