//! Exact constant-load bit patterns from original DEX instructions.
use crate::{
    engine::{NativeEngine, Project},
    native_cfg::instruction_width,
    native_dex::DexClass,
    native_engine::disassembly::{method_id, type_name},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicBool, Ordering},
};

struct Query {
    bits: u32,
    values: BTreeMap<i64, usize>,
    offset: usize,
    limit: usize,
}

fn unsigned_arg(args: &Value, key: &str, default: usize) -> Result<usize> {
    args.get(key).map_or(Ok(default), |v| {
        usize::try_from(
            v.as_u64()
                .with_context(|| format!("{key} must be a nonnegative integer"))?,
        )
        .with_context(|| format!("{key} is too large"))
    })
}

impl Query {
    fn parse(args: &Value) -> Result<Self> {
        let bits = unsigned_arg(args, "bit_width", 32)?;
        ensure!(bits == 32 || bits == 64, "bit_width must be 32 or 64");
        let input = args["values"]
            .as_array()
            .context("values must be an array of signed decimal integers or decimal/hex strings")?;
        ensure!(
            (1..=64).contains(&input.len()),
            "values must contain 1..64 entries"
        );
        let mut values = BTreeMap::new();
        for (i, v) in input.iter().enumerate() {
            values.insert(
                parse_value(v, bits as u32).with_context(|| format!("Invalid values[{i}]"))?,
                0,
            );
        }
        let limit = unsigned_arg(args, "limit", 100)?;
        ensure!((1..=500).contains(&limit), "limit must be 1..500");
        Ok(Self {
            bits: bits as u32,
            values,
            offset: unsigned_arg(args, "offset", 0)?,
            limit,
        })
    }
}

fn parse_value(v: &Value, bits: u32) -> Result<i64> {
    let min = -(1i128 << (bits - 1));
    let max = (1i128 << (bits - 1)) - 1;
    let value = if let Some(s) = v.as_str() {
        ensure!(
            !s.is_empty() && s.len() <= 32,
            "constant text must contain 1..32 bytes"
        );
        let (negative, magnitude) = s.strip_prefix('-').map_or((false, s), |s| (true, s));
        if let Some(hex) = magnitude
            .strip_prefix("0x")
            .or_else(|| magnitude.strip_prefix("0X"))
        {
            ensure!(
                !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit()),
                "invalid hexadecimal constant"
            );
            let n = i128::from_str_radix(hex, 16).context("hexadecimal constant overflow")?;
            if negative {
                -n
            } else {
                ensure!(
                    n < (1i128 << bits),
                    "hexadecimal constant exceeds bit_width"
                );
                if n > max { n - (1i128 << bits) } else { n }
            }
        } else {
            ensure!(
                !magnitude.is_empty() && magnitude.bytes().all(|b| b.is_ascii_digit()),
                "expected signed decimal or 0x hexadecimal constant"
            );
            s.parse::<i128>().context("decimal constant overflow")?
        }
    } else {
        i128::from(
            v.as_i64()
                .context("expected a signed integer or decimal/hex string")?,
        )
    };
    ensure!(
        (min..=max).contains(&value),
        "constant outside signed {bits}-bit range; use hex for an unsigned bit pattern"
    );
    Ok(value as i64)
}

pub(crate) fn validate(args: &Value) -> Result<()> {
    Query::parse(args).map(|_| ())
}

fn hex(value: i64, bits: u32) -> String {
    if bits == 32 {
        format!("0x{:08x}", value as u32)
    } else {
        format!("0x{:016x}", value as u64)
    }
}

