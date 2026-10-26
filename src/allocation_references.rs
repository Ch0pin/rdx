//! Exact new-instance references; operands are resolved within their owning DEX.
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
    collections::{BTreeSet, HashSet},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

pub(crate) fn resolve_target(
    engine: &NativeEngine,
    project: &Project,
    requested: &str,
    cancel: &AtomicBool,
) -> Result<String> {
    ensure!(
        !requested.is_empty() && requested.len() <= 4096,
        "Class selector must contain 1..4096 bytes"
    );
    let normalized = requested
        .strip_prefix('L')
        .and_then(|s| s.strip_suffix(';'))
        .map(|s| s.replace('/', "."));
    let name = normalized.as_deref().unwrap_or(requested);
    let short = normalized.is_none() && !name.contains('.');
    let mut candidates = BTreeSet::new();
    let mut seen = HashSet::new();
    for class in &project.classes {
        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        if class == name {
            return Ok(name.into());
        }
        let class = engine.dex_class(class).context("Missing DEX class")?;
        if !seen.insert(Arc::as_ptr(&class.symbols) as usize) {
            continue;
        }
        for (i, descriptor) in class.symbols.types.iter().enumerate() {
            if i % 256 == 0 {
                ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
            }
            if !descriptor.starts_with('L') || !descriptor.ends_with(';') {
                continue;
            }
            let id = type_name(descriptor);
            if id == name {
                return Ok(id);
            }
            if short && id.rsplit('.').next() == Some(name) {
                candidates.insert(id);
            }
        }
    }
    ensure!(
        !candidates.is_empty(),
        "SYMBOL_NOT_FOUND: {requested}; use a loaded or referenced class FQCN/DEX descriptor"
    );
    ensure!(
        candidates.len() == 1,
        "AMBIGUOUS_CLASS: {requested}; candidates={:?}",
        candidates.iter().take(20).collect::<Vec<_>>()
    );
    Ok(candidates.into_iter().next().unwrap())
}

