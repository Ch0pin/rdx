//! Conservative intraprocedural constant arguments at DEX invoke sites.
use crate::{
    native_cfg::{ControlFlowGraph, EdgeKind},
    native_dex::{DexClass, DexMethod},
    native_ir::{DecodedMethod, Instruction},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::atomic::{AtomicBool, Ordering},
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Constant {
    Unknown,
    String(u32),
    Bits(i64),
}
fn transfer(i: &Instruction, state: &mut [Constant]) {
    let v = match i.opcode {
        0x01..=0x03 | 0x07..=0x09 | 0x1f => i
            .reads
            .first()
            .map(|r| state[r.register as usize])
            .unwrap_or(Constant::Unknown),
        0x12..=0x15 => i.literal.map(Constant::Bits).unwrap_or(Constant::Unknown),
        0x1a | 0x1b => i
            .reference
            .map(|r| Constant::String(r.index))
            .unwrap_or(Constant::Unknown),
        _ => Constant::Unknown,
    };
    for r in &i.writes {
        for word in 0..r.kind.word_count() {
            state[r.register as usize + word] = v;
        }
    }
}
pub(crate) fn analyze(
    c: &DexClass,
    m: &DexMethod,
    cancel: &AtomicBool,
    work: &mut usize,
    wanted: &std::collections::BTreeSet<usize>,
) -> Result<BTreeMap<usize, Value>> {
    let code = m.code.as_ref().context("Method has no code")?;
    ensure!(
        code.instructions.len() <= 32768 && code.registers <= 512,
        "Argument analysis exceeds method budget"
    );
    let ir = DecodedMethod::decode(code)?;
    let cfg = ControlFlowGraph::build(code)?;
    ensure!(
        cfg.blocks.len().saturating_mul(code.registers as usize) <= 100000,
        "Argument analysis exceeds state budget"
    );
    let instructions: HashMap<_, _> = ir.instructions.iter().map(|i| (i.pc, i)).collect();
    let mut states = vec![None; cfg.blocks.len()];
    let entry = *cfg.block_at.get(&0).context("No entry block")?;
    states[entry] = Some(vec![Constant::Unknown; code.registers as usize]);
    let mut queue = VecDeque::from([entry]);
    let mut queued = vec![false; cfg.blocks.len()];
    queued[entry] = true;
    while let Some(b) = queue.pop_front() {
        queued[b] = false;
        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        let mut state = states[b].clone().unwrap();
        for pc in &cfg.blocks[b].instructions {
            *work += 1;
            ensure!(
                *work <= 2000000,
                "Argument analysis exceeds request work budget"
            );
            transfer(instructions[pc], &mut state);
        }
        for e in &cfg.blocks[b].successors {
            let incoming = if e.kind == EdgeKind::Exceptional {
                vec![Constant::Unknown; state.len()]
            } else {
                state.clone()
            };
            let changed = if let Some(old) = &mut states[e.target] {
                let mut changed = false;
                for (a, b) in old.iter_mut().zip(incoming) {
                    if *a != b && *a != Constant::Unknown {
                        *a = Constant::Unknown;
                        changed = true;
                    }
                }
                changed
            } else {
                states[e.target] = Some(incoming);
                true
            };
            if changed && !queued[e.target] {
                queued[e.target] = true;
                queue.push_back(e.target);
            }
        }
    }
    let mut out = BTreeMap::new();
    let mut output_bytes = 0;
    for (block, input) in cfg.blocks.iter().zip(states) {
        let Some(mut state) = input else {
            continue;
        };
        for pc in &block.instructions {
            ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
            let i = instructions[pc];
            if wanted.contains(pc) && matches!(i.opcode,0x6e..=0x72|0x74..=0x78) {
                let index = i.reference.context("Invoke missing method operand")?.index as usize;
                let (_, proto, _) = c
                    .symbols
                    .methods
                    .get(index)
                    .context("Invalid invoke method index")?;
                let (_, params) = c
                    .symbols
                    .protos
                    .get(*proto as usize)
                    .context("Invalid invoke prototype")?;
                let receiver = !matches!(i.opcode, 0x71 | 0x77);
                let mut word = usize::from(receiver);
                let mut args = Vec::new();
                let mut literal_bytes = 0;
                for (n, ty) in params.iter().enumerate() {
                    let reg = i
                        .reads
                        .get(word)
                        .context("Invoke argument count mismatch")?
                        .register;
                    let wide = matches!(ty.as_ref(), "J" | "D");
                    let mut item = json!({"argument_index":n,"register":reg,"descriptor":ty.as_ref(),"status":"unresolved"});
                    match state[reg as usize] {
                        Constant::String(index) if !wide => {
                            let s = c
                                .symbols
                                .strings
                                .get(index as usize)
                                .context("Invalid string operand")?;
                            if s.len() <= 4096 && literal_bytes + s.len() <= 16384 {
                                literal_bytes += s.len();
                                item["status"] = json!("constant");
                                item["kind"] = json!("string");
                                item["value"] = json!(s);
                            } else {
                                item["reason"] = json!("literal exceeds argument preview budget");
                            }
                        }
                        Constant::Bits(bits) if !wide => {
                            item["status"] = json!("constant");
                            item["kind"] = json!("bits32");
                            item["value"] = json!(bits as i32);
                        }
                        _ => {}
                    }
                    args.push(item);
                    word += if wide { 2 } else { 1 };
                }
                ensure!(word == i.reads.len(), "Invoke argument count mismatch");
                let mut record = json!({"status":"analyzed","arguments":args,"scope":"Direct string/32-bit constant loads and moves; constant only when all reachable CFG predecessors agree. Exceptional entries, wide values, arithmetic, fields, parameters and return values remain unresolved. Receiver excluded."});
                let size = serde_json::to_vec(&record)?.len();
                if output_bytes + size > 256 * 1024 {
                    record = json!({"status":"unresolved","reason":"Method argument output budget reached; request fewer call sites"});
                } else {
                    output_bytes += size;
                }
                out.insert(*pc, record);
            }
            transfer(i, &mut state);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_dex::{self, DexSymbols, DexTryRegion};
    use std::{collections::BTreeSet, sync::Arc};
    fn fixture(words: Vec<u16>) -> DexClass {
        let mut c = native_dex::parse(include_bytes!("../tests/fixtures/hello.dex"))
            .unwrap()
            .classes
            .remove(0);
        c.methods.retain(|m| m.name.as_ref() == "answer");
        c.symbols = Arc::new(DexSymbols {
            strings: vec![
                "sink".into(),
                "secret".into(),
                "other".into(),
                "decoy".into(),
            ],
            types: vec!["Lsample/Api;".into()],
            protos: vec![("V".into(), vec!["Ljava/lang/String;".into()])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        });
        let code = c.methods[0].code.as_mut().unwrap();
        code.registers = 3;
        code.ins = 1;
        code.outs = 1;
        code.instructions = words;
        c
    }
    fn at(c: &DexClass, pc: usize) -> Value {
        analyze(
            c,
            &c.methods[0],
            &AtomicBool::new(false),
            &mut 0,
            &BTreeSet::from([pc]),
        )
        .unwrap()
        .remove(&pc)
        .unwrap()
    }
    #[test]
    fn arguments_require_agreement_and_ignore_nearby_strings() {
        let mut c = fixture(vec![
            0x0238, 5, 0x001a, 1, 0x0328, 0x001a, 2, 0x011a, 3, 0x1071, 0, 0, 0x000e,
        ]);
        assert_eq!(at(&c, 9)["arguments"][0]["status"], "unresolved");
        c.methods[0].code.as_mut().unwrap().instructions[6] = 1;
        assert_eq!(at(&c, 9)["arguments"][0]["value"], "secret");
        assert!(
            analyze(
                &c,
                &c.methods[0],
                &AtomicBool::new(true),
                &mut 0,
                &BTreeSet::from([9])
            )
            .is_err()
        );
    }
    #[test]
    fn moves_preserve_values_overwrites_kill_and_exception_entries_are_unknown() {
        let mut c = fixture(vec![0x001a, 1, 0x0107, 0x001a, 3, 0x1071, 0, 1, 0x000e]);
        assert_eq!(at(&c, 5)["arguments"][0]["value"], "secret");
        c.methods[0].code.as_mut().unwrap().instructions[7] = 0;
        assert_eq!(at(&c, 5)["arguments"][0]["value"], "decoy");
        let mut c = fixture(vec![
            0x001a, 1, 0x1071, 0, 0, 0x000e, 0x010d, 0x1071, 0, 0, 0x000e,
        ]);
        let code = c.methods[0].code.as_mut().unwrap();
        code.tries = 1;
        code.try_regions = vec![DexTryRegion {
            start: 2,
            end: 5,
            catches: vec![(None, 6)].into(),
        }];
        assert_eq!(at(&c, 7)["arguments"][0]["status"], "unresolved");
        let mut budget = 2000000;
        assert!(
            analyze(
                &c,
                &c.methods[0],
                &AtomicBool::new(false),
                &mut budget,
                &BTreeSet::from([7])
            )
            .is_err()
        );
    }
}
