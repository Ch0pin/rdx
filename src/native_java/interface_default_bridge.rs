//! Bounded expansion of Kotlin's static self-interface default accessor.
//! Copy the loaded default body with the original receiver register bound to p0;
//! an ordinary interface invocation would incorrectly dispatch to an override.
use super::{method, names};
use crate::{
    engine::CodeLink,
    native_dex::{DexClass, DexCode, DexMethod},
    native_ir::DecodedMethod,
};
use anyhow::{Context, Result, ensure};

pub(super) fn reconstruct(
    name: &str,
    class: &DexClass,
    bridge: &DexMethod,
) -> Result<Option<method::MethodBody>> {
    if class.access_flags & 0x200 == 0
        || bridge.access_flags != 0x1009
        || bridge.parameters.as_slice() != [class.descriptor.clone()]
        || !bridge.return_type.starts_with('L')
        || !bridge.thrown_types.is_empty()
    {
        return Ok(None);
    }
    let Some(code) = bridge.code.as_ref() else {
        return Ok(None);
    };
    if code.registers != 1
        || code.ins != 1
        || code.outs != 1
        || code.tries != 0
        || !code.try_regions.is_empty()
        || code.instructions.len() != 5
        || code.instructions[0] != 0x106f
        || code.instructions[2..] != [0, 0x000c, 0x0011]
    {
        return Ok(None);
    }
    let &(owner, proto, target_name) = class
        .symbols
        .methods
        .get(usize::from(code.instructions[1]))
        .context("default bridge method reference")?;
    let owner = class
        .symbols
        .types
        .get(usize::from(owner))
        .context("default bridge owner")?;
    let (ret, args) = class
        .symbols
        .protos
        .get(usize::from(proto))
        .context("default bridge prototype")?;
    let target_name = class
        .symbols
        .strings
        .get(target_name as usize)
        .context("default bridge name")?;
    if owner != &class.descriptor
        || ret != &bridge.return_type
        || !args.is_empty()
        || bridge.name.as_ref() != format!("access${target_name}$jd")
    {
        return Ok(None);
    }
    let mut matching = class.methods.iter().filter(|target| {
        target.declaring_type == class.descriptor
            && target.name.as_ref() == target_name
            && target.parameters.is_empty()
            && target.return_type == *ret
    });
    let Some(target) = matching.next() else {
        return Ok(None);
    };
    if matching.next().is_some() || target.access_flags != 1 || !target.thrown_types.is_empty() {
        return Ok(None);
    }
    let Some(body_code) = target.code.as_ref() else {
        return Ok(None);
    };
    if body_code.ins != 1
        || body_code.registers == 0
        || body_code.tries != 0
        || !body_code.try_regions.is_empty()
        || body_code.instructions.len() > 1024
    {
        return Ok(None);
    }
    let decoded = DecodedMethod::decode(body_code)?;
    // General special-call context, branches, protection and monitors remain
    // outside this straight-line default-body expansion.
    if decoded
        .instructions
        .iter()
        .any(|i| matches!(i.opcode, 0x1d | 0x1e | 0x28..=0x3d | 0x6f | 0x75))
    {
        return Ok(None);
    }
    let expanded = DexMethod {
        declaring_type: bridge.declaring_type.clone(),
        name: bridge.name.clone(),
        return_type: bridge.return_type.clone(),
        parameters: bridge.parameters.clone(),
        thrown_types: vec![],
        access_flags: bridge.access_flags,
        code: Some(DexCode {
            registers: body_code.registers,
            ins: body_code.ins,
            outs: body_code.outs,
            tries: 0,
            try_regions: vec![],
            instructions: body_code.instructions.clone(),
            offset: body_code.offset,
        }),
    };
    let mut body = method::reconstruct(name, class, &expanded)?;
    ensure!(
        body.inferred_throws.is_empty(),
        "default bridge requires checked-effect-free body"
    );
    let prefix = "        // Expanded nonvirtual default: ";
    let display = names::member(target_name)?;
    let label = format!("{name}.{target_name}(){}", bridge.return_type);
    let mut text = format!("{prefix}{display}\n        if (p0 == null) {{ throw null; }}\n");
    let offset = text.chars().count();
    text.push_str(&body.text);
    for link in &mut body.links {
        link.start += offset;
        link.end += offset;
    }
    body.links.insert(
        0,
        CodeLink {
            start: prefix.chars().count(),
            end: prefix.chars().count() + display.chars().count(),
            label,
        },
    );
    body.text = text;
    Ok(Some(body))
}
