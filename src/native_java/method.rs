//! Conservative structured DEX to Java lowering. Every effectful expression is
//! materialized immediately; registers hold immutable source values, not delayed
//! expressions. Unsupported instructions reject the entire method.
#[path = "allocation.rs"]
mod allocation;
#[path = "allocation_lowering.rs"]
mod allocation_lowering;
#[path = "cleanup.rs"]
mod cleanup;
#[path = "numeric.rs"]
mod numeric;
#[path = "operations.rs"]
mod operations;
#[path = "throwing.rs"]
mod throwing;
use super::{java_type, names};
use crate::{
    engine::CodeLink,
    native_dex::{DexClass, DexMethod},
};
use anyhow::{Context, Result, bail, ensure};
use numeric::{Kind, Literal};

pub(super) fn validate_exception_type(class: &DexClass, ty: &str) -> Result<()> {
    throwing::validate_type(class, ty)
}

pub(super) struct MethodBody {
    pub text: String,
    pub links: Vec<CodeLink>,
}
#[derive(Clone, PartialEq, Eq)]
struct Value {
    text: String,
    ty: String,
    literal: Option<i32>,
    wide_literal: Option<u64>,
}
#[derive(Clone, Default)]
struct Output {
    text: String,
    links: Vec<CodeLink>,
    sequence: usize,
    chars: usize,
    indent: usize,
    receiver_locals: std::collections::HashSet<String>,
}
impl Output {
    fn append(&mut self, child: Output) {
        self.receiver_locals.extend(child.receiver_locals);
        for mut link in child.links {
            link.start += self.chars;
            link.end += self.chars;
            self.links.push(link);
        }
        self.chars += child.chars;
        self.text.push_str(&child.text);
    }
    fn line(&mut self, text: &str, refs: &[(usize, usize, String)]) {
        let spaces = 8 + self.indent * 4;
        let base = self.chars + spaces;
        self.chars += spaces + 1 + text.chars().count();
        self.text.push_str(&" ".repeat(spaces));
        self.text.push_str(text);
        self.text.push('\n');
        for (start, len, label) in refs {
            self.links.push(CodeLink {
                start: base + start,
                end: base + start + len,
                label: label.clone(),
            });
        }
    }
    fn local(
        &mut self,
        ty: &str,
        expression: &str,
        refs: &[(usize, usize, String)],
    ) -> Result<Value> {
        let name = format!("v{}", self.sequence);
        self.sequence += 1;
        if expression != "null" && !expression.contains("new ") {
            self.receiver_locals.insert(name.clone());
        }
        let prefix = format!("{} {name} = ", java_type(ty)?);
        let mut adjusted: Vec<_> = refs
            .iter()
            .map(|(start, len, label)| (start + prefix.chars().count(), *len, label.clone()))
            .collect();
        if let Some(label) = class_label(ty) {
            adjusted.push((0, java_type(ty)?.chars().count(), label));
        }
        self.line(&format!("{prefix}{expression};"), &adjusted);
        ensure!(
            self.text.len() <= 4 * 1024 * 1024,
            "reconstructed method exceeds output budget"
        );
        Ok(Value {
            text: name,
            ty: ty.into(),
            literal: None,
            wide_literal: None,
        })
    }
}
fn class_label(ty: &str) -> Option<String> {
    ty.trim_start_matches('[')
        .strip_prefix('L')
        .and_then(|s| s.strip_suffix(';'))
        .map(|s| s.replace('/', "."))
}

fn reference(ty: &str) -> bool {
    ty.starts_with('L') || ty.starts_with('[')
}
fn argument(value: &Value, ty: &str) -> Result<String> {
    if let Some(bits) = value.wide_literal {
        let kind = match ty {
            "J" => Kind::Long,
            "D" => Kind::Double,
            _ => bail!("Wide literal used as narrow value"),
        };
        return Literal::Bits64(bits).render(kind);
    }
    if ty == "F"
        && let Some(bits) = value.literal
    {
        return Literal::Bits32(bits as u32).render(Kind::Float);
    }
    if ty == "I" && value.ty == "Z" {
        return Ok(format!("({} ? 1 : 0)", value.text));
    }

    if value.ty == ty || (ty == "I" && matches!(value.ty.as_str(), "B" | "S" | "C")) {
        return Ok(value.text.clone());
    }
    if let Some(n) = value.literal {
        return Ok(match ty {
            "Z" => {
                ensure!(n == 0 || n == 1, "nonboolean literal");
                if n == 0 {
                    "false".into()
                } else {
                    "true".into()
                }
            }
            "B" | "S" | "C" => format!("({}) {n}", java_type(ty)?),
            _ if reference(ty) && n == 0 => "null".into(),
            _ => bail!("unsupported literal conversion"),
        });
    }
    if reference(&value.ty) && reference(ty) {
        return Ok(format!("(({}) {})", java_type(ty)?, value.text));
    }
    bail!("unsupported register type conversion {} to {ty}", value.ty)
}
fn receiver(value: &Value, ty: &str) -> Result<String> {
    ensure!(reference(ty), "nonreference receiver owner");
    if value.literal == Some(0) {
        return Ok(format!("(({}) null)", java_type(ty)?));
    }
    argument(value, ty)
}

fn wide(ty: &str) -> bool {
    matches!(ty, "J" | "D")
}
fn register(registers: &[Option<Value>], r: usize) -> Result<Value> {
    let value = registers
        .get(r)
        .and_then(Clone::clone)
        .context("undefined register")?;
    ensure!(
        value.ty != "<wide-tail>",
        "read from upper half of wide register"
    );
    if wide(&value.ty) {
        ensure!(
            registers
                .get(r + 1)
                .and_then(Option::as_ref)
                .is_some_and(|tail| tail.ty == "<wide-tail>" && tail.text == r.to_string()),
            "invalid wide register pair"
        );
    }
    Ok(value)
}
fn assign(registers: &mut [Option<Value>], r: usize, value: Value) -> Result<()> {
    let width = if wide(&value.ty) {
        Kind::Long.width()
    } else {
        Kind::Int.width()
    };
    ensure!(
        r.checked_add(width)
            .is_some_and(|end| end <= registers.len()),
        "register pair out of bounds"
    );
    for slot in r..r + width {
        if let Some(old) = &registers[slot] {
            if old.ty == "<wide-tail>" {
                let head: usize = old.text.parse().context("invalid wide tail marker")?;
                ensure!(head + 1 == slot, "wide tail owner mismatch");
                registers[head] = None;
            } else if wide(&old.ty) && slot + 1 < registers.len() {
                registers[slot + 1] = None;
            }
        }
        registers[slot] = None;
    }
    registers[r] = Some(value);
    if width == 2 {
        registers[r + 1] = Some(Value {
            text: r.to_string(),
            ty: "<wide-tail>".into(),
            literal: None,
            wide_literal: None,
        });
    }
    Ok(())
}

// A frame stores the upper word only as ownership metadata.  Keep checking it
// at control-flow boundaries: joins construct frames directly, whereas normal
// instruction writes go through `assign`.
fn validate_wide_frame(registers: &[Option<Value>]) -> Result<()> {
    for (r, value) in registers.iter().enumerate() {
        let Some(value) = value else { continue };
        if wide(&value.ty) {
            ensure!(
                registers
                    .get(r + 1)
                    .and_then(Option::as_ref)
                    .is_some_and(|tail| tail.ty == "<wide-tail>" && tail.text == r.to_string()),
                "invalid wide register pair"
            );
        } else if value.ty == "<wide-tail>" {
            let head: usize = value.text.parse().context("invalid wide tail marker")?;
            ensure!(
                head + 1 == r
                    && registers
                        .get(head)
                        .and_then(Option::as_ref)
                        .is_some_and(|head| wide(&head.ty)),
                "wide tail owner mismatch"
            );
        }
    }
    Ok(())
}

fn loop_slots(
    regs: &[Option<Value>],
    graph: &Graph,
    start: usize,
    out: &mut Output,
) -> Result<Vec<Option<Value>>> {
    validate_wide_frame(regs)?;
    let mut slots = vec![None; regs.len()];
    for (r, value) in regs.iter().enumerate() {
        let Some(value) = value else { continue };
        if value.ty == "<wide-tail>" {
            continue;
        }
        let live = graph.live_at(start, r) || (wide(&value.ty) && graph.live_at(start, r + 1));
        if live {
            assign(&mut slots, r, out.local(&value.ty, &value.text, &[])?)?;
        }
    }
    Ok(slots)
}
fn integral(value: &Value) -> Result<String> {
    if value.ty == "Z" {
        return argument(value, "I");
    }
    ensure!(
        matches!(value.ty.as_str(), "I" | "B" | "S" | "C"),
        "nonintegral arithmetic"
    );
    Ok(value.text.clone())
}
fn opname(op: u8) -> Result<&'static str> {
    Ok(match op {
        0 => "+",
        1 => "-",
        2 => "*",
        3 => "/",
        4 => "%",
        5 => "&",
        6 => "|",
        7 => "^",
        8 => "<<",
        9 => ">>",
        10 => ">>>",
        _ => bail!("invalid arithmetic"),
    })
}
fn string_literal(value: &str) -> Result<String> {
    super::strings::literal(value)
}

