//! A protected search loop can share a failure continuation with its catch-all.
//! Delay that continuation until outside Java try, preserving its own failures.
use super::*;
use crate::{native_dex::DexTryRegion, native_ir::DecodedMethod};
use anyhow::Context as _;
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct Plan {
    start: usize,
    end: usize,
    handler: usize,
    pub(super) join: usize,
    handler_inputs: BTreeSet<usize>,
}
#[derive(Clone)]
pub(super) struct Context {
    pub start: usize,
    pub end: usize,
    pub handler: usize,
    pub join: usize,
    pub failed: Value,
    pub values: Vec<Option<Value>>,
    pub label: String,
}
struct Scope<'a> {
    cell: &'a std::cell::RefCell<Option<Context>>,
    previous: Option<Context>,
}
impl Drop for Scope<'_> {
    fn drop(&mut self) {
        self.cell.replace(self.previous.take());
    }
}

pub(super) fn prove(
    method: &DexMethod,
    graph: &Graph,
    region: &DexTryRegion,
) -> Result<Option<Plan>> {
    let code = method.code.as_ref().context("shared catch code")?;
    if method.name.as_ref() == "<init>"
        || code.instructions.len() > 4096
        || code.registers > 256
        || code.try_regions.len() != 1
        || region.catches.len() != 1
        || region.catches[0].0.is_some()
        || graph.shared_catch_loop.borrow().is_some()
    {
        return Ok(None);
    }
    let start = region.start as usize;
    let end = region.end as usize;
    let handler = region.catches[0].1 as usize;
    if end >= handler
        || handler >= code.instructions.len()
        || !graph
            .loops
            .iter()
            .any(|l| start <= l.start && l.latch == end && l.exit == handler)
    {
        return Ok(None);
    }
    let ir = DecodedMethod::decode(code)?;
    let by_pc: BTreeMap<_, _> = ir.instructions.iter().map(|i| (i.pc, i)).collect();
    // The only extension of the protected interval is a nonthrowing backedge.
    let Some(latch) = by_pc.get(&end) else {
        return Ok(None);
    };
    if !matches!(latch.opcode, 0x28..=0x2a)
        || latch.may_throw
        || end + latch.width != handler
        || !latch.branch_target.is_some_and(|t| start <= t && t < end)
    {
        return Ok(None);
    }
    let mut nodes = BTreeSet::new();
    let mut exits = BTreeSet::new();
    let mut pending = vec![start];
    while let Some(pc) = pending.pop() {
        graph.tick()?;
        if pc >= handler {
            exits.insert(pc);
            continue;
        }
        if pc < start {
            return Ok(None);
        }
        if !nodes.insert(pc) {
            continue;
        }
        let Some(i) = by_pc.get(&pc) else {
            return Ok(None);
        };
        if matches!(i.opcode,0x0d..=0x11 | 0x1d..=0x1e | 0x27 | 0x2b..=0x2c) {
            return Ok(None);
        }
        if let Some(to) = i.branch_target {
            pending.push(to);
        }
        if !matches!(i.opcode, 0x28..=0x2a) {
            pending.push(pc + i.width);
        }
    }
    if exits.len() != 2 || exits.first() != Some(&handler) {
        return Ok(None);
    }
    let join = *exits.last().unwrap();
    if join >= code.instructions.len() || !by_pc.contains_key(&join) {
        return Ok(None);
    }
    // The shared handler is straight-line, ignores the caught exception, and
    // cannot change any value observed by the continuation.
    let mut handler_inputs = BTreeSet::new();
    let mut handler_writes = BTreeSet::new();
    let mut pc = handler;
    while pc < join {
        let Some(i) = by_pc.get(&pc) else {
            return Ok(None);
        };
        if matches!(i.opcode,0x0d..=0x11 | 0x1d..=0x1e | 0x22..=0x27 | 0x28..=0x3d)
            || i.branch_target.is_some()
            || pc + i.width > join
        {
            return Ok(None);
        }
        for read in &i.reads {
            for r in usize::from(read.register)..usize::from(read.register) + read.kind.word_count()
            {
                if !handler_writes.contains(&r) {
                    handler_inputs.insert(r);
                }
            }
        }
        for write in &i.writes {
            for r in
                usize::from(write.register)..usize::from(write.register) + write.kind.word_count()
            {
                if graph.live_at(join, r) {
                    return Ok(None);
                }
                handler_writes.insert(r);
            }
        }
        pc += i.width;
    }
    for &pc in &nodes {
        let i = by_pc[&pc];
        for write in &i.writes {
            for r in
                usize::from(write.register)..usize::from(write.register) + write.kind.word_count()
            {
                if handler_inputs.contains(&r) {
                    return Ok(None);
                }
                if graph.live_at(join, r) {
                    // Catch uses the entry value. A normal write may reach the
                    // join only through a pure forward tail, without any later
                    // throw or failure edge that could expose a different value.
                    if i.may_throw {
                        return Ok(None);
                    }
                    let mut tail = vec![pc + i.width];
                    let mut seen = BTreeSet::new();
                    while let Some(next) = tail.pop() {
                        graph.tick()?;
                        if next == join {
                            continue;
                        }
                        if next <= pc || next >= handler || !seen.insert(next) {
                            return Ok(None);
                        }
                        let Some(n) = by_pc.get(&next) else {
                            return Ok(None);
                        };
                        if !matches!(n.opcode,0x00..=0x09 | 0x12..=0x19 | 0x28..=0x2a)
                            || n.may_throw
                        {
                            return Ok(None);
                        }
                        if let Some(to) = n.branch_target {
                            tail.push(to);
                        }
                        if !matches!(n.opcode, 0x28..=0x2a) {
                            tail.push(next + n.width);
                        }
                    }
                }
            }
        }
    }
    // No foreign path enters a handler interior or the protected loop. An
    // ordinary bypass of the complete region to the shared join is harmless.
    for i in &ir.instructions {
        if let Some(to) = i.branch_target
            && ((handler < to && to < join)
                || (start < to && to < handler && !nodes.contains(&i.pc)))
        {
            return Ok(None);
        }
    }
    Ok(Some(Plan {
        start,
        end,
        handler,
        join,
        handler_inputs,
    }))
}

