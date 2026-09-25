//! Array and type operations; effects are materialized at their DEX position.
use super::{
    Output, Value, argument, assign, class_label, integral, java_type, reference, register,
};
use crate::native_dex::DexClass;
use anyhow::{Context, Result, bail, ensure};

pub(super) fn emit(
    class: &DexClass,
    op: u8,
    a: usize,
    operand: u16,
    regs: &mut [Option<Value>],
    out: &mut Output,
) -> Result<()> {
    match op {
        0x8d..=0x8f => {
            let ty = ["B", "C", "S"][(op - 0x8d) as usize];
            let input = integral(&register(regs, a >> 4)?)?;
            let value = out.local(ty, &format!("({}) ({input})", java_type(ty)?), &[])?;
            assign(regs, a & 15, value)?;
        }
        0x1c | 0x1f | 0x20 | 0x23 => {
            let ty = class
                .symbols
                .types
                .get(operand as usize)
                .context("type index")?;
            let display = java_type(ty)?;
            let refs_at = |offset| {
                class_label(ty)
                    .map(|s| vec![(offset, display.chars().count(), s)])
                    .unwrap_or_default()
            };
            match op {
                0x1c => {
                    let value = out.local(
                        "Ljava/lang/Class;",
                        &format!("{display}.class"),
                        &refs_at(0),
                    )?;
                    assign(regs, a, value)?;
                }
                0x1f => {
                    ensure!(reference(ty), "check-cast requires reference type");
                    let input = register(regs, a)?;
                    ensure!(
                        reference(&input.ty) || input.literal == Some(0),
                        "cast of nonreference"
                    );
                    let input = if input.literal == Some(0) {
                        "null".into()
                    } else if input.ty == "Ljava/lang/Object;" || input.ty == ty.as_ref() {
                        input.text
                    } else {
                        format!("((java.lang.Object) {})", input.text)
                    };
                    let value = out.local(ty, &format!("(({display}) {input})"), &refs_at(2))?;
                    assign(regs, a, value)?;
                }
                0x20 => {
                    ensure!(reference(ty), "instance-of requires reference type");
                    let input = register(regs, a >> 4)?;
                    ensure!(
                        reference(&input.ty) || input.literal == Some(0),
                        "instance-of on nonreference"
                    );
                    let input = if input.literal == Some(0) {
                        "null".into()
                    } else {
                        format!("((java.lang.Object) {})", input.text)
                    };
                    let prefix = format!("{input} instanceof ");
                    let value = out.local(
                        "Z",
                        &format!("{prefix}{display}"),
                        &refs_at(prefix.chars().count()),
                    )?;
                    assign(regs, a & 15, value)?;
                }
                0x23 => {
                    ensure!(ty.starts_with('['), "new-array requires array type");
                    let size = integral(&register(regs, a >> 4)?)?;
                    let dimensions = ty.bytes().take_while(|b| *b == b'[').count();
                    let base = java_type(&ty[dimensions..])?;
                    let expression = format!("new {base}[{size}]{}", "[]".repeat(dimensions - 1));
                    let refs = class_label(ty)
                        .map(|s| vec![(4, base.chars().count(), s)])
                        .unwrap_or_default();
                    let value = out.local(ty, &expression, &refs)?;
                    assign(regs, a & 15, value)?;
                }
                _ => unreachable!(),
            }
        }
        0x21 => {
            let array = register(regs, a >> 4)?;
            ensure!(
                array.ty.starts_with('['),
                "array-length requires known array type"
            );
            let value = out.local("I", &format!("{}.length", array.text), &[])?;
            assign(regs, a & 15, value)?;
        }
        0x44..=0x51 => {
            let array = register(regs, (operand & 255) as usize)?;
            let element = array
                .ty
                .strip_prefix('[')
                .context("array access requires known array type")?;
            let index = integral(&register(regs, (operand >> 8) as usize)?)?;
            let put = op >= 0x4b;
            let family = op - if put { 0x4b } else { 0x44 };
            ensure!(
                match family {
                    0 => matches!(element, "I" | "F"),
                    1 => matches!(element, "J" | "D"),
                    2 => reference(element),
                    3 => element == "Z",
                    4 => element == "B",
                    5 => element == "C",
                    6 => element == "S",
                    _ => false,
                },
                "array opcode/component mismatch or unsupported wide element"
            );
            if put {
                let value = register(regs, a)?;
                // A reference store must perform the VM's array store check,
                // never a synthetic component downcast before that check.
                let (array_text, value_text) = if reference(element) {
                    ensure!(
                        reference(&value.ty) || value.literal == Some(0),
                        "nonreference array store"
                    );
                    (
                        format!("((java.lang.Object[]) {})", array.text),
                        if value.literal == Some(0) {
                            "null".into()
                        } else {
                            value.text
                        },
                    )
                } else if matches!(element, "B" | "C" | "S") {
                    (
                        array.text.clone(),
                        format!("({}) ({})", java_type(element)?, integral(&value)?),
                    )
                } else {
                    (array.text.clone(), argument(&value, element)?)
                };
                out.line(&format!("{array_text}[{index}] = {value_text};"), &[]);
            } else {
                let value = out.local(element, &format!("{}[{index}]", array.text), &[])?;
                assign(regs, a, value)?;
            }
        }
        _ => bail!("unsupported array/type operation"),
    }
    Ok(())
}

