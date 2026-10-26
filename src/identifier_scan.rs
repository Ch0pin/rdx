//! Resumable DEX scan: cursors advance at original metadata/instruction boundaries.
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    key: String,
    class: usize,
    phase: u8,
    member: usize,
    sub: usize,
    pc: usize,
    matches: usize,
    owners: usize,
    classes: usize,
    methods: usize,
    class_hit: bool,
    method_hits: usize,
}

fn symbol(pool: &DexSymbols, kind: Kind, i: usize) -> Result<Symbol> {
    let (id, name) = match kind {
        Kind::Class => {
            let id = type_name(pool.types.get(i).context("Invalid type operand")?);
            let name = id.rsplit('.').next().unwrap_or(&id).to_owned();
            (id, name)
        }
        Kind::Method => {
            let &(owner, proto, name) = pool.methods.get(i).context("Invalid method operand")?;
            let owner = pool
                .types
                .get(owner as usize)
                .context("Invalid method owner")?;
            let (ret, params) = pool
                .protos
                .get(proto as usize)
                .context("Invalid method proto")?;
            let name = pool
                .strings
                .get(name as usize)
                .context("Invalid method name")?
                .clone();
            (
                format!("{}.{}({}){}", type_name(owner), name, params.join(""), ret),
                name,
            )
        }
        Kind::Field => {
            let &(owner, ty, name) = pool.fields.get(i).context("Invalid field operand")?;
            let owner = pool
                .types
                .get(owner as usize)
                .context("Invalid field owner")?;
            let ty = pool.types.get(ty as usize).context("Invalid field type")?;
            let name = pool
                .strings
                .get(name as usize)
                .context("Invalid field name")?
                .clone();
            (format!("{}.{}:{}", type_name(owner), name, ty), name)
        }
    };
    Ok(Symbol {
        kind,
        id: id.into(),
        name,
        sites: vec![],
    })
}
fn declared(kind: Kind, id: String, name: String) -> Symbol {
    Symbol {
        kind,
        id: id.into(),
        name,
        sites: vec![],
    }
}
fn ty(value: &str) -> Option<Symbol> {
    let value = value.trim_start_matches('[');
    if !value.starts_with('L') || !value.ends_with(';') {
        return None;
    }
    let id = type_name(value);
    let name = id.rsplit('.').next().unwrap_or(&id).to_owned();
    Some(declared(Kind::Class, id, name))
}