// Called only after the shared instruction decoder has validated the width.
fn constant(w: &[u16]) -> Option<(i64, u32, u16, &'static str)> {
    let op = w[0] as u8;
    let reg = w[0] >> 8;
    let (value, bits, name) = match op {
        0x12 => return Some((((w[0] as i16) >> 12) as i64, 32, reg & 15, "const/4")),
        0x13 => (w[1] as i16 as i64, 32, "const/16"),
        0x14 => (
            (u32::from(w[1]) | (u32::from(w[2]) << 16)) as i32 as i64,
            32,
            "const",
        ),
        0x15 => ((u32::from(w[1]) << 16) as i32 as i64, 32, "const/high16"),
        0x16 => (w[1] as i16 as i64, 64, "const-wide/16"),
        0x17 => (
            (u32::from(w[1]) | (u32::from(w[2]) << 16)) as i32 as i64,
            64,
            "const-wide/32",
        ),
        0x18 => (
            (u64::from(w[1])
                | (u64::from(w[2]) << 16)
                | (u64::from(w[3]) << 32)
                | (u64::from(w[4]) << 48)) as i64,
            64,
            "const-wide",
        ),
        0x19 => ((u64::from(w[1]) << 48) as i64, 64, "const-wide/high16"),
        _ => return None,
    };
    Some((value, bits, reg, name))
}

pub fn search(
    engine: &NativeEngine,
    project: &Project,
    args: &Value,
    cancel: &AtomicBool,
) -> Result<Value> {
    scan(
        project
            .classes
            .iter()
            .map(|name| engine.dex_class(name).context("Missing DEX class")),
        Query::parse(args)?,
        cancel,
    )
}

