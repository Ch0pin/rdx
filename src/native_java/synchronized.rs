//! Monitor-region reconstruction guided by JADX SynchronizedRegionMaker
//! (Apache-2.0), commit 28ff15e4ae69950aebea110a13e5ab895d234dfc.
//! Proven unchanged lock ownership, normal releases and catch-all rethrow.
//! Typed handlers may remain inside a monitor when their effects share its
//! cleanup. Reject unmatched and externally entered monitor regions.
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
    pub dispatch_join: Option<usize>,
    pub owned: HashSet<usize>,
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

// Optimizers can place a shared pure release suffix before the final release.
// Prove the real instruction paths, rather than treating the address gap as
// code executed under the lock. Every later release has only a control bridge
// to the identical shared suffix; its writes execute after Java releases it.
fn shared_post_release_tail(
    code: &DexCode,
    graph: &Graph,
    releases: &HashSet<usize>,
    join: usize,
    body: &HashSet<usize>,
    cleanup: &HashSet<usize>,
) -> bool {
    use crate::native_cfg::{ControlFlowGraph, EdgeKind};
    let prove = || -> Option<()> {
        if code.instructions.len() > 4096
            || code.registers > 256
            || usize::from(code.tries) != code.try_regions.len()
            || code.try_regions.len() > 16
            || !(2..=16).contains(&releases.len())
            || code
                .try_regions
                .iter()
                .map(|r| r.catches.len())
                .sum::<usize>()
                > 64
        {
            return None;
        }
        let ir = DecodedMethod::decode(code).ok()?;
        let at: HashMap<_, _> = ir.instructions.iter().map(|i| (i.pc, i)).collect();
        let cfg = ControlFlowGraph::build(code).ok()?;
        if cfg.blocks.len() > 512
            || !at.contains_key(&join)
            || body.contains(&join)
            || cleanup.contains(&join)
        {
            return None;
        }
        let first = *releases.iter().min()?;
        let shared = first + at.get(&first)?.width;
        let mut paths = HashSet::new();
        let mut walk = |start: usize, end: usize, bridge: bool| -> Option<()> {
            let mut pc = start;
            let mut seen = HashSet::new();
            while pc != end {
                if seen.len() == 16
                    || !seen.insert(pc)
                    || body.contains(&pc)
                    || cleanup.contains(&pc)
                    || code
                        .try_regions
                        .iter()
                        .any(|r| r.start as usize <= pc && pc < r.end as usize)
                {
                    return None;
                }
                let i = at.get(&pc)?;
                if i.may_throw
                    || i.payload_target.is_some()
                    || !matches!(i.opcode, 0x00..=0x09 | 0x12..=0x19 | 0x28..=0x2a)
                    || (bridge && !matches!(i.opcode, 0x00 | 0x28..=0x2a))
                    || (i.opcode == 0 && code.instructions[pc] != 0)
                    || i.reads.iter().chain(&i.writes).any(|operand| {
                        usize::from(operand.register) + operand.kind.word_count()
                            > usize::from(code.registers)
                    })
                    || graph.widths.get(pc) != Some(&i.width)
                    || graph.targets[pc] != i.branch_target
                {
                    return None;
                }
                let next = i.branch_target.unwrap_or(pc + i.width);
                if !at.contains_key(&next) {
                    return None;
                }
                let block = &cfg.blocks[*cfg.block_at.get(&pc)?];
                let position = block.instructions.iter().position(|p| *p == pc)?;
                if let Some(&following) = block.instructions.get(position + 1) {
                    if next != following {
                        return None;
                    }
                } else if block.successors.len() != 1
                    || block.successors[0].kind != EdgeKind::Normal
                    || cfg.blocks[block.successors[0].target].start != next
                {
                    return None;
                }
                paths.insert(pc);
                pc = next;
            }
            Some(())
        };
        walk(shared, join, false)?;
        for &release in releases {
            let instruction = at.get(&release)?;
            if instruction.opcode != 0x1e || instruction.width != 1 {
                return None;
            }
            walk(release + instruction.width, shared, true)?;
        }
        // Ordinary paths can share this unchanged pure suffix; they retain
        // their original entry frames and are rendered independently. An
        // exceptional entry would give it a second handler ownership.
        for block in &cfg.blocks {
            for edge in &block.successors {
                if paths.contains(&cfg.blocks[edge.target].start) && edge.kind != EdgeKind::Normal {
                    return None;
                }
            }
        }
        Some(())
    };
    prove().is_some()
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
    let mut result: Vec<Region> = Vec::new();
    let mut owned_bodies: HashMap<usize, HashSet<usize>> = HashMap::new();
    let mut owned_cleanup: HashMap<usize, HashSet<usize>> = HashMap::new();
    let mut claimed = HashSet::new();
    for enter in ir
        .instructions
        .iter()
        .rev()
        .filter(|insn| insn.opcode == 0x1d)
    {
        ensure!(result.len() < 32, "synchronized region budget exceeded");
        let lock = usize::from(words[enter.pc] >> 8);
        let start = enter.pc + enter.width;
        let primary = code
            .try_regions
            .iter()
            .filter(|region| {
                region.start as usize >= start
                    && region.catches.last().is_some_and(|(ty, _)| ty.is_none())
            })
            .min_by_key(|region| region.start)
            .context("monitor body has no protected cleanup region")?;
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
            if insn.opcode == 0x1d {
                let child = result
                    .iter()
                    .find(|r| r.enter == pc)
                    .context("nested monitor region has not been proven")?;
                ensure!(
                    !child.terminal && child.dispatch_join.is_none(),
                    "nested terminal or dispatch monitor not reconstructed"
                );
                // A child's throwing effects dispatch to its own cleanup. Its
                // exact release/rethrow must in turn dispatch to this cleanup.
                for cleanup_pc in &owned_cleanup[&pc] {
                    let cleanup_insn = at[cleanup_pc];
                    ensure!(
                        !cleanup_insn.writes.iter().any(|value| {
                            let first = usize::from(value.register);
                            first <= lock && lock < first + value.kind.word_count()
                        }),
                        "nested monitor cleanup overwrites outer lock"
                    );
                    ensure!(
                        !cleanup_insn.may_throw
                            || code.try_regions.iter().any(|r| {
                                dispatches_to_monitor(r)
                                    && r.start as usize <= *cleanup_pc
                                    && *cleanup_pc < r.end as usize
                            })
                            || cleanup_insn.opcode == 0x1e,
                        "nested monitor cleanup escapes outer protected range"
                    );
                }
                for child_pc in &owned_bodies[&pc] {
                    ensure!(
                        !at[child_pc].writes.iter().any(|value| {
                            let first = usize::from(value.register);
                            first <= lock && lock < first + value.kind.word_count()
                        }),
                        "nested monitor overwrites outer lock"
                    );
                }
                body.extend(owned_bodies[&pc].iter().copied());
                pending.push(child.exit + 1);
                continue;
            }
            match insn.opcode {
                0x1e => {
                    ensure!(
                        usize::from(words[pc] >> 8) == lock,
                        "monitor releases a different lock"
                    );
                    exits.insert(pc);
                }
                0x0e..=0x11 => bail!("return bypasses monitor release"),
                _ => pending.extend(successors(graph, words, pc)),
            }
        }
        ensure!(!exits.is_empty(), "monitor requires a normal release");
        ensure!(
            body.contains(&(primary.start as usize)),
            "monitor primary protection is not reached under its lock"
        );
        // Earlier releases may end in a bare return; one final release may
        // continue outside the synchronized block. Returning a register performs
        // no effect between the explicit release and Java's implicit release.
        let last_release = *exits.iter().max().unwrap();
        let mut terminal = exits.len() > 1;
        let mut exit = last_release;
        let mut dispatch_join = None;
        let mut return_tails = HashSet::new();
        let mut all_releases_return = true;
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
            if returned.is_some() {
                return_tails.extend(tail.iter().copied());
            } else {
                all_releases_return = false;
            }
            if let Some(returned) = returned.filter(|_| exits.len() > 1) {
                body.extend(tail);
                exit = exit.max(returned);
            } else {
                if *release != last_release {
                    let mut cursor = release + graph.widths[*release];
                    let mut bridge = HashSet::new();
                    let mut effectful = false;
                    loop {
                        if cursor == last_release + graph.widths[last_release] {
                            if effectful {
                                dispatch_join = Some(cursor);
                            }
                            break;
                        }
                        ensure!(
                            bridge.len() < 256 && bridge.insert(cursor),
                            "monitor release continuation cycle or budget"
                        );
                        let instruction = at
                            .get(&cursor)
                            .context("monitor release continuation boundary")?;
                        match instruction.opcode {
                            0x28..=0x2a => {
                                let target = graph.targets[cursor]
                                    .context("monitor release continuation jump")?;
                                if target > last_release
                                    && target != last_release + graph.widths[last_release]
                                {
                                    ensure!(
                                        dispatch_join.is_none_or(|join| join == target),
                                        "monitor releases have different continuations"
                                    );
                                    dispatch_join = Some(target);
                                    break;
                                }
                                cursor = target;
                            }
                            0x0e..=0x11 | 0x27 | 0x1d | 0x1e | 0x2b..=0x3d => bail!(
                                "monitor release continuation is not bounded straight-line flow"
                            ),
                            _ => {
                                effectful |= instruction.opcode != 0;
                                cursor += instruction.width;
                            }
                        }
                    }
                    if effectful {
                        ensure!(
                            dispatch_join.is_some(),
                            "effectful monitor exits lack common continuation"
                        );
                    } else if dispatch_join.is_none() {
                        body.extend(bridge);
                    }
                }
                terminal = false;
            }
        }
        if let Some(join) = dispatch_join {
            body.retain(|pc| *pc <= last_release);
            terminal = false;
            let shared_tail = shared_post_release_tail(code, graph, &exits, join, &body, &cleanup);
            ensure!(
                join > last_release && (join - last_release <= 16 || shared_tail),
                "monitor final release continuation budget"
            );
            if shared_tail {
                // Lock ownership ends at the actual release. The dispatch
                // renderer executes proven suffixes outside synchronized and
                // the caller resumes at their explicit common join.
                exit = last_release;
            } else {
                let mut cursor = last_release + graph.widths[last_release];
                while cursor < join {
                    let insn = at
                        .get(&cursor)
                        .context("monitor final continuation boundary")?;
                    ensure!(
                        !matches!(insn.opcode, 0x0e..=0x11 | 0x27..=0x3d | 0x1d | 0x1e),
                        "monitor final continuation is not straight-line"
                    );
                    cursor += insn.width;
                }
                ensure!(cursor == join, "monitor continuation crosses instruction");
                exit = join - 1;
            }
        }
        // Address order may put protected throwing arms after the last normal
        // release. They remain inside the synchronized body: Java's implicit
        // release must run after their effects and preserve the original throw.
        // Admit only a bounded, acyclic suffix whose every path ends in throw,
        // while every normal release has a no-effect path to return.
        let late_body: HashSet<_> = body
            .iter()
            .copied()
            .filter(|pc| *pc > last_release && !return_tails.contains(pc))
            .collect();
        if !late_body.is_empty() && dispatch_join.is_none() && all_releases_return {
            ensure!(late_body.len() <= 256, "monitor terminal arm budget");
            let mut indegree: HashMap<usize, usize> = late_body.iter().map(|&pc| (pc, 0)).collect();
            for &pc in &late_body {
                let insn = at[&pc];
                ensure!(
                    code.try_regions.iter().any(|region| {
                        dispatches_to_monitor(region)
                            && region.start as usize <= pc
                            && pc < region.end as usize
                    }),
                    "monitor terminal arm lacks exact cleanup protection"
                );
                ensure!(
                    !matches!(insn.opcode, 0x0e..=0x11 | 0x1d | 0x1e),
                    "monitor terminal arm returns or changes lock ownership"
                );
                let next = successors(graph, words, pc);
                ensure!(
                    (insn.opcode == 0x27 && next.is_empty())
                        || (insn.opcode != 0x27
                            && !next.is_empty()
                            && next.iter().all(|target| late_body.contains(target))),
                    "monitor terminal arm escapes or falls through"
                );
                for target in next {
                    *indegree.get_mut(&target).unwrap() += 1;
                }
            }
            let mut pending: Vec<_> = indegree
                .iter()
                .filter_map(|(&pc, &count)| (count == 0).then_some(pc))
                .collect();
            let mut seen = 0;
            while let Some(pc) = pending.pop() {
                seen += 1;
                for target in successors(graph, words, pc) {
                    let count = indegree.get_mut(&target).unwrap();
                    *count -= 1;
                    if *count == 0 {
                        pending.push(target);
                    }
                }
            }
            ensure!(seen == late_body.len(), "monitor terminal arm has a cycle");
            body.extend(return_tails);
            exit = *body.iter().max().unwrap();
            terminal = true;
        }
        ensure!(
            body.iter().all(|pc| *pc <= exit),
            "monitor body crosses release layout for {:x}: max {:?}, exit {:x}, releases {exits:?}, join {dispatch_join:?}",
            enter.pc,
            body.iter().max(),
            exit
        );
        for insn in &ir.instructions {
            for target in successors(graph, words, insn.pc) {
                ensure!(
                    !body.contains(&target)
                        || body.contains(&insn.pc)
                        || (insn.pc == enter.pc && target == start),
                    "external edge enters monitor body at {target:x} from {:x} for {:x}",
                    insn.pc,
                    enter.pc
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
                    !insn.may_throw
                        || body.contains(&insn.pc)
                        || Some(insn.pc) == release
                        || owned_cleanup
                            .iter()
                            .any(|(child, cleanup)| body.contains(child)
                                && cleanup.contains(&insn.pc)),
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
        let mut owned = body.clone();
        owned.extend(cleanup.iter().copied());
        owned.insert(enter.pc);
        owned.extend(exits.iter().copied());
        for child in &result {
            if body.contains(&child.enter) {
                owned.extend(child.owned.iter().copied());
            }
        }
        owned_bodies.insert(enter.pc, body);
        owned_cleanup.insert(enter.pc, cleanup);
        result.push(Region {
            enter: enter.pc,
            exit,
            lock,
            tries,
            terminal,
            inner_tries,
            releases: exits,
            dispatch_join,
            owned,
        });
    }
    result.sort_by_key(|region| region.enter);
    Ok(result)
}

/// Ordinary catches can surround independent code before or after a monitor.
/// Their later normal continuation may acquire the lock at its real enter;
/// no protected instruction or exceptional entry may own an already-held frame.
pub(super) fn ordinary_regions_disjoint(code: &DexCode, regions: &[Region]) -> bool {
    let prove = || -> Option<()> {
        if code.instructions.len() > 4096
            || code.registers > 256
            || usize::from(code.tries) != code.try_regions.len()
            || code.try_regions.len() > 16
            || regions.len() > 32
            || code
                .try_regions
                .iter()
                .map(|r| r.catches.len())
                .sum::<usize>()
                > 64
        {
            return None;
        }
        let ir = DecodedMethod::decode(code).ok()?;
        let boundaries: HashSet<_> = ir.instructions.iter().map(|i| i.pc).collect();
        for (index, region) in code.try_regions.iter().enumerate() {
            if regions.iter().any(|monitor| {
                monitor.tries.contains(&index) || monitor.inner_tries.contains(&index)
            }) {
                continue;
            }
            let start = region.start as usize;
            let end = region.end as usize;
            if start >= end
                || !boundaries.contains(&start)
                || (end != code.instructions.len() && !boundaries.contains(&end))
                || region.catches.is_empty()
                || region.catches.iter().any(|(_, handler)| {
                    !boundaries.contains(&(*handler as usize))
                        || regions
                            .iter()
                            .any(|monitor| monitor.owned.contains(&(*handler as usize)))
                })
            {
                return None;
            }
            let mut cursor = start;
            for instruction in ir
                .instructions
                .iter()
                .filter(|i| (start..end).contains(&i.pc))
            {
                if instruction.pc != cursor
                    || regions
                        .iter()
                        .any(|monitor| monitor.owned.contains(&instruction.pc))
                {
                    return None;
                }
                cursor += instruction.width;
            }
            if cursor != end {
                return None;
            }
        }
        Some(())
    };
    prove().is_some()
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
    // ACC_DECLARED_SYNCHRONIZED is reflective metadata in DEX, not an
    // automatic acquisition. Java's method modifier supplies the one proven
    // whole-method acquisition, so do not also render its explicit DEX block.
    let implicit = declared_method_lock(class, method, graph)? == Some(region.enter);
    if region.dispatch_join.is_some() {
        return emit_dispatch(class, method, graph, region, regs, out, depth);
    }
    let lock = register(&regs, region.lock)?;
    ensure!(reference(&lock.ty), "monitor requires a reference value");
    let mut body = Output {
        sequence: out.sequence,
        indent: out.indent + usize::from(!implicit),
        ..out.empty_fork()
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
        region.exit + 1,
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
        if !implicit {
            out.line(&format!("synchronized ({}) {{", lock.text), &[]);
        }
        out.append(body);
        if !implicit {
            out.line("}", &[]);
        }
        return Ok((regs, true));
    }
    let mut paths = vec![(0, body, values, false)];
    merge_path_registers(&mut regs, &mut paths, out, graph, region.exit + 1)?;
    if !implicit {
        out.line(&format!("synchronized ({}) {{", lock.text), &[]);
    }
    out.append(paths.remove(0).1);
    if !implicit {
        out.line("}", &[]);
    }
    Ok((regs, false))
}

/// Preserve both the reflected method modifier and the exact lock scope.
/// The explicit DEX acquisition must cover every observable method effect.
pub(super) fn declared_method_lock(
    class: &DexClass,
    method: &DexMethod,
    graph: &Graph,
) -> Result<Option<usize>> {
    if method.access_flags & 0x20000 == 0 {
        return Ok(None);
    }
    ensure!(
        !matches!(method.name.as_ref(), "<init>" | "<clinit>"),
        "initializer cannot declare synchronization"
    );
    let code = method.code.as_ref().context("declared monitor code")?;
    let ir = DecodedMethod::decode(code)?;
    let words = &code.instructions;
    let mut aliases = HashSet::new();
    let mut enter = 0;
    if method.access_flags & 8 == 0 {
        aliases.insert(usize::from(code.registers - code.ins));
    } else {
        // Static invocation has already resolved and initialized the declaring
        // class. Its exact class literal and reference copies have no additional
        // resolution, initialization or evaluation effects before acquisition.
        let mut literal = false;
        for instruction in &ir.instructions {
            enter = instruction.pc;
            if instruction.opcode == 0x1d {
                break;
            }
            match instruction.opcode {
                0x1c if !literal
                    && instruction.pc == 0
                    && class.symbols.types.get(usize::from(words[1]))
                        == Some(&class.descriptor) =>
                {
                    literal = true;
                    aliases.insert(usize::from(words[0] >> 8));
                }
                0x07..=0x09 if literal => {
                    let (dst, src) = match instruction.opcode {
                        0x07 => (
                            usize::from((words[enter] >> 8) & 15),
                            usize::from(words[enter] >> 12),
                        ),
                        0x08 => (
                            usize::from(words[enter] >> 8),
                            usize::from(words[enter + 1]),
                        ),
                        _ => (usize::from(words[enter + 1]), usize::from(words[enter + 2])),
                    };
                    ensure!(
                        aliases.contains(&src),
                        "declared static monitor copy identity"
                    );
                    aliases.insert(dst);
                }
                _ => bail!("effects precede declared method monitor acquisition"),
            }
        }
        ensure!(
            literal,
            "declared static monitor lacks declaring-class identity"
        );
    }
    let region = graph
        .synchronized
        .iter()
        .find(|region| region.enter == enter)
        .context("declared synchronization lacks whole-method monitor")?;
    ensure!(
        aliases.contains(&region.lock) && region.dispatch_join.is_none(),
        "declared method monitor lock or scope differs"
    );
    let at: HashMap<_, _> = ir.instructions.iter().map(|insn| (insn.pc, insn)).collect();
    for release in &region.releases {
        let mut pc = release + graph.widths[*release];
        let mut seen = HashSet::new();
        loop {
            ensure!(
                seen.len() < 16 && seen.insert(pc),
                "declared monitor return budget"
            );
            let instruction = at.get(&pc).context("declared monitor return boundary")?;
            match instruction.opcode {
                0x0e..=0x11 => break,
                0x00 => pc += instruction.width,
                0x28..=0x2a => pc = graph.targets[pc].context("declared monitor return jump")?,
                _ => bail!("effects follow declared method monitor release"),
            }
        }
    }
    Ok(Some(enter))
}

pub(super) struct Dispatch {
    releases: Vec<usize>,
    snapshots: HashMap<usize, Vec<Option<Value>>>,
    slots: Option<HashMap<usize, Vec<Option<Value>>>>,
    label: String,
    selector: String,
}

pub(super) fn dispatch_release(
    graph: &Graph,
    pc: usize,
    regs: &[Option<Value>],
    out: &mut Output,
) -> Result<bool> {
    let mut stack = graph.monitor_dispatch.borrow_mut();
    let Some(context) = stack.last_mut() else {
        return Ok(false);
    };
    let Some(index) = context.releases.iter().position(|release| *release == pc) else {
        return Ok(false);
    };
    let mut snapshot = regs.to_vec();
    for r in 0..snapshot.len() {
        if let Some(value) = &regs[r] {
            if value.ty == "<wide-tail>" {
                continue;
            }
            if !(graph.live_at(pc + 1, r) || (wide(&value.ty) && graph.live_at(pc + 1, r + 1))) {
                snapshot[r] = None;
                if wide(&value.ty) {
                    snapshot[r + 1] = None;
                }
            }
        }
    }
    if let Some(previous) = context.snapshots.get_mut(&pc) {
        for r in 0..snapshot.len() {
            if previous[r] == snapshot[r] {
                continue;
            }
            let left = previous[r]
                .as_ref()
                .context("monitor release loses live register")?;
            let right = snapshot[r]
                .as_ref()
                .context("monitor release gains uninitialized live register")?;
            ensure!(
                left.ty == right.ty && left.ty != "<wide-tail>",
                "monitor release has incompatible live register types"
            );
            previous[r] = Some(Value {
                text: "<monitor-merge>".into(),
                ty: left.ty.clone(),
                literal: None,
                wide_literal: None,
                raw_bits32: left.raw_bits32 && right.raw_bits32,
            });
        }
    } else {
        context.snapshots.insert(pc, snapshot);
    }
    if let Some(slots) = &context.slots {
        carry_loop_values(&slots[&pc], regs, out)?;
    }
    out.line(&format!("{} = {index};", context.selector), &[]);
    out.line(&format!("break {};", context.label), &[]);
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
fn dispatch_body(
    class: &DexClass,
    method: &DexMethod,
    graph: &Graph,
    region: &Region,
    regs: Vec<Option<Value>>,
    out: &mut Output,
    depth: usize,
    context: Dispatch,
) -> Result<(Dispatch, Output)> {
    graph.monitor_dispatch.borrow_mut().push(context);
    let mut body = Output {
        sequence: out.sequence,
        indent: out.indent + 1,
        ..out.empty_fork()
    };
    let rendered = render(
        class,
        method,
        graph,
        region.enter + 1,
        region.dispatch_join.unwrap(),
        regs,
        &mut body,
        depth + 1,
        true,
        None,
        None,
    );
    let context = graph
        .monitor_dispatch
        .borrow_mut()
        .pop()
        .context("monitor dispatch context")?;
    let (_, terminal) = rendered?;
    ensure!(terminal, "monitor dispatch has unreleased path");
    ensure!(
        context.snapshots.len() == region.releases.len(),
        "monitor dispatch release unreachable"
    );
    Ok((context, body))
}

fn emit_dispatch(
    class: &DexClass,
    method: &DexMethod,
    graph: &Graph,
    region: &Region,
    mut regs: Vec<Option<Value>>,
    out: &mut Output,
    depth: usize,
) -> Result<(Vec<Option<Value>>, bool)> {
    ensure!(
        region.inner_tries.is_empty(),
        "monitor dispatch with typed handlers not reconstructed"
    );
    ensure!(
        graph
            .synchronized
            .iter()
            .filter(|child| region.enter < child.enter && child.enter <= region.exit)
            .all(|child| child.inner_tries.is_empty()),
        "monitor dispatch with typed child handlers not reconstructed"
    );
    let lock = register(&regs, region.lock)?;
    ensure!(reference(&lock.ty), "monitor requires reference lock");
    let mut releases: Vec<_> = region.releases.iter().copied().collect();
    releases.sort_unstable();
    let selector = format!("monitorExit{}", out.sequence);
    let label = format!("monitorBody{}", out.sequence);
    out.sequence += 1;
    let context = Dispatch {
        releases,
        snapshots: HashMap::new(),
        slots: None,
        label: label.clone(),
        selector: selector.clone(),
    };
    let caught = graph.caught_values.borrow().clone();
    let rethrows = graph.catch_rethrows.borrow().clone();
    let probe = dispatch_body(
        class,
        method,
        graph,
        region,
        regs.clone(),
        out,
        depth,
        context,
    );
    *graph.caught_values.borrow_mut() = caught;
    *graph.catch_rethrows.borrow_mut() = rethrows;
    let (mut context, _) = probe?;
    let mut slots = HashMap::new();
    for release in &context.releases {
        let snapshot = &context.snapshots[release];
        let mut frame = snapshot.clone();
        for r in 0..frame.len() {
            let Some(value) = &snapshot[r] else { continue };
            if value.ty == "<wide-tail>" {
                continue;
            }
            if !(graph.live_at(release + 1, r)
                || (wide(&value.ty) && graph.live_at(release + 1, r + 1)))
            {
                frame[r] = None;
                continue;
            }
            if regs[r].as_ref() == Some(value)
                || value.literal.is_some()
                || value.wide_literal.is_some()
                || value.text.starts_with('"')
            {
                continue;
            }
            let initial = match value.ty.as_str() {
                "Z" => "false",
                "J" => "0L",
                "F" => "0.0f",
                "D" => "0.0d",
                ty if reference(ty) => "null",
                _ => "0",
            };
            frame[r] = Some(out.local(&value.ty, initial, &[])?);
            if wide(&value.ty) {
                frame[r + 1] = snapshot[r + 1].clone();
            }
        }
        slots.insert(*release, frame);
    }
    out.line(&format!("int {selector} = -1;"), &[]);
    context.snapshots.clear();
    context.slots = Some(slots.clone());
    let (_, body) = dispatch_body(
        class,
        method,
        graph,
        region,
        regs.clone(),
        out,
        depth,
        context,
    )?;
    out.sequence = body.sequence;
    out.line(&format!("{label}: synchronized ({}) {{", lock.text), &[]);
    out.append(body);
    out.line("}", &[]);
    let join = region.dispatch_join.unwrap();
    let mut paths = Vec::new();
    let mut ordered: Vec<_> = region.releases.iter().copied().collect();
    ordered.sort_unstable();
    for (index, release) in ordered.iter().enumerate() {
        let mut tail = Output {
            sequence: out.sequence,
            indent: out.indent + 1,
            ..out.empty_fork()
        };
        let (values, terminal) = render(
            class,
            method,
            graph,
            release + 1,
            join,
            slots[release].clone(),
            &mut tail,
            depth + 1,
            true,
            None,
            None,
        )?;
        ensure!(!terminal, "monitor continuation unexpectedly terminal");
        out.sequence = tail.sequence;
        paths.push((index, tail, values, false));
    }
    merge_path_registers(&mut regs, &mut paths, out, graph, join)?;
    let path_count = paths.len();
    for (index, body, _, _) in paths {
        if index + 1 == path_count {
            out.line("else {", &[]);
        } else {
            out.line(
                &format!(
                    "{} ({selector} == {index}) {{",
                    if index == 0 { "if" } else { "else if" }
                ),
                &[],
            );
        }
        out.append(body);
        out.line("}", &[]);
    }
    Ok((regs, false))
}
