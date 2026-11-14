//! Encode Dalvik instructions from mnemonic + operand text.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use std::collections::HashMap;
use std::sync::OnceLock;

use crate::error::DexError;
use crate::instruction::RefKind;
use crate::opcodes::{format_length, get_opcode_entry, Format, OpcodeEntry};

/// Resolve constant-pool references while encoding.
pub trait EncodeResolve {
    fn resolve_string(&self, s: &str) -> Result<u32, DexError>;
    fn resolve_type(&self, s: &str) -> Result<u32, DexError>;
    fn resolve_field(&self, s: &str) -> Result<u32, DexError>;
    fn resolve_method(&self, s: &str) -> Result<u32, DexError>;
    fn resolve_proto(&self, s: &str) -> Result<u32, DexError>;
    fn resolve_callsite(&self, s: &str) -> Result<u32, DexError> {
        Err(DexError::invalid_owned(format!("unsupported callsite ref: {s}")))
    }
}

/// Index-only resolver (refs already look like `string@5` / numeric).
pub struct IndexResolve;

impl EncodeResolve for IndexResolve {
    fn resolve_string(&self, s: &str) -> Result<u32, DexError> {
        parse_indexed_ref(s, "string")
    }
    fn resolve_type(&self, s: &str) -> Result<u32, DexError> {
        parse_indexed_ref(s, "type").or_else(|_| parse_u32_literal(s))
    }
    fn resolve_field(&self, s: &str) -> Result<u32, DexError> {
        parse_indexed_ref(s, "field")
    }
    fn resolve_method(&self, s: &str) -> Result<u32, DexError> {
        parse_indexed_ref(s, "method")
    }
    fn resolve_proto(&self, s: &str) -> Result<u32, DexError> {
        parse_indexed_ref(s, "proto")
    }
    fn resolve_callsite(&self, s: &str) -> Result<u32, DexError> {
        parse_indexed_ref(s, "callsite")
    }
}

fn mnemonic_map() -> &'static HashMap<&'static str, u8> {
    static MAP: OnceLock<HashMap<&'static str, u8>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut m = HashMap::new();
        for op in 0u8..=255 {
            let e = get_opcode_entry(op);
            if e.format != Format::F00x && e.mnemonic != "unused" {
                // First opcode wins for duplicate mnemonics (packed aliases rare).
                m.entry(e.mnemonic).or_insert(op);
            }
        }
        m
    })
}

/// Look up opcode byte for a mnemonic.
pub fn opcode_for_mnemonic(mnemonic: &str) -> Option<u8> {
    mnemonic_map().get(mnemonic).copied()
}

/// Encode one instruction. `branch_rel_units` is the Dalvik relative offset in
/// 16-bit code units for branch formats (ignored otherwise).
pub fn encode_instruction(
    mnemonic: &str,
    operands: &str,
    resolve: &dyn EncodeResolve,
    branch_rel_units: Option<i32>,
) -> Result<Vec<u8>, DexError> {
    let op = opcode_for_mnemonic(mnemonic).ok_or_else(|| {
        DexError::invalid_owned(format!("unknown mnemonic: {mnemonic}"))
    })?;
    let entry = get_opcode_entry(op);
    let len = format_length(entry.format) as usize;
    let mut out = vec![0u8; len];
    encode_into(&mut out, op, entry, operands, resolve, branch_rel_units)?;
    Ok(out)
}