#[derive(Clone, Copy)]
struct Loop {
    start: usize,
    latch: usize,
    guard: Option<usize>,
    exit: usize,
    // Conditional backedge with a forward guard and a one-time exit tail.
    tail: Option<usize>,
}
#[derive(Clone)]
struct Switch {
    cases: Vec<(i32, usize)>,
}
struct Graph {
    constructor_bindings: std::cell::OnceCell<
        Option<std::collections::HashMap<usize, crate::native_constructors::ConstructorBinding>>,
    >,
    live: Option<super::liveness::LiveRegisters>,
    switches: Vec<Option<Switch>>,
    payloads: Vec<bool>,
    loops: Vec<Loop>,
    widths: Vec<usize>,
    targets: Vec<Option<usize>>,
    acyclic_backwards: std::collections::HashSet<usize>,
    work: std::cell::Cell<usize>,
}
impl Graph {
    /// Build the native identity pipeline only when an extended allocation
    /// region needs it. Keep the ordinary renderer path and repeated allocation
    /// lookups cheap; failed analysis is cached as a conservative decline.
    fn constructor_binding(
        &self,
        class: &DexClass,
        method: &DexMethod,
        invoke_pc: usize,
    ) -> Option<&crate::native_constructors::ConstructorBinding> {
        self.constructor_bindings.get_or_init(|| {
            let analyze = || -> Result<std::collections::HashMap<usize, crate::native_constructors::ConstructorBinding>> {
                let code = method.code.as_ref().context("method has no code")?;
                let ir = crate::native_ir::DecodedMethod::decode(code)?;
                let graph = crate::native_cfg::ControlFlowGraph::build(code)?;
                let ssa = crate::native_ssa::SsaMethod::build(code, &ir, &graph)?;
                let bound = crate::native_calls::BoundCalls::bind(code, &ir, &class.symbols)?;
                let calls = crate::native_call_values::SsaCalls::bind(&bound, &ssa)?;
                let constructors = crate::native_constructors::ConstructorAnalysis::analyze(
                    class, method, &ir, &bound, &ssa, &calls,
                )?;
                Ok(constructors.bindings.into_iter().map(|binding| (binding.invoke_pc, binding)).collect())
            };
            analyze().ok()
        }).as_ref()?.get(&invoke_pc)
    }
    fn new(words: &[u16]) -> Result<Self> {
        Self::with_handlers(words, &[])
    }
    fn with_handlers(words: &[u16], handlers: &[usize]) -> Result<Self> {
        let mut graph = Self {
            constructor_bindings: std::cell::OnceCell::new(),
            live: None,
            loops: Vec::new(),
            switches: vec![None; words.len()],
            payloads: vec![false; words.len()],
            widths: vec![0; words.len()],
            targets: vec![None; words.len()],
            acyclic_backwards: std::collections::HashSet::new(),
            work: std::cell::Cell::new(0),
        };
        let mut pc = 0;
        let mut branches = 0;
        let mut payload_starts = Vec::new();
        while pc < words.len() {
            let op = words[pc] as u8;
            if matches!(words[pc], 0x0100 | 0x0200) {
                ensure!(pc.is_multiple_of(2), "unaligned switch payload");
                let count = *words.get(pc + 1).context("truncated switch payload")? as usize;
                ensure!(count <= 128, "switch exceeds case budget");
                let width = if words[pc] == 0x0100 {
                    4 + count * 2
                } else {
                    2 + count * 4
                };
                ensure!(pc + width <= words.len(), "truncated switch payload");
                graph.payloads[pc..pc + width].fill(true);
                payload_starts.push(pc);
                pc += width;
                continue;
            }
            let width = match op {
                0x18 => 5,
                0x00
                | 0x01
                | 0x04
                | 0x07
                | 0x0a..=0x12
                | 0x21
                | 0x27
                | 0x28
                | 0x7b..=0x8f
                | 0xb0..=0xcf => 1,
                0x02
                | 0x05
                | 0x08
                | 0x13
                | 0x15
                | 0x16
                | 0x19
                | 0x1a
                | 0x1c
                | 0x1f
                | 0x20
                | 0x22
                | 0x23
                | 0x44..=0x51
                | 0x29
                | 0x32..=0x3d
                | 0x52..=0x6d
                | 0x2d..=0x31
                | 0x90..=0xaf
                | 0xd0..=0xe2 => 2,
                0x03
                | 0x06
                | 0x09
                | 0x14
                | 0x17
                | 0x1b
                | 0x24
                | 0x25
                | 0x2a
                | 0x2b
                | 0x2c
                | 0x6e..=0x72
                | 0x74..=0x78 => 3,
                _ => bail!("unsupported opcode 0x{op:02x} at {pc}"),
            };
            ensure!(pc + width <= words.len(), "truncated instruction");
            ensure!(op != 0 || words[pc] == 0, "payload in structured body");
            graph.widths[pc] = width;
            let offset = match op {
                0x28 => Some((words[pc] >> 8) as i8 as i64),
                0x29 | 0x32..=0x3d => Some(words[pc + 1] as i16 as i64),
                0x2a => Some((words[pc + 1] as u32 | ((words[pc + 2] as u32) << 16)) as i32 as i64),
                _ => None,
            };
            if let Some(offset) = offset {
                branches += 1;
                ensure!(branches <= 128, "control flow exceeds branch budget");
                ensure!(offset != 0, "self branch not reconstructed");
                let target = pc as i64 + offset;
                ensure!(
                    target >= 0 && target < words.len() as i64,
                    "branch target outside method"
                );
                graph.targets[pc] = Some(target as usize);
            }
            pc += width;
        }
        let read_i32 = |pos: usize| -> Result<i32> {
            let lo = *words.get(pos).context("truncated switch word")? as u32;
            let hi = *words.get(pos + 1).context("truncated switch word")? as u32;
            Ok((lo | (hi << 16)) as i32)
        };
        let mut used_payloads = std::collections::HashSet::new();
        for pc in 0..words.len() {
            if graph.widths[pc] == 0 || !matches!(words[pc] as u8, 0x2b | 0x2c) {
                continue;
            }
            let payload = pc as i64 + i64::from(read_i32(pc + 1)?);
            ensure!(
                payload >= 0 && payload < words.len() as i64,
                "switch payload offset outside method"
            );
            let payload = payload as usize;
            ensure!(
                payload_starts.contains(&payload),
                "switch offset is not a payload boundary"
            );
            let packed = words[pc] as u8 == 0x2b;
            ensure!(
                words[payload] == if packed { 0x0100 } else { 0x0200 },
                "switch payload type mismatch"
            );
            used_payloads.insert(payload);
            let count = words[payload + 1] as usize;
            branches += count + 1;
            ensure!(branches <= 128, "control flow exceeds branch budget");
            let first = if packed { read_i32(payload + 2)? } else { 0 };
            let mut cases = Vec::with_capacity(count);
            for i in 0..count {
                let key = if packed {
                    first
                        .checked_add(i as i32)
                        .context("packed switch key overflow")?
                } else {
                    read_i32(payload + 2 + i * 2)?
                };
                ensure!(
                    cases.last().is_none_or(|(previous, _)| *previous < key),
                    "switch keys are not strictly ordered"
                );
                let target_pos = if packed {
                    payload + 4 + i * 2
                } else {
                    payload + 2 + count * 2 + i * 2
                };
                let target = pc as i64 + i64::from(read_i32(target_pos)?);
                ensure!(
                    target > pc as i64 && target < words.len() as i64,
                    "nonforward or invalid switch target"
                );
                let target = target as usize;
                ensure!(
                    graph.widths[target] != 0,
                    "switch target is not executable instruction boundary"
                );
                cases.push((key, target));
            }
            graph.switches[pc] = Some(Switch { cases });
        }
        ensure!(
            used_payloads.len() == payload_starts.len(),
            "unreferenced switch payload"
        );
        for target in graph.targets.iter().flatten() {
            ensure!(
                graph.widths[*target] != 0,
                "branch target is not an instruction boundary"
            );
        }
        let mut reachable = graph.reachable(0, words.len(), words)?;
        for handler in handlers {
            ensure!(
                *handler < words.len() && graph.widths[*handler] != 0,
                "handler is not an instruction boundary"
            );
            let seen = graph.reachable(*handler, words.len(), words)?;
            for (visible, extra) in reachable.iter_mut().zip(seen) {
                *visible |= extra;
            }
        }
        ensure!(
            graph
                .widths
                .iter()
                .enumerate()
                .all(|(pc, width)| *width == 0
                    || words[pc] as u8 != 0x0d
                    || handlers.contains(&pc)),
            "move-exception outside handler entry"
        );
        ensure!(
            graph
                .widths
                .iter()
                .enumerate()
                .all(|(pc, width)| *width == 0
                    || reachable[pc]
                    || (words[pc] == 0
                        && !pc.is_multiple_of(2)
                        && payload_starts.contains(&(pc + 1)))),
            "unreachable instruction region not reconstructed"
        );
        // Address order is not execution order: optimized DEX can place a
        // shared tail before one of its predecessors. Only cyclic edges can
        // form a loop. Classify before loop summaries alter reachability.
        for (from, target) in graph.targets.iter().enumerate() {
            if let Some(to) = *target
                && to < from
                && !graph.reachable(to, words.len(), words)?[from]
            {
                graph.acyclic_backwards.insert(from);
            }
        }
        for (latch, target) in graph.targets.iter().enumerate() {
            let Some(start) = *target else { continue };
            if start >= latch || graph.acyclic_backwards.contains(&latch) {
                continue;
            }
            let mut exit = latch + graph.widths[latch];
            let mut tail = None;
            ensure!(exit < words.len(), "loop has no exit instruction");
            let guard = if matches!(words[latch] as u8, 0x28..=0x2a) {
                let mut pos = start;
                while pos < latch
                    && graph.targets[pos].is_none()
                    && !matches!(words[pos] as u8, 0x0e..=0x11 | 0x27)
                {
                    pos += graph.widths[pos];
                }
                ensure!(
                    pos < latch
                        && matches!(words[pos] as u8, 0x32..=0x3d)
                        && graph.targets[pos] == Some(exit),
                    "loop requires a single forward exit guard"
                );
                Some(pos)
            } else {
                ensure!(
                    matches!(words[latch] as u8, 0x32..=0x3d),
                    "unsupported loop latch"
                );
                let mut pos = start;
                while pos < latch
                    && graph.targets[pos].is_none()
                    && !matches!(words[pos] as u8, 0x0e..=0x11 | 0x27)
                {
                    pos += graph.widths[pos];
                }
                if pos < latch
                    && matches!(words[pos] as u8, 0x32..=0x3d)
                    && let Some(join) = graph.targets[pos]
                    && join > exit
                {
                    tail = Some(exit);
                    exit = join;
                    Some(pos)
                } else {
                    None
                }
            };
            ensure!(
                graph
                    .loops
                    .iter()
                    .all(|l| exit <= l.start || start >= l.exit),
                "nested or overlapping loops not reconstructed"
            );
            for (from, target) in graph.targets.iter().enumerate() {
                graph.tick()?;
                let Some(to) = *target else { continue };
                if from == latch || Some(from) == guard {
                    continue;
                }
                if (start..exit).contains(&from) {
                    let limit = if tail.is_some_and(|tail| from >= tail) {
                        exit
                    } else {
                        latch
                    };
                    ensure!(to > from && to <= limit, "unsupported loop interior edge");
                } else {
                    ensure!(to <= start || to >= exit, "loop has an interior entry");
                }
            }
            graph.loops.push(Loop {
                start,
                latch,
                guard,
                exit,
                tail,
            });
        }
        ensure!(
            graph.loops.is_empty() || graph.switches.iter().all(Option::is_none),
            "switch and loop combination not reconstructed"
        );
        Ok(graph)
    }
    fn live_at(&self, pc: usize, register: usize) -> bool {
        self.live
            .as_ref()
            .is_none_or(|live| live.contains(pc, register))
    }
    fn tick(&self) -> Result<()> {
        let n = self.work.get() + 1;
        ensure!(n <= 1_000_000, "control flow analysis exceeds work budget");
        self.work.set(n);
        Ok(())
    }
    fn reachable(&self, start: usize, stop: usize, words: &[u16]) -> Result<Vec<bool>> {
        let mut seen = vec![false; self.widths.len() + 1];
        let mut pending = vec![start];
        while let Some(pc) = pending.pop() {
            self.tick()?;
            ensure!(pc <= stop, "control flow crosses region boundary");
            if seen[pc] {
                continue;
            }
            seen[pc] = true;
            if pc == stop {
                continue;
            }
            ensure!(
                self.widths[pc] != 0 && !self.payloads[pc],
                "control flow enters payload or instruction interior"
            );
            if let Some(region) = self.loops.iter().find(|l| l.start == pc) {
                pending.push(region.exit);
                continue;
            }
            let op = words[pc] as u8;
            if matches!(op, 0x0e..=0x11 | 0x27) {
                continue;
            }
            if let Some(switch) = &self.switches[pc] {
                pending.extend(switch.cases.iter().map(|(_, target)| *target));
            }
            if let Some(target) = self.targets[pc] {
                pending.push(target);
            }
            if !matches!(op, 0x28..=0x2a) {
                pending.push(pc + self.widths[pc]);
            }
        }
        Ok(seen)
    }
    fn join(&self, a: usize, b: usize, stop: usize, words: &[u16]) -> Result<usize> {
        // A node reached from both entries is not necessarily their shared tail:
        // either entry may have another edge which bypasses it.  Such a node would
        // split a branch region before that edge, and rendering the edge then
        // crosses the artificial region boundary.  Pick the first instruction
        // that contains every nonterminal path from both entries instead.
        let mut pc = a.max(b);
        while pc <= stop {
            ensure!(
                pc == stop || self.widths[pc] != 0,
                "join is not an instruction boundary"
            );
            if let Some(escape) = self.region_is_closed(a, pc, words)? {
                pc = escape;
                continue;
            }
            if let Some(escape) = self.region_is_closed(b, pc, words)? {
                pc = escape;
                continue;
            }
            return Ok(pc);
        }
        bail!("control flow crosses region boundary")
    }
    /// Returns an instruction reached beyond `stop`, or `None` when every path
    /// stays within the region until it reaches `stop` or terminates. An escape
    /// also proves every candidate before that instruction invalid, allowing
    /// join discovery to jump over the whole range rather than rescan it.
    fn region_is_closed(&self, start: usize, stop: usize, words: &[u16]) -> Result<Option<usize>> {
        let mut seen = vec![false; self.widths.len() + 1];
        let mut pending = vec![start];
        while let Some(pc) = pending.pop() {
            self.tick()?;
            if pc > stop {
                return Ok(Some(pc));
            }
            if seen[pc] {
                continue;
            }
            seen[pc] = true;
            if pc == stop {
                continue;
            }
            ensure!(
                self.widths[pc] != 0 && !self.payloads[pc],
                "control flow enters payload or instruction interior"
            );
            if let Some(region) = self.loops.iter().find(|l| l.start == pc) {
                pending.push(region.exit);
                continue;
            }
            let op = words[pc] as u8;
            if matches!(op, 0x0e..=0x11 | 0x27) {
                continue;
            }
            if let Some(switch) = &self.switches[pc] {
                pending.extend(switch.cases.iter().map(|(_, target)| *target));
            }
            if let Some(target) = self.targets[pc] {
                pending.push(target);
            }
            if !matches!(op, 0x28..=0x2a) {
                pending.push(pc + self.widths[pc]);
            }
        }
        Ok(None)
    }
}
fn merge_type(left: Option<&Value>, right: Option<&Value>) -> Result<String> {
    let Some(left) = left else {
        return Ok(right.context("empty merge")?.ty.clone());
    };
    let Some(right) = right else {
        return Ok(left.ty.clone());
    };
    if left.ty == "I"
        && right.ty == "I"
        && matches!(left.literal, Some(0 | 1))
        && matches!(right.literal, Some(0 | 1))
    {
        return Ok("Z".into());
    }
    if left.ty == right.ty {
        return Ok(left.ty.clone());
    }
    if matches!(left.ty.as_str(), "I" | "B" | "S" | "C")
        && matches!(right.ty.as_str(), "I" | "B" | "S" | "C")
    {
        return Ok("I".into());
    }
    for (typed, literal) in [(left, right), (right, left)] {
        if (reference(&typed.ty) && literal.literal == Some(0))
            || (typed.ty == "Z" && matches!(literal.literal, Some(0 | 1)))
        {
            return Ok(typed.ty.clone());
        }
    }
    if reference(&left.ty) && reference(&right.ty) {
        return Ok("Ljava/lang/Object;".into());
    }
    bail!("incompatible register types at control flow join")
}
fn condition(op: u8, a: usize, regs: &[Option<Value>]) -> Result<String> {
    let zero = op >= 0x38;
    let kind = if zero { op - 0x38 } else { op - 0x32 };
    let lhs = register(regs, if zero { a } else { a & 15 })?;
    let rhs = if zero {
        Value {
            text: "0".into(),
            ty: "I".into(),
            literal: Some(0),
            wide_literal: None,
        }
    } else {
        register(regs, a >> 4)?
    };
    let symbol = ["==", "!=", "<", ">=", ">", "<="][kind as usize];
    let (left, right) = if reference(&lhs.ty) || reference(&rhs.ty) {
        ensure!(kind <= 1, "ordered reference comparison");
        let expr = |v: &Value| -> Result<String> {
            if v.literal == Some(0) {
                Ok("null".into())
            } else {
                ensure!(reference(&v.ty), "reference comparison with nonreference");
                if lhs.literal == Some(0) || rhs.literal == Some(0) {
                    Ok(v.text.clone())
                } else {
                    Ok(format!("((java.lang.Object) {})", v.text))
                }
            }
        };
        (expr(&lhs)?, expr(&rhs)?)
    } else if (lhs.ty == "Z" || rhs.ty == "Z") && kind <= 1 {
        ensure!(kind <= 1, "ordered boolean comparison");
        let left = argument(&lhs, "Z")?;
        let right = argument(&rhs, "Z")?;
        for (expression, constant) in [(&left, &right), (&right, &left)] {
            if matches!(constant.as_str(), "true" | "false") {
                return Ok(if (constant == "true") == (kind == 0) {
                    expression.clone()
                } else {
                    format!("!({expression})")
                });
            }
        }
        (left, right)
    } else {
        (integral(&lhs)?, integral(&rhs)?)
    };
    Ok(format!("{left} {symbol} {right}"))
}

pub(super) fn reconstruct(
    _class_name: &str,
    class: &DexClass,
    method: &DexMethod,
) -> Result<MethodBody> {
    let code = method.code.as_ref().context("method has no code")?;
    ensure!(
        usize::from(code.tries) == code.try_regions.len() && code.try_regions.len() <= 1,
        "nested or multiple try regions not reconstructed"
    );
    ensure!(
        code.try_regions.is_empty() || method.name.as_ref() != "<init>",
        "constructor exception regions not reconstructed"
    );
    ensure!(
        code.instructions.len() <= 65_536,
        "method exceeds instruction budget"
    );
    ensure!(code.ins <= code.registers, "invalid incoming registers");
    let mut regs = vec![None; code.registers as usize];
    let mut r = (code.registers - code.ins) as usize;
    let constructor = method.name.as_ref() == "<init>";
    if method.access_flags & 8 == 0 {
        assign(
            &mut regs,
            r,
            Value {
                text: "this".into(),
                ty: class.descriptor.to_string(),
                literal: None,
                wide_literal: None,
            },
        )?;
        r += 1;
    }
    for (i, ty) in method.parameters.iter().enumerate() {
        java_type(ty)?;
        assign(
            &mut regs,
            r,
            Value {
                text: format!("p{i}"),
                ty: ty.to_string(),
                literal: None,
                wide_literal: None,
            },
        )?;
        r += if matches!(ty.as_ref(), "J" | "D") {
            2
        } else {
            1
        };
    }
    ensure!(r == regs.len(), "parameter register count mismatch");
    let mut out = Output::default();
    let handlers: Vec<_> = code
        .try_regions
        .iter()
        .flat_map(|region| region.catches.iter().map(|(_, pc)| *pc as usize))
        .collect();
    let mut graph = if handlers.is_empty() {
        Graph::new(&code.instructions)?
    } else {
        Graph::with_handlers(&code.instructions, &handlers)?
    };
    // Handler snapshots are mutable, unlike ordinary structured branch/loop
    // joins.  Wide handler-visible state still needs its own reconstruction.
    if code.tries != 0 {
        let mut has_wide = wide(&method.return_type) || method.parameters.iter().any(|ty| wide(ty));
        for (pc, width) in graph
            .widths
            .iter()
            .enumerate()
            .filter(|(_, width)| **width != 0)
        {
            let op = code.instructions[pc] as u8;
            has_wide |= matches!(op, 0x04..=0x06 | 0x0b | 0x10 | 0x16..=0x19 | 0x2f..=0x31 | 0x45 | 0x4c | 0x53 | 0x5a | 0x61 | 0x68);
            has_wide |= numeric::Unary::decode(op)
                .is_some_and(|spec| spec.input.width() == 2 || spec.result.width() == 2);
            has_wide |= numeric::Binary::decode(op).is_some_and(|spec| spec.result.width() == 2);
            if matches!(op, 0x6e..=0x72 | 0x74..=0x78) && *width == 3 {
                let (_, proto, _) = class
                    .symbols
                    .methods
                    .get(code.instructions[pc + 1] as usize)
                    .context("method index")?;
                let (ret, args) = class
                    .symbols
                    .protos
                    .get(*proto as usize)
                    .context("method prototype")?;
                has_wide |= wide(ret) || args.iter().any(|ty| wide(ty));
            }
        }
        ensure!(!has_wide, "wide exception snapshots not reconstructed");
    }
    if !code.try_regions.is_empty()
        || graph.switches.iter().any(Option::is_some)
        || graph.targets.iter().enumerate().any(|(pc, target)| {
            target.is_some() && matches!(code.instructions[pc] as u8, 0x32..=0x3d)
        })
    {
        graph.live = super::liveness::analyze(code);
    }
    for region in &code.try_regions {
        ensure!(
            region.start < region.end
                && (region.end as usize) < graph.widths.len()
                && graph.widths[region.start as usize] != 0
                && graph.widths[region.end as usize] != 0,
            "try boundary is not executable instruction boundary"
        );
    }
    let (_, returned) = render(
        class,
        method,
        &graph,
        0,
        code.instructions.len(),
        regs,
        &mut out,
        0,
        !constructor,
        None,
        None,
    )?;
    ensure!(returned, "method falls off end");
    let mut body = MethodBody {
        text: out.text,
        links: out.links,
    };
    cleanup::inline_receivers(&mut body, &out.receiver_locals);
    Ok(body)
}