pub(super) fn filled(
    class: &DexClass,
    op: u8,
    a: usize,
    type_index: u16,
    packed: u16,
    regs: &[Option<Value>],
    out: &mut Output,
) -> Result<Value> {
    let ty = class
        .symbols
        .types
        .get(type_index as usize)
        .context("filled-array type index")?;
    let element = ty
        .strip_prefix('[')
        .context("filled-array requires array type")?;
    ensure!(!matches!(element, "J" | "D"), "wide filled-array element");
    let display = java_type(ty)?;
    let inputs: Vec<_> = if op == 0x25 {
        (packed as usize..packed as usize + a).collect()
    } else {
        let count = a >> 4;
        ensure!(count <= 5, "filled-array register count");
        let all = [
            (packed & 15) as usize,
            ((packed >> 4) & 15) as usize,
            ((packed >> 8) & 15) as usize,
            (packed >> 12) as usize,
            a & 15,
        ];
        all[..count].to_vec()
    };
    let mut values = Vec::with_capacity(inputs.len());
    for r in inputs {
        let value = register(regs, r)?;
        if reference(element) && value.literal != Some(0) {
            ensure!(
                reference(&value.ty) && (value.ty == element || element == "Ljava/lang/Object;"),
                "filled-array requires known assignable reference"
            );
            values.push(value.text);
        } else {
            values.push(argument(&value, element)?);
        }
    }
    let refs = class_label(ty)
        .map(|s| vec![(4, display.chars().count(), s)])
        .unwrap_or_default();
    out.local(
        ty,
        &format!("new {display} {{{}}}", values.join(", ")),
        &refs,
    )
}

/// Returns element width, element count, and payload size in DEX code units.
pub(super) fn array_payload(words: &[u16], pc: usize) -> Result<(usize, usize, usize)> {
    ensure!(
        words.get(pc) == Some(&0x0300),
        "invalid array payload marker"
    );
    let element_width = usize::from(*words.get(pc + 1).context("truncated array payload")?);
    ensure!(
        matches!(element_width, 1 | 2 | 4 | 8),
        "invalid array element width"
    );
    let count = usize::from(*words.get(pc + 2).context("truncated array count")?)
        | (usize::from(*words.get(pc + 3).context("truncated array count")?) << 16);
    ensure!(count <= 65_536, "array payload exceeds output budget");
    let width = 4 + (count * element_width).div_ceil(2);
    ensure!(
        pc.checked_add(width).is_some_and(|end| end <= words.len()),
        "truncated array payload data"
    );
    Ok((element_width, count, width))
}

pub(super) fn fill_array(words: &[u16], pc: usize, array: &Value, out: &mut Output) -> Result<()> {
    let (width, count, _) = array_payload(words, pc)?;
    let element = array
        .ty
        .strip_prefix('[')
        .context("fill-array-data requires known array")?;
    let required = match element {
        "Z" | "B" => 1,
        "C" | "S" => 2,
        "I" | "F" => 4,
        "J" | "D" => 8,
        _ => anyhow::bail!("fill-array-data requires primitive array"),
    };
    ensure!(width == required, "array payload width/type mismatch");
    let mut values = Vec::with_capacity(count);
    for index in 0..count {
        let mut bits = 0u64;
        for byte in 0..width {
            let offset = index * width + byte;
            bits |=
                u64::from((words[pc + 4 + offset / 2] >> ((offset % 2) * 8)) & 255) << (byte * 8);
        }
        use super::numeric::{Kind, Literal};
        values.push(match element {
            "Z" => {
                ensure!(bits <= 1, "noncanonical boolean array payload");
                (bits == 1).to_string()
            }
            "B" => (bits as i8).to_string(),
            "S" => (bits as i16).to_string(),
            "C" => (bits as u16).to_string(),
            "I" => Literal::Bits32(bits as u32).render(Kind::Int)?,
            "F" => Literal::Bits32(bits as u32).render(Kind::Float)?,
            "J" => Literal::Bits64(bits).render(Kind::Long)?,
            "D" => Literal::Bits64(bits).render(Kind::Double)?,
            _ => unreachable!(),
        });
    }
    // ART FillArrayData (runtime/entrypoints/entrypoint_utils.cc) checks null
    // and the complete destination capacity before copying.
    // Checking first avoids Java's sequential stores partially mutating a short array.
    out.line(&format!("if ({}.length < {count}) {{", array.text), &[]);
    out.indent += 1;
    out.line("throw new java.lang.ArrayIndexOutOfBoundsException();", &[]);
    out.indent -= 1;
    out.line("}", &[]);
    for (index, value) in values.into_iter().enumerate() {
        out.line(&format!("{}[{index}] = {value};", array.text), &[]);
    }
    Ok(())
}