fn scan<'a>(
    classes: impl IntoIterator<Item = Result<&'a DexClass>>,
    mut query: Query,
    cancel: &AtomicBool,
) -> Result<Value> {
    let mut items = Vec::new();
    let mut total = 0usize;
    let mut bytes = 0;
    let mut full = false;
    let mut classes_scanned = 0;
    let mut methods_scanned = 0;
    let mut digest = Sha256::new();
    digest.update(query.bits.to_le_bytes());
    for value in query.values.keys() {
        digest.update(value.to_le_bytes());
    }
    for class in classes {
        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        let class = class?;
        classes_scanned += 1;
        for method in &class.methods {
            ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
            let Some(code) = &method.code else { continue };
            methods_scanned += 1;
            let mut pc = 0;
            let mut check_at = 0;
            while pc < code.instructions.len() {
                if pc >= check_at {
                    ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
                    check_at = pc.saturating_add(256);
                }
                let (width, payload) = instruction_width(&code.instructions, pc)
                    .with_context(|| format!("Invalid DEX in {} at {pc:#x}", method_id(method)))?;
                ensure!(
                    pc + width <= code.instructions.len(),
                    "Truncated DEX in {} at {pc:#x}",
                    method_id(method)
                );
                let words = &code.instructions[pc..pc + width];
                if let Some((value, bits, register, opcode)) =
                    (!payload).then(|| constant(words)).flatten()
                {
                    ensure!(
                        u32::from(register) + if bits == 64 { 2 } else { 1 }
                            <= u32::from(code.registers),
                        "Invalid constant register in {} at {pc:#x}",
                        method_id(method)
                    );
                    if let Some(count) = query.values.get_mut(&value).filter(|_| bits == query.bits)
                    {
                        *count += 1;
                        let id = method_id(method);
                        let dex = class.symbols.origin.get();
                        digest.update([0]);
                        digest.update(dex.map_or("", String::as_str).as_bytes());
                        digest.update([0]);
                        digest.update(id.as_bytes());
                        digest.update((pc as u64).to_le_bytes());
                        digest.update(code.offset.to_le_bytes());
                        for word in words {
                            digest.update(word.to_le_bytes());
                        }
                        if total >= query.offset && !full {
                            let item = json!({"method_id":id,"class_id":type_name(&class.descriptor),"dex_entry":dex,
                                "instruction_offset":pc,"offset_unit":"16-bit DEX code units","byte_offset_in_method":pc*2,
                                "method_code_offset_bytes":code.offset,"destination_register":register,"bit_width":bits,
                                "value_decimal":value.to_string(),"value_hex":hex(value,bits),"opcode":opcode,
                                "opcode_value":words[0] as u8,"instruction_words":words,"instruction":format!("{opcode} v{register}, {value}")});
                            let size = serde_json::to_vec(&item)?.len();
                            ensure!(size <= 1_000_000, "Constant record exceeds output budget");
                            if bytes + size > 1_000_000 {
                                full = true;
                            } else {
                                bytes += size;
                                items.push(item);
                                full = items.len() == query.limit;
                            }
                        }
                        total += 1;
                    }
                }
                pc += width;
            }
        }
    }
    ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
    ensure!(query.offset <= total, "Invalid offset");
    let next = query.offset + items.len();
    let values: Vec<_> = query.values.iter().map(|(v,count)| json!({"value_decimal":v.to_string(),"value_hex":hex(*v,query.bits),"total":count})).collect();
    Ok(
        json!({"values":values,"bit_width":query.bits,"items":items,"total":total,"offset":query.offset,
        "next_offset":(next<total).then_some(next),"scan_complete":true,"classes_scanned":classes_scanned,
        "code_methods_scanned":methods_scanned,"source_hash":format!("{:x}",digest.finalize()),
        "scope":"Exact raw DEX const loads across all loaded classes, including unreachable instructions. Bits may encode integers, floats, doubles or null; no usage/type inference. Arithmetic literals, computed values, field initializers and array/switch payloads are excluded."}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_dex::{DexCode, DexSymbols};
    use std::sync::Arc;

    fn fixture(dex: &str, words: Vec<u16>) -> DexClass {
        let mut class = crate::native_dex::parse(include_bytes!("../tests/fixtures/hello.dex"))
            .unwrap()
            .classes
            .remove(0);
        class.methods.truncate(1);
        class.methods[0].code = Some(DexCode {
            registers: 4,
            ins: 0,
            outs: 0,
            tries: 0,
            try_regions: vec![],
            instructions: words,
            offset: 1234,
        });
        let symbols = DexSymbols::default();
        symbols.origin.set(dex.into()).unwrap();
        class.symbols = Arc::new(symbols);
        class
    }
    fn run(classes: &[&DexClass], args: Value) -> Result<Value> {
        scan(
            classes.iter().map(|c| Ok(*c)),
            Query::parse(&args)?,
            &AtomicBool::new(false),
        )
    }

    #[test]
    fn every_const_encoding_signed_extrema_and_width_are_exact() {
        let class = fixture(
            "classes.dex",
            vec![
                0xf112, // const/4 v1,-1
                0x0213, 0x8000, // const/16 v2,-32768
                0x0314, 0x5678, 0x1234, 0x0015, 0x8000, // minimum i32
                0x0116, 0xffff, // wide sign extension
                0x0217, 0, 0x8000, // wide/32 sign extension
                0x0018, 0xffff, 0xffff, 0xffff, 0x7fff, // max i64
                0x0019, 0x8000, // minimum i64
            ],
        );
        let narrow = run(
            &[&class],
            json!({"values":[-1,-32768,"0x12345678","0x80000000"]}),
        )
        .unwrap();
        let items = narrow["items"].as_array().unwrap();
        assert_eq!(
            items
                .iter()
                .map(|v| v["value_decimal"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["-1", "-32768", "305419896", "-2147483648"]
        );
        assert_eq!(
            items
                .iter()
                .map(|v| v["destination_register"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            [1, 2, 3, 0]
        );
        let wide = run(&[&class], json!({"bit_width":64,"values":[-1,-2147483648,"9223372036854775807","0x8000000000000000"]})).unwrap();
        assert_eq!(
            wide["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v["value_decimal"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "-1",
                "-2147483648",
                "9223372036854775807",
                "-9223372036854775808"
            ]
        );
        assert_eq!(wide["items"][0]["instruction_offset"], 8);
        assert_eq!(wide["items"][0]["value_hex"], "0xffffffffffffffff");
        assert_eq!(wide["items"][3]["opcode"], "const-wide/high16");
    }

    #[test]
    fn operands_payloads_and_arithmetic_literals_are_excluded_and_pages_are_exact() {
        let a = fixture(
            "classes.dex",
            vec![
                0x13, 0x12, // 18, not an embedded const/4
                0x1a, 0x1012, // string index resembling const/4 1
                0xd8, 0x100, // add-int/lit8 literal 1
                0x300, 2, 1, 0, 0x1012, // payload resembling const/4 1
                0x1012,
            ],
        );
        let b = fixture("classes2.dex", vec![0x0113, 1]);
        let args = json!({"values":[1,"0x1","1",99],"limit":1});
        let first = run(&[&a, &b], args.clone()).unwrap();
        assert_eq!(first["total"], 2);
        assert_eq!(first["values"].as_array().unwrap().len(), 2);
        assert_eq!(first["values"][0]["total"], 2);
        assert_eq!(first["values"][1]["total"], 0);
        assert_eq!(first["items"][0]["instruction_offset"], 11);
        let mut next = args;
        next["offset"] = first["next_offset"].clone();
        let second = run(&[&a, &b], next.clone()).unwrap();
        assert_eq!(second["items"][0]["dex_entry"], "classes2.dex");
        assert_eq!(second["items"][0]["instruction_words"], json!([0x113, 1]));
        assert_eq!(second["source_hash"], first["source_hash"]);
        assert!(second["next_offset"].is_null());
        next["offset"] = json!(2);
        assert_eq!(run(&[&a, &b], next.clone()).unwrap()["items"], json!([]));
        next["offset"] = json!(3);
        assert!(run(&[&a, &b], next).is_err());
    }

    #[test]
    fn query_syntax_precision_and_bounds_are_explicit() {
        for value in [json!(-1), json!("-1"), json!("0xFFFFFFFF"), json!("-0x1")] {
            assert_eq!(parse_value(&value, 32).unwrap(), -1);
        }
        assert_eq!(parse_value(&json!("0xFFFFFFFF"), 64).unwrap(), 4294967295);
        assert_eq!(parse_value(&json!("0xffffffffffffffff"), 64).unwrap(), -1);
        for value in [
            json!(1.5),
            json!(null),
            json!(true),
            json!(""),
            json!(" 1"),
            json!("1_0"),
            json!("0x"),
            json!("0x100000000"),
            json!(2147483648i64),
            json!("-0x80000001"),
        ] {
            assert!(parse_value(&value, 32).is_err(), "{value}");
        }
        for args in [
            json!({}),
            json!({"values":[]}),
            json!({"values":vec![1;65]}),
            json!({"values":[1],"bit_width":33}),
            json!({"values":[1],"bit_width":null}),
            json!({"values":[1],"limit":0}),
            json!({"values":[1],"limit":501}),
            json!({"values":[1],"offset":-1}),
        ] {
            assert!(Query::parse(&args).is_err(), "{args}");
        }
    }

    #[test]
    fn malformed_instructions_and_registers_fail_without_claiming_a_complete_scan() {
        for words in [
            vec![0x14, 1],
            vec![0x18, 1, 2, 3],
            vec![0x0413, 1],
            vec![0x0316, 1],
            vec![0x400],
            vec![0x300, 2, 2, 0],
        ] {
            let class = fixture("classes.dex", words);
            assert!(run(&[&class], json!({"values":[1]})).is_err());
        }
        assert!(
            scan(
                std::iter::empty(),
                Query::parse(&json!({"values":[1]})).unwrap(),
                &AtomicBool::new(true)
            )
            .is_err()
        );
        assert_eq!(run(&[], json!({"values":[1]})).unwrap()["total"], 0);
    }
}