// Snapshot every carried value before assigning any loop slot: DEX moves can
// create aliases and swaps, so sequential assignments would change semantics.
fn carry_loop_values(
    slots: &[Option<Value>],
    values: &[Option<Value>],
    out: &mut Output,
) -> Result<()> {
    validate_wide_frame(slots)?;
    validate_wide_frame(values)?;
    let mut copies = Vec::new();
    for (slot, value) in slots.iter().zip(values) {
        let Some(slot) = slot else { continue };
        if slot.ty == "<wide-tail>" {
            continue;
        }
        let value = value.as_ref().context("loop loses initialized register")?;
        if value == slot {
            continue;
        }
        // No reference downcasts are introduced merely to join a loop.
        ensure!(
            value.ty == slot.ty
                || (slot.ty == "Ljava/lang/Object;" && reference(&value.ty))
                || value.literal.is_some()
                || (slot.ty == "I" && matches!(value.ty.as_str(), "B" | "C" | "S")),
            "loop changes register type"
        );
        let expression = argument(value, &slot.ty)?;
        let copy = out.local(&slot.ty, &expression, &[])?;
        copies.push((slot.text.clone(), copy.text));
    }
    for (slot, value) in copies {
        out.line(&format!("{slot} = {value};"), &[]);
    }
    Ok(())
}
#[allow(clippy::too_many_arguments)]
fn render_loop(
    class: &DexClass,
    method: &DexMethod,
    graph: &Graph,
    region: Loop,
    regs: Vec<Option<Value>>,
    out: &mut Output,
    depth: usize,
) -> Result<Vec<Option<Value>>> {
    ensure!(depth <= 32, "loop nesting exceeds budget");
    let words = &method
        .code
        .as_ref()
        .context("missing loop code")?
        .instructions;
    // Carry only values read before their next definition.  `loop_slots`
    // preserves wide heads and tails together while materializing Java locals
    // only for heads.
    let slots = loop_slots(&regs, graph, region.start, out)?;
    let exit_slots = if region.tail.is_some() {
        // Separate exit values from values required only by the next iteration.
        // In iterator loops the same DEX register may hold a String on the exit
        // path after holding an Iterator on the backedge.
        ensure!(
            (0..regs.len()).all(|r| !graph.live_at(region.exit, r) || regs[r].is_some()),
            "loop exit value has no established entry type"
        );
        Some(loop_slots(&regs, graph, region.exit, out)?)
    } else {
        None
    };
    let mut body = Output {
        sequence: out.sequence,
        indent: out.indent + 1,
        ..Default::default()
    };
    let (values, returned) = if let Some(guard) = region.guard {
        let (header_regs, returned) = render(
            class,
            method,
            graph,
            region.start,
            guard,
            slots.clone(),
            &mut body,
            depth,
            true,
            Some(region.start),
            None,
        )?;
        ensure!(!returned, "loop header returns");
        let cond = condition(
            words[guard] as u8,
            (words[guard] >> 8) as usize,
            &header_regs,
        )?;
        body.line(&format!("if ({cond}) {{"), &[]);
        body.indent += 1;
        carry_loop_values(
            exit_slots.as_ref().unwrap_or(&slots),
            &header_regs,
            &mut body,
        )?;
        body.line("break;", &[]);
        body.indent -= 1;
        body.line("}", &[]);
        render(
            class,
            method,
            graph,
            guard + graph.widths[guard],
            region.latch,
            header_regs,
            &mut body,
            depth,
            true,
            Some(region.start),
            None,
        )?
    } else {
        render(
            class,
            method,
            graph,
            region.start,
            region.latch,
            slots.clone(),
            &mut body,
            depth,
            true,
            Some(region.start),
            None,
        )?
    };
    ensure!(!returned, "loop body has no continuation");
    if let Some(tail) = region.tail {
        let cond = condition(
            words[region.latch] as u8,
            (words[region.latch] >> 8) as usize,
            &values,
        )?;
        body.line(&format!("if ({cond}) {{"), &[]);
        body.indent += 1;
        carry_loop_values(&slots, &values, &mut body)?;
        body.line("continue;", &[]);
        body.indent -= 1;
        body.line("}", &[]);
        let (tail_values, returned) = render(
            class,
            method,
            graph,
            tail,
            region.exit,
            values,
            &mut body,
            depth,
            true,
            Some(region.start),
            None,
        )?;
        ensure!(!returned, "loop exit tail has no common continuation");
        carry_loop_values(
            exit_slots.as_ref().expect("tail exit slots"),
            &tail_values,
            &mut body,
        )?;
        body.line("break;", &[]);
    } else if region.guard.is_none() {
        let cond = condition(
            (words[region.latch] as u8) ^ 1,
            (words[region.latch] >> 8) as usize,
            &values,
        )?;
        // Evaluate the condition before rewriting the loop slots.
        let test = body.local("Z", &cond, &[])?;
        carry_loop_values(&slots, &values, &mut body)?;
        body.line(&format!("if ({}) {{", test.text), &[]);
        body.indent += 1;
        body.line("break;", &[]);
        body.indent -= 1;
        body.line("}", &[]);
    } else {
        carry_loop_values(&slots, &values, &mut body)?;
    }
    out.sequence = body.sequence;
    out.line("while (true) {", &[]);
    out.append(body);
    out.line("}", &[]);
    ensure!(
        out.text.len() <= 4 * 1024 * 1024,
        "reconstructed method exceeds output budget"
    );
    Ok(exit_slots.unwrap_or(slots))
}

fn sync_exception_registers(
    slots: &[Option<Value>],
    regs: &[Option<Value>],
    previous: &mut [Option<Value>],
    out: &mut Output,
    allocating: bool,
) -> Result<()> {
    let mut visible = slots.to_vec();
    for (r, slot) in visible.iter_mut().enumerate() {
        if regs[r] == previous[r] {
            *slot = None;
            continue;
        }
        if let Some(value) = &regs[r] {
            if value.text.starts_with("<class:") {
                *slot = None;
                continue;
            }
            ensure!(
                !allocating || slot.as_ref().is_none_or(|old| old == value),
                "allocation reorders exception-visible register write"
            );
        }
    }
    carry_loop_values(&visible, regs, out)?;
    // Semantic register values remain immutable expressions. Only handlers read
    // mutable snapshots: rewriting registers to snapshot names corrupts aliases.
    previous.clone_from_slice(regs);
    Ok(())
}

fn catch_parent(ty: &str) -> Option<&'static str> {
    Some(match ty {
        "Ljava/lang/Exception;" | "Ljava/lang/Error;" => "Ljava/lang/Throwable;",
        "Ljava/lang/RuntimeException;" | "Ljava/io/IOException;" | "Lorg/json/JSONException;" => {
            "Ljava/lang/Exception;"
        }
        "Ljava/io/FileNotFoundException;" => "Ljava/io/IOException;",
        "Ljava/lang/ArithmeticException;"
        | "Ljava/lang/ArrayStoreException;"
        | "Ljava/lang/ClassCastException;"
        | "Ljava/lang/IllegalArgumentException;"
        | "Ljava/lang/IllegalStateException;"
        | "Ljava/lang/IndexOutOfBoundsException;"
        | "Ljava/lang/NullPointerException;"
        | "Ljava/lang/SecurityException;"
        | "Ljava/lang/UnsupportedOperationException;"
        | "Ljava/lang/NegativeArraySizeException;" => "Ljava/lang/RuntimeException;",
        "Ljava/lang/NumberFormatException;" => "Ljava/lang/IllegalArgumentException;",
        "Ljava/lang/ArrayIndexOutOfBoundsException;"
        | "Ljava/lang/StringIndexOutOfBoundsException;" => "Ljava/lang/IndexOutOfBoundsException;",
        _ => return None,
    })
}

fn validate_catch_order(
    class: &DexClass,
    catches: &[(Option<std::sync::Arc<str>>, u32)],
) -> Result<()> {
    ensure!(
        catches.len() <= 128,
        "exception handler count exceeds reconstruction budget"
    );
    for (index, (later, _)) in catches.iter().enumerate() {
        let later = later.as_deref().unwrap_or("Ljava/lang/Throwable;");
        for (earlier, _) in &catches[..index] {
            let earlier = earlier.as_deref().unwrap_or("Ljava/lang/Throwable;");
            ensure!(
                earlier != later && earlier != "Ljava/lang/Throwable;",
                "catch ordering shadows a later handler"
            );
            if later == "Ljava/lang/Throwable;" {
                continue;
            }
            if let Some(hierarchy) = class.symbols.hierarchy.get() {
                match hierarchy.assignable(later, earlier) {
                    crate::native_hierarchy::Relation::Proven => {
                        bail!("catch ordering shadows a later handler")
                    }
                    crate::native_hierarchy::Relation::Disproven => continue,
                    crate::native_hierarchy::Relation::Unknown => {}
                }
            }
            ensure!(
                catch_parent(earlier).is_some() && catch_parent(later).is_some(),
                "catch ordering requires unavailable type hierarchy"
            );
            let mut ancestor = Some(later);
            while let Some(ty) = ancestor {
                ensure!(ty != earlier, "catch ordering shadows a later handler");
                ancestor = catch_parent(ty);
            }
        }
    }
    Ok(())
}
// A protected region can exit through several goto trampolines. Only move
// those non-effectful instructions into the Java try; handler blocks embedded
// in its address interval must remain unreachable through normal edges.
fn protected_normal_exit(graph: &Graph, words: &[u16], start: usize, end: usize) -> Result<usize> {
    let mut pending = vec![start];
    let mut seen = vec![false; words.len()];
    let mut exits = std::collections::BTreeSet::new();
    while let Some(pc) = pending.pop() {
        graph.tick()?;
        if pc < start || pc >= end {
            let mut exit = pc;
            let mut trampolines = std::collections::HashSet::new();
            while exit < words.len() && matches!(words[exit] as u8, 0x28..=0x2a) {
                ensure!(trampolines.insert(exit), "cyclic protected exit");
                exit = graph.targets[exit].context("missing protected exit target")?;
            }
            ensure!(exit >= end, "protected flow escapes backward");
            exits.insert(exit);
            continue;
        }
        if seen[pc] {
            continue;
        }
        seen[pc] = true;
        let op = words[pc] as u8;
        ensure!(op != 0x0d, "normal flow enters move-exception");
        if matches!(op, 0x0e..=0x11 | 0x27) {
            continue;
        }
        if let Some(target) = graph.targets[pc] {
            pending.push(target);
        }
        if let Some(switch) = &graph.switches[pc] {
            pending.extend(switch.cases.iter().map(|(_, target)| *target));
        }
        if !matches!(op, 0x28..=0x2a) {
            pending.push(pc + graph.widths[pc]);
        }
    }
    ensure!(
        exits.len() <= 1,
        "protected region has distinct effectful exits"
    );
    Ok(exits.into_iter().next().unwrap_or(end))
}

