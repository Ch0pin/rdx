//! Source export, convenient declaration selectors, and decoded metadata queries.
use super::*;
#[cfg(test)]
use crate::annotation_values::elements_value;
#[cfg(test)]
use crate::native_dex::{DexSymbols, DexValue};
use crate::native_engine::disassembly;

pub(super) fn member_selector(
    engine: &NativeEngine,
    project: &Project,
    args: &Value,
    method: bool,
) -> Result<String> {
    let id_key = if method { "method_id" } else { "field_id" };
    let name_key = if method { "method_name" } else { "field_name" };
    ensure!(
        method || args.get("field").is_none() || args.get("field_id").is_none(),
        "Use only one of field or field_id"
    );
    if let Some(id) = args[id_key]
        .as_str()
        .or_else(|| (!method).then(|| args["field"].as_str()).flatten())
    {
        ensure!(
            args.get(name_key).is_none()
                && args.get("class_name").is_none()
                && args.get("descriptor").is_none(),
            "Use either {id_key} or class_name plus {name_key}"
        );
        if !method && let Some((owner, member)) = id.split_once(";->") {
            let owner = owner
                .strip_prefix('L')
                .context("Expected Lowner;->name:type")?;
            ensure!(
                !owner.is_empty() && member.contains(':') && !member.contains("->"),
                "Expected Lowner;->name:type"
            );
            return Ok(format!("{}.{}", owner.replace('/', "."), member));
        }
        return Ok(id.into());
    }
    let mut owner = resolve_class_id(project, required(args, "class_name")?)?.to_owned();
    let name = required(args, name_key)?;
    let descriptor = args["descriptor"].as_str();
    let mut seen = std::collections::HashSet::new();
    let mut choices = std::collections::BTreeMap::new();
    while seen.insert(owner.clone()) {
        ensure!(seen.len() <= 256, "Hierarchy exceeds lookup budget");
        let Some(c) = engine.dex_class(&owner) else {
            break;
        };
        if method {
            for m in &c.methods {
                if m.name.as_ref() == name {
                    let sig = format!("({}){}", m.parameters.join(""), m.return_type);
                    choices
                        .entry(sig)
                        .or_insert_with(|| disassembly::method_id(m));
                }
            }
        } else {
            for f in &c.fields {
                if f.name.as_ref() == name {
                    choices.insert(
                        f.field_type.to_string(),
                        format!("{owner}.{}:{}", f.name, f.field_type),
                    );
                }
            }
            // Name-only selection follows Java shadowing. An explicit DEX
            // type can select a different field signature in a superclass.
            if !choices.is_empty() && descriptor.is_none_or(|d| choices.contains_key(d)) {
                break;
            }
        }
        if name.starts_with('<') {
            break;
        }
        let Some(parent) = &c.superclass else {
            break;
        };
        owner = disassembly::type_name(parent);
    }
    let matches: Vec<_> = choices
        .iter()
        .filter(|(sig, _)| descriptor.is_none_or(|d| d == sig.as_str()))
        .map(|(_, id)| id.clone())
        .collect();
    if matches.len() == 1 {
        return Ok(matches[0].clone());
    }
    let candidates: Vec<_> = choices.values().take(30).collect();
    bail!(
        "{}: {}; candidates={}; supply an exact descriptor or canonical {}",
        if matches.is_empty() {
            "SYMBOL_NOT_FOUND"
        } else {
            "AMBIGUOUS_MEMBER"
        },
        name,
        serde_json::to_string(&candidates)?,
        id_key
    )
}
pub(super) fn annotations(
    engine: &NativeEngine,
    project: &Project,
    args: &Value,
    cancel: &AtomicBool,
) -> Result<Value> {
    engine.annotation_usages(project, args, cancel)
}
#[cfg(test)]
fn annotation_classes<'a>(
    classes: impl IntoIterator<Item = Result<(&'a str, &'a crate::native_dex::DexClass)>>,
    args: &Value,
    cancel: &AtomicBool,
) -> Result<Value> {
    crate::annotation_usage::scan_classes(classes, args, cancel)
}
pub(super) fn resources(
    engine: &NativeEngine,
    args: &Value,
    search: bool,
    cancel: &AtomicBool,
) -> Result<Value> {
    ensure!(
        engine.resource_error().is_none(),
        "Resource table unavailable: {}",
        engine.resource_error().unwrap_or("")
    );
    resources_from_table(engine.resource_table(), args, search, cancel)
}
fn resources_from_table(
    table: &crate::resource_table::ResourceTable,
    args: &Value,
    search: bool,
    cancel: &AtomicBool,
) -> Result<Value> {
    if !search {
        ensure!(
            args.get("resource_id").is_some() ^ args.get("resource_name").is_some(),
            "Supply exactly one of resource_id or resource_name"
        );
    }
    let mut page = scan::Page::new(args)?;
    let requested = args["resource_id"].as_str();
    let wanted = requested.and_then(|s| {
        s.strip_prefix("0x")
            .and_then(|v| u32::from_str_radix(v, 16).ok())
            .or_else(|| s.parse::<u32>().ok())
    });
    if requested.is_some() {
        ensure!(
            wanted.is_some(),
            "resource_id must be a decimal or 0x hexadecimal string"
        );
    }
    let resource = args["resource_name"]
        .as_str()
        .map(|s| s.trim_start_matches('@'));
    let query = args["query"].as_str().unwrap_or("");
    let config = args["config"].as_str();
    let mut found = 0;
    for r in table.entries.values() {
        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        let name = format!("{}:{}/{}", r.package, r.kind, r.name);
        if !(search
            || wanted == Some(r.id)
            || resource.is_some_and(|n| n == name || n == format!("{}/{}", r.kind, r.name)))
        {
            continue;
        }
        if search && !name.contains(query) && !r.variants.iter().any(|v| v.value.contains(query)) {
            continue;
        }
        found += 1;
        let resolution = matches!(r.kind.as_str(), "string" | "integer" | "bool")
            .then(|| table.resolve_scalar(&format!("@0x{:08x}", r.id), &r.package));
        for v in &r.variants {
            if config.is_some_and(|c| c != v.configuration) {
                continue;
            }
            let mut item = json!({"resource_id":format!("0x{:08x}",r.id),"resource_name":name,"resource_type":r.kind,"configuration":v.configuration,"raw_value":v.value,"reference_id":v.reference.map(|i|format!("0x{i:08x}")),"file":v.file});
            if let Some(resolution) = &resolution {
                let qualifier = v
                    .configuration
                    .rsplit_once(" [")
                    .filter(|(_, origin)| origin.ends_with(']'))
                    .map(|(q, _)| q)
                    .unwrap_or(&v.configuration);
                item["resolution"] = resolution["variants"]
                    .as_array()
                    .and_then(|variants| variants.iter().find(|x| x["configuration"] == qualifier))
                    .cloned()
                    .unwrap_or_else(|| json!({"status":"configuration_not_resolved"}));
            }
            page.add(item)?;
        }
        // An alias may inherit configurations declared only on its target.
        if let Some(resolution) = &resolution {
            ensure!(
                resolution["configuration_inventory_truncated"] != true,
                "RESOURCE_CONFIG_LIMIT: scalar alias configuration inventory exceeds budget"
            );
            if let Some(variants) = resolution["variants"].as_array() {
                for resolved in variants {
                    let q = resolved["configuration"].as_str().unwrap();
                    if config.is_some_and(|c| c != q)
                        || r.variants.iter().any(|v| {
                            v.configuration == q
                                || v.configuration
                                    .strip_prefix(q)
                                    .is_some_and(|tail| tail.starts_with(" ["))
                        })
                    {
                        continue;
                    }
                    page.add(json!({"resource_id":format!("0x{:08x}",r.id),"resource_name":name,
                        "resource_type":r.kind,"configuration":q,"inherited_configuration":true,"resolution":resolved}))?;
                }
            }
        }
    }
    if !search {
        ensure!(found > 0, "RESOURCE_NOT_FOUND");
        ensure!(
            found == 1,
            "AMBIGUOUS_RESOURCE: qualify resource_name with package"
        );
    }
    let mut result = page.finish()?;
    result["configuration_semantics"] = json!(
        "Exact stored configuration filter; no device best-match selection. Scalar references resolve in that configuration with default fallback. Other values retain decoded ARSC text."
    );
    Ok(result)
}
pub(super) fn export(
    engine: &mut NativeEngine,
    project: &Project,
    args: &Value,
    cancel: &AtomicBool,
    apk_identity: Option<Value>,
) -> Result<Value> {
    let out = Path::new(required(args, "out_path")?);
    ensure!(
        out.is_absolute(),
        "out_path must be an absolute path for a new export directory"
    );
    super::discovery::check_export_parent(out)?;
    let method = args.get("method_id").is_some() || args.get("method_name").is_some();
    let (source, coverage, identity) = if method {
        let id = member_selector(engine, project, args, true)?;
        let id = crate::method_lookup::normalize(&id)?;
        let format =
            crate::engine::MethodSourceFormat::parse(args["format"].as_str().unwrap_or("auto"))?;
        let report = engine.method_source(&id, format, cancel)?;
        (
            report.code.source,
            json!({"format":report.format,"fallback_reason":report.fallback_reason,"reconstruction_attempted":report.reconstruction_attempted}),
            json!({"method_id":id}),
        )
    } else {
        ensure!(
            args["format"].as_str().unwrap_or("auto") == "auto",
            "format=dex requires a method selector; use get_class_disassembly for class DEX"
        );
        ensure!(
            args.get("descriptor").is_none(),
            "descriptor requires method_name"
        );
        let id = resolve_class_id(project, required(args, "class_name")?)?;
        let (code, coverage) = engine.decompile_class_source_report(id, cancel)?;
        (code.source, coverage, json!({"class_id":id}))
    };
    ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
    let mut metadata = json!({"identity":identity,"apk_identity":apk_identity,"coverage":coverage,"instance_id":args["instance_id"],"project_id":args["project_id"],"sha256":format!("{:x}",Sha256::digest(source.as_bytes())),"size_bytes":source.len()});
    fs::create_dir(out)
        .context("Export directory must be new; existing paths are never overwritten")?;
    let source_path = out.join("source.txt");
    let metadata_path = out.join("coverage.json");
    let write = (|| -> Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&source_path)?;
        file.write_all(source.as_bytes())?;
        file.sync_all()?;
        private_write(&metadata_path, &metadata)?;
        Ok(())
    })();
    if let Err(error) = write {
        return Err(error.context(format!("Export incomplete; inspect {}", out.display())));
    }
    metadata.as_object_mut().unwrap().remove("coverage");
    metadata["source_path"] = json!(source_path);
    metadata["coverage_path"] = json!(metadata_path);
    metadata["coverage_sha256"] = json!(digest(&metadata_path)?);
    metadata["coverage_size_bytes"] = json!(fs::metadata(&metadata_path)?.len());
    Ok(metadata)
}

