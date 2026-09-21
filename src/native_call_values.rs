//! Attach declared call types to stable SSA word identities. These are type
//! constraints, not inferred types or virtual-dispatch resolution.
use crate::native_calls::{BoundCall, BoundCalls, TypedRegister};
use crate::native_ssa::{SsaMethod, SsaOperand, ValueId};
use anyhow::{Context, Result, ensure};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedValue {
    pub descriptor: Arc<str>,
    pub words: Vec<ValueId>,
}
#[derive(Debug)]
pub struct CallValues {
    pub pc: usize,
    pub receiver: Option<TypedValue>,
    pub arguments: Vec<TypedValue>,
    pub result: Option<TypedValue>,
}
#[derive(Debug, Default)]
pub struct SsaCalls {
    pub calls: Vec<CallValues>,
    pub unreachable_calls: usize,
}

impl SsaCalls {
    pub fn bind(bound: &BoundCalls, ssa: &SsaMethod) -> Result<Self> {
        ensure!(bound.calls.len() <= 1_000_000, "SSA call budget exceeded");
        let at: HashMap<_, _> = ssa.instructions.iter().map(|i| (i.pc, i)).collect();
        let mut output = Self::default();
        let mut words_used = 0usize;
        for call in &bound.calls {
            let Some(instruction) = at.get(&call.pc) else {
                output.unreachable_calls += 1;
                continue;
            };
            let raw: Vec<_> = instruction.reads.iter().flat_map(words).collect();
            let mut cursor = 0;
            let mut take = |register: &TypedRegister| -> Result<TypedValue> {
                let width = width(&register.descriptor);
                words_used += width;
                ensure!(words_used <= 1_000_000, "SSA call word budget exceeded");
                let slice = raw
                    .get(cursor..cursor + width)
                    .context("missing SSA call argument")?;
                for (offset, &(reg, _)) in slice.iter().enumerate() {
                    ensure!(
                        usize::from(reg) == usize::from(register.register) + offset,
                        "SSA call argument register mismatch"
                    );
                }
                cursor += width;
                Ok(TypedValue {
                    descriptor: register.descriptor.clone(),
                    words: slice.iter().map(|w| w.1).collect(),
                })
            };
            let receiver = call.receiver.as_ref().map(&mut take).transpose()?;
            let arguments = call
                .arguments
                .iter()
                .map(&mut take)
                .collect::<Result<Vec<_>>>()?;
            ensure!(cursor == raw.len(), "unconsumed SSA call argument words");
            let result = bind_result(call, &at)?;
            words_used += result.as_ref().map_or(0, |v| v.words.len());
            ensure!(words_used <= 1_000_000, "SSA call word budget exceeded");
            output.calls.push(CallValues {
                pc: call.pc,
                receiver,
                arguments,
                result,
            });
        }
        Ok(output)
    }
}
fn width(descriptor: &str) -> usize {
    if matches!(descriptor, "J" | "D") {
        2
    } else {
        1
    }
}
fn words(operand: &SsaOperand) -> impl Iterator<Item = (u16, ValueId)> + '_ {
    operand
        .words
        .iter()
        .enumerate()
        .map(|(offset, &value)| (operand.register + offset as u16, value))
}
fn bind_result(
    call: &BoundCall,
    at: &HashMap<usize, &crate::native_ssa::SsaInstruction>,
) -> Result<Option<TypedValue>> {
    let Some(result) = &call.result else {
        return Ok(None);
    };
    let instruction = at
        .get(&result.move_pc)
        .context("SSA call result instruction missing")?;
    ensure!(
        instruction.writes.len() == 1,
        "SSA call result write count mismatch"
    );
    let operand = &instruction.writes[0];
    ensure!(
        operand.register == result.register.register
            && operand.words.len() == width(&result.register.descriptor),
        "SSA call result register mismatch"
    );
    Ok(Some(TypedValue {
        descriptor: result.register.descriptor.clone(),
        words: operand.words.clone(),
    }))
}