fn encode_into(
    out: &mut [u8],
    op: u8,
    entry: &OpcodeEntry,
    operands: &str,
    resolve: &dyn EncodeResolve,
    branch_rel: Option<i32>,
) -> Result<(), DexError> {
    let ops = split_operands(operands);
    match entry.format {
        Format::F10x => {
            out[0] = op;
            out[1] = 0;
        }
        Format::F10t => {
            let rel = branch_rel.or_else(|| parse_branch_token(ops.first().copied()?)).ok_or_else(|| {
                DexError::invalid("F10t missing branch")
            })?;
            let rel8: i8 = rel.try_into().map_err(|_| DexError::invalid("F10t branch out of range"))?;
            out[0] = op;
            out[1] = rel8 as u8;
        }
        Format::F11n => {
            let r = parse_reg(ops.first().copied().unwrap_or(""))?;
            let lit = parse_i64(ops.get(1).copied().unwrap_or("0"))?;
            if r > 0xf || !(-8..=7).contains(&lit) {
                return Err(DexError::invalid("F11n range"));
            }
            out[0] = op;
            out[1] = (r as u8) | (((lit as i8) as u8) << 4);
        }
        Format::F11x => {
            let r = parse_reg(ops.first().copied().unwrap_or(""))?;
            out[0] = op;
            out[1] = r as u8;
        }
        Format::F12x => {
            let a = parse_reg(ops.first().copied().unwrap_or(""))?;
            let b = parse_reg(ops.get(1).copied().unwrap_or(""))?;
            out[0] = op;
            out[1] = (a as u8) | ((b as u8) << 4);
        }
        Format::F20t => {
            let rel = branch_rel.or_else(|| parse_branch_token(ops.first().copied()?)).ok_or_else(|| {
                DexError::invalid("F20t missing branch")
            })?;
            let rel16: i16 = rel.try_into().map_err(|_| DexError::invalid("F20t branch out of range"))?;
            out[0] = op;
            out[1] = 0;
            out[2..4].copy_from_slice(&rel16.to_le_bytes());
        }
        Format::F21c | Format::F31c | Format::F20bc => {
            let aa = if entry.format == Format::F20bc {
                parse_u32_literal(ops.first().copied().unwrap_or("0"))? as u8
            } else {
                parse_reg(ops.first().copied().unwrap_or(""))? as u8
            };
            let idx = resolve_ref(entry.ref_kind, ops.get(1).copied().unwrap_or(""), resolve)?;
            out[0] = op;
            out[1] = aa;
            if entry.format == Format::F31c {
                out[2..6].copy_from_slice(&idx.to_le_bytes());
            } else {
                let idx16: u16 = idx.try_into().map_err(|_| DexError::invalid("ref index > u16"))?;
                out[2..4].copy_from_slice(&idx16.to_le_bytes());
            }
        }
        Format::F21h => {
            let aa = parse_reg(ops.first().copied().unwrap_or(""))? as u8;
            let lit = parse_i64(ops.get(1).copied().unwrap_or("0"))?;
            let stored: i16 = if op == 0x15 {
                (lit >> 16) as i16
            } else if op == 0x19 {
                (lit >> 48) as i16
            } else {
                lit as i16
            };
            out[0] = op;
            out[1] = aa;
            out[2..4].copy_from_slice(&stored.to_le_bytes());
        }
        Format::F21s => {
            let aa = parse_reg(ops.first().copied().unwrap_or(""))? as u8;
            let lit = parse_i64(ops.get(1).copied().unwrap_or("0"))? as i16;
            out[0] = op;
            out[1] = aa;
            out[2..4].copy_from_slice(&lit.to_le_bytes());
        }
        Format::F21t => {
            let aa = parse_reg(ops.first().copied().unwrap_or(""))? as u8;
            let rel = branch_rel.or_else(|| parse_branch_token(ops.get(1).copied()?)).ok_or_else(|| {
                DexError::invalid("F21t missing branch")
            })?;
            let rel16: i16 = rel.try_into().map_err(|_| DexError::invalid("F21t branch out of range"))?;
            out[0] = op;
            out[1] = aa;
            out[2..4].copy_from_slice(&rel16.to_le_bytes());
        }
        Format::F22b => {
            let a = parse_reg(ops.first().copied().unwrap_or(""))? as u8;
            let b = parse_reg(ops.get(1).copied().unwrap_or(""))? as u8;
            let c = parse_i64(ops.get(2).copied().unwrap_or("0"))? as i8;
            out[0] = op;
            out[1] = a;
            out[2] = b;
            out[3] = c as u8;
        }
        Format::F22x => {
            let a = parse_reg(ops.first().copied().unwrap_or(""))? as u8;
            let b = parse_reg(ops.get(1).copied().unwrap_or(""))? as u16;
            out[0] = op;
            out[1] = a;
            out[2..4].copy_from_slice(&b.to_le_bytes());
        }
        Format::F22c | Format::F22cs => {
            let a = parse_reg(ops.first().copied().unwrap_or(""))?;
            let b = parse_reg(ops.get(1).copied().unwrap_or(""))?;
            let idx = resolve_ref(entry.ref_kind, ops.get(2).copied().unwrap_or(""), resolve)?;
            let idx16: u16 = idx.try_into().map_err(|_| DexError::invalid("ref index > u16"))?;
            out[0] = op;
            out[1] = (a as u8) | ((b as u8) << 4);
            out[2..4].copy_from_slice(&idx16.to_le_bytes());
        }
        Format::F22s => {
            let a = parse_reg(ops.first().copied().unwrap_or(""))?;
            let b = parse_reg(ops.get(1).copied().unwrap_or(""))?;
            let lit = parse_i64(ops.get(2).copied().unwrap_or("0"))? as i16;
            out[0] = op;
            out[1] = (a as u8) | ((b as u8) << 4);
            out[2..4].copy_from_slice(&lit.to_le_bytes());
        }
        Format::F22t => {
            let a = parse_reg(ops.first().copied().unwrap_or(""))?;
            let b = parse_reg(ops.get(1).copied().unwrap_or(""))?;
            let rel = branch_rel.or_else(|| parse_branch_token(ops.get(2).copied()?)).ok_or_else(|| {
                DexError::invalid("F22t missing branch")
            })?;
            let rel16: i16 = rel.try_into().map_err(|_| DexError::invalid("F22t branch out of range"))?;
            out[0] = op;
            out[1] = (a as u8) | ((b as u8) << 4);
            out[2..4].copy_from_slice(&rel16.to_le_bytes());
        }
        Format::F23x => {
            let a = parse_reg(ops.first().copied().unwrap_or(""))? as u8;
            let b = parse_reg(ops.get(1).copied().unwrap_or(""))? as u8;
            let c = parse_reg(ops.get(2).copied().unwrap_or(""))? as u8;
            out[0] = op;
            out[1] = a;
            out[2] = b;
            out[3] = c;
        }
        Format::F30t => {
            let rel = branch_rel.or_else(|| parse_branch_token(ops.first().copied()?)).ok_or_else(|| {
                DexError::invalid("F30t missing branch")
            })?;
            out[0] = op;
            out[1] = 0;
            out[2..6].copy_from_slice(&rel.to_le_bytes());
        }
        Format::F31i => {
            let aa = parse_reg(ops.first().copied().unwrap_or(""))? as u8;
            let lit = parse_i64(ops.get(1).copied().unwrap_or("0"))? as i32;
            out[0] = op;
            out[1] = aa;
            out[2..6].copy_from_slice(&lit.to_le_bytes());
        }
        Format::F31t => {
            let aa = parse_reg(ops.first().copied().unwrap_or(""))? as u8;
            let rel = branch_rel.or_else(|| parse_branch_token(ops.get(1).copied()?)).ok_or_else(|| {
                DexError::invalid("F31t missing branch")
            })?;
            out[0] = op;
            out[1] = aa;
            out[2..6].copy_from_slice(&rel.to_le_bytes());
        }
        Format::F32x => {
            let a = parse_reg(ops.first().copied().unwrap_or(""))? as u16;
            let b = parse_reg(ops.get(1).copied().unwrap_or(""))? as u16;
            out[0] = op;
            out[1] = 0;
            out[2..4].copy_from_slice(&a.to_le_bytes());
            out[4..6].copy_from_slice(&b.to_le_bytes());
        }
        Format::F35c | Format::F35mi | Format::F35ms => {
            encode_35c(out, op, entry, &ops, resolve)?;
        }
        Format::F3rc | Format::F3rmi | Format::F3rms => {
            encode_3rc(out, op, entry, &ops, resolve)?;
        }
        Format::F51l => {
            let aa = parse_reg(ops.first().copied().unwrap_or(""))? as u8;
            let lit = parse_i64(ops.get(1).copied().unwrap_or("0"))? as u64;
            out[0] = op;
            out[1] = aa;
            out[2..10].copy_from_slice(&lit.to_le_bytes());
        }
        Format::F45cc => {
            // A|G|op BBBB F|E|D|C HHHH — similar to 35c + proto
            let joined = ops.join(" ");
            let (regs_part, rest) = if let Some(close) = joined.find('}') {
                (
                    &joined[..=close],
                    joined[close + 1..].trim().trim_start_matches(',').trim(),
                )
            } else if ops.len() >= 3 {
                // unbraced: regs..., method, proto
                let ref_tok = ops[ops.len() - 2];
                let proto_tok = ops[ops.len() - 1];
                let regs: Vec<u16> = ops[..ops.len() - 2]
                    .iter()
                    .map(|t| parse_reg(t))
                    .collect::<Result<Vec<_>, _>>()?;
                return encode_45cc_regs(out, op, &regs, ref_tok, proto_tok, resolve);
            } else {
                return Err(DexError::invalid("F45cc bad operands"));
            };
            let regs = parse_reg_list(regs_part)?;
            let mut parts = rest.split_whitespace();
            let method_tok = parts.next().unwrap_or("");
            let proto_tok = parts.next().unwrap_or("");
            encode_45cc_regs(out, op, &regs, method_tok, proto_tok, resolve)?;
        }
        Format::F4rcc => {
            let joined = ops.join(" ");
            // v0 ... vN method proto  OR  v0 .. vN method proto
            let (start, count, method_tok, proto_tok) = parse_4rcc(&joined)?;
            let method_idx = resolve.resolve_method(&method_tok)?;
            let proto_idx = resolve.resolve_proto(&proto_tok)?;
            let m16: u16 = method_idx.try_into().map_err(|_| DexError::invalid("method idx"))?;
            let p16: u16 = proto_idx.try_into().map_err(|_| DexError::invalid("proto idx"))?;
            out[0] = op;
            out[1] = count;
            out[2..4].copy_from_slice(&m16.to_le_bytes());
            out[4..6].copy_from_slice(&start.to_le_bytes());
            out[6..8].copy_from_slice(&p16.to_le_bytes());
        }
        Format::F40sc => {
            // op BBBBBBBB AAAA — AAAA is a literal/const index, BBBB is ref
            let aa = parse_u32_literal(ops.first().copied().unwrap_or("0"))? as u16;
            let ref_tok = ops.get(1).copied().unwrap_or("");
            let idx = resolve_ref(entry.ref_kind, ref_tok, resolve)?;
            out[0] = op;
            out[1] = 0;
            out[2..6].copy_from_slice(&idx.to_le_bytes());
            out[6..8].copy_from_slice(&aa.to_le_bytes());
        }
        Format::F41c => {
            // op BBBBBBBB AAAA — vAAAA, ref
            let aa = parse_reg(ops.first().copied().unwrap_or("v0"))?;
            let ref_tok = ops.get(1).copied().unwrap_or("");
            let idx = resolve_ref(entry.ref_kind, ref_tok, resolve)?;
            out[0] = op;
            out[1] = 0;
            out[2..6].copy_from_slice(&idx.to_le_bytes());
            out[6..8].copy_from_slice(&aa.to_le_bytes());
        }
        Format::F52c => {
            // op CCCCCCCC AAAA BBBB — vAAAA, vBBBB, ref
            let a = parse_reg(ops.first().copied().unwrap_or("v0"))?;
            let b = parse_reg(ops.get(1).copied().unwrap_or("v0"))?;
            let ref_tok = ops.get(2).copied().unwrap_or("");
            let idx = resolve_ref(entry.ref_kind, ref_tok, resolve)?;
            out[0] = op;
            out[1] = 0;
            out[2..6].copy_from_slice(&idx.to_le_bytes());
            out[6..8].copy_from_slice(&a.to_le_bytes());
            out[8..10].copy_from_slice(&b.to_le_bytes());
        }
        Format::F5rc => {
            // op BBBBBBBB AAAA CCCC — count AAAA, start CCCC, ref BBBB
            let joined = ops.join(" ");
            let (start, count, ref_tok) = parse_range_invoke(&joined)?;
            let idx = resolve_ref(entry.ref_kind, &ref_tok, resolve)?;
            out[0] = op;
            out[1] = 0;
            out[2..6].copy_from_slice(&idx.to_le_bytes());
            out[6..8].copy_from_slice(&(count as u16).to_le_bytes());
            out[8..10].copy_from_slice(&start.to_le_bytes());
        }
        Format::F00x => return Err(DexError::invalid("cannot encode unused opcode")),
    }
    Ok(())
}