pub(super) fn call_arguments(
    engine: &NativeEngine,
    result: &mut Value,
    cancel: &AtomicBool,
) -> Result<()> {
    let sites = result["call_sites"]
        .as_array_mut()
        .context("include_const_args requires projection=call_sites")?;
    let mut groups = std::collections::BTreeMap::<String, Vec<(usize, usize)>>::new();
    let mut allocations = Vec::new();
    for (index, site) in sites.iter().enumerate() {
        if site["kind"] == "lambda_creation" {
            allocations.push(index);
            continue;
        }
        let id = site["caller_method_id"]
            .as_str()
            .context("Missing caller ID")?;
        let pc = site["call_site_offset"]
            .as_u64()
            .context("Missing call offset")? as usize;
        groups.entry(id.to_owned()).or_default().push((index, pc));
    }
    for index in allocations {
        sites[index]["constant_arguments"] = json!({"status":"not_applicable","reason":"lambda_creation evidence is a new-instance allocation; constant arguments apply to invoke sites only"});
    }
    let mut work = 0;
    let mut bytes = 0;
    for (id, positions) in groups {
        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        let run = (|| -> Result<_> {
            ensure!(bytes < 512 * 1024, "Constant argument page budget reached");
            let head = id.split_once('(').context("Invalid method ID")?.0;
            let owner = head.rsplit_once('.').context("Invalid method owner")?.0;
            let class = engine
                .dex_class(owner)
                .context("Caller definition unavailable")?;
            let method = crate::method_lookup::resolve(class, &id)?;
            let wanted = positions.iter().map(|(_, pc)| *pc).collect();
            crate::call_arguments::analyze(class, method, cancel, &mut work, &wanted)
        })();
        for (index, pc) in positions {
            let mut value = match &run {
                Ok(values) => values.get(&pc).cloned().unwrap_or_else(||json!({"status":"unresolved","reason":"Call site is in code unreachable from the method entry; no constant state exists for it"})),
                Err(error) => json!({"status":"unresolved","reason":format!("{error:#}")}),
            };
            let size = serde_json::to_vec(&value)?.len();
            if bytes + size > 512 * 1024 {
                value = json!({"status":"unresolved","reason":"Constant argument page budget reached; request a smaller call-site page"});
            } else {
                bytes += size;
            }
            sites[index]["constant_arguments"] = value;
        }
    }
    ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
    result["constant_arguments_hash"] = json!(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&result["call_sites"])?)
    ));
    Ok(())
}