pub(super) fn emit(
    class: &DexClass,
    method: &DexMethod,
    graph: &Graph,
    plan: &Plan,
    regs: Vec<Option<Value>>,
    out: &mut Output,
    depth: usize,
) -> Result<(Vec<Option<Value>>, usize)> {
    for &r in &plan.handler_inputs {
        register(&regs, r)?;
    }
    let mut values = regs.clone();
    let mut slots = vec![None; regs.len()];
    for (r, value) in regs.iter().enumerate() {
        if !graph.live_at(plan.join, r) {
            continue;
        }
        let value = value
            .as_ref()
            .context("shared catch join has undefined entry value")?;
        if value.ty == "<wide-tail>" {
            continue;
        }
        let slot = out.local(&value.ty, &value.text, &[])?;
        assign(&mut slots, r, slot.clone())?;
        assign(&mut values, r, slot)?;
    }
    let failed = out.local("Z", "false", &[])?;
    let label = format!("tryExit{}", out.sequence);
    out.sequence += 1;
    let context = Context {
        start: plan.start,
        end: plan.end,
        handler: plan.handler,
        join: plan.join,
        failed: failed.clone(),
        values: slots.clone(),
        label: label.clone(),
    };
    let scope = Scope {
        cell: &graph.shared_catch_loop,
        previous: graph.shared_catch_loop.replace(Some(context)),
    };
    let mut body = Output {
        indent: out.indent + 1,
        ..out.empty_fork()
    };
    let no_snapshots = vec![None; regs.len()];
    let (normal, terminal) = render(
        class,
        method,
        graph,
        plan.start,
        plan.join,
        regs.clone(),
        &mut body,
        depth,
        true,
        None,
        Some(&no_snapshots),
    )?;
    drop(scope);
    ensure!(!terminal, "shared catch normal body must join");
    graph.carry_loop_values(&slots, &normal, &mut body)?;
    // Every checked effect from this body is covered by the original catch-all.
    body.checked_effects.clear();
    out.sequence = body.sequence;
    out.line(&format!("{label}:"), &[]);
    out.line("try {", &[]);
    out.append(body);
    let caught = format!("ignored{}", out.sequence);
    out.sequence += 1;
    out.line(&format!("}} catch (java.lang.Throwable {caught}) {{"), &[]);
    out.indent += 1;
    out.line(&format!("{} = true;", failed.text), &[]);
    out.indent -= 1;
    out.line("}", &[]);
    out.line(&format!("if ({}) {{", failed.text), &[]);
    out.indent += 1;
    let (_, terminal) = render(
        class,
        method,
        graph,
        plan.handler,
        plan.join,
        regs,
        out,
        depth,
        true,
        None,
        None,
    )?;
    ensure!(!terminal, "shared catch continuation must join");
    out.indent -= 1;
    out.line("}", &[]);
    Ok((values, plan.join))
}