pub fn search(
    engine: &NativeEngine,
    project: &Project,
    class_id: &str,
    offset: usize,
    limit: usize,
    cancel: &AtomicBool,
) -> Result<Value> {
    search_filtered(
        engine,
        project,
        Some(class_id),
        None,
        None,
        offset,
        limit,
        cancel,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn search_filtered(
    engine: &NativeEngine,
    project: &Project,
    class_id: Option<&str>,
    allocating_class: Option<&str>,
    allocating_method: Option<&str>,
    offset: usize,
    limit: usize,
    cancel: &AtomicBool,
) -> Result<Value> {
    let mut result = scan_filtered(
        project
            .classes
            .iter()
            .filter(|name| allocating_class.is_none_or(|owner| owner == name.as_str()))
            .map(|name| engine.dex_class(name).context("Missing DEX class")),
        class_id,
        allocating_method,
        offset,
        limit,
        cancel,
    )?;
    // Declaration availability describes the project, independently of the
    // allocating-side filter. A reverse query selects no single allocated type.
    result["definition_available"] = json!(class_id.map(|id| engine.dex_class(id).is_some()));
    Ok(result)
}

#[cfg(test)]
fn scan<'a>(
    classes: impl IntoIterator<Item = Result<&'a DexClass>>,
    class_id: &str,
    offset: usize,
    limit: usize,
    cancel: &AtomicBool,
) -> Result<Value> {
    scan_filtered(classes, Some(class_id), None, offset, limit, cancel)
}

fn scan_filtered<'a>(
    classes: impl IntoIterator<Item = Result<&'a DexClass>>,
    class_id: Option<&str>,
    allocating_method: Option<&str>,
    offset: usize,
    limit: usize,
    cancel: &AtomicBool,
) -> Result<Value> {
    ensure!((1..=500).contains(&limit), "Limit must be 1..500");
    let descriptor = class_id.map(|id| format!("L{};", id.replace('.', "/")));
    let mut items = Vec::new();
    let mut total = 0usize;
    let mut bytes = 0;
    let mut page_full = false;
    let mut classes_scanned = 0;
    let mut methods_scanned = 0;
    let mut declaration_available = false;
    let mut digest = Sha256::new();
    digest.update(descriptor.as_deref().unwrap_or("*").as_bytes());
    for class in classes {
        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        let class = class?;
        declaration_available |= Some(class.descriptor.as_ref()) == descriptor.as_deref();
        classes_scanned += 1;
        for method in &class.methods {
            if allocating_method.is_some_and(|id| method_id(method) != id) {
                continue;
            }
            ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
            let Some(code) = &method.code else { continue };
            methods_scanned += 1;
            let mut pc = 0;
            let mut next_check = 0;
            while pc < code.instructions.len() {
                if pc >= next_check {
                    ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
                    next_check = pc.saturating_add(256);
                }
                let (width, payload) = instruction_width(&code.instructions, pc)
                    .with_context(|| format!("Invalid DEX in {} at {pc:#x}", method_id(method)))?;
                ensure!(
                    pc + width <= code.instructions.len(),
                    "Truncated DEX in {} at {pc:#x}",
                    method_id(method)
                );
                if !payload && code.instructions[pc] as u8 == 0x22 {
                    let index = code.instructions[pc + 1] as usize;
                    let ty = class.symbols.types.get(index).with_context(|| {
                        format!(
                            "Invalid allocation type in {} at {pc:#x}",
                            method_id(method)
                        )
                    })?;
                    let register = code.instructions[pc] >> 8;
                    ensure!(
                        register < code.registers,
                        "Invalid allocation register in {} at {pc:#x}",
                        method_id(method)
                    );
                    ensure!(
                        ty.starts_with('L') && ty.ends_with(';'),
                        "Invalid new-instance class descriptor in {} at {pc:#x}",
                        method_id(method)
                    );
                    if descriptor.as_deref().is_none_or(|d| ty.as_ref() == d) {
                        let id = method_id(method);
                        let dex = class.symbols.origin.get();
                        digest.update([0]);
                        digest.update(dex.map_or("", String::as_str).as_bytes());
                        digest.update([0]);
                        digest.update(id.as_bytes());
                        digest.update((pc as u64).to_le_bytes());
                        digest.update(code.offset.to_le_bytes());
                        digest.update(register.to_le_bytes());
                        digest.update((index as u64).to_le_bytes());
                        if total >= offset && !page_full {
                            let item = json!({"method_id":id,"class_id":type_name(&class.descriptor),"allocated_class_id":type_name(ty),
                                "allocated_descriptor":ty.as_ref(),"dex_entry":dex,"call_site_offset":pc,"allocation_offset":pc,
                                "offset_unit":"16-bit DEX code units","byte_offset_in_method":pc*2,"method_code_offset_bytes":code.offset,
                                "destination_register":register,"type_index":index,"opcode":"new-instance","opcode_value":0x22,
                                "instruction_words":&code.instructions[pc..pc+width],"instruction":format!("new-instance v{register}, {ty}")});
                            let size = serde_json::to_vec(&item)?.len();
                            ensure!(size <= 1_000_000, "Allocation record exceeds output budget");
                            if bytes + size > 1_000_000 {
                                page_full = true;
                            } else {
                                bytes += size;
                                items.push(item);
                                page_full = items.len() == limit;
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
    ensure!(offset <= total, "Invalid offset");
    let next = offset + items.len();
    Ok(
        json!({"allocated_class_id":class_id,"allocated_descriptor":descriptor,"definition_available":declaration_available,
        "items":items,"total":total,"offset":offset,"next_offset":(next<total).then_some(next),"scan_complete":true,
        "source_hash":format!("{:x}",digest.finalize()),"classes_scanned":classes_scanned,"code_methods_scanned":methods_scanned,
        "scope":"Exact DEX new-instance operands in the selected loaded project classes and methods. Includes unreachable instructions; does not prove successful construction or runtime reachability. Arrays, reflection and native/runtime allocations are excluded."}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_dex::{DexCode, DexSymbols};
    fn fixture(name: &str, ty: &str, words: Vec<u16>) -> DexClass {
        let mut c = crate::native_dex::parse(include_bytes!("../tests/fixtures/hello.dex"))
            .unwrap()
            .classes
            .remove(0);
        c.descriptor = format!("L{name};").into();
        c.methods.truncate(1);
        c.methods[0].declaring_type = c.descriptor.clone();
        c.methods[0].code = Some(DexCode {
            registers: 2,
            ins: 0,
            outs: 0,
            tries: 0,
            try_regions: vec![],
            instructions: words,
            offset: 1234,
        });
        let symbols = DexSymbols {
            types: vec![ty.into()],
            ..Default::default()
        };
        symbols.origin.set(format!("{name}.dex")).unwrap();
        c.symbols = Arc::new(symbols);
        c
    }
    #[test]
    fn reverse_lookup_returns_distinct_types_and_method_filter() {
        let a = fixture("CallerA", "Ltarget/One;", vec![0x22, 0, 0x22, 0]);
        let b = fixture("CallerB", "Ltarget/Two;", vec![0x22, 0]);
        let cancel = AtomicBool::new(false);
        let result = scan_filtered([Ok(&a), Ok(&b)], None, None, 0, 1, &cancel).unwrap();
        assert_eq!(result["total"], 3);
        assert_eq!(result["next_offset"], 1);
        assert_eq!(result["items"][0]["allocated_class_id"], "target.One");
        let id = method_id(&b.methods[0]);
        let result = scan_filtered([Ok(&a), Ok(&b)], None, Some(&id), 0, 10, &cancel).unwrap();
        assert_eq!(result["total"], 1);
        assert_eq!(result["items"][0]["allocated_class_id"], "target.Two");
        let result = scan_filtered(
            [Ok(&a), Ok(&b)],
            Some("target.One"),
            Some(&id),
            0,
            10,
            &cancel,
        )
        .unwrap();
        assert_eq!(result["total"], 0);
    }
    #[test]
    fn dex_local_types_payloads_and_nonallocation_operands_are_distinguished() {
        let a = fixture(
            "CallerA",
            "Ltarget/Type;",
            vec![
                0x122, 0, 0x1a, 0x22, 0x26, 4, 0, 0, 0x300, 2, 2, 0, 0x22, 0, 0x22, 0,
            ],
        );
        let b = fixture("CallerB", "Lother/Type;", vec![0x22, 0]);
        let r = scan(
            [Ok(&a), Ok(&b)],
            "target.Type",
            0,
            10,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(r["total"], 2);
        assert_eq!(r["items"][0]["dex_entry"], "CallerA.dex");
        assert_eq!(r["items"][0]["destination_register"], 1);
        assert_eq!(r["items"][1]["allocation_offset"], 14);
        assert_eq!(r["items"][0]["instruction_words"], json!([0x122, 0]));
        let second = scan(
            [Ok(&a), Ok(&b)],
            "target.Type",
            1,
            1,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(second["items"][0], r["items"][1]);
        assert_eq!(second["source_hash"], r["source_hash"]);
        assert_eq!(second["next_offset"], Value::Null);
    }
    #[test]
    fn invalid_operands_and_truncation_fail_with_method_context() {
        for words in [vec![0x22], vec![0x22, 7], vec![0x222, 0]] {
            let c = fixture("Caller", "Ltarget/Type;", words);
            assert!(scan([Ok(&c)], "target.Type", 0, 10, &AtomicBool::new(false)).is_err());
        }
        let c = fixture("Caller", "[I", vec![0x22, 0]);
        assert!(scan([Ok(&c)], "target.Type", 0, 10, &AtomicBool::new(false)).is_err());
    }
    #[test]
    fn pages_cancellation_and_empty_queries_are_explicit() {
        let c = fixture("Caller", "Ltarget/Type;", vec![0x22, 0, 0x22, 0]);
        let r = scan([Ok(&c)], "target.Type", 0, 1, &AtomicBool::new(false)).unwrap();
        assert_eq!(r["next_offset"], 1);
        assert_eq!(r["total"], 2);
        assert!(scan([Ok(&c)], "target.Type", 0, 1, &AtomicBool::new(true)).is_err());
        assert!(scan([Ok(&c)], "target.Type", 3, 1, &AtomicBool::new(false)).is_err());
        let r = scan([Ok(&c)], "missing.Type", 0, 1, &AtomicBool::new(false)).unwrap();
        assert_eq!(r["total"], 0);
        assert_eq!(r["scan_complete"], true);
    }
}