fn encode_35c(
    out: &mut [u8],
    op: u8,
    entry: &OpcodeEntry,
    ops: &[&str],
    resolve: &dyn EncodeResolve,
) -> Result<(), DexError> {
    // Decoder form: "v0, v1, method@3" or "method@3"
    // Also accept "{v0, v1}, LFoo;->bar()V"
    let (regs, ref_tok) = if ops.is_empty() {
        (Vec::new(), String::new())
    } else if ops[0].starts_with('{') {
        let joined = ops.join(" ");
        if let Some(close) = joined.find('}') {
            let regs_part = &joined[..=close];
            let rest = joined[close + 1..].trim().trim_start_matches(',').trim();
            (parse_reg_list(regs_part)?, rest.to_string())
        } else {
            return Err(DexError::invalid("unclosed register list"));
        }
    } else {
        let ref_tok = ops.last().copied().unwrap_or("").to_string();
        let regs = ops[..ops.len().saturating_sub(1)]
            .iter()
            .map(|t| parse_reg(t))
            .collect::<Result<Vec<_>, _>>()?;
        (regs, ref_tok)
    };
    if regs.len() > 5 {
        return Err(DexError::invalid("F35c max 5 registers"));
    }
    let idx = resolve_ref(entry.ref_kind, &ref_tok, resolve)?;
    let idx16: u16 = idx.try_into().map_err(|_| DexError::invalid("ref index > u16"))?;
    let a = regs.len() as u8;
    let g = *regs.get(4).unwrap_or(&0) as u8;
    let c = *regs.first().unwrap_or(&0) as u8;
    let d = *regs.get(1).unwrap_or(&0) as u8;
    let e = *regs.get(2).unwrap_or(&0) as u8;
    let f = *regs.get(3).unwrap_or(&0) as u8;
    out[0] = op;
    out[1] = (a << 4) | (g & 0x0f);
    out[2..4].copy_from_slice(&idx16.to_le_bytes());
    out[4] = (c & 0x0f) | ((d & 0x0f) << 4);
    out[5] = (e & 0x0f) | ((f & 0x0f) << 4);
    Ok(())
}

