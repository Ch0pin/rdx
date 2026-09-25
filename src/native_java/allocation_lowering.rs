//! Transactional decoder for bounded allocation regions and pure guarded copies.
use super::{
    Graph, Output, Value,
    allocation::{Allocation, Capture, Event, Expr, Symbol},
};
use crate::native_dex::{DexClass, DexMethod};
use anyhow::{Context, Result, ensure};
use std::collections::HashSet;

pub(super) struct Lowered {
    pub regs: Vec<Option<Value>>,
    pub out: Output,
    pub next_pc: usize,
}

#[derive(Clone)]
enum Atom {
    Input(Value),
    Expr { expression: Expr, ty: String },
    Uninitialized { site: usize, ty: String },
}
fn atom(regs: &[Option<Atom>], r: usize) -> Result<Atom> {
    let value = regs
        .get(r)
        .and_then(Clone::clone)
        .context("undefined allocation register")?;
    ensure!(
        atom_type(&value) != "<wide-tail>",
        "read from allocation wide tail"
    );
    if matches!(atom_type(&value), "J" | "D") {
        ensure!(
            matches!(regs.get(r + 1), Some(Some(Atom::Input(tail)))
            if tail.ty == "<wide-tail>" && tail.text == r.to_string()),
            "invalid allocation wide pair"
        );
    }
    Ok(value)
}
fn put(regs: &mut [Option<Atom>], r: usize, value: Atom) -> Result<()> {
    let width = if matches!(atom_type(&value), "J" | "D") {
        2
    } else {
        1
    };
    ensure!(
        r.checked_add(width).is_some_and(|end| end <= regs.len()),
        "allocation register out of bounds"
    );
    for slot in r..r + width {
        if let Some(old) = &regs[slot] {
            if atom_type(old) == "<wide-tail>" {
                let Atom::Input(tail) = old else {
                    anyhow::bail!("invalid allocation wide tail")
                };
                let head: usize = tail.text.parse()?;
                ensure!(head + 1 == slot, "invalid allocation wide owner");
                regs[head] = None;
            } else if matches!(atom_type(old), "J" | "D") {
                regs[slot + 1] = None;
            }
        }
        regs[slot] = None;
    }
    regs[r] = Some(value);
    if width == 2 {
        regs[r + 1] = Some(Atom::Input(Value {
            raw_bits32: false,
            text: r.to_string(),
            ty: "<wide-tail>".into(),
            literal: None,
            wide_literal: None,
        }));
    }
    Ok(())
}
fn atom_type(value: &Atom) -> &str {
    match value {
        Atom::Input(value) => &value.ty,
        Atom::Expr { ty, .. } | Atom::Uninitialized { ty, .. } => ty,
    }
}
fn expression(value: Atom, expected: &str, class: &DexClass) -> Result<Expr> {
    match value {
        Atom::Input(value) if expected == "C" && value.literal.is_some() => {
            Ok(Expr::Char(value.literal.unwrap()))
        }
        Atom::Input(value) if expected == "Z" && value.literal.is_some() => {
            let literal = value.literal.unwrap();
            ensure!(matches!(literal, 0 | 1), "nonboolean allocation literal");
            Ok(Expr::Boolean(literal != 0))
        }
        Atom::Input(value) if super::reference(expected) && value.literal == Some(0) => {
            Ok(Expr::Cast {
                ty: symbol(
                    super::java_type(expected)?,
                    super::class_label(expected).unwrap_or_default(),
                ),
                value: Box::new(Expr::Null),
            })
        }
        Atom::Input(value)
            if value.ty != expected
                && value.literal.is_none()
                && super::reference(&value.ty)
                && super::reference(expected) =>
        {
            Ok(Expr::Cast {
                ty: symbol(
                    super::java_type(expected)?,
                    super::class_label(expected).context("cast type label")?,
                ),
                value: Box::new(Expr::Local(value.text)),
            })
        }
        Atom::Input(value) => {
            let text = if expected == "I" && matches!(value.ty.as_str(), "B" | "S" | "C") {
                format!("((int) {})", value.text)
            } else {
                super::argument(&value, expected)?
            };
            if text == value.text {
                Ok(Expr::Local(text))
            } else {
                Ok(Expr::ConvertedLocal {
                    name: value.text,
                    text,
                })
            }
        }
        Atom::Expr { expression, ty } => {
            if ty != expected && super::reference(&ty) && super::reference(expected) {
                ensure!(
                    ty == "Ljava/lang/Object;"
                        || expected == "Ljava/lang/Object;"
                        || class
                            .symbols
                            .hierarchy
                            .get()
                            .is_some_and(|hierarchy| hierarchy.assignable(&ty, expected)
                                == crate::native_hierarchy::Relation::Proven),
                    "allocation reference widening requires proven hierarchy: {ty} to {expected}"
                );
                // Retain the declared DEX parameter type for overload resolution,
                // while the operand keeps the capture's event and identity.
                return Ok(Expr::Cast {
                    ty: symbol(
                        super::java_type(expected)?,
                        super::class_label(expected).context("widening type label")?,
                    ),
                    value: Box::new(expression),
                });
            }
            if expected == "I" && matches!(ty.as_str(), "B" | "S" | "C") {
                return Ok(Expr::Cast {
                    ty: symbol("int".into(), "int".into()),
                    value: Box::new(expression),
                });
            }
            ensure!(ty == expected, "allocation argument type mismatch");
            Ok(expression)
        }
        Atom::Uninitialized { .. } => anyhow::bail!("uninitialized allocation value escapes"),
    }
}
fn symbol(text: String, label: String) -> Symbol {
    Symbol { text, label }
}
fn method_symbol(
    class: &DexClass,
    index: usize,
) -> Result<(&str, &str, &[std::sync::Arc<str>], &str)> {
    let &(owner, proto, name) = class.symbols.methods.get(index).context("method index")?;
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
    Ok((owner, ret, args, name))
}
fn invoke_inputs(op: u8, a: usize, words: &[u16], pc: usize) -> Result<Vec<usize>> {
    if op >= 0x74 {
        Ok((words[pc + 2] as usize..words[pc + 2] as usize + a).collect())
    } else {
        let count = a >> 4;
        ensure!(count <= 5, "invoke register count");
        let packed = words[pc + 2];
        let all = [
            (packed & 15) as usize,
            ((packed >> 4) & 15) as usize,
            ((packed >> 8) & 15) as usize,
            ((packed >> 12) & 15) as usize,
            a & 15,
        ];
        Ok(all[..count].to_vec())
    }
}