#[allow(clippy::too_many_arguments)]
fn render_try(
    class: &DexClass,
    method: &DexMethod,
    graph: &Graph,
    region: &crate::native_dex::DexTryRegion,
    stop: usize,
    regs: Vec<Option<Value>>,
    out: &mut Output,
    depth: usize,
) -> Result<(Vec<Option<Value>>, usize, bool)> {
    ensure!(depth <= 32, "exception nesting exceeds budget");
    let words = &method
        .code
        .as_ref()
        .context("missing try code")?
        .instructions;
    let start = region.start as usize;
    let end = region.end as usize;
    ensure!(
        end <= stop && !region.catches.is_empty(),
        "try crosses enclosing region"
    );
    let normal_end = protected_normal_exit(graph, words, start, end)?;
    ensure!(
        normal_end <= stop,
        "try continuation crosses enclosing region"
    );
    let normal_reachable = graph.reachable(start, normal_end, words)?;
    ensure!(
        region
            .catches
            .iter()
            .all(|(_, handler)| { *handler >= region.end || !normal_reachable[*handler as usize] }),
        "normal flow enters interleaved handler"
    );
    for (_, handler) in region
        .catches
        .iter()
        .filter(|(_, handler)| *handler < region.end)
    {
        let handler_reachable = graph.reachable(*handler as usize, stop, words)?;
        for pc in start..end {
            // A throwing handler instruction inside its own protected range
            // can re-enter that handler. Moving it to Java catch would lose
            // that edge. Pure move/constant/branch prefixes are safe.
            ensure!(
                !handler_reachable[pc]
                    || matches!(words[pc] as u8, 0x00..=0x19 | 0x28..=0x2a | 0x32..=0x3d),
                "throwing handler instruction inside protected region"
            );
        }
    }
    validate_catch_order(class, &region.catches)?;
    for (ty, _) in region.catches.iter() {
        if let Some(ty) = ty {
            let mut declared_throw = false;
            for pc in start..end {
                if !normal_reachable[pc] || !matches!(words[pc] as u8, 0x6e..=0x72 | 0x74..=0x78) {
                    continue;
                }
                if let Some(&(owner, proto, name)) =
                    class.symbols.methods.get(words[pc + 1] as usize)
                    && let Some(owner) = class.symbols.types.get(owner as usize)
                    && let Some((ret, args)) = class.symbols.protos.get(proto as usize)
                    && let Some(name) = class.symbols.strings.get(name as usize)
                {
                    declared_throw |= throwing::known_call_throws(owner, name, args, ret, ty);
                }
            }
            throwing::validate_catch_type(class, ty, declared_throw)?;
        }
    }
    // Identify entry values actually observed by handlers (including their
    // continuation). Unused entry registers may freely change type in the try.
    let written = super::liveness::written_in(
        method.code.as_ref().context("missing try code")?,
        start,
        end,
    );
    let mut needed = vec![false; regs.len()];
    for (ty, handler) in region.catches.iter() {
        let handler = *handler as usize;
        let mut probe: Vec<_> = regs
            .iter()
            .enumerate()
            .map(|(r, value)| {
                value.as_ref().map(|v| {
                    if written.as_ref().is_some_and(|writes| !writes[r]) {
                        // Immutable entry values need no exception snapshot.
                        // Keep literal typing (notably boolean/null constants).
                        v.clone()
                    } else {
                        Value {
                            text: format!("__rdx_exception_input_{r}__"),
                            ty: v.ty.clone(),
                            literal: None,
                            wide_literal: None,
                        }
                    }
                })
            })
            .collect();
        let mut entry = handler;
        if words[handler] as u8 == 0x0d {
            assign(
                &mut probe,
                (words[handler] >> 8) as usize,
                Value {
                    text: "__rdx_caught__".into(),
                    ty: ty.as_deref().unwrap_or("Ljava/lang/Throwable;").into(),
                    literal: None,
                    wide_literal: None,
                },
            )?;
            entry += 1;
        }
        let mut dry = Output::default();
        let (_, returned) = render(
            class,
            method,
            graph,
            entry,
            words.len(),
            probe,
            &mut dry,
            depth,
            true,
            None,
            None,
        )?;
        ensure!(returned, "handler falls off method");
        for (r, observed) in needed.iter_mut().enumerate() {
            *observed |= dry.text.contains(&format!("__rdx_exception_input_{r}__"));
        }
    }
    let mut slots = vec![None; regs.len()];
    let mut initial = regs.clone();
    for (r, value) in regs.iter().enumerate() {
        if needed[r] {
            let value = value
                .as_ref()
                .context("handler observes undefined entry register")?;
            let slot = out.local(&value.ty, &value.text, &[])?;
            initial[r] = Some(slot.clone());
            slots[r] = Some(slot);
        }
    }
    let mut body = Output {
        sequence: out.sequence,
        indent: out.indent + 1,
        ..Default::default()
    };
    let (normal, normal_return) = render(
        class,
        method,
        graph,
        start,
        normal_end,
        regs,
        &mut body,
        depth,
        true,
        None,
        Some(&slots),
    )?;
    let mut visits = vec![0u16; words.len() + 1];
    let mut roots = Vec::new();
    if !normal_return {
        roots.push(normal_end);
    }
    roots.extend(region.catches.iter().map(|(_, addr)| *addr as usize));
    // Duplicate handlers count as a single path when finding a common tail.
    roots.sort_unstable();
    roots.dedup();
    for root in &roots {
        let seen = graph.reachable(*root, stop, words)?;
        for (count, seen) in visits.iter_mut().zip(seen) {
            *count += u16::from(seen);
        }
    }
    let join = if roots.len() == 1 && !normal_return && roots[0] == normal_end {
        roots[0]
    } else {
        visits
            .iter()
            .enumerate()
            .find_map(|(pc, count)| (*count > 1).then_some(pc))
            .unwrap_or(stop)
    };
    let mut paths = Vec::new();
    let mut normal_regs = normal;
    let mut normal_terminal = normal_return;
    if !normal_return {
        let normal_reachable = graph.reachable(normal_end, join, words)?;
        ensure!(
            region
                .catches
                .iter()
                .all(|(_, handler)| words[*handler as usize] as u8 != 0x0d
                    || !normal_reachable[*handler as usize]),
            "normal flow enters move-exception"
        );
        let mut continuation = Output {
            sequence: body.sequence,
            indent: body.indent,
            ..Default::default()
        };
        (normal_regs, normal_terminal) = render(
            class,
            method,
            graph,
            normal_end,
            join,
            normal_regs,
            &mut continuation,
            depth,
            true,
            None,
            None,
        )?;
        // Moving effects from outside the DEX try would change which throws
        // are caught. Only an empty goto path can be folded into the try arm.
        ensure!(
            continuation.text.is_empty(),
            "effectful normal continuation before exception join"
        );
        body.sequence = continuation.sequence;
    }
    let mut sequence = body.sequence;
    paths.push((usize::MAX, body, normal_regs, normal_terminal));
    let mut headers = Vec::new();
    let mut types = std::collections::HashSet::new();
    for (index, (ty, handler)) in region.catches.iter().enumerate() {
        let ty = ty.as_deref().unwrap_or("Ljava/lang/Throwable;");
        ensure!(types.insert(ty), "duplicate catch type");
        let mut entry = *handler as usize;
        ensure!(entry <= join, "partially overlapping exception handlers");
        let name = format!("e{sequence}");
        sequence += 1;
        let mut values = initial.clone();
        if words[entry] as u8 == 0x0d {
            assign(
                &mut values,
                (words[entry] >> 8) as usize,
                Value {
                    text: name.clone(),
                    ty: ty.into(),
                    literal: None,
                    wide_literal: None,
                },
            )?;
            entry += 1;
        }
        ensure!(
            entry <= join,
            "move-exception intersects shared continuation"
        );
        let mut handler_body = Output {
            sequence,
            indent: out.indent + 1,
            ..Default::default()
        };
        let (values, terminal) = render(
            class,
            method,
            graph,
            entry,
            join,
            values,
            &mut handler_body,
            depth,
            true,
            None,
            None,
        )?;
        sequence = handler_body.sequence;
        paths.push((index, handler_body, values, terminal));
        headers.push((
            java_type(ty)?,
            name,
            class_label(ty).context("invalid catch type")?,
        ));
    }
    out.sequence = sequence;
    let mut merged = initial;
    merge_path_registers(&mut merged, &mut paths, out, graph, join)?;
    let all_returned = paths.iter().all(|path| path.3);
    out.line("try {", &[]);
    let mut paths = paths.into_iter();
    out.append(paths.next().context("missing try body")?.1);
    for ((_, body, _, _), (ty, name, label)) in paths.zip(headers) {
        out.line(
            &format!("}} catch ({ty} {name}) {{"),
            &[(9, ty.chars().count(), label)],
        );
        out.append(body);
    }
    out.line("}", &[]);
    ensure!(
        out.text.len() <= 4 * 1024 * 1024,
        "reconstructed method exceeds output budget"
    );
    Ok((merged, join, all_returned))
}

type RenderedPath = (usize, Output, Vec<Option<Value>>, bool);
fn merge_path_registers(
    regs: &mut [Option<Value>],
    arms: &mut [RenderedPath],
    out: &mut Output,
    graph: &Graph,
    join: usize,
) -> Result<()> {
    validate_wide_frame(regs)?;
    for arm in arms.iter() {
        validate_wide_frame(&arm.2)?;
    }
    let mut r = 0;
    while r < regs.len() {
        if regs[r]
            .as_ref()
            .is_some_and(|value| value.ty == "<wide-tail>")
        {
            r += 1;
            continue;
        }
        let entry_wide = regs[r].as_ref().is_some_and(|value| wide(&value.ty));
        let live: Vec<_> = arms.iter().filter(|arm| !arm.3).collect();
        let potential_wide = entry_wide
            || live
                .iter()
                .any(|arm| arm.2[r].as_ref().is_some_and(|value| wide(&value.ty)));
        let live_here = graph.live_at(join, r) || (potential_wide && graph.live_at(join, r + 1));
        if !live_here {
            regs[r] = None;
            if entry_wide {
                regs[r + 1] = None;
                r += 2;
            } else {
                r += 1;
            }
            continue;
        }
        if live.is_empty() || live.iter().any(|arm| arm.2[r].is_none()) {
            regs[r] = None;
            if entry_wide {
                regs[r + 1] = None;
                r += 2;
            } else {
                r += 1;
            }
            continue;
        }
        let first = live[0].2[r].as_ref().context("missing switch value")?;
        ensure!(first.ty != "<wide-tail>", "wide tail merged without head");
        let merged_wide = wide(&first.ty);
        ensure!(
            live.iter().all(|arm| {
                arm.2[r]
                    .as_ref()
                    .is_some_and(|value| wide(&value.ty) == merged_wide)
            }),
            "wide register width mismatch at control flow join"
        );
        if live.iter().all(|arm| arm.2[r].as_ref() == Some(first))
            && (regs[r].as_ref() == Some(first)
                || first.literal.is_some()
                || first.text.starts_with('"'))
        {
            regs[r] = Some(first.clone());
            if merged_wide {
                regs[r + 1] = Some(Value {
                    text: r.to_string(),
                    ty: "<wide-tail>".into(),
                    literal: None,
                    wide_literal: None,
                });
                r += 2;
            } else {
                r += 1;
            }
            continue;
        }
        let mut representative = first.clone();
        for arm in &live[1..] {
            let ty = merge_type(Some(&representative), arm.2[r].as_ref())?;
            if ty != representative.ty {
                representative.ty = ty;
                representative.literal = None;
            }
        }
        let ty = representative.ty;
        let name = format!("v{}", out.sequence);
        out.sequence += 1;
        let display = java_type(&ty)?;
        let refs = class_label(&ty)
            .map(|label| vec![(0, display.chars().count(), label)])
            .unwrap_or_default();
        out.line(&format!("{display} {name};"), &refs);
        for (_, body, values, returned) in arms.iter_mut() {
            if !*returned {
                body.line(
                    &format!(
                        "{name} = {};",
                        argument(
                            values[r].as_ref().context("missing switch merge value")?,
                            &ty
                        )?
                    ),
                    &[],
                );
            }
        }
        assign(
            regs,
            r,
            Value {
                text: name,
                ty,
                literal: None,
                wide_literal: None,
            },
        )?;
        r += if merged_wide { 2 } else { 1 };
    }
    Ok(())
}
#[allow(clippy::too_many_arguments)]
fn render_switch(
    class: &DexClass,
    method: &DexMethod,
    graph: &Graph,
    pc: usize,
    stop: usize,
    switch: &Switch,
    mut regs: Vec<Option<Value>>,
    out: &mut Output,
    depth: usize,
) -> Result<(Vec<Option<Value>>, usize, bool)> {
    ensure!(depth <= 32, "switch nesting exceeds budget");
    let words = &method
        .code
        .as_ref()
        .context("missing switch code")?
        .instructions;
    let selector = integral(&register(&regs, (words[pc] >> 8) as usize)?)?;
    let default = pc + graph.widths[pc];
    let mut starts = vec![default];
    for (_, target) in &switch.cases {
        if !starts.contains(target) {
            starts.push(*target);
        }
    }
    let mut visits = vec![0u16; words.len() + 1];
    for start in &starts {
        let seen = graph.reachable(*start, stop, words)?;
        for (count, seen) in visits.iter_mut().zip(seen) {
            *count += u16::from(seen);
        }
    }
    // Stop at the first overlapping instruction, not merely a join common to
    // every arm: otherwise partially shared tails could duplicate side effects.
    let join = visits
        .iter()
        .enumerate()
        .find_map(|(pos, count)| (*count > 1).then_some(pos))
        .unwrap_or(stop);
    let mut arms = Vec::new();
    let mut sequence = out.sequence;
    for start in starts {
        ensure!(start <= join, "switch has partially overlapping arms");
        let mut body = Output {
            sequence,
            indent: out.indent + 2,
            ..Default::default()
        };
        let (values, returned) = render(
            class,
            method,
            graph,
            start,
            join,
            regs.clone(),
            &mut body,
            depth,
            true,
            None,
            None,
        )?;
        sequence = body.sequence;
        arms.push((start, body, values, returned));
    }
    out.sequence = sequence;
    merge_path_registers(&mut regs, &mut arms, out, graph, join)?;
    out.line(&format!("switch ({selector}) {{"), &[]);
    let all_returned = arms.iter().all(|arm| arm.3);
    for (start, mut body, _, returned) in arms {
        out.indent += 1;
        for (key, target) in &switch.cases {
            if *target == start {
                out.line(&format!("case {key}:"), &[]);
            }
        }
        if start == default {
            out.line("default:", &[]);
        }
        out.line("{", &[]);
        if !returned {
            body.line("break;", &[]);
        }
        out.append(body);
        out.line("}", &[]);
        out.indent -= 1;
    }
    out.line("}", &[]);
    ensure!(
        out.text.len() <= 4 * 1024 * 1024,
        "reconstructed method exceeds output budget"
    );
    Ok((regs, join, all_returned))
}