fn encode_45cc_regs(
    out: &mut [u8],
    op: u8,
    regs: &[u16],
    method_tok: &str,
    proto_tok: &str,
    resolve: &dyn EncodeResolve,
) -> Result<(), DexError> {
    if regs.len() > 5 {
        return Err(DexError::invalid("F45cc max 5 registers"));
    }
    let method_idx = resolve.resolve_method(method_tok)?;
    let proto_idx = resolve.resolve_proto(proto_tok)?;
    let m16: u16 = method_idx
        .try_into()
        .map_err(|_| DexError::invalid("method idx"))?;
    let p16: u16 = proto_idx
        .try_into()
        .map_err(|_| DexError::invalid("proto idx"))?;
    let a = regs.len() as u8;
    let g = *regs.get(4).unwrap_or(&0) as u8;
    let c = *regs.first().unwrap_or(&0) as u8;
    let d = *regs.get(1).unwrap_or(&0) as u8;
    let e = *regs.get(2).unwrap_or(&0) as u8;
    let f = *regs.get(3).unwrap_or(&0) as u8;
    out[0] = op;
    out[1] = (a << 4) | (g & 0x0f);
    out[2..4].copy_from_slice(&m16.to_le_bytes());
    out[4] = (c & 0x0f) | ((d & 0x0f) << 4);
    out[5] = (e & 0x0f) | ((f & 0x0f) << 4);
    out[6..8].copy_from_slice(&p16.to_le_bytes());
    Ok(())
}