// A pure guarded integer copy can be evaluated before Java allocation. This
// follows the readable-output policy, allowing allocation-related failure and
// class-initialization timing to move.
// No throwing instruction or uninitialized reference may enter the selection.
fn guarded_copy(
    graph: &Graph,
    words: &[u16],
    start: usize,
    stop: usize,
    allocation_register: usize,
    regs: &mut [Option<Value>],
    out: &mut Output,
) -> Result<Option<usize>> {
    if start >= stop || !matches!(words[start] as u8, 0x38 | 0x39) {
        return Ok(None);
    }
    let target = graph.targets[start].context("guarded copy target")?;
    let fall = start + 2;
    // if (...) goto copy; goto join; copy: move; join:
    // or if (...) goto join; move; join:
    let (copy, join, copy_on_taken) =
        if fall < stop && words[fall] as u8 == 0x28 && target == fall + 1 {
            (
                target,
                graph.targets[fall].context("guarded copy join")?,
                true,
            )
        } else {
            (fall, target, false)
        };
    ensure!(
        copy < stop && join < stop && join == copy + 1,
        "nonlocal guarded copy"
    );
    ensure!(
        words[copy] as u8 == 0x01,
        "guarded copy must be an integer move"
    );
    let dst = ((words[copy] >> 8) & 15) as usize;
    let src = (words[copy] >> 12) as usize;
    let cond = (words[start] >> 8) as usize;
    ensure!(
        ![dst, src, cond].contains(&allocation_register),
        "guard uses uninitialized allocation"
    );
    for (origin, target) in graph.targets.iter().enumerate() {
        if target.is_some_and(|target| start <= target && target <= join) {
            ensure!(
                origin == start || (copy_on_taken && origin == fall),
                "external entry into guarded copy"
            );
        }
    }
    let value = |r: usize| -> Result<&Value> {
        let value = regs
            .get(r)
            .and_then(Option::as_ref)
            .context("undefined guarded copy input")?;
        ensure!(value.ty == "I", "guarded copy requires integer inputs");
        Ok(value)
    };
    let predicate = if (words[start] as u8 == 0x39) == copy_on_taken {
        "!="
    } else {
        "=="
    };
    let expression = format!(
        "{} {} 0 ? {} : {}",
        value(cond)?.text,
        predicate,
        value(src)?.text,
        value(dst)?.text
    );
    let result = out.local("I", &expression, &[])?;
    regs[dst] = Some(result);
    Ok(Some(join))
}

// Render argument preparation with the ordinary structured-region renderer.
// Masking the allocation makes every use of its uninitialized identity fail;
// only the proven matching constructor may introduce the initialized object.
#[allow(clippy::too_many_arguments)]
fn try_region_staging(
    class: &DexClass,
    method: &DexMethod,
    graph: &Graph,
    words: &[u16],
    pc: usize,
    stop: usize,
    caller_regs: &[Option<Value>],
    caller_out: &Output,
) -> Result<Lowered> {
    let dst = (words[pc] >> 8) as usize;
    let ty = class
        .symbols
        .types
        .get(words[pc + 1] as usize)
        .context("allocation type")?;
    let start = pc + graph.widths[pc];
    let mut cursor = start;
    let mut constructor_pc = None;
    let mut count = 0;
    let mut allocations = 0;
    while cursor < stop && count < 256 {
        count += 1;
        graph.tick()?;
        let op = words[cursor] as u8;
        ensure!(
            graph.widths[cursor] != 0 && !graph.payloads[cursor],
            "allocation region crosses payload"
        );
        ensure!(
            !matches!(op, 0x0e..=0x11),
            "allocation region contains early return"
        );
        if op == 0x22 {
            allocations += 1;
            ensure!(allocations <= 16, "nested region staging budget");
        }
        if matches!(op, 0x70 | 0x76) {
            let (_, _, _, name) = method_symbol(class, words[cursor + 1] as usize)?;
            if name == "<init>" && graph.constructor_binding(class, method, cursor).is_some_and(|binding| matches!(&binding.origin,
                crate::native_constructors::ConstructorOrigin::Allocation { pc: origin, type_descriptor }
                if *origin == pc && type_descriptor.as_ref() == ty.as_ref())) {
                constructor_pc = Some(cursor);
                break;
            }
        }
        cursor += graph.widths[cursor];
    }
    let constructor_pc =
        constructor_pc.context("no bounded SSA constructor for allocation region")?;
    let valid_edge = |origin: usize, target: usize| {
        if start <= origin && origin < constructor_pc {
            start <= target && target <= constructor_pc
        } else {
            !(start <= target && target <= constructor_pc)
        }
    };
    for (origin, target) in graph.targets.iter().enumerate() {
        if let Some(target) = target {
            ensure!(
                valid_edge(origin, *target),
                "allocation region has escaping or external edge"
            );
        }
        if let Some(switch) = &graph.switches[origin] {
            ensure!(
                switch
                    .cases
                    .iter()
                    .all(|(_, target)| valid_edge(origin, *target)),
                "allocation region has escaping switch edge"
            );
        }
    }
    for region in &method.code.as_ref().context("method code")?.try_regions {
        let from = region.start as usize;
        let end = region.end as usize;
        ensure!(
            end <= pc
                || from > constructor_pc
                || (from <= pc && constructor_pc + graph.widths[constructor_pc] <= end),
            "allocation region crosses handler boundary"
        );
    }
    let inputs = invoke_inputs(
        words[constructor_pc] as u8,
        (words[constructor_pc] >> 8) as usize,
        words,
        constructor_pc,
    )?;
    // Keep exact uninitialized aliases while staging ordinary argument code.
    // Branches may precede alias transfers; after the first transfer, require
    // a linear suffix so all paths share the same receiver identity.
    let ir = crate::native_ir::DecodedMethod::decode(method.code.as_ref().unwrap())?;
    let mut aliases = HashSet::from([dst]);
    let mut alias_transfer = false;
    let mut staged_words = words.to_vec();
    for instruction in ir
        .instructions
        .iter()
        .filter(|i| start <= i.pc && i.pc < constructor_pc)
    {
        ensure!(
            !alias_transfer
                || (instruction.branch_target.is_none()
                    && !matches!(instruction.opcode, 0x2b | 0x2c)),
            "allocation alias crosses control flow"
        );
        let from_alias = matches!(instruction.opcode, 0x07..=0x09)
            && instruction
                .reads
                .first()
                .is_some_and(|r| aliases.contains(&(r.register as usize)));
        ensure!(
            from_alias
                || instruction
                    .reads
                    .iter()
                    .all(|r| !aliases.contains(&(r.register as usize))),
            "uninitialized allocation alias escapes"
        );
        for write in &instruction.writes {
            aliases.remove(&(write.register as usize));
            if write.kind == crate::native_ir::ValueKind::Wide64 {
                aliases.remove(&(write.register as usize + 1));
            }
        }
        if from_alias {
            let target = instruction
                .writes
                .first()
                .context("allocation alias destination")?
                .register as usize;
            aliases.insert(target);
            alias_transfer = true;
            staged_words[instruction.pc..instruction.pc + graph.widths[instruction.pc]].fill(0);
        }
    }
    ensure!(
        inputs.first().is_some_and(|r| aliases.contains(r)),
        "allocation region lost constructor receiver"
    );
    let (owner, ret, args, name) = method_symbol(class, words[constructor_pc + 1] as usize)?;
    ensure!(
        ret == "V"
            && name == "<init>"
            && (owner == ty.as_ref()
                || class
                    .symbols
                    .hierarchy
                    .get()
                    .is_some_and(|h| h.equivalent_constructor(ty, owner, args))),
        "allocation region constructor mismatch"
    );
    let mut regs = caller_regs.to_vec();
    // assign() also invalidates any overwritten wide-word pair.
    super::assign(
        &mut regs,
        dst,
        Value {
            raw_bits32: false,
            text: String::new(),
            ty: "I".into(),
            literal: None,
            wide_literal: None,
        },
    )?;
    regs[dst] = None;
    let mut out = Output {
        sequence: caller_out.sequence,
        indent: caller_out.indent,
        ..Output::default()
    };
    let staged_method = DexMethod {
        declaring_type: method.declaring_type.clone(),
        name: method.name.clone(),
        return_type: method.return_type.clone(),
        parameters: method.parameters.clone(),
        thrown_types: method.thrown_types.clone(),
        access_flags: method.access_flags,
        code: method.code.as_ref().map(|code| crate::native_dex::DexCode {
            registers: code.registers,
            ins: code.ins,
            outs: code.outs,
            tries: code.tries,
            try_regions: code.try_regions.clone(),
            instructions: staged_words,
            offset: code.offset,
        }),
    };
    let (mut regs, returned) = super::render(
        class,
        &staged_method,
        graph,
        start,
        constructor_pc,
        regs,
        &mut out,
        1,
        true,
        None,
        None,
    )?;
    ensure!(!returned, "allocation region returned before constructor");
    let mut actual = Vec::new();
    let mut input = 1;
    for arg in args {
        let r = *inputs.get(input).context("allocation region argument")?;
        ensure!(
            !aliases.contains(&r),
            "uninitialized allocation alias passed as argument"
        );
        if matches!(arg.as_ref(), "J" | "D") {
            ensure!(
                inputs.get(input + 1) == Some(&(r + 1)),
                "allocation region wide argument pair"
            );
        }
        let value = super::register(&regs, r)?;
        if value.ty != arg.as_ref() && super::reference(&value.ty) && super::reference(arg) {
            ensure!(
                value.ty == "Ljava/lang/Object;"
                    || arg.as_ref() == "Ljava/lang/Object;"
                    || class
                        .symbols
                        .hierarchy
                        .get()
                        .is_some_and(|h| h.assignable(&value.ty, arg)
                            == crate::native_hierarchy::Relation::Proven),
                "allocation region reference conversion lacks hierarchy proof"
            );
        }
        actual.push(
            if arg.as_ref() == "I" && matches!(value.ty.as_str(), "B" | "S" | "C") {
                format!("((int) {})", value.text)
            } else {
                super::argument(&value, arg)?
            },
        );
        input += if matches!(arg.as_ref(), "J" | "D") {
            2
        } else {
            1
        };
    }
    ensure!(input == inputs.len(), "extra allocation region arguments");
    let display = super::java_type(ty)?;
    let label = format!(
        "{}.{}({}){}",
        super::class_label(owner).context("constructor owner")?,
        name,
        args.join(""),
        ret
    );
    let value = out.local(
        ty,
        &format!("new {display}({})", actual.join(", ")),
        &[(4, display.chars().count(), label)],
    )?;
    for alias in aliases {
        super::assign(&mut regs, alias, value.clone())?;
    }
    ensure!(
        caller_out.text.len().saturating_add(out.text.len()) <= 4 * 1024 * 1024,
        "allocation region output budget"
    );
    Ok(Lowered {
        regs,
        out,
        next_pc: constructor_pc + graph.widths[constructor_pc],
    })
}

