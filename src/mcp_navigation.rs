//! Source-symbol navigation and conservative call-site enrichment.
use crate::{
    engine::{DecompiledCode, MethodSourceFormat, NativeEngine, Project},
    native_ir::DecodedMethod,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    sync::atomic::{AtomicBool, Ordering},
};
fn check(cancel: &AtomicBool) -> Result<()> {
    ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
    Ok(())
}

/// Only static, straight-line, one-invoke argument forwarding with an unchanged
/// return is certified. Package names are never used to infer semantic ownership.
pub(super) fn forwarder(engine: &NativeEngine, id: &str) -> Option<Value> {
    let owner = crate::call_graph::owner(id)?;
    let class = engine.dex_class(owner)?;
    forwarder_in_class(class, id)
}

/// Small helpers may have guards or side effects, so show their exact calls
/// separately from proven forwarding. No semantic owner is inferred.
pub(super) fn cross_namespace_calls(engine: &NativeEngine, id: &str) -> Option<Value> {
    let owner = crate::call_graph::owner(id)?;
    let class = engine.dex_class(owner)?;
    let method = crate::method_lookup::resolve(class, id).ok()?;
    let code = method.code.as_ref()?;
    if code.instructions.len() > 64 || method.access_flags & 8 == 0 {
        return None;
    }
    let calls =
        crate::native_xrefs::method_call_sites(class, method, &AtomicBool::new(false)).ok()?;
    let calls: Vec<_> = calls.into_iter().filter(|site| {
        crate::call_graph::owner(&site.callee).and_then(|s|s.rsplit_once('.').map(|p|p.0)) != owner.rsplit_once('.').map(|p|p.0)
    }).map(|site|json!({"target_id":site.callee,"call_site_offset":site.pc,"invoke_opcode":site.opcode})).collect();
    if calls.is_empty() {
        return None;
    }
    Some(
        json!({"status":"exact_cross_namespace_calls","method_id":id,"calls":calls,"evidence":"Direct DEX invokes in a small static helper. Calls may be conditional and the helper may perform additional work; this is not a forwarding or ownership claim."}),
    )
}
fn forwarder_in_class(class: &crate::native_dex::DexClass, id: &str) -> Option<Value> {
    let owner = crate::call_graph::owner(id)?;
    let method = crate::method_lookup::resolve(class, id).ok()?;
    let code = method.code.as_ref()?;
    if method.access_flags & 8 == 0
        || code.instructions.len() > 64
        || code.registers > 256
        || !code.try_regions.is_empty()
    {
        return None;
    }
    let ir = DecodedMethod::decode(code).ok()?;
    let calls = crate::native_calls::BoundCalls::bind(code, &ir, &class.symbols).ok()?;
    if calls.calls.len() != 1 {
        return None;
    }
    let call = &calls.calls[0];
    if call.kind != crate::native_calls::CallKind::Static || call.return_type != method.return_type
    {
        return None;
    }
    let site = crate::native_xrefs::call_site_at(class, method, call.pc).ok()??;
    if site.callee == id {
        return None;
    }
    let mut origins = vec![None; code.registers as usize];
    let start = code.registers.checked_sub(code.ins)? as usize;
    for (i, value) in origins.iter_mut().enumerate().skip(start) {
        *value = Some(i - start);
    }
    let mut arguments = vec![];
    let mut invoked = false;
    let mut returned = false;
    let mut pending = false;
    for insn in &ir.instructions {
        if returned {
            return None;
        }
        match insn.opcode {
            0x00 => {}
            0x01..=0x09 => {
                let from = insn.reads.first()?;
                let to = insn.writes.first()?;
                let width = to.kind.word_count();
                let values = origins
                    .get(from.register as usize..from.register as usize + width)?
                    .to_vec();
                origins
                    .get_mut(to.register as usize..to.register as usize + width)?
                    .copy_from_slice(&values);
                pending = false;
            }
            0x71 | 0x77 if !invoked => {
                for arg in &call.arguments {
                    let width = if matches!(arg.descriptor.as_ref(), "J" | "D") {
                        2
                    } else {
                        1
                    };
                    for word in 0..width {
                        arguments.push(origins.get(arg.register as usize + word).copied()??);
                    }
                }
                let used: HashSet<_> = arguments.iter().copied().collect();
                if used.len() != code.ins as usize || used.iter().any(|&p| p >= code.ins as usize) {
                    return None;
                }
                invoked = true;
                pending = true;
            }
            0x0a..=0x0c if pending => {
                for out in &insn.writes {
                    for word in 0..out.kind.word_count() {
                        origins[out.register as usize + word] = Some(usize::MAX - word);
                    }
                }
                pending = false;
            }
            0x0e if invoked && method.return_type.as_ref() == "V" => returned = true,
            0x0f..=0x11 if invoked && method.return_type.as_ref() != "V" => {
                let expected = match method.return_type.as_ref() {
                    "J" | "D" => 0x10,
                    s if s.starts_with('L') || s.starts_with('[') => 0x11,
                    _ => 0x0f,
                };
                if insn.opcode != expected {
                    return None;
                }
                let read = insn.reads.first()?;
                for word in 0..read.kind.word_count() {
                    if origins
                        .get(read.register as usize + word)
                        .copied()
                        .flatten()
                        != Some(usize::MAX - word)
                    {
                        return None;
                    }
                }
                returned = true;
            }
            _ => return None,
        }
    }
    if !returned {
        return None;
    }
    let delegate_owner = crate::call_graph::owner(&site.callee)?;
    Some(
        json!({"status":"verified_static_forwarder","method_id":id,"delegates_to":site.callee,"delegate_call_site_offset":site.pc,
        "parameter_word_mapping":arguments,"cross_namespace":owner.rsplit_once('.').map(|p|p.0)!=delegate_owner.rsplit_once('.').map(|p|p.0),
        "evidence":"One static invoke; entry arguments forwarded through moves; unchanged return; no branches, handlers, fields or allocations. DEX ownership is preserved."}),
    )
}
fn position(source: &str, start: usize) -> (usize, usize) {
    let mut line = 1;
    let mut column = 1;
    for c in source.chars().take(start) {
        if c == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    (line, column)
}
fn offset_at(source: &str, line: usize, column: usize) -> Result<usize> {
    let mut current_line = 1;
    let mut current_column = 1;
    for (offset, c) in source.chars().enumerate() {
        if (current_line, current_column) == (line, column) {
            return Ok(offset);
        }
        if c == '\n' {
            current_line += 1;
            current_column = 1;
        } else {
            current_column += 1;
        }
    }
    anyhow::bail!("Source position is outside the document")
}
pub(super) fn resolve(
    engine: &mut NativeEngine,
    _project: &Project,
    args: &Value,
    cancel: &AtomicBool,
) -> Result<Value> {
    check(cancel)?;
    let class = args["class_id"].as_str().context("Missing class_id")?;
    let (code, coverage, document) = if let Some(method) = args["method_id"].as_str() {
        let id = crate::method_lookup::normalize(method)?;
        ensure!(
            crate::method_lookup::owner(&id)? == class,
            "method_id belongs to another class"
        );
        let report = engine.method_source(
            &id,
            MethodSourceFormat::parse(args["format"].as_str().unwrap_or("auto"))?,
            cancel,
        )?;
        let coverage = json!({"status":if report.format=="java" {"decompiled"} else if args["format"]=="dex" {"dex-requested"} else {"dex-fallback"},"fallback_reason":report.fallback_reason});
        (
            report.code,
            coverage,
            json!({"scope":"method","method_id":id}),
        )
    } else {
        ensure!(
            args.get("format").is_none(),
            "format applies only with method_id"
        );
        let (code, coverage) = engine.decompile_class_source_report(class, cancel)?;
        (code, coverage, json!({"scope":"class","class_id":class}))
    };
    ensure!(
        args["source_hash"].as_str() == Some(code.source_hash.as_str()),
        "SOURCE_CHANGED: refresh source and retry with its source_hash"
    );
    let offset = args["source_offset"].as_u64().map(|n| n as usize);
    let line = args["line"].as_u64().map(|n| n as usize);
    ensure!(
        offset.is_some() != line.is_some(),
        "Supply source_offset or line (optionally column)"
    );
    ensure!(
        args.get("column").is_none() || line.is_some(),
        "column requires line"
    );
    let point = if let Some(offset) = offset {
        Some(offset)
    } else if let Some(column) = args["column"].as_u64() {
        Some(offset_at(&code.source, line.unwrap(), column as usize)?)
    } else {
        None
    };
    let mut line_starts = vec![0usize];
    for (i, c) in code.source.chars().enumerate() {
        if c == '\n' {
            line_starts.push(i + 1);
        }
    }
    let mut references = vec![];
    for link in &code.links {
        check(cancel)?;
        let row = line_starts.partition_point(|&start| start <= link.start);
        let column = link.start - line_starts[row - 1] + 1;
        if point.map_or_else(|| Some(row) == line, |p| link.start <= p && p < link.end) {
            let kind = if link.label.contains('(') {
                "method"
            } else if link.label.contains(':') {
                "field_or_resource"
            } else {
                "class"
            };
            references.push(json!({"target_id":link.label,"kind":kind,"start":link.start,"end":link.end,"line":row,"column":column,
                "display_text":code.source.chars().skip(link.start).take(link.end-link.start).collect::<String>(),
                "forwarder":forwarder(engine,&link.label),"cross_namespace_calls":cross_namespace_calls(engine,&link.label)}));
        }
    }
    ensure!(
        !references.is_empty(),
        "SYMBOL_NOT_FOUND_AT_POSITION: choose a linked reference token; line alone returns all references on that line"
    );
    ensure!(
        references.len() <= 500,
        "Too many references on line; provide column"
    );
    Ok(
        json!({"document":document,"source_hash":code.source_hash,"offset_unit":"unicode_scalar","line_column_base":1,"references":references,"coverage":coverage}),
    )
}

fn mapped_location(code: &DecompiledCode, start: usize) -> Value {
    let (line, column) = position(&code.source, start);
    json!({"source_hash":code.source_hash,"source_offset":start,"line":line,"column":column,"document_scope":"method","offset_unit":"unicode_scalar","line_column_base":1})
}
pub(super) fn enrich(
    engine: &mut NativeEngine,
    result: &mut Value,
    args: &Value,
    cancel: &AtomicBool,
) -> Result<()> {
    let source = args["include_source_locations"] == true;
    let forwards = args["include_forwarders"] == true;
    if !source && !forwards {
        return Ok(());
    }
    let sites = result["call_sites"]
        .as_array_mut()
        .context("Source locations and forwarders require projection=call_sites")?;
    let mut groups = BTreeMap::<String, Vec<usize>>::new();
    for (i, site) in sites.iter_mut().enumerate() {
        if forwards {
            site["cross_namespace_calls"] = cross_namespace_calls(
                engine,
                site["callee_method_id"]
                    .as_str()
                    .context("Missing callee")?,
            )
            .unwrap_or(Value::Null);
            site["forwarder"] = forwarder(
                engine,
                site["callee_method_id"]
                    .as_str()
                    .context("Missing callee")?,
            )
            .unwrap_or(Value::Null);
        }
        if source {
            groups
                .entry(
                    site["caller_method_id"]
                        .as_str()
                        .context("Missing caller")?
                        .into(),
                )
                .or_default()
                .push(i);
        }
    }
    for (n, (id, indices)) in groups.into_iter().enumerate() {
        check(cancel)?;
        if n >= 20 {
            for i in indices {
                sites[i]["source_mapping"] = json!({"status":"not_attempted","reason":"20-method enrichment budget; request a smaller page","reconstruction_status":"not_attempted"});
            }
            continue;
        }
        let report = engine.method_source(&id, MethodSourceFormat::Auto, cancel);
        check(cancel)?;
        match report {
            Err(e) => {
                for i in indices {
                    sites[i]["source_mapping"] = json!({"status":"unavailable","reason":format!("{e:#}"),"reconstruction_status":"unavailable"});
                }
            }
            Ok(report) => {
                let owner = crate::method_lookup::owner(&id)?;
                let class = engine.dex_class(owner).context("Missing caller")?;
                let method = crate::method_lookup::resolve(class, &id)?;
                let raw = crate::native_xrefs::method_call_sites(class, method, cancel)?;
                for i in indices {
                    let target = sites[i]["raw_callee_method_id"]
                        .as_str()
                        .context("Missing raw target")?;
                    let pc = sites[i]["call_site_offset"]
                        .as_u64()
                        .context("Missing call offset")? as usize;
                    let mut mapping = json!({"status":"ambiguous","reconstruction_status":if report.format=="java" {"decompiled"} else {"dex-fallback"},"fallback_reason":report.fallback_reason,"method_id":id,"source_hash":report.code.source_hash,"document_scope":"method"});
                    if report.format == "rdx-dex" {
                        let marker = format!("{pc:04x}:");
                        let mut offset = 0;
                        for line in report.code.source.lines() {
                            if line.trim_start().starts_with(&marker) {
                                mapping["status"] = json!("exact_dex_line");
                                mapping["location"] = mapped_location(&report.code, offset);
                                break;
                            }
                            offset += line.chars().count() + 1;
                        }
                    } else {
                        let links: Vec<_> = report
                            .code
                            .links
                            .iter()
                            .filter(|l| l.label == target)
                            .collect();
                        if links.len() == 1
                            && raw.iter().filter(|s| s.callee == target).count() == 1
                        {
                            mapping["status"] = json!("unique_target_reference");
                            mapping["location"] = mapped_location(&report.code, links[0].start);
                            mapping["precision"] = json!(
                                "Unique DEX target and source reference within this method; structural association, not an instruction emission trace"
                            );
                        } else {
                            mapping["reason"] = json!(
                                "Repeated or rewritten calls cannot be assigned a unique Java location"
                            );
                        }
                    }
                    sites[i]["source_mapping"] = mapping;
                }
            }
        }
    }
    Ok(())
}

struct BatchProgress {
    id: String,
    cache: crate::call_graph::QueryCache,
}
impl Drop for BatchProgress {
    fn drop(&mut self) {
        self.cache.batches.lock().unwrap().remove(&self.id);
    }
}
pub(super) fn batch(
    engine: &mut NativeEngine,
    project: &Project,
    args: &Value,
    cancel: &AtomicBool,
) -> Result<Value> {
    let methods = args["method_ids"]
        .as_array()
        .context("Missing method_ids")?;
    ensure!(
        !methods.is_empty() && methods.len() <= 16,
        "method_ids must contain 1..16 exact method selectors"
    );
    let offset = args["method_offset"].as_u64().unwrap_or(0) as usize;
    ensure!(offset <= methods.len(), "Invalid method_offset");
    let progress = BatchProgress {
        id: super::random_id()?,
        cache: engine.invoke_index.clone(),
    };
    let mut results = vec![];
    let mut bytes = 0;
    let mut next = offset;
    for (i, method) in methods.iter().enumerate().skip(offset) {
        check(cancel)?;
        progress.cache.batches.lock().unwrap().insert(progress.id.clone(),json!({"completed_queries":i-offset,"total_queries":methods.len()-offset,"current_method_id":method}));
        let mut query = args.clone();
        let object = query.as_object_mut().unwrap();
        object.remove("method_ids");
        object.remove("method_offset");
        object.insert("method_id".into(), method.clone());
        object.entry("projection").or_insert(json!("call_sites"));
        object.entry("direction").or_insert(json!("callers"));
        let item = match super::dispatch(engine, project, "get_call_graph", &query, cancel) {
            Ok(data) => json!({"method_id":method,"data":data}),
            Err(e) => {
                check(cancel)?;
                json!({"method_id":method,"error":format!("{e:#}")})
            }
        };
        let size = serde_json::to_vec(&item)?.len();
        // MCP carries both structured JSON and its escaped text representation.
        if bytes + size > 2 * 1024 * 1024 {
            break;
        }
        bytes += size;
        results.push(item);
        next = i + 1;
    }
    ensure!(
        next > offset || next == methods.len(),
        "Batch item exceeds response budget; reduce limit or optional enrichment"
    );
    let batch_id = progress.id.clone();
    drop(progress);
    let failed = results.iter().filter(|r| r.get("error").is_some()).count();
    Ok(
        json!({"batch_id":batch_id,"results":results,"failed_queries":failed,"method_offset":offset,"next_method_offset":(next<methods.len()).then_some(next),"complete":next==methods.len(),"pagination_note":"complete means all input methods were processed, including explicit per-query errors. Each result retains its own next_offset for call-site paging; next_method_offset pages the input methods","invoke_index":engine.invoke_index.status()}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_dex::{DexClass, DexCode, DexSymbols};
    use std::sync::Arc;
    fn wrapper(words: Vec<u16>) -> DexClass {
        let mut class = crate::native_dex::parse(include_bytes!("../tests/fixtures/hello.dex"))
            .unwrap()
            .classes
            .remove(0);
        class.descriptor = "Lsdk/Helper;".into();
        class.methods.truncate(1);
        let m = &mut class.methods[0];
        m.name = "check".into();
        m.declaring_type = class.descriptor.clone();
        m.parameters = vec!["Ljava/lang/String;".into()];
        m.return_type = "Z".into();
        m.access_flags = 9;
        m.code = Some(DexCode {
            registers: 2,
            ins: 1,
            outs: 1,
            tries: 0,
            try_regions: vec![],
            offset: 0,
            instructions: words,
        });
        class.symbols = Arc::new(DexSymbols {
            strings: vec!["validate".into()],
            types: vec!["Lapp/security/Validator;".into()],
            protos: vec![("Z".into(), vec!["Ljava/lang/String;".into()])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        });
        class
    }
    #[test]
    fn certifies_only_static_argument_and_result_forwarding() {
        let id = "sdk.Helper.check(Ljava/lang/String;)Z";
        let c = wrapper(vec![0x1071, 0, 1, 0x000a, 0x000f]);
        let proof = forwarder_in_class(&c, id).unwrap();
        assert_eq!(
            proof["delegates_to"],
            "app.security.Validator.validate(Ljava/lang/String;)Z"
        );
        assert_eq!(proof["cross_namespace"], true);
        assert_eq!(proof["parameter_word_mapping"], json!([0]));
        for words in [
            vec![0x1071, 0, 1, 0x000a, 0x1012, 0x000f],
            vec![0x1071, 0, 1, 0x000a, 0x1071, 0, 1, 0x000a, 0x000f],
            vec![0x0228, 0, 0x1071, 0, 1, 0x000a, 0x000f],
            vec![0x1071, 0, 0, 0x000a, 0x000f],
        ] {
            assert!(forwarder_in_class(&wrapper(words), id).is_none());
        }
        let moved = wrapper(vec![0x1007, 0x1071, 0, 0, 0x010a, 0x010f]);
        assert!(forwarder_in_class(&moved, id).is_some());
    }
    #[test]
    fn unicode_source_positions_are_one_based_and_hash_relative() {
        let text = "αβ\nfoo.bar();\n";
        assert_eq!(offset_at(text, 2, 5).unwrap(), 7);
        assert_eq!(position(text, 7), (2, 5));
        assert!(offset_at(text, 4, 1).is_err());
    }
}