fn parse_4rcc(s: &str) -> Result<(u16, u8, String, String), DexError> {
    // "v0 ... v3, method@1, proto@2" or space-separated
    let s = s.trim();
    if let Some((left, proto)) = s.rsplit_once(',') {
        let proto = proto.trim().to_string();
        if let Some((regs, method)) = left.rsplit_once(',') {
            let (start, count) = parse_reg_range(regs.trim())?;
            return Ok((start, count, method.trim().to_string(), proto));
        }
    }
    // fallback: split whitespace — last two are method, proto
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() >= 3 {
        let proto = parts[parts.len() - 1].to_string();
        let method = parts[parts.len() - 2].to_string();
        let regs = parts[..parts.len() - 2].join(" ");
        let (start, count) = parse_reg_range(&regs)?;
        return Ok((start, count, method, proto));
    }
    Err(DexError::invalid_owned(format!("bad F4rcc: {s}")))
}

fn encode_3rc(
    out: &mut [u8],
    op: u8,
    entry: &OpcodeEntry,
    ops: &[&str],
    resolve: &dyn EncodeResolve,
) -> Result<(), DexError> {
    // Decoder form: "v0 ... v3, method@1" or "v0, method@1"
    let (start, count, ref_tok) = if ops.len() >= 2 {
        let regs = ops[..ops.len() - 1].join(" ");
        let ref_tok = ops.last().copied().unwrap_or("").to_string();
        let (start, count) = parse_reg_range(&regs)?;
        (start, count, ref_tok)
    } else {
        parse_range_invoke(&ops.join(" "))?
    };
    let idx = resolve_ref(entry.ref_kind, &ref_tok, resolve)?;
    let idx16: u16 = idx.try_into().map_err(|_| DexError::invalid("ref index > u16"))?;
    out[0] = op;
    out[1] = count;
    out[2..4].copy_from_slice(&idx16.to_le_bytes());
    out[4..6].copy_from_slice(&start.to_le_bytes());
    Ok(())
}