pub(super) fn run(
    classes: Arc<BTreeMap<String, DexClass>>,
    args: &Value,
    query: Query,
    cancel: &AtomicBool,
    session: &str,
) -> Result<Value> {
    run_bounded(
        classes,
        args,
        query,
        cancel,
        session,
        Duration::from_secs(15),
        usize::MAX,
    )
}
fn run_bounded(
    classes: Arc<BTreeMap<String, DexClass>>,
    args: &Value,
    query: Query,
    cancel: &AtomicBool,
    session: &str,
    duration: Duration,
    max_steps: usize,
) -> Result<Value> {
    let start = Instant::now();
    let mut binding = args.clone();
    for k in ["resume_cursor", "offset", "limit"] {
        binding.as_object_mut().unwrap().remove(k);
    }
    let key = format!("{:x}", Sha256::digest(format!("{session}:{binding}")));
    let mut cursor = if let Some(encoded) = args["resume_cursor"].as_str() {
        ensure!(encoded.len() <= 4096, "Invalid resume_cursor");
        ensure!(query.offset == 0, "Use resume_cursor without offset");
        let (seal, payload) = encoded.split_once(':').context("Invalid resume_cursor")?;
        ensure!(
            seal == format!(
                "{:x}",
                Sha256::digest(format!("{session}:{payload}:{session}"))
            ),
            "Invalid resume_cursor integrity"
        );
        let value: Cursor = serde_json::from_str(payload).context("Invalid resume_cursor")?;
        ensure!(
            value.key == key,
            "STALE_CURSOR: project or query changed; restart without resume_cursor"
        );
        value
    } else {
        Cursor {
            key,
            ..Default::default()
        }
    };
    let projection = args["projection"].as_str().unwrap_or("full");
    let method_only = query.kind == "method" && query.occurrence == "reference";
    let names: Vec<_> = classes
        .keys()
        .filter(|n| query.accepts_class(n))
        .cloned()
        .collect();
    ensure!(
        cursor.class <= names.len() && cursor.phase <= 4,
        "Invalid scan cursor position"
    );
    let first_class = cursor.class;
    let initial_matches = cursor.matches;
    let mut index = Index {
        classes: classes.clone(),
        names: vec![],
        symbols: vec![],
        hash: String::new(),
        estimated_bytes: 0,
        code_methods: 0,
        dynamic_sites: 0,
    };
    let mut targets: HashMap<usize, Vec<bool>> = HashMap::new();
    let mut items = vec![];
    let mut bytes = 0;
    let mut steps = 0;
    while cursor.class < names.len() {
        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        if steps > 0 && (steps >= max_steps || start.elapsed() >= duration) {
            break;
        }
        steps += 1;
        let name = &names[cursor.class];
        let class = &classes[name];
        let pool = &class.symbols;
        index.names.clear();
        index.names.push(name.clone());
        if method_only && cursor.phase == 0 {
            let id = Arc::as_ptr(pool) as usize;
            if let std::collections::hash_map::Entry::Vacant(entry) = targets.entry(id) {
                let mut flags = Vec::with_capacity(pool.methods.len());
                for i in 0..pool.methods.len() {
                    if i % 256 == 0 {
                        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
                    }
                    flags.push(query.matches_symbol(&symbol(pool, Kind::Method, i)?));
                }
                entry.insert(flags);
            }
            if !targets[&id].iter().any(|v| *v) {
                cursor.class += 1;
                continue;
            }
            cursor.phase = 2;
        }
        let mut site = Site {
            class: 0,
            member: cursor.member as u32,
            owner: Kind::Class,
            context: ContextKind::Declaration,
            position: NO_POSITION,
        };
        let candidate = match cursor.phase {
            0 => {
                let descriptor = if cursor.sub == 0 {
                    Some((class.descriptor.as_ref(), ContextKind::Declaration))
                } else if cursor.sub == 1 && class.superclass.is_some() {
                    class
                        .superclass
                        .as_deref()
                        .map(|s| (s, ContextKind::Superclass))
                } else {
                    class
                        .interfaces
                        .get(cursor.sub - 1 - usize::from(class.superclass.is_some()))
                        .map(|s| (s.as_ref(), ContextKind::Interface))
                };
                if let Some((value, context)) = descriptor {
                    cursor.sub += 1;
                    site.context = context;
                    ty(value)
                } else {
                    cursor.phase = 1;
                    cursor.sub = 0;
                    None
                }
            }
            1 => {
                if let Some(field) = class.fields.get(cursor.member) {
                    site.owner = Kind::Field;
                    if cursor.sub == 0 {
                        cursor.sub = 1;
                        Some(declared(
                            Kind::Field,
                            format!(
                                "{}.{}:{}",
                                type_name(&field.declaring_type),
                                field.name,
                                field.field_type
                            ),
                            field.name.to_string(),
                        ))
                    } else {
                        cursor.sub = 0;
                        cursor.member += 1;
                        site.context = ContextKind::FieldType;
                        ty(&field.field_type)
                    }
                } else {
                    cursor.phase = 2;
                    cursor.member = 0;
                    None
                }
            }
            2 => {
                if let Some(method) = class.methods.get(cursor.member) {
                    site.owner = Kind::Method;
                    if method_only {
                        cursor.phase = 3;
                        None
                    } else if cursor.sub == 0 {
                        cursor.sub = 1;
                        Some(declared(
                            Kind::Method,
                            method_id(method),
                            method.name.to_string(),
                        ))
                    } else if cursor.sub == 1 {
                        cursor.sub = 2;
                        site.context = ContextKind::ReturnType;
                        ty(&method.return_type)
                    } else if let Some(value) = method.parameters.get(cursor.sub - 2) {
                        site.context = ContextKind::ParameterType;
                        site.position = (cursor.sub - 2) as u32;
                        cursor.sub += 1;
                        ty(value)
                    } else {
                        cursor.sub = 0;
                        cursor.phase = 3;
                        None
                    }
                } else {
                    cursor.class += 1;
                    cursor.phase = 0;
                    cursor.member = 0;
                    cursor.sub = 0;
                    cursor.pc = 0;
                    cursor.class_hit = false;
                    None
                }
            }
            3 => {
                let method = class
                    .methods
                    .get(cursor.member)
                    .context("Invalid cursor method")?;
                site.owner = Kind::Method;
                if let Some(code) = method
                    .code
                    .as_ref()
                    .filter(|c| cursor.pc < c.instructions.len())
                {
                    let pc = cursor.pc;
                    let (width, payload) = instruction_width(&code.instructions, pc)?;
                    ensure!(
                        pc + width <= code.instructions.len(),
                        "Truncated DEX in {} at {pc:#x}",
                        method_id(method)
                    );
                    cursor.pc += width;
                    let kind = if payload {
                        None
                    } else {
                        match code.instructions[pc] as u8 {
                            0x1c | 0x1f | 0x20 | 0x22..=0x25 => Some(Kind::Class),
                            0x52..=0x6d => Some(Kind::Field),
                            0x6e..=0x72 | 0x74..=0x78 | 0xfa | 0xfb => Some(Kind::Method),
                            _ => None,
                        }
                    };
                    site.context = ContextKind::Instruction;
                    site.position = pc as u32;
                    if let Some(kind) = kind {
                        if method_only && kind != Kind::Method {
                            None
                        } else {
                            let operand = code.instructions[pc + 1] as usize;
                            // Cursor resumes can begin inside a method: resolve directly until the next DEX table is cached.
                            if method_only
                                && targets
                                    .get(&(Arc::as_ptr(pool) as usize))
                                    .is_some_and(|flags| flags.get(operand) == Some(&false))
                            {
                                None
                            } else {
                                Some(symbol(pool, kind, operand)?)
                            }
                        }
                    } else {
                        None
                    }
                } else {
                    cursor.phase = 4;
                    None
                }
            }
            4 => {
                let method = class
                    .methods
                    .get(cursor.member)
                    .context("Invalid cursor method")?;
                let count = cursor.method_hits;
                if count > 0 {
                    if projection == "callers" && cursor.owners >= query.offset {
                        let item = json!({"class_id":name,"method_id":method_id(method),"matching_occurrences":count});
                        bytes += serde_json::to_vec(&item)?.len();
                        items.push(item);
                    }
                    cursor.owners += 1;
                }
                cursor.method_hits = 0;
                cursor.member += 1;
                cursor.pc = 0;
                cursor.sub = 0;
                cursor.phase = 2;
                if projection == "callers" && (items.len() >= query.limit || bytes >= 900_000) {
                    break;
                }
                None
            }
            _ => unreachable!(),
        };
        if let Some(symbol) = candidate
            && query.matches(&symbol, &site)
        {
            if !cursor.class_hit {
                cursor.classes += 1;
                cursor.class_hit = true;
            }
            if site.owner == Kind::Method {
                if cursor.method_hits == 0 {
                    cursor.methods += 1;
                }
                cursor.method_hits += 1;
            }
            if projection == "full" && cursor.matches >= query.offset {
                let item = index.item(&symbol, &site)?;
                let size = serde_json::to_vec(&item)?.len();
                ensure!(
                    size <= 100_000,
                    "Identifier record exceeds scan page budget"
                );
                bytes += size;
                items.push(item);
            }
            cursor.matches += 1;
            if projection == "full" && (items.len() >= query.limit || bytes >= 900_000) {
                break;
            }
        }
    }
    let complete = cursor.class == names.len();
    let next = if complete {
        None
    } else {
        let payload = serde_json::to_string(&cursor)?;
        Some(format!(
            "{:x}:{payload}",
            Sha256::digest(format!("{session}:{payload}:{session}"))
        ))
    };
    Ok(
        json!({"mode":"identifiers","execution_mode":"scan","projection":projection,"items":items,
        "total":complete.then_some(cursor.matches),"matches_so_far":cursor.matches,"page_matches":cursor.matches-initial_matches,
        "unique_classes_so_far":cursor.classes,"unique_methods_so_far":cursor.methods,
        "unique_class_count":complete.then_some(cursor.classes),"unique_method_count":complete.then_some(cursor.methods),
        "coverage_complete":complete,"exhausted":complete,"next_cursor":next,"next_offset":Value::Null,
        "scanned_classes":cursor.class,"page_completed_classes":cursor.class-first_class,"total_classes":names.len(),
        "index_complete":false,"index_cached":false,"pagination_order":"class_then_metadata_and_dex_instruction",
        "pagination":"cursor","offset":query.offset,"scope":IDENTIFIER_SCOPE,
        "caller_projection_scope":"Methods containing matching occurrences; class and field-only matches contribute to summary counts but have no caller row",
        "elapsed_ms":start.elapsed().as_millis()}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn walk(mut args: Value, steps: usize) -> (Vec<Value>, Value) {
        let classes = super::super::tests::classes();
        let mut items = vec![];
        for _ in 0..500 {
            let page = run_bounded(
                classes.clone(),
                &args,
                Query::parse(&args).unwrap(),
                &false.into(),
                "test-project",
                Duration::from_secs(60),
                steps,
            )
            .unwrap();
            items.extend(page["items"].as_array().unwrap().iter().cloned());
            if page["coverage_complete"] == true {
                return (items, page);
            }
            assert!(page["total"].is_null());
            args["resume_cursor"] = page["next_cursor"].clone();
        }
        panic!("cursor failed to make progress");
    }
    #[test]
    fn resumed_scan_matches_original_index_for_every_occurrence_kind() {
        let classes = super::super::tests::classes();
        let index = Index::build(classes, &false.into()).unwrap();
        for kind in ["all", "method", "field", "class"] {
            for occurrence in ["all", "reference", "declaration"] {
                let args =
                    json!({"query":".","symbol_kind":kind,"occurrence":occurrence,"limit":500});
                let expected = index
                    .search(&Query::parse(&args).unwrap(), &false.into())
                    .unwrap();
                let (items, page) = walk(args, 3);
                let mut actual: Vec<_> = items.iter().map(Value::to_string).collect();
                actual.sort();
                let mut wanted: Vec<_> = expected["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(Value::to_string)
                    .collect();
                wanted.sort();
                assert_eq!(actual, wanted, "{kind}/{occurrence}");
                assert_eq!(page["total"], expected["total"]);
            }
        }
    }
    #[test]
    fn compact_projections_count_all_occurrences_and_deduplicate_methods() {
        let args = json!({"query":"framework.Api","projection":"summary","limit":1});
        let (items, summary) = walk(args.clone(), 2);
        assert!(items.is_empty());
        assert_eq!(summary["unique_class_count"], 2);
        assert_eq!(summary["unique_method_count"], 2);
        let mut callers = args;
        callers["projection"] = json!("callers");
        let (items, page) = walk(callers, 2);
        assert_eq!(items.len(), 2);
        assert!(items.iter().all(|r| r.get("instruction_words").is_none()));
        assert_eq!(page["total"], summary["total"]);
        assert_ne!(items[0]["method_id"], items[1]["method_id"]);
    }
    #[test]
    fn cursor_rejects_changed_query_project_corruption_and_cancellation() {
        let classes = super::super::tests::classes();
        let args = json!({"query":"Api","limit":1});
        let page = run_bounded(
            classes.clone(),
            &args,
            Query::parse(&args).unwrap(),
            &false.into(),
            "one",
            Duration::from_secs(60),
            1,
        )
        .unwrap();
        let mut resumed = args;
        resumed["resume_cursor"] = page["next_cursor"].clone();
        assert!(
            run(
                classes.clone(),
                &resumed,
                Query::parse(&resumed).unwrap(),
                &false.into(),
                "two"
            )
            .is_err()
        );
        let mut changed = resumed.clone();
        changed["query"] = json!("Other");
        assert!(
            run(
                classes.clone(),
                &changed,
                Query::parse(&changed).unwrap(),
                &false.into(),
                "one"
            )
            .is_err()
        );
        let mut damaged = resumed.clone();
        damaged["resume_cursor"] =
            json!(format!("{} ", damaged["resume_cursor"].as_str().unwrap()));
        assert!(
            run(
                classes.clone(),
                &damaged,
                Query::parse(&damaged).unwrap(),
                &false.into(),
                "one"
            )
            .is_err()
        );
        assert!(
            run(
                classes,
                &resumed,
                Query::parse(&resumed).unwrap(),
                &true.into(),
                "one"
            )
            .is_err()
        );
    }
}
