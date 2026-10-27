struct InstructionState {
    pc: usize,
    regs: Vec<Option<Value>>,
    initialized: bool,
    pending: Option<Value>,
    allocation: Option<(usize, String)>,
    class_captures: Vec<(String, String, Option<String>, Option<String>)>,
    early_field_writes: bool,
    previous_exception_values: Vec<Option<Value>>,
    returned: bool,
}

// Straight-line instruction emission is kept off recursive region frames.
// This bounds stack usage independently of the number of opcode cases.
#[allow(clippy::too_many_arguments)]
#[inline(never)]
fn emit_instruction(
    class: &DexClass,
    method: &DexMethod,
    graph: &Graph,
    mut pc: usize,
    stop: usize,
    mut regs: Vec<Option<Value>>,
    out: &mut Output,
    mut initialized: bool,
    mut pending: Option<Value>,
    mut allocation: Option<(usize, String)>,
    mut class_captures: Vec<(String, String, Option<String>, Option<String>)>,
    mut early_field_writes: bool,
    mut previous_exception_values: Vec<Option<Value>>,
    exception_slots: Option<&[Option<Value>]>,
) -> Result<InstructionState> {
    let words = &method
        .code
        .as_ref()
        .context("method has no code")?
        .instructions;
    let w = words[pc];
    let instruction = graph.instruction(pc)?;
    let op = instruction.map_or(w as u8, |insn| insn.opcode);
    let a = (w >> 8) as usize;
    let width = instruction.map_or(graph.widths[pc], |insn| insn.width);
    let constructor = method.name.as_ref() == "<init>";
    let mut returned = false;
    macro_rules! state {
        ($next:expr) => {
            InstructionState {
                pc: $next,
                regs,
                initialized,
                pending,
                allocation,
                class_captures,
                early_field_writes,
                previous_exception_values,
                returned,
            }
        };
    }
    if !matches!(op, 0x0a..=0x0c) {
        pending = None;
    }
    match op {
        0x00 => ensure!(w == 0, "payload in straight-line body"),
        0x01..=0x09 => {
            let (dst, src) = if let Some(instruction) = instruction {
                decoded_move(instruction, regs.len())?
            } else {
                match (op - 1) % 3 {
                    0 => (a & 15, a >> 4),
                    1 => (a, words[pc + 1] as usize),
                    _ => (words[pc + 1] as usize, words[pc + 2] as usize),
                }
            };
            let kind = (op - 1) / 3;
            if kind == 0
                && regs.get(src).is_some_and(Option::is_none)
                && let Some(facts) = graph.loop_type_analysis(class, method)
                && let Some(bits) = literal_read_proof::constant32_at_read(
                    &facts.ir,
                    &facts.ssa,
                    &facts.types,
                    pc,
                    src,
                )
            {
                assign(
                    &mut regs,
                    src,
                    Value {
                        text: bits.to_string(),
                        ty: "I".into(),
                        literal: Some(bits),
                        wide_literal: None,
                        raw_bits32: true,
                    },
                )?;
            }
            let mut value = if !initialized
                && constructor
                && kind == 2
                && regs.get(src).and_then(Option::as_ref).is_some_and(|v| {
                    v.text == "<masked-constructor-this>" && v.ty == class.descriptor.as_ref()
                }) {
                regs[src].as_ref().unwrap().clone()
            } else {
                register(&regs, src)?
            };
            if kind == 2
                && graph.loop_entry_copy.is_none()
                && graph.parent_prefix.is_none()
                && matches!(value.ty.as_str(), "I" | "Z" | "B" | "S" | "C")
                && value.literal.is_none()
                && graph
                    .loop_type_analysis(class, method)
                    .is_some_and(|analysis| {
                        ssa_null_read::at_read(
                            &analysis.ir,
                            &analysis.ssa,
                            &analysis.types,
                            pc,
                            src,
                        )
                    })
            {
                value = Value {
                    text: "null".into(),
                    ty: "Ljava/lang/Object;".into(),
                    literal: Some(0),
                    wide_literal: None,
                    raw_bits32: false,
                };
            }
            ensure!(
                match kind {
                    0 => !reference(&value.ty) && !matches!(value.ty.as_str(), "J" | "D"),
                    1 => matches!(value.ty.as_str(), "J" | "D"),
                    _ => reference(&value.ty) || value.literal == Some(0),
                },
                "move type mismatch at {pc:04x}: register v{src} has type {}",
                value.ty
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
            let dst = if let Some(instruction) = instruction {
                decoded_result_destination(instruction, regs.len())?
            } else {
                a
            };
            if let Some(front) = &graph.front_end {
                let index = front.ir.instructions.partition_point(|insn| insn.pc < pc);
                let producer = index
                    .checked_sub(1)
                    .and_then(|index| front.ir.instructions.get(index))
                    .context("missing shared move-result producer")?;
                let call_index = front
                    .bound
                    .calls
                    .binary_search_by_key(&producer.pc, |call| call.pc)
                    .map_err(|_| anyhow::anyhow!("missing shared move-result binding"))?;
                let result = front.bound.calls[call_index]
                    .result
                    .as_ref()
                    .context("missing shared bound result")?;
                ensure!(
                    result.move_pc == pc && usize::from(result.register.register) == dst,
                    "shared move-result destination differs from bound result"
                );
            }
            let value = pending.take().context("move-result without invoke")?;
            ensure!(
                match op {
                    0x0a => !reference(&value.ty) && !matches!(value.ty.as_str(), "J" | "D"),
                    0x0b => matches!(value.ty.as_str(), "J" | "D"),
                    _ => reference(&value.ty),
                },
                "result opcode type mismatch"
            );
            assign(&mut regs, dst, value)?;
        }
        0x0d => bail!("move-exception outside handled entry"),
        0x27 => {
            ensure!(allocation.is_none(), "throw interrupts allocation");
            let terminal_throw_arm = !initialized
                && constructor
                && graph
                    .constructor_throw_boundary
                    .get()
                    .is_some_and(|(start, end)| start <= pc && pc < end)
                && method
                    .code
                    .as_ref()
                    .unwrap()
                    .try_regions
                    .iter()
                    .all(|region| {
                        !(region.start as usize..region.end as usize).contains(&pc)
                            && region
                                .catches
                                .iter()
                                .all(|(_, handler)| *handler as usize != pc)
                    });
            let throwing_prologue = !terminal_throw_arm
                && !initialized
                && constructor
                && (class.superclass.as_deref() == Some("Ljava/lang/Object;")
                    || class
                        .symbols
                        .hierarchy
                        .get()
                        .is_some_and(|h| h.has_accessible_noarg_super(&class.descriptor)))
                && method.code.as_ref().unwrap().try_regions.is_empty()
                && graph.targets.iter().all(Option::is_none)
                && graph.switches.iter().all(Option::is_none);
            ensure!(
                initialized || throwing_prologue || terminal_throw_arm,
                "throw before initialization: implicit super() would change construction/finalization effects"
            );
            let src = if let Some(instruction) = instruction {
                decoded_throw_source(instruction, regs.len())?
            } else {
                a
            };
            let value = register(&regs, src)?;
            // Java precise rethrow preserves the exception from this catch.
            // Only unchanged catch identities qualify, not arbitrary Throwable values.
            let refined = graph
                .caught_values
                .borrow()
                .contains(&value.text)
                .then(|| immediate_caught_throw_type(class, method, graph, pc, src))
                .flatten()
                .map(|ty| Value {
                    ty: ty.to_string(),
                    ..value.clone()
                });
            let mut explicit_checked_bridge = false;
            let caught_source_bridge = if graph.caught_source_site.is_some()
                && value.literal.is_none()
                && refined
                    .as_ref()
                    .is_none_or(|refined| throwing::expression(class, method, refined).is_err())
                && throwing::expression(class, method, &value).is_err()
                && graph.same_object_rethrow_authorization.is_none()
                && graph.loop_entry_copy.is_none()
                && graph.parent_prefix.is_none()
            {
                graph
                    .caught_source_certificate
                    .as_ref()
                    .filter(|(_, sites)| sites.contains(&pc))
                    .map(|(name, _)| name.clone())
            } else {
                None
            };
            let expression = if let Some(helper) = caught_source_bridge {
                graph.caught_source_uses.borrow_mut().insert(pc);
                explicit_checked_bridge = true;
                format!("{helper}({})", argument(&value, "Ljava/lang/Throwable;")?)
            } else if let Some(refined) = refined {
                let expression = format!("(({}) {})", java_type(&refined.ty)?, value.text);
                if throwing::expression(class, method, &refined).is_err()
                    && !throwing::locally_caught(class, method, pc, &refined)
                {
                    ensure!(
                        throwing::may_infer_declaration(class, method, &refined),
                        "Refined caught throw requires the exact checked declaration contract"
                    );
                    out.inferred_throws.insert(refined.ty.clone());
                }
                if throwing::checked_effect(class, &refined.ty) {
                    out.checked_effects.insert(refined.ty);
                }
                expression
            } else if graph.caught_values.borrow().contains(&value.text) {
                let typed_rethrow = throwing::expression(class, method, &value).is_ok();
                let caught_origin = graph.caught_origins.borrow().get(&value.text).copied();
                let effects =
                    if !typed_rethrow && graph.catch_rethrows.borrow().contains_key(&value.text) {
                        graph
                            .caught_dispatch_effects
                            .borrow()
                            .get(&value.text)
                            .cloned()
                    } else {
                        graph
                            .caught_checked_effects
                            .borrow()
                            .get(&value.text)
                            .cloned()
                    };
                if let Some(effects) = effects {
                    for ty in &effects {
                        let effect = Value {
                            ty: ty.clone(),
                            ..value.clone()
                        };
                        if throwing::expression(class, method, &effect).is_err()
                            && !throwing::locally_caught(class, method, pc, &effect)
                        {
                            let Some((handler_pc, caught_register)) = caught_origin else {
                                return Err(CaughtSourceFailure {
                                    throw_pc: pc,
                                    effect: ty.clone(),
                                }
                                .into());
                            };
                            let site = PreciseRethrowSite {
                                declaring_type: method.declaring_type.clone(),
                                method_name: method.name.clone(),
                                parameters: method.parameters.clone(),
                                return_type: method.return_type.clone(),
                                static_method: method.access_flags & 8 != 0,
                                code_offset: method.code.as_ref().unwrap().offset,
                                handler_pc,
                                caught_register,
                                throw_pc: pc,
                            };
                            if ty == "Ljava/lang/Throwable;"
                                && graph
                                    .same_object_rethrow_authorization
                                    .as_ref()
                                    .is_some_and(|authorization| {
                                        authorization.site == site
                                            || authorization.additional_sites.contains(&site)
                                    })
                            {
                                // Authorization changes only the final Java
                                // source obligation. Preserve the original
                                // operational effect and bind its exact type
                                // to the consumed provenance record.
                                out.checked_effects.insert(ty.clone());
                                out.same_object_rethrow_uses.push(SameObjectRethrowUse {
                                    site,
                                    effect: ty.clone(),
                                });
                                continue;
                            }
                            return Err(PreciseRethrowFailure {
                                site,
                                effect: ty.clone(),
                            }
                            .into());
                        }
                        out.checked_effects.insert(ty.clone());
                    }
                }

                if typed_rethrow {
                    value.text.clone()
                } else {
                    graph
                        .catch_rethrows
                        .borrow()
                        .get(&value.text)
                        .cloned()
                        .unwrap_or_else(|| value.text.clone())
                }
            } else {
                match throwing::expression(class, method, &value) {
                    Ok(expression) => expression,
                    Err(error) => {
                        if (graph.loop_entry_copy.is_none() && graph.parent_prefix.is_none())
                            && value.literal.is_none()
                            && graph
                                .loop_type_analysis(class, method)
                                .is_some_and(|facts| {
                                    throw_zero_proof::proves_read(
                                        &facts.ir,
                                        &facts.ssa,
                                        &facts.types,
                                        pc,
                                        src,
                                    )
                                })
                        {
                            "null".to_string()
                        } else if throwing::locally_caught(class, method, pc, &value) {
                            value.text.clone()
                        } else if throwing::may_infer_declaration(class, method, &value) {
                            out.inferred_throws.insert(value.ty.clone());
                            value.text.clone()
                        } else if let Some(helper) =
                            super::explicit_checked_throw::for_site(class, method, pc, &value.ty)
                                .or_else(|| {
                                    (graph.loop_entry_copy.is_none()
                                        && graph.parent_prefix.is_none())
                                    .then(|| {
                                        super::explicit_checked_throw::for_nonnull_field_site(
                                            class, method, pc, src, &value.ty,
                                        )
                                    })
                                    .flatten()
                                })
                        {
                            explicit_checked_bridge = true;
                            format!("{helper}({})", value.text)
                        } else {
                            if value.ty != "Ljava/lang/Throwable;" {
                                return Err(error);
                            }
                            let site = StaticSneakyRethrowSite {
                                declaring_type: method.declaring_type.clone(),
                                method_name: method.name.clone(),
                                parameters: method.parameters.clone(),
                                return_type: method.return_type.clone(),
                                static_method: method.access_flags & 8 != 0,
                                code_offset: method.code.as_ref().unwrap().offset,
                                throw_pc: pc,
                                throw_register: src as u16,
                            };
                            let Some(authorization) = graph
                                .static_sneaky_rethrow_authorization
                                .as_ref()
                                .filter(|authorization| authorization.site == site)
                            else {
                                return Err(StaticSneakyRethrowFailure {
                                    site,
                                    effect: value.ty.clone(),
                                }
                                .into());
                            };
                            out.checked_effects.insert(value.ty.clone());
                            out.static_sneaky_rethrow_uses.push(StaticSneakyRethrowUse {
                                site,
                                effect: value.ty.clone(),
                                type_parameter: authorization.type_parameter.clone(),
                            });
                            format!("(({}) {})", authorization.type_parameter, value.text)
                        }
                    }
                }
            };
            if !explicit_checked_bridge
                && !graph.caught_values.borrow().contains(&value.text)
                && expression != "null"
                && throwing::checked_effect(class, &value.ty)
            {
                out.checked_effects.insert(value.ty.clone());
            }
            if throwing_prologue {
                // Java 25 keeps the delegation source-reachable but emits
                // no superclass invocation for this always-throw prologue.
                out.line("if (true) {", &[]);
                out.indent += 1;
                out.line(&format!("throw {expression};"), &[]);
                out.indent -= 1;
                out.line("}", &[]);
                out.line("super();", &[]);
            } else {
                out.line(&format!("throw {expression};"), &[]);
            }
            returned = true;
        }
        0x0e => {
            if let Some(instruction) = instruction {
                ensure!(
                    decoded_return_source(instruction, regs.len())?.is_none(),
                    "invalid shared void return"
                );
            }
            ensure!(
                method.return_type.as_ref() == "V" && initialized,
                "invalid void return"
            );
            if let Some(plan) = &graph.initializer_exits {
                let assignments = plan
                    .returns
                    .get(&pc)
                    .context("initializer return differs from CFG proof")?;
                for assignment in assignments {
                    out.line(
                        &format!("{} = {};", assignment.name, assignment.value),
                        &[(0, assignment.name.chars().count(), assignment.label.clone())],
                    );
                }
                out.line(&format!("break {};", super::initializer_exits::LABEL), &[]);
            } else {
                out.line("return;", &[]);
            }
            returned = true;
        }
        0x0f..=0x11 => {
            let src = if let Some(instruction) = instruction {
                decoded_return_source(instruction, regs.len())?
                    .context("missing shared return source")?
            } else {
                a
            };
            ensure!(
                initialized && method.return_type.as_ref() != "V",
                "invalid return"
            );
            ensure!(
                (op == 0x11) == reference(&method.return_type)
                    && (op == 0x10) == wide(&method.return_type),
                "return opcode type mismatch"
            );
            let value = register(&regs, src)?;
            let converted =
                argument_at_read(&value, &method.return_type, src, class, method, graph, pc);
            let expr = match converted {
                Ok(expr) => expr,
                Err(original)
                    if method.return_type.as_ref() == "Z"
                        && value.ty == "I"
                        && value.literal.is_none()
                        && !value.raw_bits32 =>
                {
                    if proven_boolean_return(class, method, graph, pc, src)? {
                        format!("({} != 0)", value.text)
                    } else {
                        return Err(original);
                    }
                }
                Err(original) => return Err(original),
            };
            if exception_slots.is_some()
                && reference(&method.return_type)
                && !method
                    .code
                    .as_ref()
                    .unwrap()
                    .try_regions
                    .iter()
                    .any(|region| region.start as usize <= pc && pc < region.end as usize)
            {
                // An absorbed DEX return cannot throw. Do not introduce a
                // potentially failing Java downcast under a new catch.
                ensure!(
                    expr == value.text
                        || expr == "null"
                        || method.return_type.as_ref() == "Ljava/lang/Object;"
                        || class.symbols.hierarchy.get().is_some_and(|hierarchy| {
                            hierarchy.assignable(&value.ty, &method.return_type)
                                == crate::native_hierarchy::Relation::Proven
                        }),
                    "return tail requires a potentially throwing conversion"
                );
            }
            out.return_value(&value, &expr, &method.return_type);
            returned = true;
        }
        0x12..=0x15 => {
            let (dst, n) = if let Some(instruction) = instruction {
                let (dst, literal) = decoded_constant(instruction, regs.len())?;
                (
                    dst,
                    i32::try_from(literal).context("shared constant exceeds 32 bits")?,
                )
            } else {
                match op {
                    0x12 => (a & 15, ((w as i16) >> 12) as i32),
                    0x13 => (a, words[pc + 1] as i16 as i32),
                    0x14 => (
                        a,
                        (words[pc + 1] as u32 | ((words[pc + 2] as u32) << 16)) as i32,
                    ),
                    _ => (a, (words[pc + 1] as i32) << 16),
                }
            };
            assign(
                &mut regs,
                dst,
                Value {
                    text: n.to_string(),
                    ty: "I".into(),
                    literal: Some(n),
                    wide_literal: None,
                    raw_bits32: false,
                },
            )?;
        }
        0x16..=0x19 => {
            let (dst, bits) = if let Some(instruction) = instruction {
                let (dst, literal) = decoded_constant(instruction, regs.len())?;
                (dst, literal as u64)
            } else {
                (
                    a,
                    match op {
                        0x16 => words[pc + 1] as i16 as i64 as u64,
                        0x17 => {
                            (words[pc + 1] as u32 | ((words[pc + 2] as u32) << 16)) as i32 as i64
                                as u64
                        }
                        0x18 => (0..4).fold(0u64, |bits, i| {
                            bits | ((words[pc + 1 + i] as u64) << (16 * i))
                        }),
                        _ => (words[pc + 1] as u64) << 48,
                    },
                )
            };
            assign(
                &mut regs,
                dst,
                Value {
                    text: format!("{}L", bits as i64),
                    ty: "J".into(),
                    literal: None,
                    wide_literal: Some(bits),
                    raw_bits32: false,
                },
            )?;
        }
        0x2d..=0x31 => {
            let spec = numeric::Compare::decode(op).context("comparison opcode")?;
            let (dst, left, right) = if let Some(instruction) = instruction {
                let (dst, sources, _) = decoded_arithmetic(instruction, regs.len())?;
                (dst, sources[0], sources[1])
            } else {
                let packed = words[pc + 1];
                (a, (packed & 255) as usize, (packed >> 8) as usize)
            };
            let lhs = argument_at_read(
                &register(&regs, left)?,
                spec.input.descriptor(),
                left,
                class,
                method,
                graph,
                pc,
            )?;
            let rhs = argument_at_read(
                &register(&regs, right)?,
                spec.input.descriptor(),
                right,
                class,
                method,
                graph,
                pc,
            )?;
            let value = out.local("I", &spec.expression(&lhs, &rhs), &[])?;
            assign(&mut regs, dst, value)?;
        }
        0x1a | 0x1b => {
            let (dst, index) = if let Some(instruction) = instruction {
                decoded_reference_constant(instruction, regs.len())?
            } else {
                let mut index = words[pc + 1] as usize;
                if op == 0x1b {
                    index |= (words[pc + 2] as usize) << 16;
                }
                (a, index)
            };
            let s = class.symbols.strings.get(index).context("string index")?;
            let text = string_literal(s)?;
            let value = if (method.name.as_ref() == "<init>" && !initialized)
                || graph.argument_string_producer(class, method, pc)
                || graph.argument_region_contains(pc)
                || graph
                    .constructor_prefix_strings
                    .get()
                    .is_some_and(|(start, end)| (start..end).contains(&pc))
            {
                // Preserve the original resolution point before subsequent
                // argument-region effects, even when this literal is dead.
                out.local("Ljava/lang/String;", &text, &[])?
            } else {
                Value {
                    text,
                    ty: "Ljava/lang/String;".into(),
                    literal: None,
                    wide_literal: None,
                    raw_bits32: false,
                }
            };
            assign(&mut regs, dst, value)?;
        }
        0x1c if allocation.is_some() => {
            let ty = class
                .symbols
                .types
                .get(words[pc + 1] as usize)
                .context("class literal type")?;
            let display = java_type(ty)?;
            let snapshot = if let Some(Some(slot)) = exception_slots.and_then(|slots| slots.get(a))
            {
                ensure!(
                    slot.ty == "Ljava/lang/Class;",
                    "class capture changes exception register type"
                );
                Some(slot.text.clone())
            } else {
                None
            };
            let name = out.next_local_name()?;
            out.opaque_declaration(&format!("java.lang.Class {name};"), &[]);
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
                    raw_bits32: false,
                },
            )?;
        }
        0x8d..=0x8f if instruction.is_some() => {
            let (dst, sources, _) = decoded_arithmetic(instruction.unwrap(), regs.len())?;
            let input = register(&regs, sources[0])?;
            ensure!(
                initialized || input.text != "this",
                "uninitialized this used by array/type operation"
            );
            let ty = ["B", "C", "S"][(op - 0x8d) as usize];
            let value = out.local(
                ty,
                &format!("({}) ({})", java_type(ty)?, integral(&input)?),
                &[],
            )?;
            assign(&mut regs, dst, value)?;
        }
        0x1c | 0x1f..=0x21 | 0x23 | 0x44..=0x51 | 0x8d..=0x8f => {
            let operands = if let Some(instruction) = instruction {
                if op == 0x1c {
                    let (dst, index) = decoded_reference_constant(instruction, regs.len())?;
                    operations::Operands {
                        dst,
                        reads: [0; 3],
                        read_count: 0,
                        type_index: Some(index),
                    }
                } else {
                    decoded_array_type(instruction, regs.len())?
                }
            } else {
                operations::Operands::legacy(op, a, if width > 1 { words[pc + 1] } else { 0 })
            };
            if !initialized {
                let inputs = operands.guarded_inputs(op);
                for &input in &inputs[..operands.read_count] {
                    ensure!(
                        register(&regs, input)?.text != "this",
                        "uninitialized this used by array/type operation"
                    );
                }
            }
            operations::emit(class, op, operands, &mut regs, out)?;
        }
        0x24 | 0x25 => {
            if !initialized {
                let packed = words[pc + 2];
                let inputs: Vec<usize> = if op == 0x25 {
                    (packed as usize..packed as usize + a).collect()
                } else {
                    let count = a >> 4;
                    ensure!(count <= 5, "filled-array register count");
                    let registers = [
                        (packed & 15) as usize,
                        ((packed >> 4) & 15) as usize,
                        ((packed >> 8) & 15) as usize,
                        (packed >> 12) as usize,
                        a & 15,
                    ];
                    registers[..count].to_vec()
                };
                for input in inputs {
                    ensure!(
                        register(&regs, input)?.text != "this",
                        "uninitialized this escapes through filled array"
                    );
                }
            }
            // Preserve physical locals; reinterpret only the exact zero
            // reaching a reference-array operand in the original SSA graph.
            let mut array_regs = regs.clone();
            if graph.loop_entry_copy.is_none()
                && graph.parent_prefix.is_none()
                && class
                    .symbols
                    .types
                    .get(words[pc + 1] as usize)
                    .and_then(|ty| ty.strip_prefix('['))
                    .is_some_and(reference)
                && let Some(analysis) = graph.loop_type_analysis(class, method)
                && let Some(insn) = analysis.ir.instructions.iter().find(|insn| insn.pc == pc)
            {
                for input in &insn.reads {
                    let r = usize::from(input.register);
                    if let Some(Some(value)) = array_regs.get_mut(r)
                        && value.ty == "I"
                        && value.literal.is_none()
                        && ssa_null_read::at_read(
                            &analysis.ir,
                            &analysis.ssa,
                            &analysis.types,
                            pc,
                            r,
                        )
                    {
                        *value = Value {
                            text: "null".into(),
                            ty: "Ljava/lang/Object;".into(),
                            literal: Some(0),
                            wide_literal: None,
                            raw_bits32: false,
                        };
                    }
                }
            }
            pending = Some(operations::filled(
                class,
                op,
                a,
                words[pc + 1],
                words[pc + 2],
                &array_regs,
                out,
            )?);
        }
        0x26 => {
            let offset = i32::from_le_bytes([
                words[pc + 1] as u8,
                (words[pc + 1] >> 8) as u8,
                words[pc + 2] as u8,
                (words[pc + 2] >> 8) as u8,
            ]);
            let payload = usize::try_from(pc as i64 + i64::from(offset))
                .context("array payload outside method")?;
            operations::fill_array(words, payload, &register(&regs, a)?, out)?;
        }
        0x22 => {
            // The allocation decoder must not read or capture uninitialized
            // this, including any aliases. Mask those inputs transactionally.
            let prologue_regs = (!initialized).then(|| {
                regs.iter()
                    .map(|value| {
                        value.clone().filter(|value| {
                            !matches!(value.text.as_str(), "this" | "<masked-constructor-this>")
                        })
                    })
                    .collect::<Vec<_>>()
            });
            if (initialized || constructor)
                    // Staging is safe for handler state only when no entry
                    // register needs a mutable exception snapshot.
                    && exception_slots.is_none_or(|slots| slots.iter().all(Option::is_none))
                    && let Some(lowered) = allocation_lowering::try_lower(
                        class, method, graph, words, pc, stop,
                        prologue_regs.as_deref().unwrap_or(&regs), out,
                    )?
                    && method.code.as_ref().is_some_and(|code| code.try_regions.iter().all(|region| {
                        let start = region.start as usize;
                        let end = region.end as usize;
                        end <= pc || start >= lowered.next_pc || (start <= pc && lowered.next_pc <= end) || lowered.internal_handler_regions.iter().any(|owned|owned==region)
                    }))
            {
                let mut values = lowered.regs;
                if !initialized {
                    let written = super::liveness::written_in(
                        method.code.as_ref().unwrap(),
                        pc,
                        lowered.next_pc,
                    )
                    .context("constructor allocation register writes unknown")?;
                    for (index, original) in regs.iter().enumerate() {
                        if original.as_ref().is_some_and(|value| {
                            matches!(value.text.as_str(), "this" | "<masked-constructor-this>")
                        }) && !written[index]
                        {
                            ensure!(
                                values[index].is_none(),
                                "masked constructor receiver replaced"
                            );
                            values[index] = original.clone();
                        }
                    }
                }
                regs = values;
                out.sequence = lowered.out.sequence;
                out.append(lowered.out);
                pc = lowered.next_pc;
                return Ok(state!(pc));
            }
            // Staged nested construction above is allowed; the simple
            // sentinel fallback tracks exactly one allocation identity.
            ensure!(allocation.is_none(), "overlapping unstaged allocations");
            ensure!(
                exception_slots.is_none_or(|slots| slots.get(a).is_none_or(Option::is_none)),
                "allocation overwrites exception-visible register"
            );
            // Independent allocations are valid Java 25 constructor prologue
            // statements. Their arguments must not expose the receiver.
            ensure!(
                initialized || constructor,
                "allocation before constructor initialization"
            );
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
                    raw_bits32: false,
                },
            )?;
            allocation = Some((a, ty.to_string()));
        }
        0x52..=0x6d => {
            let (reg, receiver_reg, index) = if let Some(instruction) = instruction {
                decoded_field(instruction, regs.len())?
            } else {
                (
                    if op >= 0x60 { a } else { a & 15 },
                    if op >= 0x60 { None } else { Some(a >> 4) },
                    words[pc + 1] as usize,
                )
            };
            let &(owner, ty, name) = class.symbols.fields.get(index).context("field index")?;
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
            let independent_early_field = constructor
                && if is_static {
                    !put
                } else {
                    register(&regs, receiver_reg.context("missing field receiver")?)?.text != "this"
                        && (!put || register(&regs, reg)?.text != "this")
                };
            if !(initialized || independent_early_field) {
                // Independent static reads do not touch the uninitialized receiver.
                // Emit them in DEX order; single-use adjacent delegation arguments
                // can be inlined later without repeating or moving a field read.
                // Java 25 early construction permits assignments to fields
                // declared in this class, but forbids reading/escaping this.
                // Keep DEX order: superclass callbacks may observe the write.
                ensure!(
                    constructor
                        && !is_static
                        && put
                        && owner.as_ref() == class.descriptor.as_ref()
                        && register(&regs, receiver_reg.context("missing field receiver")?)?.text
                            == "this"
                        && class.fields.iter().any(|field| !field.is_static
                            && field.declaring_type == class.descriptor
                            && field.name.as_ref() == name
                            && field.field_type.as_ref() == ty.as_ref()),
                    "unsupported field access before constructor initialization"
                );
                ensure!(
                    register(&regs, reg)?.text != "this",
                    "uninitialized this escapes through field value"
                );
                early_field_writes = true;
            }
            let target = if is_static {
                java_type(owner)?
            } else {
                receiver(
                    &register(&regs, receiver_reg.context("missing field receiver")?)?,
                    owner,
                )?
            };
            // Own blank-final fields require a simple assignment name in
            // Java. A field can also hide the class/package qualifier.
            // Do not use a bare name that could bind a generated local.
            let declared_field = class.fields.iter().find(|field| {
                field.is_static
                    && field.declaring_type == class.descriptor
                    && field.name.as_ref() == name.as_str()
                    && field.field_type.as_ref() == ty.as_ref()
            });
            let initializer_needs_bare = is_static
                && put
                && method.name.as_ref() == "<clinit>"
                && owner == &class.descriptor
                && declared_field.is_some()
                && (declared_field.is_some_and(|field| field.access_flags & 0x10 != 0)
                    || class.fields.iter().any(|field| {
                        names::member(&field.name).is_ok_and(|field_name| {
                            Some(field_name.as_str()) == target.split('.').next()
                                || Some(field_name.as_str()) == target.rsplit('.').next()
                        })
                    }));
            let generated_collision = ["v", "e", "caught", "monitorExit"].iter().any(|prefix| {
                if *prefix != "v"
                    && method
                        .code
                        .as_ref()
                        .is_some_and(|code| code.try_regions.is_empty())
                {
                    return false;
                }
                display_name.strip_prefix(prefix).is_some_and(|suffix| {
                    !suffix.is_empty()
                        && suffix.bytes().all(|b| b.is_ascii_digit())
                        && (*prefix != "v"
                            || suffix
                                .parse::<usize>()
                                .is_ok_and(|index| index >= graph.local_sequence_floor))
                })
            });
            ensure!(
                !initializer_needs_bare || !generated_collision,
                "initializer field collides with generated local namespace"
            );
            let bare_initializer_field = initializer_needs_bare;
            let expr = if bare_initializer_field {
                display_name.clone()
            } else {
                format!("{target}.{display_name}")
            };
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
                if bare_initializer_field {
                    0
                } else {
                    target.chars().count() + 1
                },
                display_name.chars().count(),
                label,
            )];
            if is_static && !bare_initializer_field {
                refs.push((
                    0,
                    target.chars().count(),
                    class_label(owner).context("invalid owner")?,
                ));
            }
            if put {
                let stored = register(&regs, reg)?;
                // Narrow DEX field stores truncate the incoming integral
                // register. Java requires that conversion explicitly.
                let value = if stored.ty == "I" && matches!(ty.as_ref(), "B" | "S" | "C") {
                    format!("(({}) {})", java_type(ty)?, stored.text)
                } else {
                    argument_at_read(&stored, ty, reg, class, method, graph, pc)?
                };
                out.line(&format!("{expr} = {value};"), &refs);
            } else {
                let value = out.local(ty, &expr, &refs)?;
                assign(&mut regs, reg, value)?;
            }
        }
        0x6e..=0x72 | 0x74..=0x78 => {
            let kind = if op >= 0x74 { op - 6 } else { op };
            let static_call = kind == 0x71;
            let inputs: Vec<usize> = if let Some(insn) = instruction {
                insn.reads
                    .iter()
                    .map(|input| usize::from(input.register))
                    .collect()
            } else if op >= 0x74 {
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
            let local_binding;
            let call = if let Some(front) = &graph.front_end {
                let index = front
                    .bound
                    .calls
                    .binary_search_by_key(&pc, |call| call.pc)
                    .map_err(|_| anyhow::anyhow!("missing shared invocation binding at {pc}"))?;
                &front.bound.calls[index]
            } else {
                local_binding = crate::native_calls::bind_invocation(
                    op,
                    pc,
                    u32::from(words[pc + 1]),
                    &inputs,
                    &class.symbols,
                )?;
                &local_binding
            };
            let crate::native_calls::CallTarget::Method {
                declaring_type: owner,
                name,
                prototype_index,
                ..
            } = &call.target
            else {
                bail!("ordinary invoke has non-method target");
            };
            let name = name.as_ref();
            let args = &class.symbols.protos[usize::from(*prototype_index)].1;
            let ret = &call.return_type;
            let target = if static_call {
                java_type(owner)?
            } else {
                let value = register(
                    &regs,
                    usize::from(
                        call.receiver
                            .as_ref()
                            .context("missing invoke receiver")?
                            .register,
                    ),
                )?;
                let value = if graph
                    .guarded_interface_receivers
                    .borrow()
                    .as_ref()
                    .is_some_and(|sites| sites.contains(&pc))
                {
                    ensure!(kind == 0x72 && args.is_empty(), "invalid guarded interface call");
                    let value = out.local("Ljava/lang/Object;", &value.text, &[])?;
                    out.line(
                        &format!(
                            "if ({} != null && !({} instanceof {})) throw new java.lang.IncompatibleClassChangeError();",
                            value.text, value.text, java_type(owner)?
                        ),
                        &[],
                    );
                    value
                } else {
                    value
                };
                if receiver_cleanup::sdk_number_conversion_receiver(
                    &value,
                    owner,
                    name,
                    args,
                    ret,
                    op,
                    class.symbols.hierarchy.get().map(AsRef::as_ref),
                ) || receiver_cleanup::proven_void_receiver(
                    &value,
                    owner,
                    name,
                    args,
                    ret,
                    op,
                    class.symbols.hierarchy.get().map(AsRef::as_ref),
                ) {
                    value.text.clone()
                } else {
                    receiver(&value, owner)?
                }
            };
            let effect_owner = if name == "<init>" {
                allocation
                    .as_ref()
                    .map(|(_, ty)| ty.clone())
                    .unwrap_or_else(|| owner.to_string())
            } else if !static_call {
                let value = register(
                    &regs,
                    usize::from(
                        call.receiver
                            .as_ref()
                            .context("missing invoke receiver")?
                            .register,
                    ),
                )?;
                if value.text == target && reference(&value.ty) {
                    value.ty.clone()
                } else {
                    owner.to_string()
                }
            } else {
                owner.to_string()
            };
            if let Some(hierarchy) = class.symbols.hierarchy.get()
                && let Some(effects) =
                    hierarchy.call_checked_effects(&effect_owner, name, args, ret, static_call)
            {
                out.checked_effects.extend(
                    effects
                        .into_iter()
                        .filter_map(|ty| throwing::checked_effect_type(class, &ty)),
                );
            }

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
            let unambiguous_object_call = static_call
                && args.iter().any(|ty| ty.as_ref() == "Ljava/lang/Object;")
                && class
                    .symbols
                    .hierarchy
                    .get()
                    .is_some_and(|hierarchy| hierarchy.is_unambiguous_object_call(&label));
            let mut actual = Vec::new();
            let mut capture_count = 0;
            let mut capture_refs = Vec::new();
            for (argument_index, input) in call.arguments.iter().enumerate() {
                let ty = &input.descriptor;
                let reg = usize::from(input.register);
                let mut value = register(&regs, reg)?;
                ensure!(
                    !reference(ty)
                        || ty.as_ref() == value.ty
                        || !graph.invalid_reference_call_argument(
                            class,
                            method,
                            pc,
                            argument_index
                        ),
                    "DEX reference argument is not assignable to invoked descriptor"
                );
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
                    value.text != "<uninitialized>" && (initialized || value.text != "this"),
                    "uninitialized invocation argument"
                );
                // Typed null preserves the DEX descriptor when Java has
                // overloads (including constructors and varargs arrays).
                let omit_cast = receiver_cleanup::proven_call_argument(
                    &value,
                    owner,
                    name,
                    args,
                    ret,
                    op,
                    argument_index,
                    class.symbols.hierarchy.get().map(AsRef::as_ref),
                );
                actual.push(if value.literal == Some(0) && reference(ty) {
                    if omit_cast {
                        "null".into()
                    } else {
                        format!("(({}) null)", java_type(ty)?)
                    }
                } else if omit_cast
                    || (unambiguous_object_call
                        && ty.as_ref() == "Ljava/lang/Object;"
                        && reference(&value.ty))
                {
                    value.text.clone()
                } else if ty.as_ref() == "I" && matches!(value.ty.as_str(), "B" | "S" | "C") {
                    format!("((int) {})", value.text)
                } else {
                    match argument_at_read(&value, ty, reg, class, method, graph, pc) {
                        Ok(converted) => converted,
                        Err(original)
                            if ty.as_ref() == "Z"
                                && value.ty == "I"
                                && value.literal.is_none()
                                && !value.raw_bits32
                                && (graph.loop_entry_copy.is_none()
                                    && graph.parent_prefix.is_none())
                                && words.len() <= 4096 =>
                        {
                            let analysis =
                                crate::native_method::MethodAnalysis::build(class, method)?;
                            if boolean_return_definition_proven(
                                analysis.instructions(),
                                analysis.ssa(),
                                analysis.calls(),
                                pc,
                                reg,
                            ) {
                                format!("({} != 0)", value.text)
                            } else {
                                return Err(original);
                            }
                        }
                        Err(error) => return Err(error),
                    }
                });
            }
            ensure!(name != "<clinit>", "static initializer invocation");
            if name == "<init>" {
                if let Some((dst, ty)) = allocation.take() {
                    let source_bridge = class.symbols.hierarchy.get().is_some_and(|h| h.source_constructor_bridge(&ty, owner, args));
                    let retargeted = owner.as_ref() != ty
                        && class.symbols.hierarchy.get().is_some_and(|hierarchy| {
                            hierarchy.equivalent_constructor(&ty, owner, args) || source_bridge
                                || (owner.as_ref() == "Ljava/lang/Object;"
                                    && args.is_empty()
                                    && hierarchy.fresh_null_constructor(&ty)
                                    && method.code.as_ref().is_some_and(|raw| {
                                        pc >= 2
                                            && raw.instructions.get(pc - 2).is_some_and(|word| {
                                                *word == (((dst as u16) << 8) | 0x22)
                                            })
                                            && raw
                                                .instructions
                                                .get(pc - 1)
                                                .and_then(|index| {
                                                    class.symbols.types.get(*index as usize)
                                                })
                                                .is_some_and(|actual| actual.as_ref() == ty)
                                            && raw.instructions.get(pc..pc + width)
                                                == words.get(pc..pc + width)
                                    }))
                        });
                    ensure!(
                        kind == 0x70
                            && inputs.first().is_some_and(|&receiver| {
                                regs.get(receiver)
                                    .and_then(Option::as_ref)
                                    .is_some_and(|v| v.text == "<uninitialized>" && v.ty == ty)
                            })
                            && (owner.as_ref() == ty || retargeted)
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
                    let display = java_type(&ty)?;
                    if source_bridge { actual.push("((java.lang.Void) null)".into()); }
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
                    let value = out.local(&ty, &expr, &refs)?;
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
                    if let Some(slots) = exception_slots
                        && graph
                            .exception_snapshot_end
                            .get()
                            .is_none_or(|end| pc + width < end)
                    {
                        sync_exception_registers(
                            slots,
                            &regs,
                            &mut previous_exception_values,
                            out,
                            false,
                        )?;
                    }
                    pc += width;
                    return Ok(state!(pc));
                }
                let retargeted_super = equivalent_constructor_delegation(class, owner, args);
                ensure!(
                    constructor
                        && !initialized
                        && kind == 0x70
                        && (owner.as_ref() == class.descriptor.as_ref()
                            || class.superclass.as_deref() == Some(owner.as_ref())
                            || retargeted_super)
                        && ret.as_ref() == "V",
                    "unsupported constructor invocation"
                );
                let receiver = register(&regs, inputs[0])?;
                ensure!(receiver.text == "this", "constructor receiver");
                if retargeted_super {
                    // Java selects an overload on the restored direct parent,
                    // while ordinary cast cleanup examined the raw ancestor.
                    // Pin every argument's original invocation descriptor.
                    for (expression, ty) in actual.iter_mut().zip(args) {
                        *expression = format!("(({}) {})", java_type(ty)?, expression);
                    }
                }
                // Invocation binding separates argument heads from DEX's
                // raw wide words; tails are not independently readable values.
                for argument in &call.arguments {
                    ensure!(
                        register(&regs, usize::from(argument.register))?.text != "this",
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
                let mut display_name = names::member(name)?;
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
                let target = if static_call
                    && let Some(bridge) = super::checked_call_bridge::for_site(
                        class,
                        method,
                        pc,
                        graph.caught_source_site.is_some(),
                    ) {
                    display_name = bridge.name;
                    java_type(&class.descriptor)?
                } else if kind == 0x6f {
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
                    let receiver_is_this = receiver.text == "this"
                        || (method.access_flags & 8 == 0
                            && words.len() <= 4096
                            && graph.loop_entry_copy.is_none()
                            && graph.parent_prefix.is_none()
                            && crate::native_method::MethodAnalysis::build(class, method)
                                .ok()
                                .is_some_and(|analysis| {
                                    crate::native_hierarchy::this_receiver_identity::proves_read(
                                        method,
                                        analysis.instructions(),
                                        analysis.ssa(),
                                        pc,
                                        inputs[0],
                                    )
                                }));
                    if !receiver_is_this {
                        let bridge = super::super_bridge::for_site(class, method, pc)
                            .context("invalid super receiver")?;
                        ensure!(
                            receiver.ty == class.descriptor.as_ref() || receiver.literal == Some(0),
                            "invalid super bridge receiver type"
                        );
                        display_name = bridge.name;
                        format!(
                            "(({}) {})",
                            java_type(&class.descriptor)?,
                            argument(&receiver, &class.descriptor)?
                        )
                    } else if ancestor {
                        "super".into()
                    } else {
                        let qualifier = class
                            .symbols
                            .hierarchy
                            .get()
                            .and_then(|hierarchy| {
                                hierarchy.interface_super_qualifier(class, owner, name, args, ret)
                            })
                            .context("invalid super receiver")?;
                        // The shared argument reader already preserves
                        // descriptor types (including null and primitive
                        // widening). Its cast-cleanup proof excludes super
                        // opcodes, so retain those conversions exactly once
                        // when selecting this direct-interface qualifier.
                        format!("{}.super", java_type(&qualifier)?)
                    }
                } else {
                    target
                };
                let shrink_index = if static_call && allocation.is_none() {
                    out.last_local.as_ref().and_then(|local| {
                        call.arguments
                            .iter()
                            .enumerate()
                            .skip(1)
                            .find_map(|(index, argument)| {
                                let value = register(&regs, usize::from(argument.register)).ok()?;
                                if value != local.value
                                    || local.indent != out.indent
                                    || local.value.ty != argument.descriptor.as_ref()
                                    || actual[index] != value.text
                                    || !actual[..index].iter().zip(&call.arguments[..index]).all(
                                        |(rendered, earlier)| {
                                            register(&regs, usize::from(earlier.register))
                                                .is_ok_and(|value| {
                                                    value.ty == earlier.descriptor.as_ref()
                                                        && rendered == &value.text
                                                        && value
                                                            .text
                                                            .strip_prefix('v')
                                                            .or_else(|| {
                                                                value.text.strip_prefix('p')
                                                            })
                                                            .is_some_and(|digits| {
                                                                !digits.is_empty()
                                                                    && digits.bytes().all(|byte| {
                                                                        byte.is_ascii_digit()
                                                                    })
                                                            })
                                                })
                                        },
                                    )
                                    || !graph.single_use_call_result(
                                        method,
                                        pc,
                                        argument.register,
                                        &local.value.ty,
                                    )
                                {
                                    return None;
                                }
                                Some(index)
                            })
                    })
                } else {
                    None
                };
                let moved_local = shrink_index.and_then(|index| {
                    let prefix = format!(
                        "(({}) ",
                        java_type(&out.last_local.as_ref()?.value.ty).ok()?
                    );
                    let local = out.last_local.take()?;
                    actual[index] = format!("{prefix}{})", local.expression);
                    Some((index, local, prefix.chars().count()))
                });
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
                if let Some((index, local, cast_prefix)) = moved_local {
                    let offset = target.chars().count()
                        + display_name.chars().count()
                        + 2
                        + actual[..index]
                            .iter()
                            .map(|arg| arg.chars().count() + 2)
                            .sum::<usize>();
                    refs.extend(local.refs.iter().map(|(start, len, label)| {
                        (offset + cast_prefix + start, *len, label.clone())
                    }));
                    out.truncate_local(&local);
                }
                let consumes_result = if graph.front_end.is_some() {
                    call.result.is_some()
                } else {
                    words
                        .get(pc + width)
                        .is_some_and(|word| matches!(word & 0xff, 0x0a..=0x0c))
                };
                if ret.as_ref() == "V" || !consumes_result {
                    out.line(&format!("{expr};"), &refs);
                } else {
                    pending = Some(out.local(ret, &expr, &refs)?);
                }
            }
        }
        0x7b..=0x8c => {
            let spec = numeric::Unary::decode(op).context("unary opcode")?;
            let (dst, src) = if let Some(instruction) = instruction {
                let (dst, sources, _) = decoded_arithmetic(instruction, regs.len())?;
                (dst, sources[0])
            } else {
                (a & 15, a >> 4)
            };
            let source = argument_at_read(
                &register(&regs, src)?,
                spec.input.descriptor(),
                src,
                class,
                method,
                graph,
                pc,
            )?;
            let value = out.local(spec.result.descriptor(), &spec.expression(&source), &[])?;
            ensure!(value.ty == spec.result_descriptor, "unary result type");
            assign(&mut regs, dst, value)?;
        }
        0x9b..=0xaf | 0xbb..=0xcf => {
            let spec = numeric::Binary::decode(op).context("binary opcode")?;
            let (dst, left, right) = if let Some(instruction) = instruction {
                let (dst, sources, _) = decoded_arithmetic(instruction, regs.len())?;
                (dst, sources[0], sources[1])
            } else if op >= 0xb0 {
                (a & 15, a & 15, a >> 4)
            } else {
                let packed = words[pc + 1];
                (a, (packed & 255) as usize, (packed >> 8) as usize)
            };
            let left = argument_at_read(
                &register(&regs, left)?,
                spec.left.descriptor(),
                left,
                class,
                method,
                graph,
                pc,
            )?;
            let right = argument_at_read(
                &register(&regs, right)?,
                spec.right.descriptor(),
                right,
                class,
                method,
                graph,
                pc,
            )?;
            let value = out.local(
                spec.result.descriptor(),
                &spec.expression(&left, &right),
                &[],
            )?;
            assign(&mut regs, dst, value)?;
        }
        0x90..=0x9a | 0xb0..=0xba | 0xd0..=0xe2 => {
            let decoded = instruction
                .map(|instruction| decoded_arithmetic(instruction, regs.len()))
                .transpose()?;
            if let Some((dst, expression)) =
                boolean_bitwise_expression(op, a, words, pc, &regs, decoded.as_ref())?
            {
                let value = out.local("Z", &expression, &[])?;
                assign(&mut regs, dst, value)?;
            } else {
                let (dst, lhs, rhs, kind) = if let Some((dst, sources, literal)) = &decoded {
                    let kind = if op <= 0x9a {
                        op - 0x90
                    } else if op <= 0xba {
                        op - 0xb0
                    } else if op <= 0xd7 {
                        op - 0xd0
                    } else {
                        op - 0xd8
                    };
                    (
                        *dst,
                        integral(&register(&regs, sources[0])?)?,
                        if let Some(n) = literal {
                            n.to_string()
                        } else {
                            integral(&register(&regs, sources[1])?)?
                        },
                        kind,
                    )
                } else if op <= 0x9a {
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
        0x1d | 0x1e => bail!("unmatched or unproven monitor instruction"),
        _ => bail!("unsupported opcode 0x{op:02x} at {pc}"),
    }
    ensure!(
        out.text.len() <= 4 * 1024 * 1024,
        "reconstructed method exceeds output budget"
    );
    if !returned
        && graph
            .exception_snapshot_end
            .get()
            .is_none_or(|end| pc + width < end)
        && !bare_return_tail(graph, words, pc + width)
        && let Some(slots) = exception_slots
    {
        sync_exception_registers(
            slots,
            &regs,
            &mut previous_exception_values,
            out,
            allocation.is_some(),
        )?;
    }
    pc += width;
    Ok(state!(pc))
}
