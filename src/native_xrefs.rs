//! Method call edges from DEX instructions, independent of Java reconstruction.
use crate::{
    native_cfg::instruction_width,
    native_dex::{DexClass, DexCode, DexMethod},
};
use anyhow::{Context, Result, ensure};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallSite {
    pub caller: String,
    pub callee: String,
    pub pc: usize,
    pub dispatch: &'static str,
    pub dex_entry: Option<String>,
    pub method_index: usize,
    pub opcode: u8,
    pub words: Vec<u16>,
    pub code_offset: u32,
}

pub fn call_sites(
    class: &DexClass,
    method: Option<&str>,
    cancel: &AtomicBool,
) -> Result<Vec<CallSite>> {
    let mut sites = Vec::new();
    for m in &class.methods {
        if cancel.load(Ordering::Relaxed) {
            anyhow::bail!("Call search cancelled");
        }
        let caller = crate::native_engine::disassembly::method_id(m);
        if method.is_some_and(|target| target != caller) {
            continue;
        }
        sites.extend(method_call_sites(class, m, cancel)?);
    }
    Ok(sites)
}

/// One `new-instance` operand, resolved within its owning DEX.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Allocation {
    pub method: String,
    pub class: String,
    pub descriptor: String,
    pub pc: usize,
    pub register: u16,
    pub type_index: usize,
    pub dex_entry: Option<String>,
    pub words: Vec<u16>,
    pub code_offset: u32,
}

/// Visit every instruction boundary of a body, skipping payload tables.
fn walk(
    code: &DexCode,
    cancel: &AtomicBool,
    mut visit: impl FnMut(usize) -> Result<()>,
) -> Result<()> {
    let mut pc = 0;
    let mut next_cancel_pc = 0;
    while pc < code.instructions.len() {
        if pc >= next_cancel_pc {
            ensure!(!cancel.load(Ordering::Relaxed), "Call search cancelled");
            next_cancel_pc = pc.saturating_add(256);
        }
        let (width, payload) = instruction_width(&code.instructions, pc)?;
        ensure!(
            pc + width <= code.instructions.len(),
            "Truncated instruction"
        );
        if !payload {
            visit(pc)?;
        }
        pc += width;
    }
    Ok(())
}

/// Scan one already-resolved method without walking unrelated method bodies.
pub fn method_call_sites(
    class: &DexClass,
    m: &DexMethod,
    cancel: &AtomicBool,
) -> Result<Vec<CallSite>> {
    ensure!(!cancel.load(Ordering::Relaxed), "Call search cancelled");
    let mut sites = Vec::new();
    let Some(code) = &m.code else {
        return Ok(sites);
    };
    walk(code, cancel, |pc| {
        if let Some(site) = call_site_at(class, m, pc)? {
            sites.push(site);
        }
        Ok(())
    })?;
    Ok(sites)
}

/// Scan one body once for both invokes and `new-instance` allocations.
pub fn method_sites(
    class: &DexClass,
    m: &DexMethod,
    cancel: &AtomicBool,
) -> Result<(Vec<CallSite>, Vec<Allocation>)> {
    ensure!(!cancel.load(Ordering::Relaxed), "Call search cancelled");
    let mut calls = Vec::new();
    let mut allocations = Vec::new();
    let Some(code) = &m.code else {
        return Ok((calls, allocations));
    };
    walk(code, cancel, |pc| {
        if let Some(site) = call_site_at(class, m, pc)? {
            calls.push(site);
        } else if let Some(allocation) = allocation_at(class, m, pc)? {
            allocations.push(allocation);
        }
        Ok(())
    })?;
    Ok((calls, allocations))
}

/// Decode a `new-instance` at an instruction boundary established by the DEX scanner.
pub fn allocation_at(class: &DexClass, m: &DexMethod, pc: usize) -> Result<Option<Allocation>> {
    let code = m.code.as_ref().context("Method has no code")?;
    let word = *code
        .instructions
        .get(pc)
        .context("Invalid allocation offset")?;
    if word as u8 != 0x22 {
        return Ok(None);
    }
    let (width, payload) = instruction_width(&code.instructions, pc)?;
    ensure!(
        !payload && pc + width <= code.instructions.len(),
        "Truncated new-instance"
    );
    let type_index = code.instructions[pc + 1] as usize;
    let descriptor = class
        .symbols
        .types
        .get(type_index)
        .context("Invalid allocation type")?;
    ensure!(
        descriptor.starts_with('L') && descriptor.ends_with(';'),
        "Invalid new-instance class descriptor"
    );
    let register = word >> 8;
    ensure!(register < code.registers, "Invalid allocation register");
    Ok(Some(Allocation {
        method: crate::native_engine::disassembly::method_id(m),
        class: crate::native_engine::disassembly::type_name(descriptor),
        descriptor: descriptor.to_string(),
        pc,
        register,
        type_index,
        dex_entry: class.symbols.origin.get().cloned(),
        words: code.instructions[pc..pc + width].to_vec(),
        code_offset: code.offset,
    }))
}

/// Decode an invoke at an instruction boundary established by the DEX scanner.
pub fn call_site_at(class: &DexClass, m: &DexMethod, pc: usize) -> Result<Option<CallSite>> {
    let code = m.code.as_ref().context("Method has no code")?;
    let op = *code.instructions.get(pc).context("Invalid call offset")? as u8;
    let dispatch = match op {
        0x6e | 0x74 => "virtual (declared target)",
        0x6f | 0x75 => "super",
        0x70 | 0x76 => "direct",
        0x71 | 0x77 => "static",
        0x72 | 0x78 => "interface (declared target)",
        0xfa | 0xfb => "polymorphic (declared target)",
        _ => return Ok(None),
    };
    let (width, payload) = instruction_width(&code.instructions, pc)?;
    ensure!(
        !payload && pc + width <= code.instructions.len(),
        "Truncated invoke"
    );
    let index = code.instructions[pc + 1] as usize;
    let &(owner, proto, name) = class
        .symbols
        .methods
        .get(index)
        .context("Invalid method index")?;
    let (ret, args) = class
        .symbols
        .protos
        .get(proto as usize)
        .context("Invalid prototype")?;
    let owner = class
        .symbols
        .types
        .get(owner as usize)
        .context("Invalid owner")?;
    let name = class
        .symbols
        .strings
        .get(name as usize)
        .context("Invalid method name")?;
    Ok(Some(CallSite {
        caller: crate::native_engine::disassembly::method_id(m),
        callee: format!(
            "{}.{}({}){}",
            crate::native_engine::disassembly::type_name(owner),
            name,
            args.join(""),
            ret
        ),
        pc,
        dispatch,
        dex_entry: class.symbols.origin.get().cloned(),
        method_index: index,
        opcode: op,
        words: code.instructions[pc..pc + width].to_vec(),
        code_offset: code.offset,
    }))
}