#[allow(clippy::too_many_arguments)]
fn render(
    class: &DexClass,
    method: &DexMethod,
    graph: &Graph,
    mut pc: usize,
    stop: usize,
    mut regs: Vec<Option<Value>>,
    out: &mut Output,
    depth: usize,
    mut initialized: bool,
    suppressed_loop: Option<usize>,
    exception_slots: Option<&[Option<Value>]>,
) -> Result<(Vec<Option<Value>>, bool)> {
    ensure!(depth <= 32, "control flow nesting exceeds budget");
    let words = &method
        .code
        .as_ref()
        .context("method has no code")?
        .instructions;
    let constructor = method.name.as_ref() == "<init>";
    let mut returned = false;
    let mut pending: Option<Value> = None;
    let mut allocation: Option<(usize, String)> = None;
    let mut class_captures: Vec<(String, String, Option<String>, Option<String>)> = Vec::new();
    let mut previous_exception_values = regs.clone();
    let mut early_field_writes = false;
    while pc < stop {
        graph.tick()?;
        ensure!(!returned, "instructions after return");
        if exception_slots.is_none()
            && let Some(region) = method
                .code
                .as_ref()
                .unwrap()
                .try_regions
                .iter()
                .find(|region| region.start as usize == pc)
        {
            ensure!(
                initialized && allocation.is_none() && pending.is_none(),
                "try interrupts instruction state"
            );
            let (values, next, terminal) =
                render_try(class, method, graph, region, stop, regs, out, depth + 1)?;
            regs = values;
            if terminal {
                return Ok((regs, true));
            }
            pc = next;
            continue;
        }
        if Some(pc) != suppressed_loop
            && let Some(region) = graph.loops.iter().find(|l| l.start == pc)
        {
            ensure!(
                initialized && pending.is_none() && allocation.is_none(),
                "loop interrupts instruction state"
            );
            ensure!(region.exit <= stop, "loop crosses region boundary");
            ensure!(
                exception_slots.is_none(),
                "loop inside try not reconstructed"
            );
            regs = render_loop(class, method, graph, *region, regs, out, depth + 1)?;
            pc = region.exit;
            continue;
        }
        let w = words[pc];
        let op = w as u8;
        let a = (w >> 8) as usize;
        ensure!(
            allocation.is_none() || matches!(op, 0x00..=0x09 | 0x12..=0x19 | 0x1c | 0x70 | 0x76),
            "effectful instruction between allocation and constructor"
        );
        let width = graph.widths[pc];
        if matches!(op, 0x28..=0x2a) {
            ensure!(initialized, "branch before constructor initialization");
            let target = graph.targets[pc].context("missing goto target")?;
            ensure!(
                target > pc || graph.acyclic_backwards.contains(&pc),
                "unstructured backward jump"
            );
            pc = target;
            ensure!(pc <= stop, "goto crosses region boundary");
            pending = None;
            continue;
        }
        if let Some(switch) = &graph.switches[pc] {
            ensure!(
                exception_slots.is_none(),
                "switch inside try not reconstructed"
            );
            ensure!(initialized, "switch before constructor initialization");
            let (next_regs, next_pc, all_returned) =
                render_switch(class, method, graph, pc, stop, switch, regs, out, depth + 1)?;
            regs = next_regs;
            if all_returned {
                return Ok((regs, true));
            }
            pc = next_pc;
            pending = None;
            continue;
        }
        if matches!(op, 0x32..=0x3d) {
            ensure!(initialized, "branch before constructor initialization");
            let target = graph.targets[pc].context("missing branch target")?;
            ensure!(
                target > pc && target <= stop,
                "branch crosses region boundary"
            );
            let condition = condition(op, a, &regs)?;
            let inverse = self::condition(op ^ 1, a, &regs)?;
            let join = graph.join(pc + width, target, stop, words)?;
            let mut yes = Output {
                sequence: out.sequence,
                indent: out.indent + 1,
                ..Default::default()
            };
            let (yes_regs, yes_return) = render(
                class,
                method,
                graph,
                target,
                join,
                regs.clone(),
                &mut yes,
                depth + 1,
                initialized,
                suppressed_loop,
                exception_slots,
            )?;
            let mut no = Output {
                sequence: yes.sequence,
                indent: out.indent + 1,
                ..Default::default()
            };
            let (no_regs, no_return) = render(
                class,
                method,
                graph,
                pc + width,
                join,
                regs.clone(),
                &mut no,
                depth + 1,
                initialized,
                suppressed_loop,
                exception_slots,
            )?;
            out.sequence = no.sequence;
            validate_wide_frame(&regs)?;
            validate_wide_frame(&yes_regs)?;
            validate_wide_frame(&no_regs)?;
            let mut r = 0;
            while r < regs.len() {
                if regs[r]
                    .as_ref()
                    .is_some_and(|value| value.ty == "<wide-tail>")
                {
                    r += 1;
                    continue;
                }
                let entry_wide = regs[r].as_ref().is_some_and(|value| wide(&value.ty));
                let left = if yes_return {
                    None
                } else {
                    yes_regs[r].as_ref()
                };
                let right = if no_return { None } else { no_regs[r].as_ref() };
                let potential_wide = entry_wide
                    || left.is_some_and(|value| wide(&value.ty))
                    || right.is_some_and(|value| wide(&value.ty));
                if !(graph.live_at(join, r) || (potential_wide && graph.live_at(join, r + 1))) {
                    regs[r] = None;
                    if entry_wide {
                        regs[r + 1] = None;
                        r += 2;
                    } else {
                        r += 1;
                    }
                    continue;
                }
                let next = match (left, right) {
                    (None, None) => None,
                    (Some(v), None) if no_return => Some(v),
                    (None, Some(v)) if yes_return => Some(v),
                    (Some(v), Some(w)) if v == w => Some(v),
                    (Some(_), Some(_)) => None,
                    _ => {
                        regs[r] = None;
                        if entry_wide {
                            regs[r + 1] = None;
                            r += 2;
                        } else {
                            r += 1;
                        }
                        continue;
                    }
                };
                if let Some(value) = next {
                    ensure!(value.ty != "<wide-tail>", "wide tail merged without head");
                }
                let merged_wide = left.or(right).is_some_and(|value| wide(&value.ty));
                ensure!(
                    left.is_none_or(|value| wide(&value.ty) == merged_wide)
                        && right.is_none_or(|value| wide(&value.ty) == merged_wide),
                    "wide register width mismatch at control flow join"
                );
                if let Some(v) = next
                    && (regs[r].as_ref() == Some(v)
                        || v.literal.is_some()
                        || v.text.starts_with('"'))
                {
                    regs[r] = Some(v.clone());
                    if merged_wide {
                        regs[r + 1] = Some(Value {
                            text: r.to_string(),
                            ty: "<wide-tail>".into(),
                            literal: None,
                            wide_literal: None,
                        });
                        r += 2;
                    } else {
                        r += 1;
                    }
                    continue;
                }
                if left.is_none() && right.is_none() {
                    regs[r] = None;
                    if entry_wide {
                        regs[r + 1] = None;
                        r += 2;
                    } else {
                        r += 1;
                    }
                    continue;
                }
                let ty = merge_type(left, right)?;
                let name = format!("v{}", out.sequence);
                out.sequence += 1;
                let display = java_type(&ty)?;
                let refs = class_label(&ty)
                    .map(|label| vec![(0, display.chars().count(), label)])
                    .unwrap_or_default();
                out.line(&format!("{display} {name};"), &refs);
                if let Some(v) = left {
                    yes.line(&format!("{name} = {};", argument(v, &ty)?), &[]);
                }
                if let Some(v) = right {
                    no.line(&format!("{name} = {};", argument(v, &ty)?), &[]);
                }
                assign(
                    &mut regs,
                    r,
                    Value {
                        text: name,
                        ty,
                        literal: None,
                        wide_literal: None,
                    },
                )?;
                r += if merged_wide { 2 } else { 1 };
            }
            if yes.text.is_empty() {
                out.line(&format!("if ({inverse}) {{"), &[]);
                out.append(no);
                out.line("}", &[]);
            } else if no.text.is_empty() {
                out.line(&format!("if ({condition}) {{"), &[]);
                out.append(yes);
                out.line("}", &[]);
            } else {
                out.line(&format!("if ({condition}) {{"), &[]);
                out.append(yes);
                out.line("} else {", &[]);
                out.append(no);
                out.line("}", &[]);
            }
            ensure!(
                out.text.len() <= 4 * 1024 * 1024,
                "reconstructed method exceeds output budget"
            );
            if yes_return && no_return {
                return Ok((regs, true));
            }
            pc = join;
            previous_exception_values.clone_from_slice(&regs);
            pending = None;
            continue;
        }
        if !matches!(op, 0x0a..=0x0c) {
            pending = None;
        }
        match op {
            0x00 => ensure!(w == 0, "payload in straight-line body"),
            0x01..=0x09 => {
                let (dst, src) = match (op - 1) % 3 {
                    0 => (a & 15, a >> 4),
                    1 => (a, words[pc + 1] as usize),
                    _ => (words[pc + 1] as usize, words[pc + 2] as usize),
                };
                let value = register(&regs, src)?;
                let kind = (op - 1) / 3;
                ensure!(
                    match kind {
                        0 => !reference(&value.ty) && !matches!(value.ty.as_str(), "J" | "D"),
                        1 => matches!(value.ty.as_str(), "J" | "D"),
                        _ => reference(&value.ty) || value.literal == Some(0),
                    },
                    "move type mismatch"
                );
                ensure!(
                    dst == src
                        || !value.text.starts_with("<class:")
                        || exception_slots
                            .is_none_or(|slots| slots.get(dst).is_none_or(Option::is_none)),
                    "deferred class alias writes exception-visible register"
                );
                assign(&mut regs, dst, value)?;
            }
            0x0a..=0x0c => {
                let value = pending.take().context("move-result without invoke")?;
                ensure!(
                    match op {
                        0x0a => !reference(&value.ty) && !matches!(value.ty.as_str(), "J" | "D"),
                        0x0b => matches!(value.ty.as_str(), "J" | "D"),
                        _ => reference(&value.ty),
                    },
                    "result opcode type mismatch"
                );
                assign(&mut regs, a, value)?;
            }
            0x0d => bail!("move-exception outside handled entry"),
            0x27 => {
                ensure!(
                    initialized && allocation.is_none(),
                    "throw before initialization"
                );
                let expression = throwing::expression(class, method, &register(&regs, a)?)?;
                out.line(&format!("throw {expression};"), &[]);
                returned = true;
            }
            0x0e => {
                ensure!(
                    method.return_type.as_ref() == "V" && initialized,
                    "invalid void return"
                );
                out.line("return;", &[]);
                returned = true;
            }
            0x0f..=0x11 => {
                ensure!(
                    initialized && method.return_type.as_ref() != "V",
                    "invalid return"
                );
                ensure!(
                    (op == 0x11) == reference(&method.return_type)
                        && (op == 0x10) == wide(&method.return_type),
                    "return opcode type mismatch"
                );
                let value = register(&regs, a)?;
                let expr = argument(&value, &method.return_type)?;
                out.line(&format!("return {expr};"), &[]);
                returned = true;
            }
            0x12..=0x15 => {
                let (dst, n) = match op {
                    0x12 => (a & 15, ((w as i16) >> 12) as i32),
                    0x13 => (a, words[pc + 1] as i16 as i32),
                    0x14 => (
                        a,
                        (words[pc + 1] as u32 | ((words[pc + 2] as u32) << 16)) as i32,
                    ),
                    _ => (a, (words[pc + 1] as i32) << 16),
                };
                assign(
                    &mut regs,
                    dst,
                    Value {
                        text: n.to_string(),
                        ty: "I".into(),
                        literal: Some(n),
                        wide_literal: None,
                    },
                )?;
            }
            0x16..=0x19 => {
                let bits = match op {
                    0x16 => words[pc + 1] as i16 as i64 as u64,
                    0x17 => {
                        (words[pc + 1] as u32 | ((words[pc + 2] as u32) << 16)) as i32 as i64 as u64
                    }
                    0x18 => (0..4).fold(0u64, |bits, i| {
                        bits | ((words[pc + 1 + i] as u64) << (16 * i))
                    }),
                    _ => (words[pc + 1] as u64) << 48,
                };
                assign(
                    &mut regs,
                    a,
                    Value {
                        text: format!("{}L", bits as i64),
                        ty: "J".into(),
                        literal: None,
                        wide_literal: Some(bits),
                    },
                )?;
            }
            0x2d..=0x31 => {
                let spec = numeric::Compare::decode(op).context("comparison opcode")?;
                let packed = words[pc + 1];
                let lhs = argument(
                    &register(&regs, (packed & 255) as usize)?,
                    spec.input.descriptor(),
                )?;
                let rhs = argument(
                    &register(&regs, (packed >> 8) as usize)?,
                    spec.input.descriptor(),
                )?;
                let value = out.local("I", &spec.expression(&lhs, &rhs), &[])?;
                assign(&mut regs, a, value)?;
            }
            0x1a | 0x1b => {
                let mut index = words[pc + 1] as usize;
                if op == 0x1b {
                    index |= (words[pc + 2] as usize) << 16;
                }
                let s = class.symbols.strings.get(index).context("string index")?;
                assign(
                    &mut regs,
                    a,
                    Value {
                        text: string_literal(s)?,
                        ty: "Ljava/lang/String;".into(),
                        literal: None,
                        wide_literal: None,
                    },
                )?;
            }
            0x1c if allocation.is_some() => {
                let ty = class
                    .symbols
                    .types
                    .get(words[pc + 1] as usize)
                    .context("class literal type")?;
                let display = java_type(ty)?;
                let snapshot =
                    if let Some(Some(slot)) = exception_slots.and_then(|slots| slots.get(a)) {
                        ensure!(
                            slot.ty == "Ljava/lang/Class;",
                            "class capture changes exception register type"
                        );
                        Some(slot.text.clone())
                    } else {
                        None
                    };
                let name = format!("v{}", out.sequence);
                out.sequence += 1;
                out.line(&format!("java.lang.Class {name};"), &[]);
                let index = class_captures.len();
                class_captures.push((name, display, class_label(ty), snapshot));
                assign(
                    &mut regs,
                    a,
                    Value {
                        text: format!("<class:{index}>"),
                        ty: "Ljava/lang/Class;".into(),
                        literal: None,
                        wide_literal: None,
                    },
                )?;
            }
            0x1c | 0x1f..=0x21 | 0x23 | 0x44..=0x51 | 0x8d..=0x8f => {
                ensure!(
                    initialized,
                    "array/type operation before constructor initialization"
                );
                operations::emit(
                    class,
                    op,
                    a,
                    if width > 1 { words[pc + 1] } else { 0 },
                    &mut regs,
                    out,
                )?;
            }
            0x24 | 0x25 => {
                ensure!(
                    initialized,
                    "filled array before constructor initialization"
                );
                pending = Some(operations::filled(
                    class,
                    op,
                    a,
                    words[pc + 1],
                    words[pc + 2],
                    &regs,
                    out,
                )?);
            }
            0x22 => {
                if initialized
                    && exception_slots.is_none()
                    && method
                        .code
                        .as_ref()
                        .is_some_and(|code| code.try_regions.is_empty())
                    && let Some(lowered) = allocation_lowering::try_lower(
                        class, method, graph, words, pc, stop, &regs, out,
                    )?
                {
                    regs = lowered.regs;
                    out.sequence = lowered.out.sequence;
                    out.append(lowered.out);
                    pc = lowered.next_pc;
                    continue;
                }
                ensure!(
                    exception_slots.is_none_or(|slots| slots.get(a).is_none_or(Option::is_none)),
                    "allocation overwrites exception-visible register"
                );
                ensure!(initialized, "allocation before constructor initialization");
                let ty = class
                    .symbols
                    .types
                    .get(words[pc + 1] as usize)
                    .context("allocation type")?;
                ensure!(ty.starts_with('L'), "new-instance requires class");
                java_type(ty)?;
                assign(
                    &mut regs,
                    a,
                    Value {
                        text: "<uninitialized>".into(),
                        ty: ty.to_string(),
                        literal: None,
                        wide_literal: None,
                    },
                )?;
                allocation = Some((a, ty.to_string()));
            }
            0x52..=0x6d => {
                let &(owner, ty, name) = class
                    .symbols
                    .fields
                    .get(words[pc + 1] as usize)
                    .context("field index")?;
                let owner = class
                    .symbols
                    .types
                    .get(owner as usize)
                    .context("field owner")?;
                let ty = class.symbols.types.get(ty as usize).context("field type")?;
                let name = class
                    .symbols
                    .strings
                    .get(name as usize)
                    .context("field name")?;
                let display_name = names::member(name)?;
                let family = if op >= 0x67 {
                    op - 0x67
                } else if op >= 0x60 {
                    op - 0x60
                } else if op >= 0x59 {
                    op - 0x59
                } else {
                    op - 0x52
                };
                ensure!(
                    match family {
                        0 => matches!(ty.as_ref(), "I" | "F"),
                        1 => wide(ty),
                        2 => reference(ty),
                        3 => ty.as_ref() == "Z",
                        4 => ty.as_ref() == "B",
                        5 => ty.as_ref() == "C",
                        6 => ty.as_ref() == "S",
                        _ => false,
                    },
                    "field opcode type mismatch"
                );
                let is_static = op >= 0x60;
                let put = if is_static { op >= 0x67 } else { op >= 0x59 };
                if !initialized {
                    // Java 25 early construction permits assignments to fields
                    // declared in this class, but forbids reading/escaping this.
                    // Keep DEX order: superclass callbacks may observe the write.
                    ensure!(
                        constructor
                            && !is_static
                            && put
                            && owner.as_ref() == class.descriptor.as_ref()
                            && register(&regs, a >> 4)?.text == "this"
                            && class.fields.iter().any(|field| !field.is_static
                                && field.declaring_type == class.descriptor
                                && field.name.as_ref() == name
                                && field.field_type.as_ref() == ty.as_ref()),
                        "unsupported field access before constructor initialization"
                    );
                    ensure!(
                        register(&regs, a & 15)?.text != "this",
                        "uninitialized this escapes through field value"
                    );
                    early_field_writes = true;
                }
                let target = if is_static {
                    java_type(owner)?
                } else {
                    receiver(&register(&regs, a >> 4)?, owner)?
                };
                let expr = format!("{target}.{display_name}");
                let label = format!(
                    "{}.{}:{}",
                    owner
                        .trim_start_matches('L')
                        .trim_end_matches(';')
                        .replace('/', "."),
                    name,
                    ty
                );
                let mut refs = vec![(
                    target.chars().count() + 1,
                    display_name.chars().count(),
                    label,
                )];
                if is_static {
                    refs.push((
                        0,
                        target.chars().count(),
                        class_label(owner).context("invalid owner")?,
                    ));
                }
                let reg = if is_static { a } else { a & 15 };
                if put {
                    let value = argument(&register(&regs, reg)?, ty)?;
                    out.line(&format!("{expr} = {value};"), &refs);
                } else {
                    let value = out.local(ty, &expr, &refs)?;
                    assign(&mut regs, reg, value)?;
                }
            }
            0x6e..=0x72 | 0x74..=0x78 => {
                let &(owner, proto, name) = class
                    .symbols
                    .methods
                    .get(words[pc + 1] as usize)
                    .context("method index")?;
                let owner = class
                    .symbols
                    .types
                    .get(owner as usize)
                    .context("method owner")?;
                let (ret, args) = class
                    .symbols
                    .protos
                    .get(proto as usize)
                    .context("method prototype")?;
                let name = class
                    .symbols
                    .strings
                    .get(name as usize)
                    .context("method name")?;
                let kind = if op >= 0x74 { op - 6 } else { op };
                let static_call = kind == 0x71;
                let inputs: Vec<usize> = if op >= 0x74 {
                    (words[pc + 2] as usize..words[pc + 2] as usize + a).collect()
                } else {
                    let n = a >> 4;
                    ensure!(n <= 5, "invoke register count");
                    let packed = words[pc + 2];
                    let all = [
                        (packed & 15) as usize,
                        ((packed >> 4) & 15) as usize,
                        ((packed >> 8) & 15) as usize,
                        ((packed >> 12) & 15) as usize,
                        a & 15,
                    ];
                    all[..n].to_vec()
                };
                let mut cursor = 0;
                let target = if static_call {
                    java_type(owner)?
                } else {
                    let value =
                        register(&regs, *inputs.first().context("missing invoke receiver")?)?;
                    cursor = 1;
                    receiver(&value, owner)?
                };
                let mut actual = Vec::new();
                let mut capture_count = 0;
                let mut capture_refs = Vec::new();
                for ty in args {
                    let reg = *inputs.get(cursor).context("missing invoke arg")?;
                    let mut value = register(&regs, reg)?;
                    if let Some(index) = value
                        .text
                        .strip_prefix("<class:")
                        .and_then(|v| v.strip_suffix('>'))
                        .and_then(|v| v.parse::<usize>().ok())
                    {
                        ensure!(
                            allocation.is_some() && name == "<init>",
                            "deferred class literal outside allocation"
                        );
                        let (local, display, label, snapshot) = &class_captures[index];
                        ensure!(
                            index <= capture_count,
                            "constructor arguments reorder class resolution"
                        );
                        if index == capture_count {
                            let prefix = if let Some(snapshot) = snapshot {
                                format!("({snapshot} = ({local} = ")
                            } else {
                                format!("({local} = ")
                            };
                            value.text = format!(
                                "{prefix}{display}.class{}",
                                if snapshot.is_some() { "))" } else { ")" }
                            );
                            if let Some(label) = label {
                                capture_refs.push((
                                    actual.len(),
                                    prefix.chars().count(),
                                    display.chars().count(),
                                    label.clone(),
                                ));
                            }
                            capture_count += 1;
                        } else {
                            value.text = local.clone();
                        }
                    }
                    ensure!(
                        value.text != "<uninitialized>",
                        "uninitialized invocation argument"
                    );
                    // Typed null preserves the DEX descriptor when Java has
                    // overloads (including constructors and varargs arrays).
                    actual.push(if value.literal == Some(0) && reference(ty) {
                        format!("(({}) null)", java_type(ty)?)
                    } else if ty.as_ref() == "I" && matches!(value.ty.as_str(), "B" | "S" | "C") {
                        format!("((int) {})", value.text)
                    } else {
                        argument(&value, ty)?
                    });
                    if matches!(ty.as_ref(), "J" | "D") {
                        ensure!(
                            inputs.get(cursor + 1) == Some(&(reg + 1)),
                            "nonconsecutive wide argument"
                        );
                        cursor += 2;
                    } else {
                        cursor += 1;
                    }
                }
                ensure!(cursor == inputs.len(), "extra invoke args");
                let label = format!(
                    "{}.{}({}){}",
                    owner
                        .trim_start_matches('L')
                        .trim_end_matches(';')
                        .replace('/', "."),
                    name,
                    args.join(""),
                    ret
                );
                ensure!(name != "<clinit>", "static initializer invocation");
                if name == "<init>" {
                    if let Some((dst, ty)) = allocation.take() {
                        ensure!(
                            kind == 0x70
                                && inputs.first() == Some(&dst)
                                && owner.as_ref() == ty
                                && ret.as_ref() == "V",
                            "allocation constructor mismatch"
                        );
                        ensure!(
                            register(&regs, dst)?.text == "<uninitialized>",
                            "allocation register overwritten"
                        );
                        ensure!(
                            capture_count == class_captures.len(),
                            "unused class literal during allocation"
                        );
                        let display = java_type(owner)?;
                        let expr = format!("new {display}({})", actual.join(", "));
                        let mut refs = vec![(4, display.chars().count(), label)];
                        for (arg, start, len, label) in capture_refs {
                            let offset = 5
                                + display.chars().count()
                                + actual[..arg]
                                    .iter()
                                    .map(|s| s.chars().count() + 2)
                                    .sum::<usize>();
                            // The Class-valued capture itself has no conversion
                            // unless the declared parameter type differs.
                            let cast_prefix = if args[arg].as_ref() == "Ljava/lang/Class;" {
                                0
                            } else {
                                format!("(({}) ", java_type(&args[arg])?).chars().count()
                            };
                            refs.push((offset + cast_prefix + start, len, label));
                        }
                        let value = out.local(owner, &expr, &refs)?;
                        for register in regs.iter_mut().flatten() {
                            if register.text == "<uninitialized>" {
                                *register = value.clone();
                            }
                            if let Some(index) = register
                                .text
                                .strip_prefix("<class:")
                                .and_then(|v| v.strip_suffix('>'))
                                .and_then(|v| v.parse::<usize>().ok())
                            {
                                register.text = class_captures[index].0.clone();
                            }
                        }
                        class_captures.clear();
                        if let Some(slots) = exception_slots {
                            sync_exception_registers(
                                slots,
                                &regs,
                                &mut previous_exception_values,
                                out,
                                false,
                            )?;
                        }
                        pc += width;
                        continue;
                    }
                    ensure!(
                        constructor
                            && !initialized
                            && kind == 0x70
                            && (owner.as_ref() == class.descriptor.as_ref()
                                || class.superclass.as_deref() == Some(owner.as_ref()))
                            && ret.as_ref() == "V",
                        "unsupported constructor invocation"
                    );
                    let receiver = register(&regs, inputs[0])?;
                    ensure!(receiver.text == "this", "constructor receiver");
                    for reg in &inputs[1..] {
                        ensure!(
                            register(&regs, *reg)?.text != "this",
                            "uninitialized this as constructor argument"
                        );
                    }
                    let keyword = if owner.as_ref() == class.descriptor.as_ref() {
                        ensure!(
                            !early_field_writes,
                            "field writes before this delegation not reconstructed"
                        );
                        ensure!(
                            *args != method.parameters,
                            "recursive constructor delegation"
                        );
                        "this"
                    } else {
                        "super"
                    };
                    out.line(
                        &format!("{keyword}({});", actual.join(", ")),
                        &[(0, keyword.len(), label)],
                    );
                    initialized = true;
                } else {
                    ensure!(allocation.is_none(), "invalid method call");
                    if !initialized {
                        ensure!(
                            constructor && kind != 0x6f,
                            "invalid early construction call"
                        );
                        for input in &inputs {
                            if regs
                                .get(*input)
                                .and_then(Option::as_ref)
                                .is_some_and(|value| value.ty == "<wide-tail>")
                            {
                                continue;
                            }
                            let value = register(&regs, *input)?;
                            ensure!(
                                value.text != "this" && value.text != "<uninitialized>",
                                "uninitialized this escapes through invocation"
                            );
                        }
                    }
                    let display_name = names::member(name)?;
                    if kind == 0x70 {
                        ensure!(
                            owner.as_ref() == class.descriptor.as_ref()
                                && class.methods.iter().any(|m| m.name.as_ref() == name
                                    && m.parameters == *args
                                    && m.return_type == *ret
                                    && m.access_flags & 2 != 0),
                            "nonprivate direct call not reconstructed"
                        );
                    }
                    let target = if kind == 0x6f {
                        let receiver = register(&regs, inputs[0])?;
                        // DEX may name an ancestor above the direct superclass.
                        // Both class invoke-super and Java super dispatch from
                        // the nearest superclass; do not turn this into a cast
                        // or a normal virtual call. Interface-super is distinct.
                        let ancestor = class.symbols.hierarchy.get().map_or_else(
                            || class.superclass.as_deref() == Some(owner.as_ref()),
                            |hierarchy| {
                                hierarchy.strict_superclass(&class.descriptor, owner)
                                    == crate::native_hierarchy::Relation::Proven
                            },
                        );
                        ensure!(
                            receiver.text == "this" && ancestor,
                            "invalid super receiver"
                        );
                        "super".into()
                    } else {
                        target
                    };
                    let expr = format!("{target}.{display_name}({})", actual.join(", "));
                    let mut refs = vec![(
                        target.chars().count() + 1,
                        display_name.chars().count(),
                        label,
                    )];
                    if static_call {
                        refs.push((
                            0,
                            target.chars().count(),
                            class_label(owner).context("invalid owner")?,
                        ));
                    }
                    if ret.as_ref() == "V" {
                        out.line(&format!("{expr};"), &refs);
                    } else {
                        pending = Some(out.local(ret, &expr, &refs)?);
                    }
                }
            }
            0x7b..=0x8c => {
                let spec = numeric::Unary::decode(op).context("unary opcode")?;
                let source = argument(&register(&regs, a >> 4)?, spec.input.descriptor())?;
                let value = out.local(spec.result.descriptor(), &spec.expression(&source), &[])?;
                ensure!(value.ty == spec.result_descriptor, "unary result type");
                assign(&mut regs, a & 15, value)?;
            }
            0x9b..=0xaf | 0xbb..=0xcf => {
                let spec = numeric::Binary::decode(op).context("binary opcode")?;
                let (dst, left, right) = if op >= 0xb0 {
                    (a & 15, a & 15, a >> 4)
                } else {
                    let packed = words[pc + 1];
                    (a, (packed & 255) as usize, (packed >> 8) as usize)
                };
                let left = argument(&register(&regs, left)?, spec.left.descriptor())?;
                let right = argument(&register(&regs, right)?, spec.right.descriptor())?;
                let value = out.local(
                    spec.result.descriptor(),
                    &spec.expression(&left, &right),
                    &[],
                )?;
                assign(&mut regs, dst, value)?;
            }
            0x90..=0x9a | 0xb0..=0xba | 0xd0..=0xe2 => {
                let boolean_xor_literal = if matches!(op, 0xd7 | 0xdf) {
                    let (source, literal) = if op == 0xd7 {
                        (a >> 4, words[pc + 1] as i16 as i32)
                    } else {
                        (
                            (words[pc + 1] & 255) as usize,
                            (words[pc + 1] >> 8) as i8 as i32,
                        )
                    };
                    let source = register(&regs, source)?;
                    (source.ty == "Z" && matches!(literal, 0 | 1)).then_some((
                        if op == 0xd7 { a & 15 } else { a },
                        source,
                        literal,
                    ))
                } else {
                    None
                };
                if let Some((dst, source, literal)) = boolean_xor_literal {
                    let expression = if literal == 0 {
                        source.text
                    } else {
                        format!("!({})", source.text)
                    };
                    let value = out.local("Z", &expression, &[])?;
                    assign(&mut regs, dst, value)?;
                } else {
                    let (dst, lhs, rhs, kind) = if op <= 0x9a {
                        let w = words[pc + 1];
                        (
                            a,
                            integral(&register(&regs, (w & 255) as usize)?)?,
                            integral(&register(&regs, (w >> 8) as usize)?)?,
                            op - 0x90,
                        )
                    } else if op <= 0xba {
                        (
                            a & 15,
                            integral(&register(&regs, a & 15)?)?,
                            integral(&register(&regs, a >> 4)?)?,
                            op - 0xb0,
                        )
                    } else if op <= 0xd7 {
                        (
                            a & 15,
                            integral(&register(&regs, a >> 4)?)?,
                            (words[pc + 1] as i16).to_string(),
                            op - 0xd0,
                        )
                    } else {
                        (
                            a,
                            integral(&register(&regs, (words[pc + 1] & 255) as usize)?)?,
                            ((words[pc + 1] >> 8) as i8).to_string(),
                            op - 0xd8,
                        )
                    };
                    let (lhs, rhs) = if matches!(op, 0xd1 | 0xd9) {
                        (rhs, lhs)
                    } else {
                        (lhs, rhs)
                    };
                    let value = out.local("I", &format!("{lhs} {} ({rhs})", opname(kind)?), &[])?;
                    assign(&mut regs, dst, value)?;
                }
            }
            _ => unreachable!(),
        }
        ensure!(
            out.text.len() <= 4 * 1024 * 1024,
            "reconstructed method exceeds output budget"
        );
        if !returned && let Some(slots) = exception_slots {
            sync_exception_registers(
                slots,
                &regs,
                &mut previous_exception_values,
                out,
                allocation.is_some(),
            )?;
        }
        pc += width;
        if returned {
            return Ok((regs, true));
        }
    }
    ensure!(allocation.is_none(), "unfinished allocation");
    Ok((regs, returned))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_dex::{DexCode, DexSymbols};
    use std::sync::Arc;
    fn fixture(
        words: Vec<u16>,
        registers: u16,
        ins: u16,
        parameters: Vec<Arc<str>>,
        ret: &str,
    ) -> (DexClass, DexMethod) {
        let class = DexClass {
            symbols: Arc::new(DexSymbols::default()),
            descriptor: "Lsample/Example;".into(),
            superclass: Some("Ljava/lang/Object;".into()),
            interfaces: vec![],
            access_flags: 1,
            annotations_offset: 0,
            static_values_offset: 0,
            static_values: vec![],
            fields: vec![],
            methods: vec![],
        };
        let method = DexMethod {
            declaring_type: class.descriptor.clone(),
            name: "run".into(),
            return_type: ret.into(),
            parameters,
            thrown_types: vec![],
            access_flags: 9,
            code: Some(DexCode {
                registers,
                ins,
                outs: 0,
                tries: 0,
                try_regions: vec![],
                instructions: words,
                offset: 0,
            }),
        };
        (class, method)
    }
    #[test]
    fn dead_incompatible_branch_values_do_not_block_reconstruction() {
        let (c, mut m) = fixture(
            vec![0x0138, 4, 0x3001, 0x0228, 0x2007, 0x000e],
            4,
            3,
            vec!["I".into(), "Ljava/lang/Object;".into(), "I".into()],
            "V",
        );
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert!(body.text.ends_with("        return;\n"));
        assert!(!body.text.contains("java.lang.Object v"));
        m.return_type = "Ljava/lang/Object;".into();
        m.code.as_mut().unwrap().instructions[5] = 0x0011;
        assert!(
            reconstruct("sample.Example", &c, &m).is_err(),
            "a genuinely live incompatible join must still reject"
        );
    }
    #[test]
    fn dead_switch_joins_discard_types_but_preserve_case_structure() {
        let (c, m) = fixture(
            vec![
                0x012b, 10, 0, 0x2007, 0x0528, 0x3001, 0x0328, 0x7012, 0x0128, 0x000e, 0x0100, 2,
                0, 0, 5, 0, 7, 0,
            ],
            4,
            3,
            vec!["I".into(), "Ljava/lang/Object;".into(), "I".into()],
            "V",
        );
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert!(body.text.contains("switch (p0)"));
        assert!(!body.text.contains("java.lang.Object v"));
    }
    #[test]
    fn dead_join_elimination_keeps_effectful_field_reads() {
        let (mut c, m) = fixture(
            vec![0x0138, 5, 0x0060, 0, 0x0328, 0x0062, 1, 0x000e],
            2,
            1,
            vec!["I".into()],
            "V",
        );
        c.symbols = Arc::new(DexSymbols {
            types: vec![
                "Lsample/Example;".into(),
                "I".into(),
                "Ljava/lang/Object;".into(),
            ],
            strings: vec!["number".into(), "object".into()],
            fields: vec![(0, 1, 0), (0, 2, 1)],
            ..Default::default()
        });
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert_eq!(body.text.matches("sample.Example.number").count(), 1);
        assert_eq!(body.text.matches("sample.Example.object").count(), 1);
        assert!(!body.text.contains("java.lang.Object v2;"));
        assert_eq!(
            body.links
                .iter()
                .filter(|link| link.label.ends_with(".number:I")
                    || link.label.ends_with(".object:Ljava/lang/Object;"))
                .count(),
            2
        );
    }
    #[test]
    fn interleaved_handler_and_multiple_goto_exits_preserve_shared_tail() {
        let (c, mut m) = fixture(
            vec![
                0x0138, 7, 0x1012, 0x0628, 0x000d, 0xf012, 0x0428, 0x2012, 0x0228, 0x0128, 0x000f,
            ],
            2,
            1,
            vec!["I".into()],
            "I",
        );
        let code = m.code.as_mut().unwrap();
        code.tries = 1;
        code.try_regions.push(crate::native_dex::DexTryRegion {
            start: 0,
            end: 9,
            catches: vec![(Some("Ljava/lang/Exception;".into()), 4)].into(),
        });
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert_eq!(
            body.text.matches("catch (java.lang.Exception").count(),
            1,
            "{}",
            body.text
        );
        assert_eq!(body.text.matches("return ").count(), 1, "{}", body.text);
        assert!(body.text.contains("= -1;"), "{}", body.text);
    }

    #[test]
    fn interleaved_handler_cannot_lose_self_catching_throw_edges() {
        let (c, mut m) = fixture(
            vec![
                0x0138, 8, 0x1012, 0x0728, 0x000d, 0x001a, 0, 0x0428, 0x2012, 0x0228, 0x0128,
                0x000f,
            ],
            2,
            1,
            vec!["I".into()],
            "I",
        );
        let code = m.code.as_mut().unwrap();
        code.tries = 1;
        code.try_regions.push(crate::native_dex::DexTryRegion {
            start: 0,
            end: 10,
            catches: vec![(Some("Ljava/lang/Exception;".into()), 4)].into(),
        });
        assert!(
            reconstruct("sample.Example", &c, &m)
                .err()
                .unwrap()
                .to_string()
                .contains("throwing handler instruction inside protected region")
        );
    }

    #[test]
    fn normal_flow_cannot_enter_interleaved_handler() {
        let (c, mut m) = fixture(vec![0x0012, 0x000d, 0x0012, 0x000f], 1, 0, vec![], "I");
        let code = m.code.as_mut().unwrap();
        code.tries = 1;
        code.try_regions.push(crate::native_dex::DexTryRegion {
            start: 0,
            end: 3,
            catches: vec![(Some("Ljava/lang/Exception;".into()), 1)].into(),
        });
        assert!(
            reconstruct("sample.Example", &c, &m)
                .err()
                .unwrap()
                .to_string()
                .contains("normal flow enters")
        );
    }

    #[test]
    fn unused_exception_join_does_not_merge_incompatible_entry_parameter() {
        let (c, mut m) = fixture(
            vec![0x7012, 0x000e],
            1,
            1,
            vec!["Ljava/lang/Object;".into()],
            "V",
        );
        let code = m.code.as_mut().unwrap();
        code.tries = 1;
        code.try_regions.push(crate::native_dex::DexTryRegion {
            start: 0,
            end: 1,
            catches: vec![(Some("Ljava/lang/Exception;".into()), 1)].into(),
        });
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert!(body.text.contains("catch (java.lang.Exception"));
        assert!(!body.text.contains("java.lang.Object v"));
    }
    #[test]
    fn sibling_reference_branches_join_as_object_without_downcasts() {
        let (c, m) = fixture(
            vec![0x0138, 4, 0x2007, 0x0228, 0x3007, 0x0011],
            4,
            3,
            vec!["I".into(), "Lsample/Left;".into(), "Lsample/Right;".into()],
            "Ljava/lang/Object;",
        );
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert!(body.text.contains("java.lang.Object v0;"));
        assert!(body.text.contains("((java.lang.Object) p1)"));
        assert!(body.text.contains("((java.lang.Object) p2)"));
        assert!(body.text.ends_with("        return v0;\n"));
        assert!(!body.text.contains("((sample.Left)"));
        assert!(!body.text.contains("((sample.Right)"));
    }
    #[test]
    fn null_reference_joins_preserve_reference_type_and_object_loop_slots_accept_references() {
        let null = Value {
            text: "0".into(),
            ty: "I".into(),
            literal: Some(0),
            wide_literal: None,
        };
        let reference_value = Value {
            text: "p0".into(),
            ty: "Lsample/Left;".into(),
            literal: None,
            wide_literal: None,
        };
        assert_eq!(
            merge_type(Some(&null), Some(&reference_value)).unwrap(),
            "Lsample/Left;"
        );
        assert_eq!(
            merge_type(Some(&reference_value), Some(&null)).unwrap(),
            "Lsample/Left;"
        );
        let slot = Value {
            text: "slot".into(),
            ty: "Ljava/lang/Object;".into(),
            literal: None,
            wide_literal: None,
        };
        let mut out = Output::default();
        carry_loop_values(&[Some(slot)], &[Some(reference_value)], &mut out).unwrap();
        assert!(out.text.contains("((java.lang.Object) p0)"));
        assert!(!out.text.contains("((sample.Left)"));
    }
    #[test]
    fn wide_loop_slots_keep_pair_ownership_and_never_lower_a_tail() {
        let wide = Value {
            text: "p0".into(),
            ty: "J".into(),
            literal: None,
            wide_literal: None,
        };
        let regs = vec![
            Some(wide),
            Some(Value {
                text: "0".into(),
                ty: "<wide-tail>".into(),
                literal: None,
                wide_literal: None,
            }),
        ];
        let graph = Graph::new(&[0x000e]).unwrap();
        let mut out = Output::default();
        let slots = loop_slots(&regs, &graph, 0, &mut out).unwrap();
        assert_eq!(slots[0].as_ref().unwrap().ty, "J");
        assert_eq!(slots[1].as_ref().unwrap().ty, "<wide-tail>");
        assert_eq!(slots[1].as_ref().unwrap().text, "0");
        assert!(out.text.contains("long v0 = p0;"));
        assert!(!out.text.contains("wide-tail"));
    }
    #[test]
    fn malformed_wide_frame_is_rejected_before_loop_lowering() {
        let regs = vec![Some(Value {
            text: "p0".into(),
            ty: "D".into(),
            literal: None,
            wide_literal: None,
        })];
        let graph = Graph::new(&[0x000e]).unwrap();
        assert!(loop_slots(&regs, &graph, 0, &mut Output::default()).is_err());
    }
    #[test]
    fn wide_diamond_merges_a_single_long_head() {
        // if (p2 == 0) return p0; else return p1; with the shared return
        // reached through v0/v1 in both arms.
        let (c, m) = fixture(
            vec![0x0438, 4, 0x2004, 0x0228, 0x0000, 0x0010],
            5,
            5,
            vec!["J".into(), "J".into(), "I".into()],
            "J",
        );
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert_eq!(body.text.matches("long v").count(), 1, "{}", body.text);
        assert!(body.text.ends_with("        return v0;\n"), "{}", body.text);
        assert!(!body.text.contains("wide-tail"));
    }
    #[test]
    fn double_branch_merges_a_single_head() {
        let (c, m) = fixture(
            vec![0x0438, 4, 0x2004, 0x0228, 0x0000, 0x0010],
            5,
            5,
            vec!["D".into(), "D".into(), "I".into()],
            "D",
        );
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert_eq!(body.text.matches("double v").count(), 1, "{}", body.text);
        assert!(body.text.ends_with("        return v0;\n"), "{}", body.text);
        assert!(!body.text.contains("wide-tail"));
    }
    #[test]
    fn ambiguous_raw_double_branch_preserves_the_literal_fallback() {
        // A branch cannot infer whether raw const-wide bits are a long or a
        // double before a downstream typed use.  Keep the existing fail-closed
        // fallback; in particular, do not invent tail locals or reinterpret
        // signed-zero bits as long values.
        let (c, m) = fixture(
            vec![
                0x0438, 8, 0x0018, 0, 0, 0, 0x8000, 0x0628, 0x0018, 0, 0, 0, 0, 0x0010,
            ],
            5,
            1,
            vec!["I".into()],
            "D",
        );
        assert!(reconstruct("sample.Example", &c, &m).is_err());
    }
    #[test]
    fn wide_loop_carries_only_the_long_head() {
        // while (p1 != 0) p1--; return p0;
        let (c, m) = fixture(
            vec![0x0238, 5, 0x02d8, 0xff02, 0xfc28, 0x0010],
            3,
            3,
            vec!["J".into(), "I".into()],
            "J",
        );
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert!(body.text.contains("while (true)"), "{}", body.text);
        assert!(body.text.contains("long v0 = p0;"), "{}", body.text);
        assert!(body.text.ends_with("        return v0;\n"), "{}", body.text);
        assert!(!body.text.contains("wide-tail"));
    }
    #[test]
    fn switch_reference_joins_support_typed_receiver_after_merge() {
        let (mut c, m) = fixture(
            vec![
                0x012b, 14, 0, 0x2007, 0x0528, 0x3007, 0x0328, 0x4007, 0x0128, 0x106e, 0, 0,
                0x000a, 0x000f, 0x0100, 2, 0, 0, 5, 0, 7, 0,
            ],
            5,
            4,
            vec![
                "I".into(),
                "Lsample/Left;".into(),
                "Lsample/Right;".into(),
                "Ljava/lang/Object;".into(),
            ],
            "I",
        );
        c.symbols = Arc::new(DexSymbols {
            types: vec!["Ljava/lang/Object;".into()],
            strings: vec!["hashCode".into()],
            protos: vec![("I".into(), vec![])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        });
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert!(body.text.contains("java.lang.Object v0;"));
        assert_eq!(body.text.matches("v0.hashCode()").count(), 1);
        assert_eq!(
            body.links
                .iter()
                .filter(|l| l.label == "java.lang.Object.hashCode()I")
                .count(),
            1
        );
    }
    #[test]
    fn invocation_widening_cast_preserves_integer_overload() {
        let (mut c, m) = fixture(vec![0x1071, 0, 0, 0x000e], 1, 1, vec!["B".into()], "V");
        c.symbols = Arc::new(DexSymbols {
            types: vec!["Lsample/Example;".into()],
            strings: vec!["accept".into()],
            protos: vec![("V".into(), vec!["I".into()])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        });
        assert!(
            reconstruct("sample.Example", &c, &m)
                .unwrap()
                .text
                .contains("accept(((int) p0))")
        );
    }
    #[test]
    fn ancestor_super_calls_emit_super_and_preserve_raw_method_links() {
        let (mut c, mut m) = fixture(vec![0x206f, 0, 0x0010, 0x000e], 2, 2, vec!["I".into()], "V");
        c.superclass = Some("Lsample/Parent;".into());
        c.symbols = Arc::new(DexSymbols {
            types: vec!["Lsample/Grandparent;".into(), "Lsample/Interface;".into()],
            strings: vec!["callback".into()],
            protos: vec![("V".into(), vec!["I".into()])],
            methods: vec![(0, 0, 0), (1, 0, 0)],
            ..Default::default()
        });
        let (mut parent, _) = fixture(vec![0x000e], 0, 0, vec![], "V");
        parent.descriptor = "Lsample/Parent;".into();
        parent.superclass = Some("Lsample/Grandparent;".into());
        parent.interfaces = vec!["Lsample/Interface;".into()];
        let hierarchy =
            crate::native_hierarchy::TypeHierarchy::from_classes([&c, &parent]).unwrap();
        c.symbols.hierarchy.set(Arc::new(hierarchy)).unwrap();
        m.access_flags = 1;
        for invoke in [vec![0x206f, 0, 0x0010, 0x000e], vec![0x0275, 0, 0, 0x000e]] {
            m.code.as_mut().unwrap().instructions = invoke;
            let body = reconstruct("sample.Example", &c, &m).unwrap();
            assert_eq!(body.text, "        super.callback(p0);\n        return;\n");
            assert_eq!(body.links[0].label, "sample.Grandparent.callback(I)V");
            assert_eq!(
                &body.text[body.links[0].start..body.links[0].end],
                "callback"
            );
        }
        // An interface is assignable but is not a superclass dispatch target.
        m.code.as_mut().unwrap().instructions = vec![0x206f, 1, 0x0010, 0x000e];
        assert!(reconstruct("sample.Example", &c, &m).is_err());
        // Merely having the same receiver type is insufficient: it must be this.
        m.access_flags = 9;
        m.parameters = vec!["Lsample/Example;".into(), "I".into()];
        m.code.as_mut().unwrap().instructions = vec![0x206f, 0, 0x0010, 0x000e];
        assert!(reconstruct("sample.Example", &c, &m).is_err());
    }

    #[test]
    fn superclass_constructor_accepts_parameters_and_preserves_navigation() {
        let (mut c, mut m) = fixture(vec![0x2070, 0, 0x0010, 0x000e], 2, 2, vec!["I".into()], "V");
        c.superclass = Some("Lsample/Parent;".into());
        c.symbols = Arc::new(DexSymbols {
            types: vec!["Lsample/Parent;".into()],
            strings: vec!["<init>".into()],
            protos: vec![("V".into(), vec!["I".into()])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        });
        m.name = "<init>".into();
        m.access_flags = 1;
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert_eq!(body.text, "        super(p0);\n        return;\n");
        assert_eq!(body.links[0].label, "sample.Parent.<init>(I)V");
        assert_eq!(&body.text[body.links[0].start..body.links[0].end], "super");
        // invoke-direct/range has identical constructor semantics.
        m.code.as_mut().unwrap().instructions = vec![0x0276, 0, 0, 0x000e];
        assert_eq!(
            reconstruct("sample.Example", &c, &m).unwrap().text,
            body.text
        );
        c.superclass = Some("Lsample/Unrelated;".into());
        assert!(reconstruct("sample.Example", &c, &m).is_err());
    }
    #[test]
    fn delegated_constructor_accepts_constant_and_java25_prologue_computation() {
        let (mut c, mut m) = fixture(vec![0x7012, 0x2070, 0, 0x0001, 0x000e], 2, 1, vec![], "V");
        c.symbols = Arc::new(DexSymbols {
            types: vec!["Lsample/Example;".into()],
            strings: vec!["<init>".into()],
            protos: vec![("V".into(), vec!["I".into()])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        });
        m.name = "<init>".into();
        m.access_flags = 1;
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert_eq!(body.text, "        this(7);\n        return;\n");
        m.code.as_mut().unwrap().instructions =
            vec![0x7012, 0x00d8, 0x0100, 0x2070, 0, 0x0001, 0x000e];
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert!(
            body.text.find('+').unwrap() < body.text.find("this(").unwrap(),
            "{}",
            body.text
        );
        assert_eq!(body.text.matches("this(").count(), 1);
    }
    #[test]
    fn constructor_rejects_uninitialized_receiver_as_argument() {
        let (mut c, mut m) = fixture(vec![0x2070, 0, 0x0000, 0x000e], 1, 1, vec![], "V");
        c.symbols = Arc::new(DexSymbols {
            types: vec!["Ljava/lang/Object;".into()],
            strings: vec!["<init>".into()],
            protos: vec![("V".into(), vec!["Ljava/lang/Object;".into()])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        });
        m.name = "<init>".into();
        m.access_flags = 1;
        assert!(reconstruct("sample.Example", &c, &m).is_err());
    }
    #[test]
    fn switch_shared_field_effect_and_navigation_are_emitted_once() {
        let (mut c, m) = fixture(
            vec![
                0x012b, 12, 0, 0x0012, 0x0528, 0x1012, 0x0328, 0x2012, 0x0128, 0x0060, 0, 0x000f,
                0x0100, 2, 0, 0, 5, 0, 7, 0,
            ],
            2,
            1,
            vec!["I".into()],
            "I",
        );
        c.symbols = Arc::new(DexSymbols {
            types: vec!["Lsample/Example;".into(), "I".into()],
            strings: vec!["value".into()],
            fields: vec![(0, 1, 0)],
            ..Default::default()
        });
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert_eq!(body.text.matches("sample.Example.value").count(), 1);
        let links: Vec<_> = body
            .links
            .iter()
            .filter(|link| link.label == "sample.Example.value:I")
            .collect();
        assert_eq!(links.len(), 1);
        let link = links[0];
        assert_eq!(
            body.text
                .chars()
                .skip(link.start)
                .take(link.end - link.start)
                .collect::<String>(),
            "value"
        );
    }
    #[test]
    fn switch_payload_is_never_an_executable_target() {
        // A valid packed payload exists, but normal switch fallthrough enters it.
        assert!(Graph::new(&[0x012b, 4, 0, 0, 0x0100, 0, 0, 0]).is_err());
        // Orphaned payloads are rejected as well.
        assert!(Graph::new(&[0x000e, 0, 0x0100, 0, 0, 0]).is_err());
    }
    #[test]
    fn backward_shared_tail_is_acyclic_and_preserves_each_branch_value() {
        // Both arms add 3; the second arm jumps backwards to the shared add.
        // That edge cannot reach its source and must not become a while loop.
        let words = vec![
            0x0138, 6, 0x1012, 0x00d8, 0x0300, 0x0328, 0x2012, 0xfc28, 0x000f,
        ];
        let graph = Graph::new(&words).unwrap();
        assert!(graph.loops.is_empty());
        assert!(graph.acyclic_backwards.contains(&7));
        let (c, m) = fixture(words, 2, 1, vec!["I".into()], "I");
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert!(!body.text.contains("while"), "{}", body.text);
        assert!(body.text.contains("1 + (3)"), "{}", body.text);
        assert!(body.text.contains("2 + (3)"), "{}", body.text);
        assert_eq!(body.text.matches("return ").count(), 1);
    }
    #[test]
    fn guarded_conditional_loop_keeps_exit_values_separate_from_backedge_values() {
        // if p0<=0 exit; p0--; if p0>0 repeat; p1=99; p1++; exit:return p1.
        let (c, m) = fixture(
            vec![
                0x003d, 10, 0x00d8, 0xff00, 0x003c, 0xfffc, 0x0113, 99, 0x01d8, 0x0101, 0x010f,
            ],
            2,
            2,
            vec!["I".into(), "I".into()],
            "I",
        );
        let graph = Graph::new(&m.code.as_ref().unwrap().instructions).unwrap();
        assert_eq!(graph.loops.len(), 1);
        assert_eq!(graph.loops[0].tail, Some(6));
        assert_eq!(graph.loops[0].exit, 10);
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert_eq!(body.text.matches("break;").count(), 2, "{}", body.text);
        let tail = body.text.find("99 + (1)").unwrap();
        assert!(body.text[..tail].contains("continue;"), "{}", body.text);
        assert!(body.text.ends_with("        return v2;\n"), "{}", body.text);
        assert_eq!(
            body.text.matches("v2 = ").count(),
            3,
            "entry, guard exit and tail exit: {}",
            body.text
        );
    }

    #[test]
    fn exit_tail_may_change_a_dead_loop_register_type_but_not_a_live_exit_type() {
        // p0 is integral on the backedge and a String only on the exit tail.
        let (mut c, mut m) = fixture(
            vec![0x003d, 8, 0x00d8, 0xff00, 0x003c, 0xfffc, 0x001a, 0, 0x000e],
            1,
            1,
            vec!["I".into()],
            "V",
        );
        Arc::get_mut(&mut c.symbols)
            .unwrap()
            .strings
            .push("exit".into());
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert!(body.text.contains("while (true) {"));
        m.return_type = "I".into();
        m.code.as_mut().unwrap().instructions[8] = 0x000f;
        assert!(reconstruct("sample.Example", &c, &m).is_err());
    }

    #[test]
    fn guarded_loop_rejects_tail_entry_and_tail_backedge() {
        // A branch outside the loop enters its one-time exit tail.
        let entry = [
            0x0138, 8, 0x003d, 10, 0x00d8, 0xff00, 0x003c, 0xfffc, 0x0113, 99, 0x01d8, 0x0101,
            0x010f,
        ];
        let error = Graph::new(&entry).err().unwrap().to_string();
        assert!(error.contains("interior entry"), "{error}");
        let backedge = [
            0x003d, 10, 0x00d8, 0xff00, 0x003c, 0xfffc, 0x0113, 99, 0x0139, 0xfffa, 0x010f,
        ];
        assert!(Graph::new(&backedge).is_err());
    }

    #[test]
    fn while_loop_materializes_carried_values_and_keeps_exit_in_scope() {
        // v0 = 0; while (v0 < p0) { v0++; } return v0.
        let (c, m) = fixture(
            vec![0x0012, 0x1035, 5, 0x00d8, 0x0100, 0xfc28, 0x000f],
            2,
            1,
            vec!["I".into()],
            "I",
        );
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert!(body.text.contains("while (true) {"));
        assert!(body.text.contains("if (v0 >= v1) {"));
        assert!(body.text.contains("break;"));
        assert!(body.text.ends_with("        return v0;\n"));
    }
    #[test]
    fn do_loop_tests_before_parallel_carried_assignments() {
        // do { p0--; } while (p0 > 0); return p0.
        let (c, m) = fixture(
            vec![0x00d8, 0xff00, 0x003c, 0xfffe, 0x000f],
            1,
            1,
            vec!["I".into()],
            "I",
        );
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        let test = body.text.find("boolean ").unwrap();
        let update = body.text.find("v0 = v").unwrap();
        assert!(test < update);
        assert!(body.text.ends_with("        return v0;\n"));
    }
    #[test]
    fn loops_reject_interior_entries_and_overlapping_backedges() {
        assert!(Graph::new(&[0x0228, 0, 0x003c, 0xffff, 0x000e]).is_err());
        assert!(Graph::new(&[0, 0x003c, 0xffff, 0x003c, 0xfffd, 0x000e]).is_err());
    }
    #[test]
    fn register_overwrites_do_not_rewrite_previous_values() {
        // v0=p0+p1; v1=v0; v0=5; return v1.
        let (c, m) = fixture(
            vec![0x0090, 0x0302, 0x0101, 0x5012, 0x010f],
            4,
            2,
            vec!["I".into(), "I".into()],
            "I",
        );
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert_eq!(
            body.text,
            "        int v0 = p0 + (p1);\n        return v0;\n"
        );
    }
    #[test]
    fn boolean_and_null_constants_have_contextual_types() {
        let (c, m) = fixture(vec![0x1012, 0x000f], 1, 0, vec![], "Z");
        assert!(
            reconstruct("sample.Example", &c, &m)
                .unwrap()
                .text
                .contains("return true;")
        );
        let (c, m) = fixture(vec![0x0012, 0x0011], 1, 0, vec![], "Ljava/lang/Object;");
        assert!(
            reconstruct("sample.Example", &c, &m)
                .unwrap()
                .text
                .contains("return null;")
        );
    }
    #[test]
    fn invalid_return_kind_and_branch_are_rejected() {
        let (c, m) = fixture(vec![0x1012, 0x0011], 1, 0, vec![], "I");
        assert!(reconstruct("sample.Example", &c, &m).is_err());
        let (c, m) = fixture(vec![0x0028], 1, 0, vec![], "V");
        assert!(reconstruct("sample.Example", &c, &m).is_err());
    }
    #[test]
    fn adjacent_allocation_constructor_preserves_effects_and_reference() {
        let (mut c, m) = fixture(
            vec![0x0022, 0, 0x1070, 0, 0, 0x0011],
            1,
            0,
            vec![],
            "Lsample/Example;",
        );
        c.symbols = Arc::new(DexSymbols {
            types: vec!["Lsample/Example;".into()],
            strings: vec!["<init>".into()],
            protos: vec![("V".into(), vec![])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        });
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert_eq!(
            body.text,
            "        sample.Example v0 = new sample.Example();\n        return v0;\n"
        );
        assert_eq!(body.links.len(), 2);
        assert_eq!(body.links[0].label, "sample.Example.<init>()V");
        let link = &body.links[0];
        assert_eq!(
            body.text
                .chars()
                .skip(link.start)
                .take(link.end - link.start)
                .collect::<String>(),
            "sample.Example"
        );
        let mut m = m;
        m.code.as_mut().unwrap().instructions.insert(2, 0x001a);
        assert!(reconstruct("sample.Example", &c, &m).is_err());
    }
    #[test]
    fn null_receiver_is_explicitly_typed_before_field_access() {
        let (mut c, m) = fixture(vec![0x0012, 0x0052, 0, 0x000f], 1, 0, vec![], "I");
        c.symbols = Arc::new(DexSymbols {
            types: vec!["Lsample/Example;".into(), "I".into()],
            strings: vec!["value".into()],
            fields: vec![(0, 1, 0)],
            ..Default::default()
        });
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert!(body.text.contains("((sample.Example) null).value"));
        let link = &body.links[0];
        assert_eq!(
            body.text
                .chars()
                .skip(link.start)
                .take(link.end - link.start)
                .collect::<String>(),
            "value"
        );
    }
    #[test]
    fn expression_references_have_explicit_unicode_offsets() {
        let mut out = Output::default();
        out.local("I", "é.a(\".a\")", &[(2, 1, "example.Owner.a()I".into())])
            .unwrap();
        assert_eq!(out.links.len(), 1);
        let link = &out.links[0];
        assert_eq!(
            out.text
                .chars()
                .skip(link.start)
                .take(link.end - link.start)
                .collect::<String>(),
            "a"
        );
    }
}

#[cfg(test)]
mod readability_conditions {
    use super::*;
    #[test]
    fn boolean_zero_tests_simplify_but_integer_tests_do_not() {
        let boolean = Value {
            text: "flag".into(),
            ty: "Z".into(),
            literal: None,
            wide_literal: None,
        };
        let regs = [Some(boolean)];
        assert_eq!(condition(0x39, 0, &regs).unwrap(), "flag");
        assert_eq!(condition(0x38, 0, &regs).unwrap(), "!(flag)");
        let integer = Value {
            text: "number".into(),
            ty: "I".into(),
            literal: None,
            wide_literal: None,
        };
        assert_eq!(condition(0x39, 0, &[Some(integer)]).unwrap(), "number != 0");
    }
    #[test]
    fn null_checks_need_no_object_cast_but_unrelated_reference_equality_does() {
        let value = |text: &str, ty: &str| {
            Some(Value {
                text: text.into(),
                ty: ty.into(),
                literal: None,
                wide_literal: None,
            })
        };
        let regs = [
            value("left", "Lsample/Left;"),
            value("right", "Lsample/Right;"),
        ];
        assert_eq!(condition(0x38, 0, &regs).unwrap(), "left == null");
        assert_eq!(
            condition(0x32, 0x10, &regs).unwrap(),
            "((java.lang.Object) left) == ((java.lang.Object) right)"
        );
    }
}
