//! Bounded proof about generated Java declarations, not runtime effects.
//! Null throws and exact monitor-cleanup rethrows cannot introduce a new
//! checked throws header. Every other explicit throw keeps the old boundary.
use super::SneakyMethodAnalysis;
use crate::{
    native_dex::{DexClass, DexMethod},
    native_ssa::DefinitionKind,
};
use std::collections::{HashMap, HashSet};

pub(super) fn closed(class: &DexClass, method: &DexMethod, budget: &mut usize) -> bool {
    let Some(code) = &method.code else {
        return false;
    };
    if code.instructions.len() > 4096 || code.registers > 256 {
        return false;
    }
    let local = (code
        .instructions
        .len()
        .saturating_mul(usize::from(code.registers).max(1))
        .saturating_mul(8))
    .clamp(1024, 1_000_000);
    let charge = local * 3;
    if charge > *budget {
        return false;
    }
    *budget -= charge;
    let Ok(analysis) = SneakyMethodAnalysis::build(class, method, local) else {
        return false;
    };
    let ir: HashMap<_, _> = analysis.ir.instructions.iter().map(|i| (i.pc, i)).collect();
    let uses: HashMap<_, _> = analysis
        .ssa
        .instructions
        .iter()
        .map(|i| (i.pc, i))
        .collect();
    let phis: HashMap<_, _> = analysis.ssa.phis.iter().map(|p| (p.result, p)).collect();
    let mut work = 65_536usize;
    for instruction in analysis.ir.instructions.iter().filter(|i| i.opcode == 0x27) {
        let Some(read) = uses.get(&instruction.pc).and_then(|i| i.reads.first()) else {
            return false;
        };
        if read.words.len() != 1 {
            return false;
        }
        // The renderer either emits synchronized or rejects this shape. The
        // unchanged catch parameter is not eligible for inferred declarations.
        let monitor_rethrow = instruction.pc.checked_sub(2).is_some_and(|handler| {
            ir.get(&handler)
                .is_some_and(|i| i.opcode == 0x0d && i.width == 1)
                && ir
                    .get(&(handler + 1))
                    .is_some_and(|i| i.opcode == 0x1e && i.width == 1)
                && code.try_regions.iter().any(|r| {
                    r.catches
                        .iter()
                        .any(|(ty, pc)| ty.is_none() && *pc as usize == handler)
                })
                && uses
                    .get(&handler)
                    .and_then(|i| i.writes.first())
                    .is_some_and(|w| w.words == read.words)
        });
        if monitor_rethrow {
            continue;
        }
        let mut pending = read.words.clone();
        let mut seen = HashSet::new();
        let mut literal = false;
        while let Some(id) = pending.pop() {
            if work == 0 {
                return false;
            }
            work -= 1;
            if !seen.insert(id) {
                continue;
            }
            match analysis.ssa.definitions.get(id).map(|d| &d.kind) {
                Some(DefinitionKind::Instruction { pc, word: 0, .. }) => {
                    let Some(i) = ir.get(pc) else { return false };
                    match i.opcode {
                        0x12..=0x15
                            if i.literal == Some(0)
                                && i.writes.len() == 1
                                && i.writes[0].kind.word_count() == 1 =>
                        {
                            literal = true;
                        }
                        0x01..=0x03 | 0x07..=0x09 => {
                            let Some(u) = uses.get(pc) else { return false };
                            if u.reads.len() != 1
                                || u.reads[0].words.len() != 1
                                || u.writes.len() != 1
                                || u.writes[0].words.len() != 1
                            {
                                return false;
                            }
                            pending.extend(&u.reads[0].words);
                        }
                        _ => return false,
                    }
                }
                Some(DefinitionKind::Phi { .. }) => {
                    let Some(phi) = phis.get(&id) else {
                        return false;
                    };
                    let mut incoming = false;
                    for (from, value) in &phi.incoming {
                        if from.is_some_and(|b| {
                            !analysis.ssa.reachable.get(b).copied().unwrap_or(false)
                        }) {
                            continue;
                        }
                        incoming = true;
                        pending.push(*value);
                    }
                    if !incoming {
                        return false;
                    }
                }
                _ => return false,
            }
        }
        if !literal {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_dex::{DexCode, DexSymbols, DexTryRegion};
    use std::sync::Arc;
    fn fixture(
        words: Vec<u16>,
        registers: u16,
        parameters: Vec<Arc<str>>,
        tries: Vec<DexTryRegion>,
    ) -> DexClass {
        DexClass {
            descriptor: "Lsample/Headers;".into(),
            superclass: Some("Ljava/lang/Object;".into()),
            interfaces: vec![],
            access_flags: 0x11,
            annotations_offset: 0,
            static_values_offset: 0,
            static_values: vec![],
            fields: vec![],
            symbols: Arc::new(DexSymbols::default()),
            methods: vec![DexMethod {
                declaring_type: "Lsample/Headers;".into(),
                name: "run".into(),
                return_type: "V".into(),
                access_flags: 9,
                thrown_types: vec![],
                code: Some(DexCode {
                    registers,
                    ins: parameters.len() as u16,
                    outs: 0,
                    tries: tries.len() as u16,
                    try_regions: tries,
                    instructions: words,
                    offset: 0,
                }),
                parameters,
            }],
        }
    }
    fn proven(class: &DexClass) -> bool {
        closed(class, &class.methods[0], &mut 1_000_000)
    }
    #[test]
    fn null_alias_and_phi_are_closed_but_nonzero_and_parameters_are_not() {
        assert!(proven(&fixture(
            vec![0x0012, 0x0107, 0x0127],
            2,
            vec![],
            vec![]
        )));
        let mut phi = fixture(
            vec![0x0138, 4, 0x0012, 0x0228, 0x0012, 0x0027],
            2,
            vec!["Z".into()],
            vec![],
        );
        assert!(proven(&phi));
        phi.methods[0].code.as_mut().unwrap().instructions[4] = 0x1012;
        assert!(!proven(&phi));
        assert!(!proven(&fixture(
            vec![0x0027],
            1,
            vec!["Ljava/lang/Throwable;".into()],
            vec![]
        )));
        assert!(!proven(&fixture(
            vec![0x0016, 0, 0x0027],
            2,
            vec![],
            vec![]
        )));
    }
    #[test]
    fn monitor_catch_identity_and_work_budget_are_required() {
        let mut class = fixture(
            vec![0x011d, 0x0012, 0x011e, 0x000e, 0x000d, 0x011e, 0x0027],
            2,
            vec!["Ljava/lang/Object;".into()],
            vec![
                DexTryRegion {
                    start: 1,
                    end: 3,
                    catches: vec![(None, 4)].into(),
                },
                DexTryRegion {
                    start: 5,
                    end: 6,
                    catches: vec![(None, 4)].into(),
                },
            ],
        );
        assert!(proven(&class));
        assert!(!closed(&class, &class.methods[0], &mut 0));
        class.methods[0].code.as_mut().unwrap().instructions[6] = 0x0127;
        assert!(!proven(&class));
    }
}
