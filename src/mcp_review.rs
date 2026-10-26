//! Source export, convenient declaration selectors, and decoded metadata queries.
use super::*;
use crate::{
    native_dex::{DexSymbols, DexValue},
    native_engine::disassembly,
};

pub(super) fn member_selector(
    engine: &NativeEngine,
    project: &Project,
    args: &Value,
    method: bool,
) -> Result<String> {
    let id_key = if method { "method_id" } else { "field_id" };
    let name_key = if method { "method_name" } else { "field_name" };
    if let Some(id) = args[id_key].as_str() {
        ensure!(
            args.get(name_key).is_none()
                && args.get("class_name").is_none()
                && args.get("descriptor").is_none(),
            "Use either {id_key} or class_name plus {name_key}"
        );
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
fn value(v: &DexValue, s: &DexSymbols, remaining: &mut usize) -> Result<Value> {
    ensure!(
        *remaining > 0,
        "ANNOTATION_VALUE_LIMIT: nested value exceeds response budget"
    );
    *remaining -= 1;
    if let DexValue::String(index) = v {
        let bytes = s
            .strings
            .get(*index as usize)
            .context("Invalid annotation string")?
            .len();
        ensure!(
            bytes <= *remaining,
            "ANNOTATION_VALUE_LIMIT: string exceeds response budget"
        );
        *remaining -= bytes;
    }
    let (kind, data) = match v {
        DexValue::Byte(v) => ("byte", json!(v)),
        DexValue::Short(v) => ("short", json!(v)),
        DexValue::Char(v) => ("char", json!(v)),
        DexValue::Int(v) => ("int", json!(v)),
        DexValue::Long(v) => ("long", json!(v.to_string())),
        DexValue::Float(v) => ("float_bits", json!(format!("0x{v:08x}"))),
        DexValue::Double(v) => ("double_bits", json!(format!("0x{v:016x}"))),
        DexValue::Boolean(v) => ("boolean", json!(v)),
        DexValue::Null => ("null", Value::Null),
        DexValue::String(i) => (
            "string",
            json!(
                s.strings
                    .get(*i as usize)
                    .context("Invalid annotation string")?
            ),
        ),
        DexValue::Type(i) => (
            "type",
            json!(
                s.types
                    .get(*i as usize)
                    .context("Invalid annotation type")?
                    .as_ref()
            ),
        ),
        DexValue::Field(i) | DexValue::Enum(i) => (
            if matches!(v, DexValue::Enum(_)) {
                "enum"
            } else {
                "field"
            },
            json!(disassembly::symbol(s, 'f', *i as usize).context("Invalid annotation field")?),
        ),
        DexValue::Method(i) => (
            "method",
            json!(disassembly::symbol(s, 'm', *i as usize).context("Invalid annotation method")?),
        ),
        DexValue::MethodType(i) => (
            "method_type",
            json!(
                s.protos
                    .get(*i as usize)
                    .map(|(r, p)| format!("({}){}", p.join(""), r))
                    .context("Invalid annotation prototype")?
            ),
        ),
        DexValue::MethodHandle(i) => ("method_handle_index", json!(i)),
        DexValue::Array(v) => (
            "array",
            Value::Array(
                v.iter()
                    .map(|v| value(v, s, remaining))
                    .collect::<Result<_>>()?,
            ),
        ),
        DexValue::Annotation { type_idx, elements } => (
            "annotation",
            json!({"type":s.types.get(*type_idx as usize).context("Invalid nested annotation type")?.as_ref(),"values":elements_value(elements,s,remaining)?}),
        ),
    };
    // Charge composite values too: this conservatively bounds symbol text,
    // escaped strings and array/object overhead before building the next value.
    let bytes = serde_json::to_vec(&data)?.len();
    ensure!(
        bytes <= *remaining,
        "ANNOTATION_VALUE_LIMIT: decoded value exceeds response budget"
    );
    *remaining -= bytes;
    Ok(json!({"kind":kind,"value":data}))
}
fn elements_value(
    elements: &[(u32, DexValue)],
    s: &DexSymbols,
    remaining: &mut usize,
) -> Result<Value> {
    let mut out = serde_json::Map::new();
    for (i, v) in elements {
        let name = s
            .strings
            .get(*i as usize)
            .context("Invalid annotation name")?;
        ensure!(
            name.len() <= *remaining,
            "ANNOTATION_VALUE_LIMIT: names exceed response budget"
        );
        *remaining -= name.len();
        out.insert(name.clone(), value(v, s, remaining)?);
    }
    Ok(Value::Object(out))
}
pub(super) fn annotations(
    engine: &NativeEngine,
    project: &Project,
    args: &Value,
    cancel: &AtomicBool,
) -> Result<Value> {
    let requested = required(args, "annotation_type")?;
    let descriptor = if requested.starts_with('L') && requested.ends_with(';') {
        requested.to_owned()
    } else {
        format!("L{};", requested.replace('.', "/"))
    };
    let element = args["element"].as_str().unwrap_or("all");
    let mut page = scan::Page::new(args)?;
    for name in &project.classes {
        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        if !scan::package_matches(name, args) {
            continue;
        }
        let c = engine
            .dex_class(name)
            .context("Missing class during annotation scan")?;
        scan::annotation_sets(c, name, |owner, set| {
            if element != "all" && owner["element"] != element {
                return Ok(());
            }
            for annotation in set.iter() {
                ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
                if c.symbols
                    .types
                    .get(annotation.type_idx as usize)
                    .is_none_or(|t| t.as_ref() != descriptor)
                {
                    continue;
                }
                let mut item = owner.clone();
                item["annotation_type"] = json!(descriptor);
                item["visibility"] = json!(annotation.visibility);
                item["values"] =
                    elements_value(&annotation.elements, &c.symbols, &mut (256 * 1024))?;
                item["dex_entry"] = json!(c.symbols.origin.get());
                page.add(item)?;
            }
            Ok(())
        })?;
    }
    let mut result = page.finish()?;
    result["scope"] = json!(
        "Explicit DEX declaration annotations; parameter_index excludes the receiver. No inherited annotations or annotation defaults inferred."
    );
    Ok(result)
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
    for (index, site) in sites.iter().enumerate() {
        let id = site["caller_method_id"]
            .as_str()
            .context("Missing caller ID")?;
        let pc = site["call_site_offset"]
            .as_u64()
            .context("Missing call offset")? as usize;
        groups.entry(id.to_owned()).or_default().push((index, pc));
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
                Ok(values) => values.get(&pc).cloned().unwrap_or_else(||json!({"status":"unresolved","reason":"Call site is unreachable or unsupported"})),
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

#[cfg(test)]
mod tests {
    use super::*;
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
