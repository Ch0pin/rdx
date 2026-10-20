//! Cancellable field access scans over original DEX operands, without source rendering.
use crate::{
    engine::{NativeEngine, Project},
    native_dex::DexClass,
    native_engine::disassembly::{method_id, type_name, width},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    sync::atomic::{AtomicBool, Ordering},
};

const OPCODES: [&str; 28] = [
    "iget",
    "iget-wide",
    "iget-object",
    "iget-boolean",
    "iget-byte",
    "iget-char",
    "iget-short",
    "iput",
    "iput-wide",
    "iput-object",
    "iput-boolean",
    "iput-byte",
    "iput-char",
    "iput-short",
    "sget",
    "sget-wide",
    "sget-object",
    "sget-boolean",
    "sget-byte",
    "sget-char",
    "sget-short",
    "sput",
    "sput-wide",
    "sput-object",
    "sput-boolean",
    "sput-byte",
    "sput-char",
    "sput-short",
];

fn access(op: u8) -> Option<&'static str> {
    match op {
        0x52..=0x58 | 0x60..=0x66 => Some("read"),
        0x59..=0x5f | 0x67..=0x6d => Some("write"),
        _ => None,
    }
}

// Field resolution follows declared fields, superinterfaces, then superclass.
// Missing hierarchy nodes and conflicting interface declarations stay unresolved.
fn resolve_field(
    owner: &str,
    name: &str,
    ty: &str,
    classes: &HashMap<&str, &DexClass>,
    visiting: &mut HashSet<String>,
    memo: &mut HashMap<String, Option<String>>,
    cancel: &AtomicBool,
) -> Result<Option<String>> {
    ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
    if let Some(result) = memo.get(owner) {
        return Ok(result.clone());
    }
    if visiting.len() >= 256 || !visiting.insert(owner.to_owned()) {
        return Ok(None);
    }
    let result = (|| {
        let class = classes.get(owner)?;
        if class
            .fields
            .iter()
            .any(|f| f.name.as_ref() == name && f.field_type.as_ref() == ty)
        {
            return Some(format!("{}.{name}:{ty}", type_name(owner)));
        }
        let mut interface_match = None;
        for interface in &class.interfaces {
            let resolved =
                resolve_field(interface, name, ty, classes, visiting, memo, cancel).ok()??;
            // Empty string means a fully known hierarchy with no matching field.
            if !resolved.is_empty() {
                if interface_match
                    .as_ref()
                    .is_some_and(|previous| previous != &resolved)
                {
                    return None;
                }
                interface_match = Some(resolved);
            }
        }
        if interface_match.is_some() {
            return interface_match;
        }
        if let Some(parent) = &class.superclass {
            return resolve_field(parent, name, ty, classes, visiting, memo, cancel).ok()?;
        }
        Some(String::new())
    })();
    visiting.remove(owner);
    ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
    memo.insert(owner.to_owned(), result.clone());
    Ok(result)
}

pub(crate) fn search(
    engine: &NativeEngine,
    project: &Project,
    field_id: &str,
    access_filter: &str,
    offset: usize,
    count: usize,
    cancel: &AtomicBool,
) -> Result<Value> {
    let classes = project.classes.iter().map(|name| {
        engine
            .dex_class(name)
            .with_context(|| format!("DEX class missing: {name}"))
    });
    scan(classes, field_id, access_filter, offset, count, cancel)
}