fn parse_reg_range(regs: &str) -> Result<(u16, u8), DexError> {
    let regs = regs.trim();
    if let Some((a, b)) = regs.split_once("...") {
        let start = parse_reg(a.trim())? as u16;
        let end = parse_reg(b.trim())? as u16;
        if end < start {
            return Err(DexError::invalid("bad register range"));
        }
        return Ok((start, (end - start + 1) as u8));
    }
    if let Some((a, b)) = regs.split_once("..") {
        let start = parse_reg(a.trim())? as u16;
        let end = parse_reg(b.trim())? as u16;
        return Ok((start, (end - start + 1) as u8));
    }
    Ok((parse_reg(regs)? as u16, 1))
}

fn parse_range_invoke(s: &str) -> Result<(u16, u8, String), DexError> {
    let s = s.trim();
    if let Some((regs, rest)) = s.rsplit_once(',') {
        let ref_tok = rest.trim().to_string();
        let regs = regs.trim();
        if let Some((a, b)) = regs.split_once("...") {
            let start = parse_reg(a.trim())? as u16;
            let end = parse_reg(b.trim())? as u16;
            if end < start {
                return Err(DexError::invalid("bad register range"));
            }
            let count = (end - start + 1) as u8;
            return Ok((start, count, ref_tok));
        }
        if let Some((a, b)) = regs.split_once("..") {
            let start = parse_reg(a.trim())? as u16;
            let end = parse_reg(b.trim())? as u16;
            let count = (end - start + 1) as u8;
            return Ok((start, count, ref_tok));
        }
        let start = parse_reg(regs)? as u16;
        return Ok((start, 1, ref_tok));
    }
    Err(DexError::invalid_owned(format!("bad range invoke: {s}")))
}

fn parse_reg_list(s: &str) -> Result<Vec<u16>, DexError> {
    let s = s.trim().trim_start_matches('{').trim_end_matches('}');
    if s.is_empty() {
        return Ok(Vec::new());
    }
    s.split(',')
        .map(|t| parse_reg(t.trim()))
        .collect()
}

