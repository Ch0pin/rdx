//! Transactional decoder for a bounded, straight-line allocation region.
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
    regs.get(r)
        .and_then(Clone::clone)
        .context("undefined allocation register")
}
fn put(regs: &mut [Option<Atom>], r: usize, value: Atom) -> Result<()> {
    *regs
        .get_mut(r)
        .context("allocation register out of bounds")? = Some(value);
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
                    super::class_label(expected).context("null type label")?,
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
        Atom::Input(value) => Ok(Expr::Local(super::argument(&value, expected)?)),
        Atom::Expr { expression, ty } => {
            if ty != expected && super::reference(&ty) && super::reference(expected) {
                ensure!(
                    class
                        .symbols
                        .hierarchy
                        .get()
                        .is_some_and(|hierarchy| hierarchy.assignable(&ty, expected)
                            == crate::native_hierarchy::Relation::Proven),
                    "allocation reference widening requires proven hierarchy"
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
            ensure!(
                ty == expected || (expected == "I" && matches!(ty.as_str(), "B" | "S" | "C")),
                "allocation argument type mismatch"
            );
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
        const MAX_INSTRUCTIONS: usize = 64;
        let dst = (words[pc] >> 8) as usize;
        let ty = class
            .symbols
            .types
            .get(words[pc + 1] as usize)
            .context("allocation type")?
            .to_string();
        ensure!(ty.starts_with('L'), "new-instance requires class");
        let display = super::java_type(&ty)?;
        ensure!(
            caller_regs
                .iter()
                .flatten()
                .all(|value| !matches!(value.ty.as_str(), "J" | "D" | "<wide-tail>")),
            "allocation decoder declines live wide registers"
        );
        let mut regs: Vec<Option<Atom>> = caller_regs
            .iter()
            .cloned()
            .map(|value| value.map(Atom::Input))
            .collect();
        ensure!(dst < regs.len(), "allocation register out of bounds");
        regs[dst] = Some(Atom::Uninitialized {
            site: pc,
            ty: ty.clone(),
        });
        let mut nested = false;
        let mut captures = Vec::new();
        let mut local_names: Vec<String> = caller_regs
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
        let mut cursor = pc + graph.widths[pc];
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
                !incoming_targets.contains(&cursor),
                "branch enters allocation window"
            );
            let word = words[cursor];
            let op = word as u8;
            let a = (word >> 8) as usize;
            match op {
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
                0x01 | 0x07 => {
                    let value = atom(&regs, a >> 4)?;
                    ensure!(
                        (op == 0x07)
                            == (atom_type(&value).starts_with('L')
                                || atom_type(&value).starts_with('[')),
                        "move opcode type mismatch"
                    );
                    put(&mut regs, a & 15, value)?;
                }
                0x02 | 0x08 => {
                    let value = atom(&regs, words[cursor + 1] as usize)?;
                    ensure!(
                        (op == 0x08)
                            == (atom_type(&value).starts_with('L')
                                || atom_type(&value).starts_with('[')),
                        "move opcode type mismatch"
                    );
                    put(&mut regs, a, value)?;
                }
                0x03 | 0x09 => {
                    let to = words[cursor + 1] as usize;
                    let value = atom(&regs, words[cursor + 2] as usize)?;
                    ensure!(
                        (op == 0x09)
                            == (atom_type(&value).starts_with('L')
                                || atom_type(&value).starts_with('[')),
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
                            text: n.to_string(),
                            ty: "I".into(),
                            literal: Some(n),
                            wide_literal: None,
                        }),
                    )?;
                    local_names.push(n.to_string());
                }
                0x14 => {
                    let n = (words[cursor + 1] as u32 | (words[cursor + 2] as u32) << 16) as i32;
                    put(
                        &mut regs,
                        a,
                        Atom::Input(Value {
                            text: n.to_string(),
                            ty: "I".into(),
                            literal: Some(n),
                            wide_literal: None,
                        }),
                    )?;
                    local_names.push(n.to_string());
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
                        name: format!("v{}", caller_out.sequence + index),
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
                        name: format!("v{}", caller_out.sequence + index),
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
                    let label = super::class_label(descriptor).context("check-cast label")?;
                    let index = captures.len();
                    captures.push(Capture {
                        ty: super::java_type(descriptor)?,
                        name: format!("v{}", caller_out.sequence + index),
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
                    ensure!(
                        !matches!(field_ty.as_str(), "J" | "D"),
                        "wide allocation field capture is unsupported"
                    );
                    let family = if op >= 0x60 { op - 0x60 } else { op - 0x52 };
                    ensure!(
                        match family {
                            0 => matches!(field_ty.as_str(), "I" | "F"),
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
                        name: format!("v{}", caller_out.sequence + index),
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
                        ensure!(
                            !matches!(arg_ty.as_ref(), "J" | "D"),
                            "wide allocation invocation argument is unsupported"
                        );
                        let r = *inputs
                            .get(input_cursor)
                            .context("missing invoke argument")?;
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
                            && args.is_empty()
                            && class.symbols.hierarchy.get().is_some_and(|hierarchy| {
                                hierarchy.equivalent_noarg_constructor(&allocation_ty, owner)
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
                                name: format!("v{}", caller_out.sequence + index),
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
                            Err(error) if nested => return Err(error),
                            Err(_) => allocation.render_staged_checked(&events, &local_refs)?,
                        };
                        // A constructor expression links its type token to the
                        // raw overloaded constructor identity.
                        rendered
                            .links
                            .first_mut()
                            .context("missing allocation type link")?
                            .label = label;
                        let mut out = Output {
                            sequence: caller_out.sequence,
                            indent: caller_out.indent,
                            ..Output::default()
                        };
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
                                    text: format!("v{}", caller_out.sequence + index),
                                    ty: ty.clone(),
                                    literal: None,
                                    wide_literal: None,
                                }),
                                Some(Atom::Expr { .. }) => final_regs[i].clone(),
                                _ => final_regs[i].clone(),
                            };
                        }
                        return Ok(Lowered {
                            regs: final_regs,
                            out,
                            next_pc: cursor + graph.widths[cursor],
                        });
                    }
                    ensure!(
                        ret != "V",
                        "void effect cannot be delayed into constructor arguments"
                    );
                    ensure!(
                        !matches!(ret, "J" | "D"),
                        "wide allocation call capture is unsupported"
                    );
                    let next = cursor + graph.widths[cursor];
                    let result_opcode = if ret.starts_with('L') || ret.starts_with('[') {
                        0x0c
                    } else {
                        0x0a
                    };
                    let has_result = next < words.len()
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
                        && matches!(args[0].as_ref(), "Ljava/lang/String;" | "C")
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
                    ensure!(
                        has_result || fluent_receiver.is_some(),
                        "nonvoid invoke requires immediate move-result"
                    );
                    let call = Expr::Call {
                        site: cursor,
                        target: Box::new(target),
                        method: symbol(super::names::member(raw_name)?, label.clone()),
                        args: actual,
                    };
                    let index = captures.len();
                    captures.push(Capture {
                        ty: super::java_type(ret)?,
                        name: format!("v{}", caller_out.sequence + index),
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
                    } else {
                        put(&mut regs, (words[next] >> 8) as usize, result)?;
                        cursor = next;
                    }
                }
                _ => anyhow::bail!("unsupported instruction in allocation window"),
            }
            cursor += graph.widths[cursor];
        }
    };
    Ok(attempt().ok())
}