fn scan<'a>(
    classes: impl IntoIterator<Item = Result<&'a DexClass>>,
    field_id: &str,
    access_filter: &str,
    offset: usize,
    count: usize,
    cancel: &AtomicBool,
) -> Result<Value> {
    ensure!(
        matches!(access_filter, "all" | "read" | "write"),
        "access must be all, read, or write"
    );
    ensure!(
        (1..=1000).contains(&count),
        "count must be between 1 and 1000"
    );
    ensure!(
        field_id.rsplit_once(':').is_some_and(|(name, ty)| name
            .rsplit_once('.')
            .is_some_and(|(owner, name)| !owner.is_empty() && !name.is_empty())
            && !ty.is_empty()),
        "field_id must be a canonical owner.name:descriptor ID from get_fields_of_class"
    );
    let mut loaded = Vec::new();
    for class in classes {
        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        loaded.push(class?);
    }
    let classes = loaded;
    let hierarchy: HashMap<_, _> = classes
        .iter()
        .map(|class| (class.descriptor.as_ref(), *class))
        .collect();
    let (query_name, query_type) = field_id.rsplit_once(':').unwrap();
    let (query_owner, query_name) = query_name.rsplit_once('.').unwrap();
    let query_declarations: usize = classes
        .iter()
        .filter(|class| type_name(&class.descriptor) == query_owner)
        .map(|class| {
            class
                .fields
                .iter()
                .filter(|field| {
                    field.declaring_type == class.descriptor
                        && field.name.as_ref() == query_name
                        && field.field_type.as_ref() == query_type
                })
                .count()
        })
        .sum();
    ensure!(
        query_declarations <= 1,
        "Ambiguous field identity: multiple loaded declarations match field_id"
    );
    let declaration_resolved = query_declarations == 1;
    // All resolutions in this scan use the same field name/type, so owner is a sufficient memo key.
    let mut resolution_memo = HashMap::new();
    let mut matches_by_symbols = HashMap::new();
    let mut unresolved_accesses = 0usize;
    let mut unresolved_all_accesses = 0usize;
    let mut reference_hash = Sha256::new();
    reference_hash.update(field_id.as_bytes());
    reference_hash.update([0]);
    reference_hash.update(access_filter.as_bytes());
    let mut unresolved_operands = Vec::new();
    let mut reads = 0usize;
    let mut writes = 0usize;
    let mut references = Vec::new();
    let mut total = 0usize;
    let mut classes_scanned = 0usize;
    let mut methods_scanned = 0usize;
    let mut code_methods_scanned = 0usize;
    for class in classes {
        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");

        let symbol_key = std::sync::Arc::as_ptr(&class.symbols) as usize;
        let matching = if let Some(matching) = matches_by_symbols.get(&symbol_key) {
            matching
        } else {
            let mut matching = Vec::with_capacity(class.symbols.fields.len());
            for (index, &(owner, ty, name)) in class.symbols.fields.iter().enumerate() {
                if index % 256 == 0 {
                    ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
                }
                let owner = class
                    .symbols
                    .types
                    .get(owner as usize)
                    .context("Invalid DEX field owner")?;
                let ty = class
                    .symbols
                    .types
                    .get(ty as usize)
                    .context("Invalid DEX field type")?;
                let name = class
                    .symbols
                    .strings
                    .get(name as usize)
                    .context("Invalid DEX field name")?;
                let operand_id = format!("{}.{name}:{ty}", type_name(owner));
                let state = if operand_id == field_id {
                    1u8
                } else if name == query_name && ty.as_ref() == query_type {
                    match resolve_field(
                        owner,
                        name,
                        ty,
                        &hierarchy,
                        &mut HashSet::new(),
                        &mut resolution_memo,
                        cancel,
                    )? {
                        Some(resolved) => u8::from(resolved == field_id),
                        None => 2,
                    }
                } else {
                    0
                };
                matching.push(state);
            }
            matches_by_symbols.insert(symbol_key, matching);
            &matches_by_symbols[&symbol_key]
        };
        for method in &class.methods {
            ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
            methods_scanned += 1;
            let Some(code) = &method.code else { continue };
            code_methods_scanned += 1;
            let words = &code.instructions;
            let mut pc = 0;
            while pc < words.len() {
                ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
                let size = width(words, pc).with_context(|| {
                    format!(
                        "Invalid DEX instruction in {} at code-unit {pc}",
                        method_id(method)
                    )
                })?;
                let op = words[pc] as u8;
                if let Some(kind) = access(op) {
                    let matches = *matching
                        .get(words[pc + 1] as usize)
                        .context("Invalid DEX field operand")?;
                    if matches == 2 {
                        unresolved_all_accesses += 1;
                    }
                    if matches == 2 && (access_filter == "all" || access_filter == kind) {
                        unresolved_accesses += 1;
                        if unresolved_operands.len() < 20 {
                            unresolved_operands.push(json!({"method_id":method_id(method),"code_unit_offset":pc,
                                "symbolic_field_id":crate::native_engine::disassembly::symbol(&class.symbols, 'f', words[pc + 1] as usize),
                                "reason":"missing, cyclic, too-deep, or conflicting loaded hierarchy"}));
                        }
                    }
                    if matches == 1 {
                        if kind == "read" {
                            reads += 1;
                        } else {
                            writes += 1;
                        }
                    }
                    if matches == 1 && (access_filter == "all" || access_filter == kind) {
                        let caller = method_id(method);
                        reference_hash.update([0]);
                        reference_hash.update(caller.as_bytes());
                        reference_hash.update((pc as u64).to_le_bytes());
                        reference_hash.update([op]);
                        let symbolic = crate::native_engine::disassembly::symbol(
                            &class.symbols,
                            'f',
                            words[pc + 1] as usize,
                        )
                        .context("Invalid DEX field operand")?;
                        reference_hash.update(symbolic.as_bytes());
                        if total >= offset && references.len() < count {
                            references.push(
                                json!({"field_id":field_id,"method_id":method_id(method),
                                "class":type_name(&class.descriptor),"code_unit_offset":pc,
                                "opcode":OPCODES[(op - 0x52) as usize],"opcode_value":op,"access":kind,
                                "offset_unit":"16-bit DEX code units","byte_offset_in_method":pc * 2,
                                "symbolic_field_id":crate::native_engine::disassembly::symbol(&class.symbols, 'f', words[pc + 1] as usize)}),
                            );
                        }
                        total += 1;
                    }
                }
                pc += size;
            }
        }
        classes_scanned += 1;
    }
    ensure!(offset <= total, "Invalid offset");
    let source_hash = format!("{:x}", reference_hash.finalize());
    let returned = references.len();
    Ok(
        json!({"field_id":field_id,"access":access_filter,"items":references,"source_hash":source_hash,
        "total":total,"offset":offset,"count":returned,
        "has_more":offset.saturating_add(returned) < total,
        "next_offset":if offset.saturating_add(returned) < total {Some(offset + returned)} else {None},
        "scope":"DEX field instructions in all loaded project classes; inherited owners resolved through loaded declarations and hierarchy",
        "complete":unresolved_accesses == 0 && declaration_resolved,"scan_complete":true,
        "identity_scope":if declaration_resolved {"loaded unique field declaration"} else {"symbolic operand identity; declaration unresolved"},
        "declaration_resolved":declaration_resolved,
        "read_count":reads,"write_count":writes,"counts_complete":declaration_resolved && unresolved_all_accesses == 0,"unresolved_all_access_count":unresolved_all_accesses,"unresolved_access_count":unresolved_accesses,
        "unresolved_examples":unresolved_operands,"offset_unit":"16-bit DEX code units","classes_scanned":classes_scanned,"methods_scanned":methods_scanned,
        "code_methods_scanned":code_methods_scanned,
        "limitations":["Reflection, native code, and runtime-generated accesses require runtime analysis."]}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_dex::{DexCode, DexSymbols};
    use std::sync::Arc;

    fn fixture(words: Vec<u16>) -> DexClass {
        let mut class = crate::native_dex::parse(include_bytes!("../tests/fixtures/hello.dex"))
            .unwrap()
            .classes
            .remove(0);
        class.symbols = Arc::new(DexSymbols {
            types: vec!["Lsample/Owner;".into(), "I".into()],
            strings: vec!["value".into()],
            fields: vec![(0, 1, 0)],
            ..Default::default()
        });
        class.methods.truncate(1);
        class.methods[0].code = Some(DexCode {
            registers: 2,
            ins: 0,
            outs: 0,
            tries: 0,
            try_regions: vec![],
            instructions: words,
            offset: 0,
        });
        class
    }
    fn query(class: &DexClass, filter: &str, offset: usize, count: usize) -> Result<Value> {
        scan(
            [Ok(class)],
            "sample.Owner.value:I",
            filter,
            offset,
            count,
            &AtomicBool::new(false),
        )
    }
    #[test]
    fn inherited_owner_alias_resolves_and_shadowing_wins() {
        let mut parent = fixture(vec![]);
        parent.descriptor = "Lsample/Owner;".into();
        parent.superclass = None;
        parent.interfaces.clear();
        parent.fields = vec![crate::native_dex::DexField {
            declaring_type: parent.descriptor.clone(),
            name: "value".into(),
            field_type: "I".into(),
            access_flags: 0,
            is_static: false,
        }];
        let mut child = fixture(vec![0x52, 0, 0x67, 0]);
        child.descriptor = "Lsample/Child;".into();
        child.superclass = Some(parent.descriptor.clone());
        child.interfaces.clear();
        child.fields.clear();
        Arc::get_mut(&mut child.symbols).unwrap().types[0] = child.descriptor.clone();
        let run = |child: &DexClass| {
            scan(
                [Ok(&parent), Ok(child)],
                "sample.Owner.value:I",
                "all",
                0,
                10,
                &AtomicBool::new(false),
            )
            .unwrap()
        };
        let resolved = run(&child);
        assert_eq!(resolved["total"], 2);
        assert_eq!(resolved["read_count"], 1);
        assert_eq!(resolved["write_count"], 1);
        assert_eq!(resolved["complete"], true);
        assert_eq!(
            resolved["items"][0]["symbolic_field_id"],
            "sample.Child.value:I"
        );
        child.fields.push(crate::native_dex::DexField {
            declaring_type: child.descriptor.clone(),
            name: "value".into(),
            field_type: "I".into(),
            access_flags: 0,
            is_static: false,
        });
        assert_eq!(run(&child)["total"], 0);
        child.fields.clear();
        child.superclass = Some("Lmissing/Parent;".into());
        let unresolved = run(&child);
        assert_eq!(unresolved["complete"], false);
        assert_eq!(unresolved["unresolved_access_count"], 2);
        let filtered = scan(
            [Ok(&parent), Ok(&child)],
            "sample.Owner.value:I",
            "read",
            0,
            10,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(filtered["unresolved_access_count"], 1);
        assert_eq!(filtered["unresolved_all_access_count"], 2);
        assert_eq!(filtered["counts_complete"], false);
    }

    #[test]
    fn conflicting_interfaces_are_unresolved() {
        let mut owner = fixture(vec![]);
        owner.descriptor = "Lsample/Owner;".into();
        owner.superclass = None;
        owner.interfaces.clear();
        owner.fields = vec![crate::native_dex::DexField {
            declaring_type: owner.descriptor.clone(),
            name: "value".into(),
            field_type: "I".into(),
            access_flags: 0,
            is_static: true,
        }];
        let mut other = fixture(vec![]);
        other.descriptor = "Lsample/Other;".into();
        other.superclass = None;
        other.interfaces.clear();
        other.fields = vec![crate::native_dex::DexField {
            declaring_type: other.descriptor.clone(),
            name: "value".into(),
            field_type: "I".into(),
            access_flags: 0,
            is_static: true,
        }];
        let mut child = fixture(vec![0x60, 0]);
        child.descriptor = "Lsample/Child;".into();
        child.fields.clear();
        child.superclass = None;
        child.interfaces = vec![owner.descriptor.clone(), other.descriptor.clone()];
        Arc::get_mut(&mut child.symbols).unwrap().types[0] = child.descriptor.clone();
        let value = scan(
            [Ok(&owner), Ok(&other), Ok(&child)],
            "sample.Owner.value:I",
            "all",
            0,
            10,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(value["complete"], false);
        assert_eq!(value["unresolved_access_count"], 1);
    }

    #[test]
    fn symbolic_only_query_stays_incomplete_and_resolution_is_memoized() {
        let mut class = fixture(vec![0x52, 0]);
        class.fields.clear();
        let value = query(&class, "all", 0, 10).unwrap();
        assert_eq!(value["total"], 1);
        assert_eq!(value["declaration_resolved"], false);
        assert_eq!(value["complete"], false);
        class.descriptor = "Lsample/Leaf;".into();
        class.superclass = None;
        class.interfaces.clear();
        let hierarchy = HashMap::from([(class.descriptor.as_ref(), &class)]);
        let mut memo = HashMap::new();
        assert_eq!(
            resolve_field(
                &class.descriptor,
                "value",
                "I",
                &hierarchy,
                &mut HashSet::new(),
                &mut memo,
                &AtomicBool::new(false)
            )
            .unwrap(),
            Some(String::new())
        );
        assert_eq!(memo.len(), 1);
        assert_eq!(
            resolve_field(
                &class.descriptor,
                "value",
                "I",
                &HashMap::new(),
                &mut HashSet::new(),
                &mut memo,
                &AtomicBool::new(false)
            )
            .unwrap(),
            Some(String::new())
        );
    }

    #[test]
    fn all_field_opcodes_and_paging() {
        let class = fixture((0x52..=0x6d).flat_map(|op| [op, 0]).collect());
        let reads = query(&class, "read", 0, 100).unwrap();
        let writes = query(&class, "write", 0, 100).unwrap();
        assert_eq!(reads["total"], 14);
        assert_eq!(writes["total"], 14);
        let page = query(&class, "all", 13, 2).unwrap();
        assert_eq!(page["total"], 28);
        assert_eq!(page["items"][0]["opcode"], "iput-short");
        assert_eq!(page["items"][0]["code_unit_offset"], 26);
        assert_eq!(page["items"][1]["access"], "read");
        assert_eq!(page["next_offset"], 15);
        assert_eq!(
            page["source_hash"],
            query(&class, "all", 0, 100).unwrap()["source_hash"]
        );
        assert_ne!(page["source_hash"], reads["source_hash"]);
    }
    #[test]
    fn malformed_and_cancelled_scans_fail_without_partial_results() {
        assert!(query(&fixture(vec![0x52]), "all", 0, 10).is_err());
        assert!(query(&fixture(vec![0x52, 1]), "all", 0, 10).is_err());
        let class = fixture(vec![0x52, 0]);
        assert!(
            scan(
                [Ok(&class)],
                "sample.Owner.value:I",
                "all",
                0,
                10,
                &AtomicBool::new(true)
            )
            .is_err()
        );
        assert!(query(&class, "invalid", 0, 10).is_err());
    }
    #[test]
    fn symbolic_owner_is_exact_and_empty_page_preserves_total() {
        let class = fixture(vec![0x52, 0]);
        let value = scan(
            [Ok(&class)],
            "sample.Child.value:I",
            "all",
            0,
            10,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(value["total"], 0);
        assert!(query(&class, "all", 100, 10).is_err());
        let value = query(&class, "all", 1, 10).unwrap();
        assert_eq!(value["total"], 1);
        assert_eq!(value["items"], json!([]));
        assert_eq!(value["has_more"], false);
    }
}
