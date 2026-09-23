//! Monitor-region reconstruction guided by JADX SynchronizedRegionMaker
//! (Apache-2.0), commit 28ff15e4ae69950aebea110a13e5ab895d234dfc.
//! Initial subset: unchanged lock register, one normal release, catch-all
//! release/rethrow. Reject unmatched, nested and externally entered regions.
use super::*;
use crate::{native_dex::DexCode, native_ir::DecodedMethod};
use std::collections::{HashMap, HashSet};

#[derive(Clone)]
pub(super) struct Region {
    pub enter: usize,
    pub exit: usize,
    pub lock: usize,
    pub tries: Vec<usize>,
}

fn successors(graph: &Graph, words: &[u16], pc: usize) -> Vec<usize> {
    match words[pc] as u8 {
        0x0e..=0x11 | 0x27 => vec![],
        0x28..=0x2a => graph.targets[pc].into_iter().collect(),
        _ => {
            let mut edges = vec![pc + graph.widths[pc]];
            edges.extend(graph.targets[pc]);
            if let Some(switch) = &graph.switches[pc] {
                edges.extend(switch.cases.iter().map(|(_, target)| *target));
            }
            edges
        }
    }
}

pub(super) fn analyze(code: &DexCode, graph: &Graph) -> Result<Vec<Region>> {
    if !graph
        .widths
        .iter()
        .enumerate()
        .any(|(pc, width)| *width != 0 && code.instructions[pc] as u8 == 0x1d)
    {
        return Ok(vec![]);
    }
    let ir = DecodedMethod::decode(code)?;
    let at: HashMap<_, _> = ir.instructions.iter().map(|insn| (insn.pc, insn)).collect();
    let words = &code.instructions;
    let mut result = Vec::new();
    let mut claimed = HashSet::new();
    for enter in ir.instructions.iter().filter(|insn| insn.opcode == 0x1d) {
        ensure!(result.len() < 32, "synchronized region budget exceeded");
        let lock = usize::from(words[enter.pc] >> 8);
        let start = enter.pc + enter.width;
        let primary = code
            .try_regions
            .iter()
            .find(|region| region.start as usize == start)
            .context("monitor body has no immediate protected region")?;
        ensure!(
            primary.catches.len() == 1 && primary.catches[0].0.is_none(),
            "monitor requires a sole catch-all release handler"
        );
        let handler = primary.catches[0].1 as usize;
        let first = at.get(&handler).context("monitor handler boundary")?;
        ensure!(
            first.opcode == 0x0d,
            "monitor handler must capture exception"
        );
        let caught = words[handler] >> 8;
        ensure!(
            usize::from(caught) != lock,
            "monitor handler overwrites lock"
        );
        let mut cleanup = HashSet::from([handler]);
        let mut pc = handler + first.width;
        let mut release = None;
        loop {
            ensure!(
                cleanup.len() < 8 && cleanup.insert(pc),
                "monitor cleanup cycle or budget"
            );
            let insn = at.get(&pc).context("monitor cleanup boundary")?;
            match insn.opcode {
                0x28..=0x2a => pc = graph.targets[pc].context("monitor cleanup jump")?,
                0x1e if release.is_none() && usize::from(words[pc] >> 8) == lock => {
                    release = Some(pc);
                    pc += insn.width;
                }
                0x27 if release.is_some() && words[pc] >> 8 == caught => break,
                _ => bail!("monitor handler is not exact release/rethrow"),
            }
        }
        let mut body = HashSet::new();
        let mut pending = vec![start];
        let mut exits = HashSet::new();
        while let Some(pc) = pending.pop() {
            if !body.insert(pc) {
                continue;
            }
            ensure!(body.len() <= 65_536, "monitor body budget exceeded");
            let insn = at.get(&pc).context("monitor body boundary")?;
            ensure!(!cleanup.contains(&pc), "normal flow enters monitor cleanup");
            // DEX may end protection before a nonthrowing loop latch or the
            // proven normal release. Every other throwing instruction must
            // still dispatch to the exact cleanup handler.
            ensure!(
                pc >= start
                    && (pc < primary.end as usize
                        || !insn.may_throw
                        || (insn.opcode == 0x1e && usize::from(words[pc] >> 8) == lock)),
                "throwing monitor body instruction escapes protected range"
            );
            ensure!(
                !insn.writes.iter().any(|value| {
                    let first = usize::from(value.register);
                    first <= lock && lock < first + value.kind.word_count()
                }),
                "monitor lock register overwritten"
            );
            match insn.opcode {
                0x1e => {
                    ensure!(
                        usize::from(words[pc] >> 8) == lock,
                        "monitor releases a different lock"
                    );
                    exits.insert(pc);
                }
                0x1d => bail!("nested monitor regions not reconstructed"),
                0x0e..=0x11 => bail!("return bypasses monitor release"),
                _ => pending.extend(successors(graph, words, pc)),
            }
        }
        ensure!(exits.len() == 1, "monitor requires one normal release");
        let exit = *exits.iter().next().unwrap();
        ensure!(
            body.iter().all(|pc| *pc <= exit),
            "monitor body crosses release layout"
        );
        for insn in &ir.instructions {
            for target in successors(graph, words, insn.pc) {
                ensure!(
                    !body.contains(&target)
                        || body.contains(&insn.pc)
                        || (insn.pc == enter.pc && target == start),
                    "external edge enters monitor body"
                );
                ensure!(
                    !cleanup.contains(&target) || cleanup.contains(&insn.pc),
                    "normal edge enters monitor handler"
                );
            }
        }
        let mut tries = Vec::new();
        for (index, region) in code.try_regions.iter().enumerate() {
            if region.catches.as_ref() != primary.catches.as_ref() {
                continue;
            }
            ensure!(
                !claimed.contains(&index),
                "shared monitor cleanup across regions"
            );
            for insn in ir
                .instructions
                .iter()
                .filter(|insn| insn.pc >= region.start as usize && insn.pc < region.end as usize)
            {
                ensure!(
                    !insn.may_throw || body.contains(&insn.pc) || Some(insn.pc) == release,
                    "monitor catch-all protects unrelated effects"
                );
            }
            claimed.insert(index);
            tries.push(index);
        }
        result.push(Region {
            enter: enter.pc,
            exit,
            lock,
            tries,
        });
    }
    Ok(result)
}

pub(super) fn emit(
    class: &DexClass,
    method: &DexMethod,
    graph: &Graph,
    region: &Region,
    mut regs: Vec<Option<Value>>,
    out: &mut Output,
    depth: usize,
) -> Result<Vec<Option<Value>>> {
    let lock = register(&regs, region.lock)?;
    ensure!(reference(&lock.ty), "monitor requires a reference value");
    let mut body = Output {
        sequence: out.sequence,
        indent: out.indent + 1,
        ..Default::default()
    };
    let (values, terminal) = render(
        class,
        method,
        graph,
        region.enter + 1,
        region.exit,
        regs.clone(),
        &mut body,
        depth + 1,
        true,
        None,
        None,
    )?;
    ensure!(!terminal, "monitor normal release is unreachable");
    out.sequence = body.sequence;
    let mut paths = vec![(0, body, values, false)];
    merge_path_registers(&mut regs, &mut paths, out, graph, region.exit + 1)?;
    out.line(&format!("synchronized ({}) {{", lock.text), &[]);
    out.append(paths.remove(0).1);
    out.line("}", &[]);
    Ok(regs)
}