/// Decode one allocation through its matching constructor. Unsupported regions
/// decline without exposing any staged register or output mutation.
#[allow(clippy::too_many_arguments)]
pub(super) fn try_lower(
    class: &DexClass,
    method: &DexMethod,
    graph: &Graph,
    words: &[u16],
    pc: usize,
    stop: usize,
    caller_regs: &[Option<Value>],
    caller_out: &Output,
) -> Result<Option<Lowered>> {
    let attempt = || -> Result<Lowered> {
        // Large generated injection constructors can exceed 128 instructions.
        // Keep a fixed linear scan cap plus expression-node/output budgets.
        const MAX_INSTRUCTIONS: usize = 256;
        let dst = (words[pc] >> 8) as usize;
        let ty = class
            .symbols
            .types
            .get(words[pc + 1] as usize)
            .context("allocation type")?
            .to_string();
        ensure!(ty.starts_with('L'), "new-instance requires class");
        let display = super::java_type(&ty)?;
        let mut prelude = Output {
            sequence: caller_out.sequence,
            indent: caller_out.indent,
            ..Output::default()
        };
        let mut selected_regs = caller_regs.to_vec();
        let start = pc + graph.widths[pc];
        let selected_join = guarded_copy(
            graph,
            words,
            start,
            stop,
            dst,
            &mut selected_regs,
            &mut prelude,
        )?;
        let base_sequence = prelude.sequence;
        let mut regs: Vec<Option<Atom>> = selected_regs
            .iter()
            .cloned()
            .map(|value| value.map(Atom::Input))
            .collect();
        ensure!(dst < regs.len(), "allocation register out of bounds");
        put(
            &mut regs,
            dst,
            Atom::Uninitialized {
                site: pc,
                ty: ty.clone(),
            },
        )?;
        let mut nested = false;
        let mut captures = Vec::new();
        let mut discarded = Vec::new();
        let mut local_names: Vec<String> = selected_regs
            .iter()
            .flatten()
            .map(|value| value.text.clone())
            .collect();
        let incoming_targets: HashSet<usize> = graph.targets.iter().flatten().copied().collect();
        let allocation_label = super::class_label(&ty).context("allocation label")?;
        let mut events = vec![Event::Allocate {
            site: pc,
            ty: allocation_label.clone(),
        }];
        let mut cursor = selected_join.unwrap_or(start);
        let mut instructions = 0;
        loop {
            instructions += 1;
            ensure!(
                instructions <= MAX_INSTRUCTIONS && cursor < words.len() && cursor < stop,
                "allocation window exceeds budget"
            );
            ensure!(
                graph.widths[cursor] != 0 && graph.targets[cursor].is_none(),
                "allocation crosses control flow"
            );
            ensure!(
                !incoming_targets.contains(&cursor) || selected_join == Some(cursor),
                "branch enters allocation window"
            );
            let word = words[cursor];
            let op = word as u8;
            let a = (word >> 8) as usize;
            match op {
                0x21 | 0x7b..=0x8f => {
                    let operand = atom(&regs, a >> 4)?;
                    let (value, prefix, suffix, ty) = if op == 0x21 {
                        let ty = atom_type(&operand).to_string();
                        ensure!(
                            ty.starts_with('['),
                            "allocation array-length requires array type"
                        );
                        (
                            expression(operand, &ty, class)?,
                            String::new(),
                            ".length",
                            "I",
                        )
                    } else {
                        let unary =
                            super::numeric::Unary::decode(op).context("allocation unary opcode")?;
                        let prefix = unary
                            .expression("")
                            .strip_suffix("()")
                            .context("unary format")?
                            .to_string();
                        (
                            expression(operand, unary.input.descriptor(), class)?,
                            prefix,
                            "",
                            unary.result_descriptor,
                        )
                    };
                    let index = captures.len();
                    captures.push(Capture {
                        ty: super::java_type(ty)?,
                        name: format!("v{}", base_sequence + index),
                        expression: Expr::Unary {
                            site: cursor,
                            prefix,
                            suffix,
                            value: Box::new(value),
                        },
                    });
                    events.push(Event::Compute { site: cursor });
                    put(
                        &mut regs,
                        a & 15,
                        Atom::Expr {
                            expression: Expr::Capture(index),
                            ty: ty.into(),
                        },
                    )?;
                }
                0x90..=0xcf => {
                    let binary =
                        super::numeric::Binary::decode(op).context("allocation binary opcode")?;
                    let (to, left, right) = if op >= 0xb0 {
                        (a & 15, a & 15, a >> 4)
                    } else {
                        (
                            a,
                            (words[cursor + 1] & 255) as usize,
                            (words[cursor + 1] >> 8) as usize,
                        )
                    };
                    let left = expression(atom(&regs, left)?, binary.left.descriptor(), class)?;
                    let right = expression(atom(&regs, right)?, binary.right.descriptor(), class)?;
                    let index = captures.len();
                    captures.push(Capture {
                        ty: super::java_type(binary.result.descriptor())?,
                        name: format!("v{}", base_sequence + index),
                        expression: Expr::Binary {
                            site: cursor,
                            operator: binary.operator,
                            left: Box::new(left),
                            right: Box::new(right),
                        },
                    });
                    events.push(Event::Compute { site: cursor });
                    put(
                        &mut regs,
                        to,
                        Atom::Expr {
                            expression: Expr::Capture(index),
                            ty: binary.result.descriptor().into(),
                        },
                    )?;
                }
                0xd0..=0xe2 => {
                    let (to, from, literal, offset) = if op <= 0xd7 {
                        (a & 15, a >> 4, words[cursor + 1] as i16 as i32, op - 0xd0)
                    } else {
                        (
                            a,
                            (words[cursor + 1] & 255) as usize,
                            (words[cursor + 1] >> 8) as i8 as i32,
                            op - 0xd8,
                        )
                    };
                    let operators = ["+", "-", "*", "/", "%", "&", "|", "^", "<<", ">>", ">>>"];
                    let operator = operators[offset as usize];
                    let source = atom(&regs, from)?;
                    let boolean = atom_type(&source) == "Z"
                        && matches!(offset, 5..=7)
                        && matches!(literal, 0 | 1);
                    let result_ty = if boolean { "Z" } else { "I" };
                    let operand = expression(source, result_ty, class)?;
                    let text = literal.to_string();
                    local_names.push(text.clone());
                    let literal = if boolean {
                        Expr::Boolean(literal != 0)
                    } else {
                        Expr::Local(text)
                    };
                    let (left, right) = if offset == 1 {
                        (literal, operand)
                    } else {
                        (operand, literal)
                    };
                    let index = captures.len();
                    captures.push(Capture {
                        ty: super::java_type(result_ty)?,
                        name: format!("v{}", base_sequence + index),
                        expression: Expr::Binary {
                            site: cursor,
                            operator,
                            left: Box::new(left),
                            right: Box::new(right),
                        },
                    });
                    events.push(Event::Compute { site: cursor });
                    put(
                        &mut regs,
                        to,
                        Atom::Expr {
                            expression: Expr::Capture(index),
                            ty: result_ty.into(),
                        },
                    )?;
                }
                0x22 => {
                    let ty = class
                        .symbols
                        .types
                        .get(words[cursor + 1] as usize)
                        .context("nested allocation type")?
                        .to_string();
                    ensure!(ty.starts_with('L'), "new-instance requires class");
                    super::java_type(&ty)?;
                    events.push(Event::Allocate {
                        site: cursor,
                        ty: super::class_label(&ty).context("nested allocation label")?,
                    });
                    put(&mut regs, a, Atom::Uninitialized { site: cursor, ty })?;
                    nested = true;
                }
                0x04..=0x06 => {
                    let (to, from) = match op {
                        0x04 => (a & 15, a >> 4),
                        0x05 => (a, words[cursor + 1] as usize),
                        _ => (words[cursor + 1] as usize, words[cursor + 2] as usize),
                    };
                    let value = atom(&regs, from)?;
                    ensure!(
                        matches!(atom_type(&value), "J" | "D"),
                        "move-wide requires wide value"
                    );
                    put(&mut regs, to, value)?;
                }
                0x01 | 0x07 => {
                    let value = atom(&regs, a >> 4)?;
                    ensure!(
                        !matches!(atom_type(&value), "J" | "D"),
                        "narrow move of wide allocation value"
                    );
                    ensure!(
                        (op == 0x07 && matches!(&value, Atom::Input(v) if v.literal == Some(0)))
                            || ((op == 0x07)
                                == (atom_type(&value).starts_with('L')
                                    || atom_type(&value).starts_with('['))),
                        "move opcode type mismatch"
                    );
                    put(&mut regs, a & 15, value)?;
                }
                0x02 | 0x08 => {
                    let value = atom(&regs, words[cursor + 1] as usize)?;
                    ensure!(
                        !matches!(atom_type(&value), "J" | "D"),
                        "narrow move of wide allocation value"
                    );
                    ensure!(
                        (op == 0x08 && matches!(&value, Atom::Input(v) if v.literal == Some(0)))
                            || ((op == 0x08)
                                == (atom_type(&value).starts_with('L')
                                    || atom_type(&value).starts_with('['))),
                        "move opcode type mismatch"
                    );
                    put(&mut regs, a, value)?;
                }
                0x03 | 0x09 => {
                    let to = words[cursor + 1] as usize;
                    let value = atom(&regs, words[cursor + 2] as usize)?;
                    ensure!(
                        !matches!(atom_type(&value), "J" | "D"),
                        "narrow move of wide allocation value"
                    );
                    ensure!(
                        (op == 0x09 && matches!(&value, Atom::Input(v) if v.literal == Some(0)))
                            || ((op == 0x09)
                                == (atom_type(&value).starts_with('L')
                                    || atom_type(&value).starts_with('['))),
                        "move opcode type mismatch"
                    );
                    put(&mut regs, to, value)?;
                }
                0x12 => {
                    let to = a & 15;
                    let n = ((a >> 4) as i8) << 4 >> 4;
                    put(
                        &mut regs,
                        to,
                        Atom::Input(Value {
                            raw_bits32: false,
                            text: n.to_string(),
                            ty: "I".into(),
                            literal: Some(n as i32),
                            wide_literal: None,
                        }),
                    )?;
                    local_names.push(n.to_string());
                }
                0x13 => {
                    let n = words[cursor + 1] as i16 as i32;
                    put(
                        &mut regs,
                        a,
                        Atom::Input(Value {
                            raw_bits32: false,
                            text: n.to_string(),
                            ty: "I".into(),
                            literal: Some(n),
                            wide_literal: None,
                        }),
                    )?;
                    local_names.push(n.to_string());
                }
                0x14 | 0x15 => {
                    let n = if op == 0x15 {
                        ((words[cursor + 1] as u32) << 16) as i32
                    } else {
                        (words[cursor + 1] as u32 | (words[cursor + 2] as u32) << 16) as i32
                    };
                    put(
                        &mut regs,
                        a,
                        Atom::Input(Value {
                            raw_bits32: false,
                            text: n.to_string(),
                            ty: "I".into(),
                            literal: Some(n),
                            wide_literal: None,
                        }),
                    )?;
                    local_names.push(n.to_string());
                }
                0x16..=0x19 => {
                    let bits = match op {
                        0x16 => words[cursor + 1] as i16 as i64,
                        0x17 => {
                            (words[cursor + 1] as u32 | ((words[cursor + 2] as u32) << 16)) as i32
                                as i64
                        }
                        0x18 => (0..4).fold(0u64, |n, i| {
                            n | ((words[cursor + 1 + i] as u64) << (16 * i))
                        }) as i64,
                        _ => ((words[cursor + 1] as u64) << 48) as i64,
                    };
                    let text = format!("{bits}L");
                    local_names.push(text.clone());
                    put(
                        &mut regs,
                        a,
                        Atom::Input(Value {
                            raw_bits32: false,
                            text,
                            ty: "J".into(),
                            literal: None,
                            wide_literal: Some(bits as u64),
                        }),
                    )?;
                }
                0x1a | 0x1b => {
                    let string_index = if op == 0x1a {
                        words[cursor + 1] as usize
                    } else {
                        (words[cursor + 1] as u32 | (words[cursor + 2] as u32) << 16) as usize
                    };
                    let display = class
                        .symbols
                        .strings
                        .get(string_index)
                        .context("string index")?;
                    let string = Expr::StringConstant {
                        site: cursor,
                        literal: super::string_literal(display)?,
                    };
                    let index = captures.len();
                    captures.push(Capture {
                        ty: "java.lang.String".into(),
                        name: format!("v{}", base_sequence + index),
                        expression: string,
                    });
                    events.push(Event::StringResolution { site: cursor });
                    put(
                        &mut regs,
                        a,
                        Atom::Expr {
                            expression: Expr::Capture(index),
                            ty: "Ljava/lang/String;".into(),
                        },
                    )?;
                }
                0x1c => {
                    let descriptor = class
                        .symbols
                        .types
                        .get(words[cursor + 1] as usize)
                        .context("class literal type")?;
                    let display = super::java_type(descriptor)?;
                    let label = super::class_label(descriptor).context("class literal label")?;
                    let class_literal = Expr::ClassConstant {
                        site: cursor,
                        ty: symbol(display, label.clone()),
                    };
                    let index = captures.len();
                    captures.push(Capture {
                        ty: "java.lang.Class".into(),
                        name: format!("v{}", base_sequence + index),
                        expression: class_literal,
                    });
                    events.push(Event::ClassResolution {
                        site: cursor,
                        ty: label,
                    });
                    put(
                        &mut regs,
                        a,
                        Atom::Expr {
                            expression: Expr::Capture(index),
                            ty: "Ljava/lang/Class;".into(),
                        },
                    )?;
                }
                0x1f => {
                    let descriptor = class
                        .symbols
                        .types
                        .get(words[cursor + 1] as usize)
                        .context("check-cast type")?;
                    ensure!(
                        super::reference(descriptor),
                        "check-cast requires reference type"
                    );
                    let input = atom(&regs, a)?;
                    let source_type = atom_type(&input).to_string();
                    let null = matches!(&input, Atom::Input(value) if value.literal == Some(0));
                    let operand = match input {
                        Atom::Input(value) if value.literal == Some(0) => Expr::Null,
                        Atom::Input(value) => {
                            ensure!(
                                super::reference(&value.ty),
                                "check-cast requires reference value"
                            );
                            Expr::Local(value.text)
                        }
                        Atom::Expr { expression, ty } => {
                            ensure!(super::reference(&ty), "check-cast requires reference value");
                            expression
                        }
                        Atom::Uninitialized { .. } => {
                            anyhow::bail!("check-cast of uninitialized allocation")
                        }
                    };
                    // DEX permits runtime checks between unrelated reference
                    // declarations that Java rejects directly. Object erasure is
                    // nonthrowing; retain exactly one target check event.
                    let related = source_type == descriptor.as_ref()
                        || source_type == "Ljava/lang/Object;"
                        || descriptor.as_ref() == "Ljava/lang/Object;"
                        || class.symbols.hierarchy.get().is_some_and(|hierarchy| {
                            hierarchy.assignable(&source_type, descriptor)
                                == crate::native_hierarchy::Relation::Proven
                                || hierarchy.assignable(descriptor, &source_type)
                                    == crate::native_hierarchy::Relation::Proven
                        });
                    let operand = if related || null {
                        operand
                    } else {
                        Expr::Cast {
                            ty: symbol("java.lang.Object".into(), "java.lang.Object".into()),
                            value: Box::new(operand),
                        }
                    };
                    let label =
                        super::class_label(descriptor).unwrap_or_else(|| descriptor.to_string());
                    let index = captures.len();
                    captures.push(Capture {
                        ty: super::java_type(descriptor)?,
                        name: format!("v{}", base_sequence + index),
                        expression: Expr::CheckCast {
                            site: cursor,
                            ty: symbol(super::java_type(descriptor)?, label.clone()),
                            value: Box::new(operand),
                        },
                    });
                    events.push(Event::CheckCast {
                        site: cursor,
                        ty: label,
                    });
                    // A cast remains observable even if its result is overwritten.
                    // Staged emission must retain the check and its dependencies.
                    discarded.push(index);
                    put(
                        &mut regs,
                        a,
                        Atom::Expr {
                            expression: Expr::Capture(index),
                            ty: descriptor.to_string(),
                        },
                    )?;
                }
                0x52..=0x58 | 0x60..=0x66 => {
                    let &(owner_i, field_ty_i, name_i) = class
                        .symbols
                        .fields
                        .get(words[cursor + 1] as usize)
                        .context("field index")?;
                    let owner = class
                        .symbols
                        .types
                        .get(owner_i as usize)
                        .context("field owner")?;
                    let field_ty = class
                        .symbols
                        .types
                        .get(field_ty_i as usize)
                        .context("field type")?
                        .to_string();
                    let family = if op >= 0x60 { op - 0x60 } else { op - 0x52 };
                    ensure!(
                        match family {
                            0 => matches!(field_ty.as_str(), "I" | "F"),
                            1 => matches!(field_ty.as_str(), "J" | "D"),
                            2 => field_ty.starts_with('L') || field_ty.starts_with('['),
                            3 => field_ty == "Z",
                            4 => field_ty == "B",
                            5 => field_ty == "C",
                            6 => field_ty == "S",
                            _ => false,
                        },
                        "field opcode type mismatch"
                    );
                    let raw_name = class
                        .symbols
                        .strings
                        .get(name_i as usize)
                        .context("field name")?;
                    let label = format!(
                        "{}.{}:{}",
                        super::class_label(owner).context("field owner label")?,
                        raw_name,
                        field_ty
                    );
                    let (to, target) = if op >= 0x60 {
                        let owner = super::java_type(owner)?;
                        local_names.push(owner.clone());
                        (a, Expr::Local(owner))
                    } else {
                        (a & 15, expression(atom(&regs, a >> 4)?, owner, class)?)
                    };
                    let read = Expr::FieldRead {
                        site: cursor,
                        receiver: Box::new(target),
                        field: symbol(super::names::member(raw_name)?, label.clone()),
                    };
                    let index = captures.len();
                    captures.push(Capture {
                        ty: super::java_type(&field_ty)?,
                        name: format!("v{}", base_sequence + index),
                        expression: read,
                    });
                    events.push(Event::Read {
                        site: cursor,
                        field: label,
                    });
                    put(
                        &mut regs,
                        to,
                        Atom::Expr {
                            expression: Expr::Capture(index),
                            ty: field_ty,
                        },
                    )?;
                }
                0x59..=0x5f | 0x67..=0x6d => {
                    let &(owner_i, type_i, name_i) = class
                        .symbols
                        .fields
                        .get(words[cursor + 1] as usize)
                        .context("allocation store field")?;
                    let owner = class
                        .symbols
                        .types
                        .get(owner_i as usize)
                        .context("allocation store owner")?;
                    let ty = class
                        .symbols
                        .types
                        .get(type_i as usize)
                        .context("allocation store type")?;
                    let raw_name = class
                        .symbols
                        .strings
                        .get(name_i as usize)
                        .context("allocation store name")?;
                    let family = if op >= 0x67 { op - 0x67 } else { op - 0x59 };
                    ensure!(
                        match family {
                            0 => matches!(ty.as_ref(), "I" | "F"),
                            1 => matches!(ty.as_ref(), "J" | "D"),
                            2 => super::reference(ty),
                            3 => ty.as_ref() == "Z",
                            4 => ty.as_ref() == "B",
                            5 => ty.as_ref() == "C",
                            6 => ty.as_ref() == "S",
                            _ => false,
                        },
                        "allocation field store opcode type mismatch"
                    );
                    let (receiver, from) = if op >= 0x67 {
                        let display = super::java_type(owner)?;
                        local_names.push(display.clone());
                        (Expr::Local(display), a)
                    } else {
                        (expression(atom(&regs, a >> 4)?, owner, class)?, a & 15)
                    };
                    let value = expression(atom(&regs, from)?, ty, class)?;
                    let label = format!(
                        "{}.{}:{}",
                        super::class_label(owner).context("allocation store owner label")?,
                        raw_name,
                        ty
                    );
                    let index = captures.len();
                    captures.push(Capture {
                        ty: "void".into(),
                        name: format!("v{}", base_sequence + index),
                        expression: Expr::FieldStore {
                            site: cursor,
                            receiver: Box::new(receiver),
                            field: symbol(super::names::member(raw_name)?, label.clone()),
                            value: Box::new(value),
                        },
                    });
                    discarded.push(index);
                    events.push(Event::Write {
                        site: cursor,
                        field: label,
                    });
                }
                0x23 => {
                    let array = class
                        .symbols
                        .types
                        .get(words[cursor + 1] as usize)
                        .context("new array type")?;
                    ensure!(array.starts_with('['), "new array requires array type");
                    let length = expression(atom(&regs, a >> 4)?, "I", class)?;
                    let display = super::java_type(array)?;
                    let index = captures.len();
                    captures.push(Capture {
                        ty: display.clone(),
                        name: format!("v{}", base_sequence + index),
                        expression: Expr::NewArray {
                            site: cursor,
                            ty: symbol(
                                display.clone(),
                                super::class_label(array).unwrap_or_default(),
                            ),
                            length: Box::new(length),
                        },
                    });
                    events.push(Event::Allocate {
                        site: cursor,
                        ty: display,
                    });
                    events.push(Event::Compute { site: cursor });
                    put(
                        &mut regs,
                        a & 15,
                        Atom::Expr {
                            expression: Expr::Capture(index),
                            ty: array.to_string(),
                        },
                    )?;
                }
                0x44..=0x4a => {
                    let array = atom(&regs, (words[cursor + 1] & 255) as usize)?;
                    let ty = atom_type(&array).to_string();
                    let component = ty
                        .strip_prefix('[')
                        .context("allocation array read requires array type")?;
                    ensure!(
                        match op - 0x44 {
                            0 => matches!(component, "I" | "F"),
                            1 => matches!(component, "J" | "D"),
                            2 => super::reference(component),
                            3 => component == "Z",
                            4 => component == "B",
                            5 => component == "C",
                            6 => component == "S",
                            _ => false,
                        },
                        "allocation array opcode type mismatch"
                    );
                    let array = expression(array, &ty, class)?;
                    let index_value =
                        expression(atom(&regs, (words[cursor + 1] >> 8) as usize)?, "I", class)?;
                    let index = captures.len();
                    captures.push(Capture {
                        ty: super::java_type(component)?,
                        name: format!("v{}", base_sequence + index),
                        expression: Expr::ArrayRead {
                            site: cursor,
                            array: Box::new(array),
                            index: Box::new(index_value),
                        },
                    });
                    events.push(Event::Read {
                        site: cursor,
                        field: "<array>".into(),
                    });
                    put(
                        &mut regs,
                        a,
                        Atom::Expr {
                            expression: Expr::Capture(index),
                            ty: component.to_string(),
                        },
                    )?;
                }
                0x4b..=0x51 => {
                    let array = atom(&regs, (words[cursor + 1] & 255) as usize)?;
                    let ty = atom_type(&array).to_string();
                    let component = ty
                        .strip_prefix('[')
                        .context("allocation array store requires array type")?;
                    ensure!(
                        match op - 0x4b {
                            0 => matches!(component, "I" | "F"),
                            1 => matches!(component, "J" | "D"),
                            2 => super::reference(component),
                            3 => component == "Z",
                            4 => component == "B",
                            5 => component == "C",
                            6 => component == "S",
                            _ => false,
                        },
                        "allocation array store opcode type mismatch"
                    );
                    let mut array = expression(array, &ty, class)?;
                    // Widen only the array receiver for aput-object: Java's array
                    // store check must throw ArrayStoreException, not a new value
                    // check-cast's ClassCastException.
                    let expected = if super::reference(component) {
                        array = Expr::Cast {
                            ty: symbol("java.lang.Object[]".into(), "java.lang.Object".into()),
                            value: Box::new(array),
                        };
                        "Ljava/lang/Object;"
                    } else {
                        component
                    };
                    let value = expression(atom(&regs, a)?, expected, class)?;
                    let offset =
                        expression(atom(&regs, (words[cursor + 1] >> 8) as usize)?, "I", class)?;
                    let index = captures.len();
                    captures.push(Capture {
                        ty: "void".into(),
                        name: format!("v{}", base_sequence + index),
                        expression: Expr::ArrayStore {
                            site: cursor,
                            array: Box::new(array),
                            index: Box::new(offset),
                            value: Box::new(value),
                        },
                    });
                    discarded.push(index);
                    events.push(Event::Write {
                        site: cursor,
                        field: "<array>".into(),
                    });
                }
                0x24 | 0x25 => {
                    let array = class
                        .symbols
                        .types
                        .get(words[cursor + 1] as usize)
                        .context("array type")?;
                    let component = array
                        .strip_prefix('[')
                        .context("filled array requires array type")?;
                    ensure!(
                        !matches!(component, "J" | "D"),
                        "wide filled array unsupported"
                    );
                    let inputs =
                        invoke_inputs(if op == 0x25 { 0x74 } else { op }, a, words, cursor)?;
                    let elements = inputs
                        .into_iter()
                        .map(|r| expression(atom(&regs, r)?, component, class))
                        .collect::<Result<Vec<_>>>()?;
                    let next = cursor + graph.widths[cursor];
                    ensure!(
                        next < stop
                            && !incoming_targets.contains(&next)
                            && words[next] as u8 == 0x0c,
                        "filled array must have an adjacent object result"
                    );
                    let label = super::class_label(array).unwrap_or_default();
                    let index = captures.len();
                    captures.push(Capture {
                        ty: super::java_type(array)?,
                        name: format!("v{}", base_sequence + index),
                        expression: Expr::FilledArray {
                            site: cursor,
                            ty: symbol(super::java_type(array)?, label.clone()),
                            elements,
                        },
                    });
                    events.push(Event::Allocate {
                        site: cursor,
                        ty: super::java_type(array)?,
                    });
                    put(
                        &mut regs,
                        (words[next] >> 8) as usize,
                        Atom::Expr {
                            expression: Expr::Capture(index),
                            ty: array.to_string(),
                        },
                    )?;
                    cursor = next;
                }
                0x6e | 0x70..=0x72 | 0x74 | 0x76..=0x78 => {
                    let (owner, ret, args, raw_name) =
                        method_symbol(class, words[cursor + 1] as usize)?;
                    ensure!(raw_name != "<clinit>", "static initializer invocation");
                    let kind = if op >= 0x74 { op - 6 } else { op };
                    let static_call = kind == 0x71;
                    let inputs = invoke_inputs(op, a, words, cursor)?;
                    let mut input_cursor = usize::from(!static_call);
                    let target = if raw_name == "<init>" {
                        Expr::Local(String::new())
                    } else if static_call {
                        let owner = super::java_type(owner)?;
                        local_names.push(owner.clone());
                        Expr::Local(owner)
                    } else {
                        expression(
                            atom(&regs, *inputs.first().context("missing invoke receiver")?)?,
                            owner,
                            class,
                        )?
                    };
                    let mut actual = Vec::new();
                    for arg_ty in args {
                        let r = *inputs
                            .get(input_cursor)
                            .context("missing invoke argument")?;
                        if matches!(arg_ty.as_ref(), "J" | "D") {
                            ensure!(
                                inputs.get(input_cursor + 1) == Some(&(r + 1)),
                                "nonadjacent allocation wide argument"
                            );
                        }
                        actual.push(expression(atom(&regs, r)?, arg_ty, class)?);
                        input_cursor += if matches!(arg_ty.as_ref(), "J" | "D") {
                            2
                        } else {
                            1
                        };
                    }
                    ensure!(input_cursor == inputs.len(), "extra invoke arguments");
                    let label = format!(
                        "{}.{}({}){}",
                        super::class_label(owner).context("method owner label")?,
                        raw_name,
                        args.join(""),
                        ret
                    );
                    if raw_name == "<init>" {
                        let receiver_register =
                            *inputs.first().context("missing constructor receiver")?;
                        let Atom::Uninitialized {
                            site,
                            ty: allocation_ty,
                        } = atom(&regs, receiver_register)?
                        else {
                            anyhow::bail!("constructor does not consume allocation")
                        };
                        let retargeted = owner != allocation_ty
                            && class.symbols.hierarchy.get().is_some_and(|hierarchy| {
                                hierarchy.equivalent_constructor(&allocation_ty, owner, args)
                            });
                        ensure!(
                            kind == 0x70 && (owner == allocation_ty || retargeted) && ret == "V",
                            "allocation constructor mismatch"
                        );
                        if nested {
                            let binding = graph
                                .constructor_binding(class, method, cursor)
                                .context("constructor identity unavailable")?;
                            ensure!(
                                (!binding.owner_retarget_required || retargeted)
                                    && matches!(&binding.origin,
                                crate::native_constructors::ConstructorOrigin::Allocation { pc: origin, type_descriptor }
                                if *origin == site && type_descriptor.as_ref() == allocation_ty),
                                "constructor SSA identity mismatch"
                            );
                        }
                        events.push(Event::Construct {
                            site: cursor,
                            ty: super::class_label(&allocation_ty)
                                .context("constructor allocation label")?,
                        });
                        if site != pc {
                            let index = captures.len();
                            captures.push(Capture {
                                ty: super::java_type(&allocation_ty)?,
                                name: format!("v{}", base_sequence + index),
                                expression: Expr::SharedNew {
                                    allocation: Box::new(Allocation {
                                        site,
                                        constructor_site: cursor,
                                        ty: symbol(
                                            super::java_type(&allocation_ty)?,
                                            super::class_label(&allocation_ty)
                                                .context("child label")?,
                                        ),
                                        captures: Vec::new(),
                                        arguments: actual,
                                    }),
                                    constructor_label: label,
                                },
                            });
                            for staged in &mut regs {
                                if matches!(staged, Some(Atom::Uninitialized { site: alias, .. }) if *alias == site)
                                {
                                    *staged = Some(Atom::Expr {
                                        expression: Expr::Capture(index),
                                        ty: allocation_ty.clone(),
                                    });
                                }
                            }
                            cursor += graph.widths[cursor];
                            continue;
                        }
                        ensure!(allocation_ty == ty, "outer allocation type mismatch");
                        ensure!(
                            regs.iter().all(|slot| !matches!(slot,
                            Some(Atom::Uninitialized { site, .. }) if *site != pc)),
                            "nested allocation remains uninitialized"
                        );
                        // Every decoded read/resolution/call remains observable,
                        // including captures overwritten before the constructor or
                        // used only after it. Staging retains these as statements.
                        discarded.extend(0..captures.len());
                        let allocation = Allocation {
                            site: pc,
                            constructor_site: cursor,
                            ty: symbol(display, allocation_label),
                            captures,
                            arguments: actual,
                        };
                        let local_refs: Vec<&str> =
                            local_names.iter().map(String::as_str).collect();
                        let mut rendered = match allocation.render_checked(&events, &local_refs) {
                            Ok(rendered) => rendered,
                            Err(_) => allocation.render_staged_with_discarded(
                                &events,
                                &local_refs,
                                &discarded,
                            )?,
                        };
                        // A constructor expression links its type token to the
                        // raw overloaded constructor identity.
                        rendered
                            .links
                            .first_mut()
                            .context("missing allocation type link")?
                            .label = label;
                        let mut out = prelude;
                        for (declaration, links) in rendered
                            .declarations
                            .iter()
                            .zip(&rendered.declaration_links)
                        {
                            let refs: Vec<_> = links
                                .iter()
                                .map(|link| (link.start, link.end - link.start, link.label.clone()))
                                .collect();
                            out.line(declaration, &refs);
                        }
                        out.sequence += rendered.declarations.len();
                        let refs: Vec<_> = rendered
                            .links
                            .iter()
                            .map(|link| (link.start, link.end - link.start, link.label.clone()))
                            .collect();
                        let value = out.local(&ty, &rendered.expression, &refs)?;
                        ensure!(
                            caller_out.text.len().saturating_add(out.text.len()) <= 4 * 1024 * 1024,
                            "reconstructed method exceeds output budget"
                        );
                        let mut final_regs = caller_regs.to_vec();
                        for (i, staged) in regs.iter().enumerate() {
                            final_regs[i] = match staged {
                                Some(Atom::Input(value)) => Some(value.clone()),
                                Some(Atom::Uninitialized { site, .. }) if *site == pc => {
                                    Some(value.clone())
                                }
                                Some(Atom::Expr {
                                    expression: Expr::Capture(index),
                                    ty,
                                }) => Some(Value {
                                    raw_bits32: false,
                                    text: format!("v{}", base_sequence + index),
                                    ty: ty.clone(),
                                    literal: None,
                                    wide_literal: None,
                                }),
                                Some(Atom::Expr { .. }) => final_regs[i].clone(),
                                None => None,
                                _ => final_regs[i].clone(),
                            };
                        }
                        return Ok(Lowered {
                            regs: final_regs,
                            out,
                            next_pc: cursor + graph.widths[cursor],
                        });
                    }
                    let next = cursor + graph.widths[cursor];
                    let result_opcode = if matches!(ret, "J" | "D") {
                        0x0b
                    } else if ret.starts_with('L') || ret.starts_with('[') {
                        0x0c
                    } else {
                        0x0a
                    };
                    let has_result = ret != "V"
                        && next < words.len()
                        && next < stop
                        && !incoming_targets.contains(&next)
                        && words[next] as u8 == result_opcode
                        && graph.widths[next] == 1;
                    // Pinned JADX SimplifyVisitor also recognizes unchained builder
                    // uses. Keep the actual append calls here (no concat rewrite).
                    // Only this exact final-platform-class overload guarantees
                    // that an ignored result is the original receiver identity.
                    let fluent_receiver = if !has_result
                        && kind == 0x6e
                        && owner == "Ljava/lang/StringBuilder;"
                        && raw_name == "append"
                        && ret == owner
                        && args.len() == 1
                        && matches!(args[0].as_ref(), "Ljava/lang/String;" | "C" | "I" | "Z")
                    {
                        match atom(&regs, inputs[0])? {
                            Atom::Expr {
                                expression: Expr::Capture(index),
                                ty,
                            } if ty == owner => Some(index),
                            _ => None,
                        }
                    } else {
                        None
                    };

                    let call = Expr::Call {
                        site: cursor,
                        target: Box::new(target),
                        method: symbol(super::names::member(raw_name)?, label.clone()),
                        args: actual,
                    };
                    let index = captures.len();
                    captures.push(Capture {
                        ty: if ret == "V" {
                            "void".into()
                        } else {
                            super::java_type(ret)?
                        },
                        name: format!("v{}", base_sequence + index),
                        expression: call,
                    });
                    events.push(Event::Call {
                        site: cursor,
                        method: label,
                    });
                    let result = Atom::Expr {
                        expression: Expr::Capture(index),
                        ty: ret.to_string(),
                    };
                    if let Some(receiver) = fluent_receiver {
                        // Every alias must depend on the latest mutation, so a later
                        // use cannot omit or duplicate an ignored append effect.
                        for staged in &mut regs {
                            if matches!(staged, Some(Atom::Expr { expression: Expr::Capture(alias), .. }) if *alias == receiver)
                            {
                                *staged = Some(result.clone());
                            }
                        }
                    } else if has_result {
                        put(&mut regs, (words[next] >> 8) as usize, result)?;
                        cursor = next;
                    } else {
                        // An ignored invoke result still represents an observable call.
                        discarded.push(index);
                    }
                }
                _ => anyhow::bail!("unsupported instruction in allocation window: {op:02x}"),
            }
            cursor += graph.widths[cursor];
        }
    };
    let result = attempt().or_else(|_| {
        try_region_staging(
            class,
            method,
            graph,
            words,
            pc,
            stop,
            caller_regs,
            caller_out,
        )
    });
    Ok(result.ok())
}
