//! DEX `debug_info_item` parser (parameter and local variable names).

use std::collections::HashMap;

use crate::bounds::{ensure_count_fits, vec_with_capacity};
use crate::error::{DexError, Result};
use crate::leb128::{read_sleb128, read_uleb128, read_uleb128p1};

/// DBG_* opcodes from the Android DEX format.
const DBG_END_SEQUENCE: u8 = 0x00;
const DBG_ADVANCE_PC: u8 = 0x01;
const DBG_ADVANCE_LINE: u8 = 0x02;
const DBG_START_LOCAL: u8 = 0x03;
const DBG_START_LOCAL_EXTENDED: u8 = 0x04;
const DBG_END_LOCAL: u8 = 0x05;
const DBG_RESTART_LOCAL: u8 = 0x06;
const DBG_SET_PROLOGUE_END: u8 = 0x07;
const DBG_SET_EPILOGUE_BEGIN: u8 = 0x08;
const DBG_SET_FILE: u8 = 0x09;

/// One `DBG_START_LOCAL` / restart entry (register may host several over the method).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DebugLocal {
    pub name: String,
    /// DEX type descriptor when present (e.g. `F`, `D`, `Ljava/lang/String;`).
    pub type_desc: Option<String>,
}

/// Parsed debug information for one method.
#[derive(Clone, Debug, Default)]
pub struct DebugInfo {
    pub line_start: u32,
    /// Parameter names in declaration order (`None` = unnamed).
    pub parameter_names: Vec<Option<String>>,
    /// Best known local name per register (from START_LOCAL / RESTART_LOCAL).
    pub register_names: HashMap<u32, String>,
    /// Best known local type descriptor per register (e.g. `F`, `D`, `Ljava/lang/String;`).
    pub register_types: HashMap<u32, String>,
    /// All locals that lived on each register, in debug order (handles D8 reuse).
    pub register_locals: HashMap<u32, Vec<DebugLocal>>,
}

impl DebugInfo {
    /// Name for register `reg`, if known.
    pub fn name_for_reg(&self, reg: u32) -> Option<&str> {
        self.register_names.get(&reg).map(|s| s.as_str())
    }

    /// Type descriptor for register `reg`, if known.
    pub fn type_for_reg(&self, reg: u32) -> Option<&str> {
        self.register_types.get(&reg).map(|s| s.as_str())
    }

    /// Every named local that lived on `reg`, in encounter order.
    pub fn locals_for_reg(&self, reg: u32) -> &[DebugLocal] {
        self.register_locals
            .get(&reg)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }
}

/// Parse `debug_info_item` at `debug_info_off` within `data`.
///
/// `get_string` resolves string_id indices to UTF-8 names.
/// Callers should skip invoking this when `debug_info_off == 0` (no debug item).
pub fn parse_debug_info(
    data: &[u8],
    debug_info_off: u32,
    get_string: &dyn Fn(u32) -> Result<String>,
) -> Result<DebugInfo> {
    parse_debug_info_with_types(data, debug_info_off, get_string, None)
}