fn resolve_ref(kind: RefKind, tok: &str, resolve: &dyn EncodeResolve) -> Result<u32, DexError> {
    let tok = tok.trim().trim_end_matches(',');
    match kind {
        RefKind::String => {
            // Pass the full quoted token through when present. Never use
            // trim_matches('"') — that eats escaped trailing quotes in
            // literals like `"\""` / `" \""` and corrupts the lookup key.
            resolve.resolve_string(tok)
        }
        RefKind::Type => resolve.resolve_type(tok),
        RefKind::Field => resolve.resolve_field(tok),
        RefKind::Method => resolve.resolve_method(tok),
        RefKind::MethodProto => resolve.resolve_proto(tok),
        RefKind::CallSite => resolve.resolve_callsite(tok),
        RefKind::None | RefKind::Varies => parse_u32_literal(tok),
    }
}

fn split_operands(operands: &str) -> Vec<&str> {
    // Keep braced groups and double-quoted strings together; otherwise split on commas.
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escape = false;
    let bytes = operands.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if in_str {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => depth -= 1,
            b',' if depth == 0 => {
                let tok = operands[start..i].trim();
                if !tok.is_empty() {
                    out.push(tok);
                }
                start = i + 1;
            }
            _ => {}
        }
    }
    let tok = operands[start..].trim();
    if !tok.is_empty() {
        out.push(tok);
    }
    out
}

fn parse_reg(tok: &str) -> Result<u16, DexError> {
    let tok = tok.trim().trim_end_matches(',');
    if let Some(rest) = tok.strip_prefix('v').or_else(|| tok.strip_prefix('p')) {
        rest.parse()
            .map_err(|_| DexError::invalid_owned(format!("bad register: {tok}")))
    } else {
        Err(DexError::invalid_owned(format!("expected register: {tok}")))
    }
}

fn parse_i64(tok: &str) -> Result<i64, DexError> {
    let tok = tok.trim().trim_end_matches(',');
    if let Some(hex) = tok.strip_prefix("0x").or_else(|| tok.strip_prefix("0X")) {
        return i64::from_str_radix(hex.trim_end_matches('h'), 16)
            .map_err(|_| DexError::invalid_owned(format!("bad literal: {tok}")));
    }
    if tok.ends_with('h') {
        let body = tok.trim_end_matches('h');
        let (neg, digits) = if let Some(rest) = body.strip_prefix('+') {
            (false, rest)
        } else if let Some(rest) = body.strip_prefix('-') {
            (true, rest)
        } else {
            (false, body)
        };
        let v = i64::from_str_radix(digits, 16)
            .map_err(|_| DexError::invalid_owned(format!("bad literal: {tok}")))?;
        return Ok(if neg { -v } else { v });
    }
    tok.parse()
        .map_err(|_| DexError::invalid_owned(format!("bad literal: {tok}")))
}

fn parse_u32_literal(tok: &str) -> Result<u32, DexError> {
    parse_i64(tok).map(|v| v as u32)
}

fn parse_branch_token(tok: &str) -> Option<i32> {
    let tok = tok.trim();
    if tok.starts_with(':') {
        return None; // label — caller must supply branch_rel
    }
    parse_i64(tok).ok().map(|v| v as i32)
}

fn parse_indexed_ref(tok: &str, prefix: &str) -> Result<u32, DexError> {
    let p = format!("{prefix}@");
    if let Some(rest) = tok.strip_prefix(&p) {
        rest.parse()
            .map_err(|_| DexError::invalid_owned(format!("bad {prefix} ref: {tok}")))
    } else {
        Err(DexError::invalid_owned(format!("expected {prefix}@N, got {tok}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_return_void() {
        let bytes = encode_instruction("return-void", "", &IndexResolve, None).unwrap();
        assert_eq!(bytes, vec![0x0e, 0x00]);
    }

    #[test]
    fn encode_const4() {
        let bytes = encode_instruction("const/4", "v0, 1", &IndexResolve, None).unwrap();
        assert_eq!(bytes[0], 0x12);
        assert_eq!(bytes[1] & 0x0f, 0);
        assert_eq!((bytes[1] >> 4) as i8, 1);
    }

    #[test]
    fn encode_goto() {
        let bytes = encode_instruction("goto", "+02h", &IndexResolve, Some(2)).unwrap();
        assert_eq!(bytes, vec![0x28, 0x02]);
    }
}
