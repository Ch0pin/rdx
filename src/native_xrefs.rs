//! Method call edges from DEX instructions, independent of Java reconstruction.
use crate::{native_cfg::instruction_width, native_dex::DexClass};
use anyhow::{Context, Result, ensure};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, PartialEq, Eq)]
pub struct CallSite {
    pub caller: String,
    pub callee: String,
    pub pc: usize,
    pub dispatch: &'static str,
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
        let Some(code) = &m.code else { continue };
        let mut pc = 0;
        while pc < code.instructions.len() {
            if pc.is_multiple_of(256) && cancel.load(Ordering::Relaxed) {
                anyhow::bail!("Call search cancelled");
            }
            let (width, payload) = instruction_width(&code.instructions, pc)?;
            ensure!(
                pc + width <= code.instructions.len(),
                "Truncated instruction"
            );
            let op = code.instructions[pc] as u8;
            let dispatch = match op {
                0x6e | 0x74 => "virtual (declared target)",
                0x6f | 0x75 => "super",
                0x70 | 0x76 => "direct",
                0x71 | 0x77 => "static",
                0x72 | 0x78 => "interface (declared target)",
                0xfa | 0xfb => "polymorphic (declared target)",
                _ => "",
            };
            if !payload && !dispatch.is_empty() {
                let index = *code
                    .instructions
                    .get(pc + 1)
                    .context("Missing method operand")? as usize;
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
                sites.push(CallSite {
                    caller: caller.clone(),
                    callee: format!(
                        "{}.{}({}){}",
                        crate::native_engine::disassembly::type_name(owner),
                        name,
                        args.join(""),
                        ret
                    ),
                    pc,
                    dispatch,
                });
            }
            pc += width;
        }
    }
    Ok(sites)
}