/// Like [`parse_debug_info`], but also resolves local type descriptors when `get_type` is set.
pub fn parse_debug_info_with_types(
    data: &[u8],
    debug_info_off: u32,
    get_string: &dyn Fn(u32) -> Result<String>,
    get_type: Option<&dyn Fn(u32) -> Result<String>>,
) -> Result<DebugInfo> {
    let mut off = debug_info_off as usize;
    if off >= data.len() {
        return Err(DexError::Truncated("debug_info_off".into()));
    }

    let (line_start, n) =
        read_uleb128(data, off).ok_or(DexError::Truncated("debug line_start".into()))?;
    off += n;
    let (parameters_size, n) =
        read_uleb128(data, off).ok_or(DexError::Truncated("debug parameters_size".into()))?;
    off += n;

    let parameters_size = parameters_size as usize;
    ensure_count_fits(parameters_size, data.len().saturating_sub(off), "debug parameters")?;
    let mut parameter_names = vec_with_capacity(parameters_size, "debug parameters")?;
    for _ in 0..parameters_size {
        let (name_idx_p1, n) =
            read_uleb128p1(data, off).ok_or(DexError::Truncated("debug param name".into()))?;
        off += n;
        let name = if name_idx_p1 < 0 {
            None
        } else {
            get_string(name_idx_p1 as u32).ok()
        };
        parameter_names.push(name);
    }

    let mut address: u32 = 0;
    let mut register_names: HashMap<u32, String> = HashMap::new();
    let mut register_types: HashMap<u32, String> = HashMap::new();
    let mut register_locals: HashMap<u32, Vec<DebugLocal>> = HashMap::new();
    // Remember last ended local so RESTART_LOCAL can revive it.
    let mut last_local: HashMap<u32, String> = HashMap::new();
    let mut last_type: HashMap<u32, String> = HashMap::new();

    let push_local = |reg: u32,
                      name: Option<String>,
                      ty: Option<String>,
                      register_names: &mut HashMap<u32, String>,
                      register_types: &mut HashMap<u32, String>,
                      register_locals: &mut HashMap<u32, Vec<DebugLocal>>,
                      last_local: &mut HashMap<u32, String>,
                      last_type: &mut HashMap<u32, String>| {
        if let Some(ref n) = name {
            if !n.is_empty() {
                last_local.insert(reg, n.clone());
                register_names.insert(reg, n.clone());
            }
        }
        if let Some(ref t) = ty {
            if !t.is_empty() {
                last_type.insert(reg, t.clone());
                register_types.insert(reg, t.clone());
            }
        }
        if let Some(n) = name {
            if !n.is_empty() {
                register_locals.entry(reg).or_default().push(DebugLocal {
                    name: n,
                    type_desc: ty.filter(|t| !t.is_empty()),
                });
            }
        }
    };

    loop {
        if off >= data.len() {
            break;
        }
        let opcode = data[off];
        off += 1;
        match opcode {
            DBG_END_SEQUENCE => break,
            DBG_ADVANCE_PC => {
                let (diff, n) =
                    read_uleb128(data, off).ok_or(DexError::Truncated("DBG_ADVANCE_PC".into()))?;
                off += n;
                address = address.saturating_add(diff);
            }
            DBG_ADVANCE_LINE => {
                let (_diff, n) =
                    read_sleb128(data, off).ok_or(DexError::Truncated("DBG_ADVANCE_LINE".into()))?;
                off += n;
            }
            DBG_START_LOCAL | DBG_START_LOCAL_EXTENDED => {
                let (reg, n) =
                    read_uleb128(data, off).ok_or(DexError::Truncated("DBG_START_LOCAL reg".into()))?;
                off += n;
                let (name_idx_p1, n) = read_uleb128p1(data, off)
                    .ok_or(DexError::Truncated("DBG_START_LOCAL name".into()))?;
                off += n;
                let (type_idx_p1, n) = read_uleb128p1(data, off)
                    .ok_or(DexError::Truncated("DBG_START_LOCAL type".into()))?;
                off += n;
                if opcode == DBG_START_LOCAL_EXTENDED {
                    let (_sig, n) = read_uleb128p1(data, off)
                        .ok_or(DexError::Truncated("DBG_START_LOCAL_EXTENDED sig".into()))?;
                    off += n;
                }
                let name = if name_idx_p1 >= 0 {
                    get_string(name_idx_p1 as u32).ok()
                } else {
                    None
                };
                let ty = if type_idx_p1 >= 0 {
                    get_type.and_then(|gt| gt(type_idx_p1 as u32).ok())
                } else {
                    None
                };
                push_local(
                    reg,
                    name,
                    ty,
                    &mut register_names,
                    &mut register_types,
                    &mut register_locals,
                    &mut last_local,
                    &mut last_type,
                );
                let _ = address;
            }
            DBG_END_LOCAL => {
                let (reg, n) =
                    read_uleb128(data, off).ok_or(DexError::Truncated("DBG_END_LOCAL".into()))?;
                off += n;
                // Keep name in register_names for naming; track for restart.
                if let Some(name) = register_names.get(&reg).cloned() {
                    last_local.insert(reg, name);
                }
                if let Some(ty) = register_types.get(&reg).cloned() {
                    last_type.insert(reg, ty);
                }
            }
            DBG_RESTART_LOCAL => {
                let (reg, n) = read_uleb128(data, off)
                    .ok_or(DexError::Truncated("DBG_RESTART_LOCAL".into()))?;
                off += n;
                let name = last_local.get(&reg).cloned();
                let ty = last_type.get(&reg).cloned();
                if name.is_some() || ty.is_some() {
                    push_local(
                        reg,
                        name,
                        ty,
                        &mut register_names,
                        &mut register_types,
                        &mut register_locals,
                        &mut last_local,
                        &mut last_type,
                    );
                }
            }
            DBG_SET_PROLOGUE_END | DBG_SET_EPILOGUE_BEGIN => {}
            DBG_SET_FILE => {
                let (_name_idx_p1, n) =
                    read_uleb128p1(data, off).ok_or(DexError::Truncated("DBG_SET_FILE".into()))?;
                off += n;
            }
            _ if opcode >= 0x0a => {
                // Special opcodes adjust line + address; we only care about names.
                let adjusted = opcode - 0x0a;
                let addr_diff = (adjusted / 15) as u32;
                address = address.saturating_add(addr_diff);
            }
            _ => {
                // Unknown opcode — stop safely.
                break;
            }
        }
    }

    Ok(DebugInfo {
        line_start,
        parameter_names,
        register_names,
        register_types,
        register_locals,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_end_sequence_only() {
        // line_start=1, parameters_size=0, END_SEQUENCE
        let dbg = parse_debug_info(&[0x01, 0x00, 0x00], 0, &|_| Ok(String::new())).unwrap();
        assert!(dbg.parameter_names.is_empty());
        assert!(dbg.register_names.is_empty());
    }

    #[test]
    fn parse_params_and_start_local() {
        // line_start=1, parameters_size=1, param name string_idx=0,
        // START_LOCAL reg=1 name=0 type=-1, END_SEQUENCE
        let mut data = Vec::new();
        data.push(0x01); // line_start
        data.push(0x01); // parameters_size
        data.push(0x01); // uleb128p1 → string 0
        data.push(DBG_START_LOCAL);
        data.push(0x01); // reg 1
        data.push(0x01); // name string 0
        data.push(0x00); // type no-index (uleb128p1 of 0 → -1)
        data.push(DBG_END_SEQUENCE);

        let get_string = |idx: u32| -> Result<String> {
            match idx {
                0 => Ok("count".into()),
                _ => Err(DexError::Parse(format!("string {idx}"))),
            }
        };
        let dbg = parse_debug_info(&data, 0, &get_string).unwrap();
        assert_eq!(dbg.parameter_names, vec![Some("count".into())]);
        assert_eq!(dbg.name_for_reg(1), Some("count"));
    }

    #[test]
    fn parse_start_local_with_type() {
        let mut data = Vec::new();
        data.push(0x01);
        data.push(0x00);
        data.push(DBG_START_LOCAL);
        data.push(0x02); // reg 2
        data.push(0x01); // name string 0
        data.push(0x01); // type_idx 0
        data.push(DBG_END_SEQUENCE);
        let get_string = |idx: u32| -> Result<String> {
            match idx {
                0 => Ok("fff".into()),
                _ => Err(DexError::Parse(format!("string {idx}"))),
            }
        };
        let get_type = |idx: u32| -> Result<String> {
            match idx {
                0 => Ok("F".into()),
                _ => Err(DexError::Parse(format!("type {idx}"))),
            }
        };
        let dbg =
            parse_debug_info_with_types(&data, 0, &get_string, Some(&get_type)).unwrap();
        assert_eq!(dbg.name_for_reg(2), Some("fff"));
        assert_eq!(dbg.type_for_reg(2), Some("F"));
    }
}