/// Keeps only call sites whose constant proof binds `argument_index` to
/// `argument_literal`. Excluded (different proven constant) and unresolved
/// sites stay visible as compact records so negative coverage remains bounded.
pub(super) fn argument_filter(result: &mut Value, args: &Value) -> Result<()> {
    let index = args["argument_index"]
        .as_u64()
        .context("argument_index must be an integer")? as usize;
    let literal = &args["argument_literal"];
    ensure!(
        literal.is_string() || literal.is_i64(),
        "argument_literal must be a string or a signed 32-bit integer"
    );
    let sites = result["call_sites"]
        .as_array_mut()
        .context("argument_index/argument_literal require projection=call_sites")?;
    let page_total = sites.len();
    let (mut matched, mut excluded, mut unresolved) = (Vec::new(), Vec::new(), Vec::new());
    for site in sites.drain(..) {
        let compact = |reason: Value, proven: Option<&Value>| {
            let mut record = json!({"caller_method_id":site["caller_method_id"],"call_site_offset":site["call_site_offset"],
                "invoke_instruction":site["invoke_instruction"],"depth":site["depth"],"result_offset":site["result_offset"],"reason":reason});
            if let Some(argument) = proven {
                record["argument"] = argument.clone();
            }
            record
        };
        let proof = &site["constant_arguments"];
        if proof["status"] != "analyzed" {
            let reason = proof["reason"]
                .as_str()
                .unwrap_or("constant argument analysis unavailable for this call site");
            unresolved.push(compact(json!(reason), None));
            continue;
        }
        let arguments = proof["arguments"].as_array().cloned().unwrap_or_default();
        let Some(argument) = arguments.get(index) else {
            excluded.push(compact(
                json!(format!(
                    "argument_index {index} exceeds the invoke's parameter count ({})",
                    arguments.len()
                )),
                None,
            ));
            continue;
        };
        if argument["status"] != "constant" {
            let reason = argument["reason"].as_str().unwrap_or(
                "value is not a single proven constant on every reachable CFG path (merged registers, exception entry, arithmetic, field, parameter or call result)",
            );
            unresolved.push(compact(json!(reason), Some(argument)));
            continue;
        }
        let same = match (literal, argument["kind"].as_str()) {
            (Value::String(wanted), Some("string")) => argument["value"] == *wanted,
            (Value::Number(wanted), Some("bits32")) => {
                argument["value"].as_i64() == wanted.as_i64()
            }
            _ => false,
        };
        if same {
            matched.push(site);
        } else {
            excluded.push(compact(
                json!("proven constant differs from argument_literal"),
                Some(argument),
            ));
        }
    }
    result["argument_filter"] = json!({
        "argument_index":index,"argument_literal":literal,"page_total":page_total,
        "matched":matched.len(),"excluded":excluded.len(),"unresolved":unresolved.len(),
        "semantics":"Applied to this page only; follow next_offset for remaining pages. matched sites prove the literal at argument_index via intraprocedural constant flow and keep constant_arguments. excluded sites prove a different constant, kind or parameter count. unresolved sites are neither proven nor refuted and need manual review. Receiver excluded from indexing. constant_arguments_hash covers the analyzed page before filtering."
    });
    result["call_sites"] = json!(matched);
    result["excluded_call_sites"] = json!(excluded);
    result["unresolved_call_sites"] = json!(unresolved);
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;

    fn site(offset: u64, proof: Value) -> Value {
        json!({"caller_method_id":"app.Caller.run()V","call_site_offset":offset,"depth":1,
            "invoke_instruction":format!("{offset:04x}: invoke-virtual"),"constant_arguments":proof})
    }
    fn constant(index: usize, kind: &str, value: Value) -> Value {
        json!({"status":"analyzed","arguments":[{"argument_index":index,"register":1,"descriptor":"Ljava/lang/String;","status":"constant","kind":kind,"value":value}]})
    }
    #[test]
    fn argument_filter_partitions_matched_excluded_and_unresolved() {
        let mut result = json!({"call_sites":[
            site(1, constant(0, "string", json!("token"))),
            site(2, constant(0, "string", json!("other"))),
            site(3, constant(0, "bits32", json!(0))),
            site(4, json!({"status":"analyzed","arguments":[{"argument_index":0,"register":2,"descriptor":"Ljava/lang/String;","status":"unresolved"}]})),
            site(5, json!({"status":"unresolved","reason":"Caller definition unavailable"})),
            site(6, json!({"status":"analyzed","arguments":[]})),
        ],"returned_count":6});
        argument_filter(
            &mut result,
            &json!({"argument_index":0,"argument_literal":"token"}),
        )
        .unwrap();
        let summary = &result["argument_filter"];
        assert_eq!(summary["page_total"], 6);
        assert_eq!(summary["matched"], 1);
        assert_eq!(summary["excluded"], 3);
        assert_eq!(summary["unresolved"], 2);
        assert_eq!(result["call_sites"][0]["call_site_offset"], 1);
        assert_eq!(
            result["call_sites"][0]["constant_arguments"]["status"],
            "analyzed"
        );
        let excluded = result["excluded_call_sites"].as_array().unwrap();
        assert_eq!(excluded[0]["argument"]["value"], "other");
        assert_eq!(excluded[1]["argument"]["kind"], "bits32");
        assert!(
            excluded[2]["reason"]
                .as_str()
                .unwrap()
                .contains("parameter count (0)")
        );
        let unresolved = result["unresolved_call_sites"].as_array().unwrap();
        assert_eq!(unresolved[0]["call_site_offset"], 4);
        assert_eq!(unresolved[0]["argument"]["register"], 2);
        assert_eq!(unresolved[1]["reason"], "Caller definition unavailable");
        assert_eq!(result["returned_count"], 6);
        let mut by_int = json!({"call_sites":[site(7, constant(0, "bits32", json!(21)))]});
        argument_filter(
            &mut by_int,
            &json!({"argument_index":0,"argument_literal":21}),
        )
        .unwrap();
        assert_eq!(by_int["argument_filter"]["matched"], 1);
        let mut missing = json!({"graph":[]});
        assert!(
            argument_filter(
                &mut missing,
                &json!({"argument_index":0,"argument_literal":"x"})
            )
            .is_err()
        );
    }
    #[test]
    fn annotation_presence_distinguishes_absence_unused_types_and_filtered_matches() {
        use crate::native_dex::{self, DexAnnotation, DexAnnotationDirectory};
        let mut classes = native_dex::parse(include_bytes!("../tests/fixtures/hello.dex"))
            .unwrap()
            .classes;
        let c = &mut classes[0];
        c.annotations_offset = 1;
        let mut symbols = DexSymbols {
            types: vec!["Lqj/o;".into(), "Lqj/p;".into()],
            ..Default::default()
        };
        symbols.annotations.insert(
            1,
            Arc::new(DexAnnotationDirectory {
                class: Some(
                    vec![Arc::new(DexAnnotation {
                        visibility: 1,
                        type_idx: 0,
                        elements: vec![],
                    })]
                    .into(),
                ),
                ..Default::default()
            }),
        );
        c.symbols = Arc::new(symbols);
        let cancel = AtomicBool::new(false);
        let query = |args: Value| {
            annotation_classes(
                [
                    Ok(("sample.One", &classes[0])),
                    Ok(("sample.Two", &classes[0])),
                ],
                &args,
                &cancel,
            )
            .unwrap()
        };
        let absent = query(json!({"annotation_type":"retrofit2.http.POST"}));
        assert_eq!(absent["total"], 0);
        assert_eq!(absent["complete"], true);
        assert_eq!(absent["requested_type_present"], false);
        assert_eq!(absent["annotation_type"], "Lretrofit2/http/POST;");
        assert_eq!(absent["warnings"][0]["code"], "ANNOTATION_TYPE_ABSENT");
        assert_eq!(absent["match_mode"], "exact_descriptor");
        let unused = query(json!({"annotation_type":"qj.p"}));
        assert_eq!(unused["total"], 0);
        assert_eq!(unused["requested_type_present"], true);
        assert_eq!(unused["warnings"], json!([]));
        for args in [
            json!({"annotation_type":"qj.o", "package_prefix":"other"}),
            json!({"annotation_type":"qj.o", "element":"parameter"}),
        ] {
            let filtered = query(args);
            assert_eq!(filtered["total"], 0);
            assert_eq!(filtered["requested_type_present"], true);
            assert_eq!(filtered["warnings"], json!([]));
        }
        let first = query(json!({"annotation_type":"qj.o", "limit":1}));
        assert_eq!(first["total"], 2);
        assert_eq!(first["next_offset"], 1);
        assert_eq!(first["requested_type_present"], true);
        assert_eq!(first["items"][0]["annotation_type"], "Lqj/o;");
        assert_eq!(first["matched_types"], json!(["Lqj/o;"]));
        assert_eq!(first["matched_types_complete"], true);
        assert_eq!(first, query(json!({"annotation_type":"Lqj/o;", "limit":1})));
        let second = query(json!({"annotation_type":"qj.o", "offset":1, "limit":1}));
        assert_eq!(second["total"], 2);
        assert!(second["next_offset"].is_null());
        assert_ne!(first["items"], second["items"]);
        assert!(
            annotation_classes(
                [Ok(("sample.One", &classes[0]))],
                &json!({"annotation_type":"qj.o"}),
                &AtomicBool::new(true)
            )
            .is_err()
        );
    }

    #[test]
    fn annotation_simple_name_and_prefix_modes_match_relocated_types() {
        use crate::native_dex::{self, DexAnnotation, DexAnnotationDirectory};
        let mut c = native_dex::parse(include_bytes!("../tests/fixtures/hello.dex"))
            .unwrap()
            .classes
            .remove(0);
        c.methods.retain(|m| m.name.as_ref() == "answer");
        c.annotations_offset = 1;
        let annotation = |type_idx| {
            Arc::from(vec![Arc::new(DexAnnotation {
                visibility: 1,
                type_idx,
                elements: vec![],
            })])
        };
        let mut symbols = DexSymbols {
            types: vec![
                "Lretrofit2/http/GET;".into(),
                "Lff/a/GET;".into(),
                "Lretrofit2/http/Url;".into(),
                "Lretrofit2/Call;".into(),
            ],
            ..Default::default()
        };
        symbols.annotations.insert(
            1,
            Arc::new(DexAnnotationDirectory {
                class: Some(annotation(0)),
                methods: vec![Some(annotation(1))],
                parameters: vec![vec![Some(annotation(2))]],
                ..Default::default()
            }),
        );
        c.symbols = Arc::new(symbols);
        let cancel = AtomicBool::new(false);
        let query = |args: Value| annotation_classes([Ok(("ff.g", &c))], &args, &cancel).unwrap();
        let simple = query(json!({"annotation_type":"GET","match_mode":"simple_name"}));
        assert_eq!(simple["total"], 2, "{simple}");
        assert_eq!(simple["match_mode"], "simple_name");
        assert_eq!(simple["annotation_type"], "GET");
        assert_eq!(simple["requested_type_present"], true);
        assert_eq!(simple["warnings"], json!([]));
        assert_eq!(
            simple["matched_types"],
            json!(["Lff/a/GET;", "Lretrofit2/http/GET;"])
        );
        assert_eq!(simple["items"][0]["element"], "class");
        assert_eq!(
            simple["items"][0]["annotation_type"],
            "Lretrofit2/http/GET;"
        );
        assert_eq!(simple["items"][1]["element"], "method");
        assert_eq!(simple["items"][1]["annotation_type"], "Lff/a/GET;");
        // Qualified spellings reduce to the same simple name.
        for spelling in ["retrofit2.http.GET", "Lretrofit2/http/GET;", "ff/a/GET"] {
            assert_eq!(
                query(json!({"annotation_type":spelling,"match_mode":"simple_name"}))["total"],
                2
            );
        }
        let prefix = query(json!({"annotation_type":"retrofit2.http","match_mode":"prefix"}));
        assert_eq!(prefix["total"], 2, "{prefix}");
        assert_eq!(prefix["annotation_type"], "retrofit2/http");
        assert_eq!(
            prefix["matched_types"],
            json!(["Lretrofit2/http/GET;", "Lretrofit2/http/Url;"])
        );
        assert_eq!(prefix["items"][1]["element"], "parameter");
        assert_eq!(prefix["items"][1]["parameter_index"], 0);
        assert_eq!(
            query(json!({"annotation_type":"retrofit2.http.U","match_mode":"prefix"}))["total"],
            1
        );
        // Presence follows the mode: retrofit2.Call exists but is not used as
        // an annotation, while nothing in the tables starts with okhttp3.
        let unused = query(json!({"annotation_type":"retrofit2.Call","match_mode":"prefix"}));
        assert_eq!(unused["total"], 0);
        assert_eq!(unused["requested_type_present"], true);
        let absent = query(json!({"annotation_type":"okhttp3","match_mode":"prefix"}));
        assert_eq!(absent["total"], 0);
        assert_eq!(absent["requested_type_present"], false);
        assert_eq!(absent["warnings"][0]["code"], "ANNOTATION_TYPE_ABSENT");
        assert!(
            absent["warnings"][0]["message"]
                .as_str()
                .unwrap()
                .contains("internal-name prefix \"okhttp3\"")
        );
        let missing = query(json!({"annotation_type":"Nope","match_mode":"simple_name"}));
        assert_eq!(missing["requested_type_present"], false);
        assert_eq!(missing["warnings"][0]["code"], "ANNOTATION_TYPE_ABSENT");
        let exact = query(json!({"annotation_type":"retrofit2.http.GET"}));
        assert_eq!(exact["total"], 1);
        assert_eq!(exact["match_mode"], "exact_descriptor");
        assert!(
            annotation_classes(
                [Ok(("ff.g", &c))],
                &json!({"annotation_type":"GET","match_mode":"fuzzy"}),
                &cancel
            )
            .is_err()
        );
    }

    #[test]
    fn annotation_type_presence_checks_other_dex_tables_before_package_filter() {
        let first =
            crate::native_dex::parse(include_bytes!("../tests/fixtures/hello.dex")).unwrap();
        let mut second =
            crate::native_dex::parse(include_bytes!("../tests/fixtures/hello.dex")).unwrap();
        second.classes[0].symbols = Arc::new(DexSymbols {
            types: vec!["Lexternal/Marker;".into()],
            ..Default::default()
        });
        let result = annotation_classes(
            [
                Ok(("sample.One", &first.classes[0])),
                Ok(("other.Two", &second.classes[0])),
            ],
            &json!({"annotation_type":"external.Marker", "package_prefix":"sample"}),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(result["total"], 0);
        assert_eq!(result["requested_type_present"], true);
        assert_eq!(result["warnings"], json!([]));
    }

    #[test]
    fn resource_alias_configurations_cycles_and_selection_are_explicit() {
        use crate::resource_table::{Resource, ResourceTable, Variant};
        let mut table = ResourceTable::default();
        for (id, name, variants) in [
            (
                1,
                "url",
                vec![
                    ("default", "https://default", None),
                    ("fr", "https://fr", None),
                ],
            ),
            (2, "alias", vec![("default", "@0x00000001", Some(1))]),
            (3, "cycle", vec![("default", "@0x00000003", Some(3))]),
        ] {
            table.entries.insert(
                id,
                Resource {
                    id,
                    package: "sample".into(),
                    kind: "string".into(),
                    name: name.into(),
                    variants: variants
                        .into_iter()
                        .map(|(config, value, reference)| Variant {
                            configuration: config.into(),
                            value: value.into(),
                            reference,
                            file: None,
                        })
                        .collect(),
                },
            );
        }
        let cancel = AtomicBool::new(false);
        let args = json!({"resource_name":"string/alias"});
        let result = resources_from_table(&table, &args, false, &cancel).unwrap();
        assert_eq!(result["total"], 2);
        assert_eq!(result["items"][1]["inherited_configuration"], true);
        assert_eq!(result["items"][1]["resolution"]["value"], "https://fr");
        let result =
            resources_from_table(&table, &json!({"resource_id":"3"}), false, &cancel).unwrap();
        assert_eq!(result["items"][0]["resolution"]["status"], "alias_cycle");
        assert!(
            resources_from_table(
                &table,
                &json!({"resource_id":"1","resource_name":"string/cycle"}),
                false,
                &cancel
            )
            .is_err()
        );
        let result =
            resources_from_table(&table, &json!({"query":"url","config":"fr"}), true, &cancel)
                .unwrap();
        assert_eq!(result["total"], 1);
        assert_eq!(result["items"][0]["raw_value"], "https://fr");
    }
    #[test]
    fn annotation_parameter_owner_and_nested_values_preserve_dex_identity() {
        use crate::native_dex::{self, DexAnnotation, DexAnnotationDirectory};
        let mut c = native_dex::parse(include_bytes!("../tests/fixtures/hello.dex"))
            .unwrap()
            .classes
            .remove(0);
        c.methods.retain(|m| m.name.as_ref() == "answer");
        c.annotations_offset = 1;
        let annotation = DexAnnotation {
            visibility: 1,
            type_idx: 0,
            elements: vec![(
                0,
                DexValue::Array(vec![DexValue::String(1), DexValue::Int(7)]),
            )],
        };
        let mut symbols = DexSymbols {
            strings: vec!["value".into(), "Authorization".into()],
            types: vec!["Lsample/Header;".into()],
            ..Default::default()
        };
        symbols.annotations.insert(
            1,
            Arc::new(DexAnnotationDirectory {
                parameters: vec![vec![None, Some(vec![Arc::new(annotation)].into())]],
                ..Default::default()
            }),
        );
        c.symbols = Arc::new(symbols);
        let mut result = vec![];
        scan::annotation_sets(&c, "sample.Hello", |mut owner, set| {
            owner["values"] = elements_value(&set[0].elements, &c.symbols, &mut 1000)?;
            result.push(owner);
            Ok(())
        })
        .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["parameter_index"], 1);
        assert!(result[0]["method_id"].as_str().unwrap().contains("answer"));
        assert_eq!(
            result[0]["values"]["value"]["value"][0]["value"],
            "Authorization"
        );
        assert!(
            elements_value(
                &c.symbols.annotations[&1].parameters[0][1].as_ref().unwrap()[0].elements,
                &c.symbols,
                &mut 1
            )
            .is_err()
        );
    }
}
