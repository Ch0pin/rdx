//! Writes confined to a nonthrowing return path cannot change a catch input.
use crate::{native_dex::DexCode, native_ir::DecodedMethod};

pub(super) fn written_in(code: &DexCode, start: usize, end: usize) -> Option<Vec<bool>> {
    if start > end || end > code.instructions.len() || code.instructions.len() > 32768 {
        return None;
    }
    let ir = DecodedMethod::decode(code).ok()?;
    let bare_return = |mut pc: usize| {
        for _ in 0..32 {
            let Ok(index) = ir.instructions.binary_search_by_key(&pc, |i| i.pc) else {
                return false;
            };
            let i = &ir.instructions[index];
            if i.may_throw {
                return false;
            }
            match i.opcode {
                0x0e..=0x11 => return true,
                0x28..=0x2a => {
                    let Some(target) = i.branch_target else {
                        return false;
                    };
                    pc = target;
                }
                _ => return false,
            }
        }
        false
    };
    let mut next = start;
    let mut written = vec![false; usize::from(code.registers)];
    for i in ir
        .instructions
        .iter()
        .filter(|i| (start..end).contains(&i.pc))
    {
        if i.pc != next || i.pc + i.width > end {
            return None;
        }
        next += i.width;
        if !i.may_throw && matches!(i.opcode, 0x01..=0x09 | 0x12..=0x19) && bare_return(next) {
            continue;
        }
        for operand in &i.writes {
            let r = usize::from(operand.register);
            written
                .get_mut(r..r + operand.kind.word_count())?
                .fill(true);
        }
    }
    (next == end).then_some(written)
}
