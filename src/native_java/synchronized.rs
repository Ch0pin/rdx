//! Monitor-region reconstruction guided by JADX SynchronizedRegionMaker
//! (Apache-2.0), commit 28ff15e4ae69950aebea110a13e5ab895d234dfc.
//! Proven unchanged lock ownership, normal releases and catch-all rethrow.
//! Typed handlers may remain inside a monitor when their effects share its
//! cleanup. Reject unmatched, nested and externally entered monitor regions.
use super::*;
use crate::{native_dex::DexCode, native_ir::DecodedMethod};
use std::collections::{HashMap, HashSet};

#[derive(Clone)]
pub(super) struct Region {
    pub enter: usize,
    pub exit: usize,
    pub lock: usize,
    pub tries: Vec<usize>,
    pub terminal: bool,
    pub inner_tries: Vec<usize>,
    pub releases: HashSet<usize>,
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
            primary.catches.last().is_some_and(|(ty, _)| ty.is_none())
                && primary.catches[..primary.catches.len() - 1]
                    .iter()
                    .all(|(ty, _)| ty.is_some()),
            "monitor requires a final catch-all release handler"
        );
        let handler = primary.catches.last().unwrap().1 as usize;
        let dispatches_to_monitor = |region: &crate::native_dex::DexTryRegion| {
            region
                .catches
                .last()
                .is_some_and(|(ty, pc)| ty.is_none() && *pc as usize == handler)
                && region.catches[..region.catches.len() - 1]
                    .iter()
                    .all(|(ty, _)| ty.is_some())
        };
        let first = at.get(&handler).context("monitor handler boundary")?;
        ensure!(
            first.opcode == 0x0d,
            "monitor handler must capture exception"
        );
        let caught = words[handler] >> 8;
        let mut exception_aliases = HashSet::from([usize::from(caught)]);
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
                0x00 => pc += insn.width,
                0x07..=0x09 => {
                    let (dst, src) = match insn.opcode {
                        0x07 => (((words[pc] >> 8) & 15) as usize, (words[pc] >> 12) as usize),
                        0x08 => ((words[pc] >> 8) as usize, words[pc + 1] as usize),
                        _ => (words[pc + 1] as usize, words[pc + 2] as usize),
                    };
                    ensure!(
                        dst != lock && exception_aliases.contains(&src),
                        "monitor cleanup move does not preserve exception"
                    );
                    exception_aliases.insert(dst);
                    pc += insn.width;
                }
                0x28..=0x2a => pc = graph.targets[pc].context("monitor cleanup jump")?,
                0x1e if release.is_none() && usize::from(words[pc] >> 8) == lock => {
                    release = Some(pc);
                    pc += insn.width;
                }
                0x27 if release.is_some()
                    && exception_aliases.contains(&usize::from(words[pc] >> 8)) =>
                {
                    break;
                }
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
                    && (code.try_regions.iter().any(|region| {
                        dispatches_to_monitor(region)
                            && region.start as usize <= pc
                            && pc < region.end as usize
                    }) || !insn.may_throw
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
            // A typed handler inside the monitor owns the same lock. Traverse
            // exceptional entries as well as normal edges so its entire body
            // must satisfy the same release and dispatch invariants.
            for region in code.try_regions.iter().filter(|r| {
                dispatches_to_monitor(r) && r.start as usize <= pc && pc < r.end as usize
            }) {
                pending.extend(
                    region
                        .catches
                        .iter()
                        .filter_map(|(ty, handler)| ty.as_ref().map(|_| *handler as usize)),
                );
            }
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
        ensure!(!exits.is_empty(), "monitor requires a normal release");
        // Earlier releases may end in a bare return; one final release may
        // continue outside the synchronized block. Returning a register performs
        // no effect between the explicit release and Java's implicit release.
        let last_release = *exits.iter().max().unwrap();
        let mut terminal = exits.len() > 1;
        let mut exit = last_release;
        for release in &exits {
            let mut tail = Vec::new();
            let mut next = release + graph.widths[*release];
            let returned = loop {
                if tail.len() >= 16 || tail.contains(&next) {
                    break None;
                }
                let instruction = at.get(&next).context("monitor return boundary")?;
                tail.push(next);
                match instruction.opcode {
                    0x0e..=0x11 => break Some(next),
                    0x00 => next += instruction.width,
                    0x28..=0x2a => next = graph.targets[next].context("monitor return jump")?,
                    _ => break None,
                }
            };
            if let Some(returned) = returned.filter(|_| exits.len() > 1) {
                body.extend(tail);
                exit = exit.max(returned);
            } else {
                ensure!(
                    *release == last_release,
                    "multiple monitor releases require immediate returns"
                );
                terminal = false;
            }
        }
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
        let mut inner_tries = Vec::new();
        for (index, region) in code.try_regions.iter().enumerate() {
            if !dispatches_to_monitor(region) {
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
            if region.catches.len() == 1 {
                tries.push(index);
            } else {
                inner_tries.push(index);
            }
        }
        result.push(Region {
            enter: enter.pc,
            exit,
            lock,
            tries,
            terminal,
            inner_tries,
            releases: exits,
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
) -> Result<(Vec<Option<Value>>, bool)> {
    let lock = register(&regs, region.lock)?;
    ensure!(reference(&lock.ty), "monitor requires a reference value");
    let mut body = Output {
        sequence: out.sequence,
        indent: out.indent + 1,
        ..Default::default()
    };
    // Remove only the proven outer monitor cleanup from nested typed tries.
    // Java synchronized supplies that final catch-all on both body and typed
    // handler exceptions. Keep indices stable for the graph's claimed regions.
    let lowered;
    let rendered_method = if region.inner_tries.is_empty() {
        method
    } else {
        let code = method.code.as_ref().context("monitor code")?;
        lowered = DexMethod {
            declaring_type: method.declaring_type.clone(),
            name: method.name.clone(),
            return_type: method.return_type.clone(),
            parameters: method.parameters.clone(),
            thrown_types: method.thrown_types.clone(),
            access_flags: method.access_flags,
            code: Some(DexCode {
                registers: code.registers,
                ins: code.ins,
                outs: code.outs,
                tries: code.tries,
                instructions: code.instructions.clone(),
                offset: code.offset,
                try_regions: code
                    .try_regions
                    .iter()
                    .enumerate()
                    .map(|(i, r)| crate::native_dex::DexTryRegion {
                        start: r.start,
                        end: r.end,
                        catches: if region.inner_tries.contains(&i) {
                            r.catches[..r.catches.len() - 1].to_vec().into()
                        } else {
                            r.catches.clone()
                        },
                    })
                    .collect(),
            }),
        };
        &lowered
    };
    let (values, terminal) = render(
        class,
        rendered_method,
        graph,
        region.enter + 1,
        region.exit + usize::from(region.terminal),
        regs.clone(),
        &mut body,
        depth + 1,
        true,
        None,
        None,
    )?;
    ensure!(
        terminal == region.terminal,
        "monitor normal release is unreachable"
    );
    out.sequence = body.sequence;
    if terminal {
        out.line(&format!("synchronized ({}) {{", lock.text), &[]);
        out.append(body);
        out.line("}", &[]);
        return Ok((regs, true));
    }
    let mut paths = vec![(0, body, values, false)];
    merge_path_registers(&mut regs, &mut paths, out, graph, region.exit + 1)?;
    out.line(&format!("synchronized ({}) {{", lock.text), &[]);
    out.append(paths.remove(0).1);
    out.line("}", &[]);
    Ok((regs, false))
}
