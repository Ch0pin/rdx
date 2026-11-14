//! Generated-source search paginated by hits, with a bounded resumable scan.
use super::{NativeEngine, Project, required};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

/// Agent completion contract, shared by initialize, tool discovery and page messages.
pub(super) const COMPLETION_GUIDANCE: &str = "For source-mode search_code, follow EVERY next_cursor as resume_cursor, keeping the pinned instance_id/project_id, query, filters, scopes and pagination unchanged; legacy clients follow both next_offset and next_source_offset. Honor requested page sizes; limit may change between pages. Continue until exhausted=true and no continuation remains. Inspect EVERY page and accumulate errors, page_error_count, errors_truncated, truncated_resources and other coverage diagnostics across the whole search. Zero hits on a partial page mean the search remains incomplete. page_status.complete describes this response only; a clean final page cannot erase an earlier page's errors or truncation. Report complete coverage or absence within the requested scope only after every continuation is consumed and no unresolved coverage gaps remain across ANY page. Pagination remains client-driven; these instructions guide agents and clients are responsible for following them.";

/// The boolean describes this page; agents must retain diagnostics across all pages.
pub(super) fn page_status(empty: bool, more: bool, errors: bool) -> Value {
    let state = if more && empty {
        "No hits; this search remains incomplete."
    } else if more {
        "More source remains."
    } else if errors {
        "No continuation; this page has coverage diagnostics."
    } else {
        "No continuation; this page has no coverage diagnostics."
    };
    let action = if more {
        "Resume next_cursor as resume_cursor with pinned IDs/filters unchanged (legacy: both next_offset and next_source_offset), until exhausted=true with no continuation."
    } else {
        "Verify exhausted=true. Claim completeness only after every page has no unresolved gaps."
    };
    json!({"complete":!more && !errors,"completion_scope":"page",
        "message":format!("{state} {action} Retain every page's errors/truncation and coverage diagnostics; complete is page-local and earlier gaps remain.")})
}

/// Cold generated-source text requires reconstruction; warm searches reuse the session index. Point identifier-shaped
/// queries at the indexed symbol search whenever this page was empty or the
/// scan is still open, which is where decompilation round trips accumulate.
pub(super) fn identifier_hint(query: &str, empty: bool, exhausted: bool) -> Value {
    let mut chars = query.chars();
    let identifier = chars
        .next()
        .is_some_and(|ch| ch.is_ascii_alphabetic() || matches!(ch, '_' | '$'))
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '$'));
    if identifier && (empty || !exhausted) {
        json!(
            "The query is a plain identifier. mode=identifiers searches DEX class, method and field names from the symbol index without decompiling any class; find_string_references indexes DEX string literals. Cold source searches reconstruct scanned classes; warm searches reuse the shared session source index."
        )
    } else {
        Value::Null
    }
}

/// `offset` remains a filtered-class cursor; `limit` bounds returned occurrences.
/// A time boundary returns the next untouched class so empty pages are resumable.
pub(super) fn search_hits(
    engine: &mut NativeEngine,
    project: &Project,
    args: &Value,
    cancel: &AtomicBool,
) -> Result<Value> {
    use crate::search::{CompiledQuery, SearchQuery, SearchUpdate};
    let query = required(args, "query")?;
    let scope = crate::package_scope::PackageScope::from_args(args)?;
    let resource_scope = args["scope"].as_str().unwrap_or("code");
    ensure!(
        ["code", "resources", "all"].contains(&resource_scope),
        "Invalid source scope"
    );
    let regex = args["regex"].as_bool().unwrap_or(false);
    let sensitive = args["case_sensitive"].as_bool().unwrap_or(true);
    let compiled = CompiledQuery::new(SearchQuery {
        text: query.into(),
        regex,
        case_sensitive: sensitive,
        code: resource_scope != "resources",
        comments: true,
        resources: resource_scope != "code",
        excluded_packages: vec![],
        ..Default::default()
    })
    .map_err(anyhow::Error::msg)?;
    let classes: Vec<_> = project
        .classes
        .iter()
        .filter(|class| resource_scope != "resources" && scope.matches(class))
        .collect();
    let archive = (resource_scope != "code")
        .then(|| {
            engine
                .prepared_container()
                .map(crate::apk::Archive::from_prepared)
        })
        .flatten();
    ensure!(
        resource_scope == "code" || archive.is_some(),
        "Resource search requires a prepared archive"
    );
    let prefix = args["resource_prefix"].as_str().unwrap_or("");
    let resources: Vec<_> = archive
        .as_ref()
        .map(|archive| {
            archive
                .entries
                .iter()
                .filter(|entry| entry.path.starts_with(prefix))
                .collect()
        })
        .unwrap_or_default();
    let total = classes.len() + resources.len();
    let pagination = args["pagination"].as_str().unwrap_or("hits");
    let limit = args["limit"]
        .as_u64()
        .unwrap_or(if pagination == "classes" { 25 } else { 100 }) as usize;
    ensure!((1..=500).contains(&limit), "Invalid search page");
    let binding = crate::engine::source_identity(
        &json!({"session":engine.search_index().identity().map_err(anyhow::Error::msg)?,
        "query":query,"case_sensitive":sensitive,"regex":regex,"scope":resource_scope,
        "package_scope":scope.describe(),"resource_prefix":prefix,"pagination":pagination})
        .to_string(),
    );
    let mut offset = args["offset"].as_u64().unwrap_or(0) as usize;
    let mut source_offset = args["source_offset"].as_u64().unwrap_or(0) as usize;
    if let Some(cursor) = args["resume_cursor"].as_str() {
        ensure!(
            args.get("offset").is_none() && args.get("source_offset").is_none(),
            "Use resume_cursor or offsets"
        );
        let parts: Vec<_> = cursor.split(':').collect();
        ensure!(
            parts.len() == 3 && parts[0] == binding,
            "INVALID_CURSOR: project or source filters changed"
        );
        offset = parts[1].parse().context("Invalid cursor offset")?;
        source_offset = parts[2].parse().context("Invalid cursor source offset")?;
    }
    ensure!(
        offset <= total && (offset < total || source_offset == 0),
        "Invalid search page"
    );
    let index = engine.search_index();
    let _permit = index
        .source_page(|| cancel.load(Ordering::Relaxed))
        .context("CANCELLED")?;
    let started = Instant::now();
    let deadline = started + Duration::from_secs(20);
    let end = if pagination == "classes" {
        offset.saturating_add(limit).min(total)
    } else {
        total
    };
    let hit_limit = if pagination == "classes" { 100 } else { limit };
    let mut hits = Vec::with_capacity(hit_limit);
    let mut errors = Vec::new();
    let mut error_count = 0;
    let mut scanned_classes = 0;
    let mut scanned_resources = 0;
    let mut skipped_binary = 0;
    let mut truncated_resources = 0;
    let mut position = offset;
    let mut next_source = 0;
    let mut boundary = "exhausted";
    if position < classes.len() && position < end {
        let summary = crate::search::source_page(
            engine,
            &classes[position..end.min(classes.len())],
            &compiled,
            cancel,
            hit_limit,
            source_offset,
            deadline,
            &mut |update| {
                if let SearchUpdate::Batch(batch) = update {
                    hits.extend(batch);
                }
            },
        );
        ensure!(
            !summary.cancelled && !cancel.load(Ordering::Relaxed),
            "CANCELLED"
        );
        ensure!(
            !summary
                .errors
                .iter()
                .any(|error| error == "Invalid source_offset"),
            "Invalid source_offset"
        );
        scanned_classes = summary.scanned + usize::from(summary.page_source_offset > 0);
        error_count += summary.error_count;
        errors.extend(summary.errors.into_iter().map(|error| {
            if let Some((class, message)) = error.split_once(": ") {
                json!({"class_id":class,"error":message})
            } else {
                json!({"error":error})
            }
        }));
        position += summary.page_position;
        next_source = summary.page_source_offset;
        if summary.limited {
            boundary = if summary.time_budget {
                "time_budget"
            } else {
                "hit_limit"
            };
        }
    }
    while position >= classes.len()
        && position < end
        && hits.len() < hit_limit
        && boundary == "exhausted"
    {
        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        if scanned_classes + scanned_resources > 0 && Instant::now() >= deadline {
            boundary = "time_budget";
            break;
        }
        let entry = resources[position - classes.len()];
        let from = if position == offset { source_offset } else { 0 };
        let result = crate::search::source_resource(
            engine,
            archive.as_ref().expect("resource archive"),
            entry,
            &compiled,
            cancel,
            hit_limit - hits.len() + 1,
            from,
        );
        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        scanned_resources += 1;
        match result {
            Ok((mut found, skipped, note)) => {
                skipped_binary += usize::from(skipped);
                if let Some(note) = note {
                    truncated_resources += 1;
                    error_count += 1;
                    if errors.len() < 100 {
                        errors.push(json!({"path":entry.path,"error":note}));
                    }
                }
                let room = hit_limit - hits.len();
                if found.len() > room {
                    found.truncate(room);
                    next_source = found.last().map_or(0, |hit| hit.end);
                    hits.extend(found);
                    boundary = "hit_limit";
                    break;
                }
                hits.extend(found);
            }
            Err(error) => {
                ensure!(error != "Invalid source_offset", "Invalid source_offset");
                error_count += 1;
                if errors.len() < 100 {
                    errors.push(json!({"path":entry.path,"error":error}));
                }
            }
        }
        position += 1;
        next_source = 0;
    }
    if boundary == "exhausted" && position < total {
        boundary = if hits.len() == hit_limit {
            "hit_limit"
        } else {
            "class_limit"
        };
    }
    let more = position < total;
    let cursor = more.then(|| format!("{binding}:{position}:{next_source}"));
    let items: Vec<_> = hits.into_iter().map(|hit| {
        let resource = matches!(hit.origin.target, crate::search::SearchTarget::Resource(_));
        let mut item = json!({"source_hash":hit.origin.source_hash,"start":hit.start,"end":hit.end,
            "line":hit.line,"snippet":hit.preview,"format":if resource {hit.origin.syntax.as_str()} else {"java-or-mixed-dex"}});
        item[if resource {"path"} else {"class_id"}] = json!(hit.origin.name);
        item
    }).collect();
    Ok(
        json!({"items":items,"pagination":pagination,"scope":resource_scope,"regex":regex,
        "case_sensitive":sensitive,"offset":offset,"limit":limit,"scanned_classes":scanned_classes,
        "total_classes":classes.len(),"scanned_resources":scanned_resources,"total_resources":resources.len(),
        "skipped_binary_resources":skipped_binary,"truncated_resources":truncated_resources,
        "page_status":page_status(items.is_empty(),more,error_count>0),"errors":errors,
        "next_offset":more.then_some(position),"next_source_offset":more.then_some(next_source),
        "next_cursor":cursor,"exhausted":!more,"scan_boundary":boundary,"page_error_count":error_count,
        "errors_truncated":error_count>errors.len(),"scan_budget_ms":20000,"elapsed_ms":started.elapsed().as_millis(),
        "package_scope":scope.describe(),"resource_prefix":prefix,
        "hint":identifier_hint(query,items.is_empty(),!more)}),
    )
}

#[cfg(test)]
fn search_with(
    project_classes: &[String],
    args: &Value,
    cancel: &AtomicBool,
    mut render: impl FnMut(&str) -> Result<crate::engine::DecompiledCode>,
) -> Result<Value> {
    use std::sync::Arc;
    ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
    let scope = crate::package_scope::PackageScope::from_args(args)?;
    let mut engine = NativeEngine::start()?;
    for class in project_classes.iter().filter(|class| scope.matches(class)) {
        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        // Only test data is synthetic; pagination, matching, errors and cancellation
        // all execute the production shared pipeline. Failed synthetic renders
        // remain uncached and exercise the native renderer's real error path.
        if let Ok(code) = render(class) {
            let target = crate::search::SearchTarget::Class(class.clone());
            engine.search_index().insert(
                target.clone(),
                &Some((
                    Arc::new(crate::search::SearchDocument {
                        target,
                        name: class.clone(),
                        source: code.source,
                        syntax: "java".into(),
                        links: code.links,
                        source_hash: Some(code.source_hash),
                        metadata_complete: true,
                    }),
                    Arc::new(code.definitions),
                    None,
                )),
            );
        }
    }
    search_hits(
        &mut engine,
        &Project {
            classes: project_classes.to_vec(),
            resources: vec![],
        },
        args,
        cancel,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn code(source: &str) -> crate::engine::DecompiledCode {
        crate::engine::DecompiledCode {
            source: source.into(),
            links: vec![],
            definitions: vec![],
            source_hash: crate::engine::source_identity(source),
        }
    }
    fn classes(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).into()).collect()
    }

    #[test]
    fn api_identifier_after_five_hundred_empty_classes_is_found_in_one_request() {
        let mut names: Vec<_> = (0..550).map(|index| format!("app.C{index}")).collect();
        names.push("app.Target".into());
        let page = search_with(
            &names,
            &json!({"query":"addJavascriptInterface","limit":500}),
            &AtomicBool::new(false),
            |name| {
                Ok(code(if name == "app.Target" {
                    "webview.addJavascriptInterface(bridge);"
                } else {
                    "class Empty {}"
                }))
            },
        )
        .unwrap();
        assert_eq!(page["scanned_classes"], 551);
        assert_eq!(page["items"].as_array().unwrap().len(), 1);
        assert_eq!(page["items"][0]["class_id"], "app.Target");
        assert_eq!(page["exhausted"], true);
    }

    #[test]
    fn hit_cursor_resumes_unicode_source_without_duplicates() {
        let names = classes(&["app.A"]);
        let cancel = AtomicBool::new(false);
        let mut args = json!({"query":"api","limit":1});
        let mut starts = Vec::new();
        let mut hashes = Vec::new();
        loop {
            let page = search_with(&names, &args, &cancel, |_| {
                Ok(code("é api(); api(); api();"))
            })
            .unwrap();
            for hit in page["items"].as_array().unwrap() {
                starts.push(hit["start"].as_u64().unwrap());
                hashes.push(hit["source_hash"].clone());
            }
            if page["exhausted"] == true {
                break;
            }
            args["offset"] = page["next_offset"].clone();
            args["source_offset"] = page["next_source_offset"].clone();
        }
        assert_eq!(starts, vec![2, 9, 16]);
        assert!(hashes.windows(2).all(|pair| pair[0] == pair[1]));
    }

    #[test]
    fn empty_results_exhaust_the_complete_filter() {
        let page = search_with(
            &classes(&["app.A", "app.B"]),
            &json!({"query":"missing"}),
            &AtomicBool::new(false),
            |_| Ok(code("present")),
        )
        .unwrap();
        assert_eq!(page["scanned_classes"], 2);
        assert_eq!(page["items"], json!([]));
        assert_eq!(page["exhausted"], true);
        assert_eq!(page["next_offset"], Value::Null);
        assert!(page["hint"].as_str().unwrap().contains("mode=identifiers"));
        assert_eq!(page["package_scope"]["prefixes"], json!([]));
    }

    #[test]
    fn package_scope_lists_and_obfuscated_root_filter_classes() {
        let names = classes(&[
            "app.A",
            "app.sub.B",
            "application.C",
            "wr.b$a",
            "tk.a",
            "Top",
            "com.vendor.D",
        ]);
        let page = search_with(
            &names,
            &json!({"query":"api","package_prefix":["app.","obfuscated-root"]}),
            &AtomicBool::new(false),
            |_| Ok(code("api();")),
        )
        .unwrap();
        assert_eq!(page["total_classes"], 5);
        let found: Vec<_> = page["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|hit| hit["class_id"].as_str().unwrap())
            .collect();
        assert_eq!(found, ["app.A", "app.sub.B", "wr.b$a", "tk.a", "Top"]);
        assert_eq!(page["pagination"], "hits");
        assert_eq!(page["package_scope"]["prefixes"], json!(["app"]));
        assert_eq!(page["package_scope"]["obfuscated_root"], true);
        assert_eq!(page["hint"], Value::Null);
        let error = search_with(
            &names,
            &json!({"query":"api","package_prefix":["app", ""]}),
            &AtomicBool::new(false),
            |_| Ok(code("api();")),
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("must not be empty"));
    }

    #[test]
    fn identifier_hint_covers_empty_or_open_scans_only() {
        assert!(
            identifier_hint("GetGoogleIdOption", true, true)
                .as_str()
                .unwrap()
                .contains("mode=identifiers")
        );
        assert!(identifier_hint("GetGoogleIdOption", false, false).is_string());
        assert_eq!(
            identifier_hint("GetGoogleIdOption", false, true),
            Value::Null
        );
        assert_eq!(identifier_hint("api.key", true, true), Value::Null);
        assert_eq!(identifier_hint("9lives", true, true), Value::Null);
        assert_eq!(identifier_hint("", true, true), Value::Null);
    }

    #[test]
    fn package_filter_respects_segments_and_source_search_is_case_sensitive() {
        let page = search_with(
            &classes(&["app.A", "app.sub.B", "application.C", "APP.D"]),
            &json!({"query":"API","package_prefix":"app."}),
            &AtomicBool::new(false),
            |name| Ok(code(if name == "app.A" { "api();" } else { "API();" })),
        )
        .unwrap();
        assert_eq!(page["total_classes"], 2);
        assert_eq!(page["items"].as_array().unwrap().len(), 1);
        assert_eq!(page["items"][0]["class_id"], "app.sub.B");
    }

    #[test]
    fn renderer_failures_are_counted_and_error_details_are_bounded() {
        let names: Vec<_> = (0..105)
            .map(|index| format!("app.C{index}"))
            .chain(std::iter::once("app.Target".into()))
            .collect();
        let page = search_with(
            &names,
            &json!({"query":"api"}),
            &AtomicBool::new(false),
            |name| {
                if name == "app.Target" {
                    Ok(code("api();"))
                } else {
                    anyhow::bail!("unsupported method")
                }
            },
        )
        .unwrap();
        assert_eq!(page["page_error_count"], 105);
        assert!(page["errors"].as_array().unwrap().len() <= 100);
        assert!(page["errors"][0]["class_id"].is_string());
        assert_eq!(page["errors_truncated"], true);
        assert_eq!(page["items"].as_array().unwrap().len(), 1);
        assert_eq!(page["exhausted"], true);
    }

    #[test]
    fn cancellation_before_and_during_render_stops_scan() {
        let names = classes(&["app.A", "app.B"]);
        let cancel = AtomicBool::new(true);
        assert!(
            search_with(&names, &json!({"query":"api"}), &cancel, |_| panic!(
                "render called after cancellation"
            ))
            .unwrap_err()
            .to_string()
            .contains("CANCELLED")
        );
        cancel.store(false, Ordering::Relaxed);
        let result = search_with(&names, &json!({"query":"api"}), &cancel, |_| {
            cancel.store(true, Ordering::Relaxed);
            Ok(code("api();"))
        });
        assert!(result.unwrap_err().to_string().contains("CANCELLED"));
    }
    fn cached_engine(source: &str) -> (NativeEngine, Project) {
        use std::sync::Arc;
        let engine = NativeEngine::start().unwrap();
        let name = "app.A";
        engine.search_index().insert(
            crate::search::SearchTarget::Class(name.into()),
            &Some((
                Arc::new(crate::search::SearchDocument {
                    target: crate::search::SearchTarget::Class(name.into()),
                    name: name.into(),
                    source: source.into(),
                    syntax: "java".into(),
                    links: vec![],
                    source_hash: Some(crate::engine::source_identity(source)),
                    metadata_complete: true,
                }),
                Arc::new(vec![]),
                None,
            )),
        );
        (
            engine,
            Project {
                classes: vec![name.into()],
                resources: vec![],
            },
        )
    }

    #[test]
    fn shared_pages_match_unicode_regex_case_and_skip_zero_width() {
        let (mut engine, project) = cached_engine("é Api API api K k; // api");
        let cancel = AtomicBool::new(false);
        let page = search_hits(&mut engine, &project, &json!({"query":"api"}), &cancel).unwrap();
        assert_eq!(page["items"].as_array().unwrap().len(), 2);
        let mut args = json!({"query":"api|k","regex":true,"case_sensitive":false,"limit":1});
        let mut starts = Vec::new();
        for _ in 0..10 {
            let page = search_hits(&mut engine, &project, &args, &cancel).unwrap();
            starts.extend(
                page["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|item| item["start"].as_u64().unwrap()),
            );
            if page["exhausted"] == true {
                break;
            }
            args["resume_cursor"] = page["next_cursor"].clone();
        }
        assert_eq!(starts, [2, 6, 10, 14, 16, 22]);
        for expression in ["^", "$", "\\b", "missing.*"] {
            let page = search_hits(
                &mut engine,
                &project,
                &json!({"query":expression,"regex":true}),
                &cancel,
            )
            .unwrap();
            assert_eq!(page["items"], json!([]));
            assert_eq!(page["exhausted"], true);
        }
        assert!(
            search_hits(
                &mut engine,
                &project,
                &json!({"query":"[","regex":true}),
                &cancel
            )
            .is_err()
        );
        assert_eq!(engine.search_index().renders(), 0);
    }

    #[test]
    fn numeric_alias_pages_keep_earlier_literals_and_bound_dense_hits() {
        use std::sync::Arc;
        let source = "é 2130772070 R.anim.fade 2130772070";
        let (mut engine, project) = cached_engine(source);
        let key = crate::search::SearchTarget::Class("app.A".into());
        engine.search_index().remove(&key);
        engine.search_index().insert(
            key.clone(),
            &Some((
                Arc::new(crate::search::SearchDocument {
                    target: key,
                    name: "app.A".into(),
                    source: source.into(),
                    syntax: "java".into(),
                    links: vec![crate::engine::CodeLink {
                        start: 13,
                        end: 24,
                        label: "resource://7f010066/anim/fade".into(),
                    }],
                    source_hash: Some(crate::engine::source_identity(source)),
                    metadata_complete: true,
                }),
                Arc::new(vec![]),
                None,
            )),
        );
        let cancel = AtomicBool::new(false);
        let mut args = json!({"query":"2130772070","limit":1});
        let mut starts = Vec::new();
        for _ in 0..5 {
            let page = search_hits(&mut engine, &project, &args, &cancel).unwrap();
            starts.extend(
                page["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|item| item["start"].as_u64().unwrap()),
            );
            if page["exhausted"] == true {
                break;
            }
            args["resume_cursor"] = page["next_cursor"].clone();
        }
        assert_eq!(starts, [2, 13, 25]);
        let regex = search_hits(
            &mut engine,
            &project,
            &json!({"query":"2130772070","regex":true}),
            &cancel,
        )
        .unwrap();
        assert_eq!(
            regex["items"].as_array().unwrap().len(),
            2,
            "regex searches displayed source, without numeric alias expansion"
        );
        let (mut engine, project) = cached_engine(&"api ".repeat(100_000));
        let page = search_hits(
            &mut engine,
            &project,
            &json!({"query":"api","limit":2}),
            &cancel,
        )
        .unwrap();
        assert_eq!(page["items"].as_array().unwrap().len(), 2);
        assert_eq!(page["next_source_offset"], 7);
    }

    #[test]
    fn source_cursor_rejects_query_options_offsets_and_project_changes() {
        let (mut engine, project) = cached_engine("api api");
        let cancel = AtomicBool::new(false);
        let page = search_hits(
            &mut engine,
            &project,
            &json!({"query":"api","limit":1}),
            &cancel,
        )
        .unwrap();
        let args = json!({"query":"api","limit":1,"resume_cursor":page["next_cursor"]});
        let mut resized = args.clone();
        resized["limit"] = json!(2);
        assert!(
            search_hits(&mut engine, &project, &resized, &cancel).is_ok(),
            "limit remains a changeable page control"
        );
        assert_eq!(
            search_hits(&mut engine, &project, &args, &cancel).unwrap()["items"][0]["start"],
            4
        );
        for (key, value) in [
            ("query", json!("API")),
            ("regex", json!(true)),
            ("case_sensitive", json!(false)),
            ("offset", json!(0)),
            ("pagination", json!("classes")),
            ("package_prefix", json!("app")),
        ] {
            let mut changed = args.clone();
            changed[key] = value;
            assert!(
                search_hits(&mut engine, &project, &changed, &cancel).is_err(),
                "{key}"
            );
        }
        assert!(engine.search_index().rejects(
            &crate::search::SearchTarget::Class("app.A".into()),
            "absent",
            true,
            false,
            false
        ));
        assert!(
            search_hits(
                &mut engine,
                &project,
                &json!({"query":"absent","source_offset":99}),
                &cancel
            )
            .is_err()
        );
        let (mut other, _) = cached_engine("api api");
        assert!(search_hits(&mut other, &project, &args, &cancel).is_err());
        assert!(
            search_hits(
                &mut engine,
                &project,
                &json!({"query":"api","source_offset":99}),
                &cancel
            )
            .is_err()
        );
        assert!(
            search_hits(&mut engine, &project, &args, &AtomicBool::new(true))
                .unwrap_err()
                .to_string()
                .contains("CANCELLED")
        );
    }

    #[test]
    fn gui_and_mcp_reuse_each_others_cold_renders() {
        use crate::engine::DecompilerEngine;
        use crate::search::{CompiledQuery, SearchCache, SearchQuery, run_search};
        for gui_first in [true, false] {
            let mut engine = NativeEngine::start().unwrap();
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/navigation.apk");
            let project = engine.open(&path).unwrap();
            let query = CompiledQuery::new(SearchQuery {
                text: "definitely-absent".into(),
                excluded_packages: vec![],
                ..Default::default()
            })
            .unwrap();
            let cancel = AtomicBool::new(false);
            let index = engine.search_index();
            let gui = |engine: &mut NativeEngine| {
                run_search(
                    engine,
                    &mut SearchCache::default(),
                    1,
                    &project.classes,
                    None,
                    &query,
                    &cancel,
                    |_| {},
                )
            };
            let mcp = |engine: &mut NativeEngine| {
                search_hits(
                    engine,
                    &project,
                    &json!({"query":"different-absent"}),
                    &cancel,
                )
                .unwrap()
            };
            if gui_first {
                assert!(gui(&mut engine).errors.is_empty());
            } else {
                assert_eq!(mcp(&mut engine)["exhausted"], true);
            }
            let cold = index.renders();
            assert_eq!(cold as usize, project.classes.len());
            if gui_first {
                assert_eq!(mcp(&mut engine)["exhausted"], true);
            } else {
                assert!(gui(&mut engine).errors.is_empty());
            }
            assert_eq!(
                index.renders(),
                cold,
                "warm cross-interface queries reuse renders"
            );
        }
    }

    #[test]
    fn resource_scope_decodes_xml_and_reports_skipped_binary_content() {
        use crate::engine::DecompilerEngine;
        let mut engine = NativeEngine::start().unwrap();
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/navigation.apk");
        let project = engine.open(&path).unwrap();
        let cancel = AtomicBool::new(false);
        let page = search_hits(&mut engine, &project, &json!({"query":"sample","scope":"resources","resource_prefix":"AndroidManifest.xml","package_prefix":"unrelated"}), &cancel).unwrap();
        assert_eq!(page["total_classes"], 0);
        assert_eq!(page["total_resources"], 1);
        assert_eq!(page["errors"], json!([]));
        assert!(!page["items"].as_array().unwrap().is_empty());
        assert_eq!(page["items"][0]["path"], "AndroidManifest.xml");
        let page = search_hits(
            &mut engine,
            &project,
            &json!({"query":"absent","scope":"resources"}),
            &cancel,
        )
        .unwrap();
        assert!(page["skipped_binary_resources"].as_u64().unwrap() > 0);
        assert_eq!(engine.search_index().renders(), 0);
    }
    #[test]
    #[ignore = "Set RDX_SOURCE_SEARCH_APK; bounded 100-class cold/warm MCP source measurement"]
    fn realistic_source_page_cold_warm_benchmark() {
        let path = std::path::PathBuf::from(std::env::var_os("RDX_SOURCE_SEARCH_APK").unwrap());
        let mut engine = NativeEngine::start().unwrap();
        let loading = Instant::now();
        let project = engine
            .open_with_limits(&path, crate::load_limits::LoadLimits::Large)
            .unwrap();
        let total = project.classes.len();
        let count = total.min(100);
        let sample = Project {
            classes: (0..count)
                .map(|i| project.classes[i * total / count].clone())
                .collect(),
            resources: vec![],
        };
        eprintln!(
            "MCP_SOURCE_BENCH {}",
            json!({"phase":"load","path":path,"project_classes":total,
            "sample_classes":count,"sample":"evenly spaced in project class order","load_profile":"large",
            "elapsed_ms":loading.elapsed().as_millis(),"renders_at_start":engine.search_index().renders()})
        );
        let args =
            json!({"query":"RDX_SOURCE_BENCH_ABSENT_93671","pagination":"classes","limit":100});
        let mut cold = Value::Null;
        for phase in [
            "cold",
            "warm_same_query",
            "warm_other_query",
            "warm_matching_regex",
        ] {
            let mut args = args.clone();
            if phase == "warm_other_query" {
                args["query"] = json!("RDX_SOURCE_BENCH_OTHER_ABSENT_54899");
            }
            if phase == "warm_matching_regex" {
                args["query"] = json!("(?s)^.");
                args["regex"] = json!(true);
                args["pagination"] = json!("hits");
                args["limit"] = json!(500);
            }
            let before = engine.search_index().renders();
            let page = search_hits(&mut engine, &sample, &args, &AtomicBool::new(false)).unwrap();
            eprintln!(
                "MCP_SOURCE_BENCH {}",
                json!({"phase":phase,"sample_classes":count,"scanned_classes":page["scanned_classes"],
                "elapsed_ms":page["elapsed_ms"],"new_renders":engine.search_index().renders()-before,"scan_boundary":page["scan_boundary"],
                "exhausted":page["exhausted"],"page_error_count":page["page_error_count"],"hits":page["items"].as_array().unwrap().len(),
                "index_stats":engine.search_index().stats(),"workers_max":4,"in_flight_estimate_mib":32})
            );
            assert_eq!(page["scanned_classes"], count);
            assert_eq!(page["exhausted"], true);
            assert_eq!(page["page_error_count"], 0);
            if phase == "cold" {
                cold = page;
                assert_eq!(engine.search_index().renders() - before, count as u64);
            } else {
                assert_eq!(engine.search_index().renders(), before);
                if phase == "warm_matching_regex" {
                    assert_eq!(page["items"].as_array().unwrap().len(), count);
                } else {
                    assert_eq!(page["items"], cold["items"]);
                }
            }
        }
    }
    #[test]
    fn resource_text_pagination_reports_truncation_and_xml_decode_errors() {
        use crate::engine::DecompilerEngine;
        use std::io::Write;
        let file = tempfile::Builder::new().suffix(".apk").tempfile().unwrap();
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/navigation.apk");
        std::fs::copy(&path, file.path()).unwrap();
        let writer = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(file.path())
            .unwrap();
        let mut zip = zip::ZipWriter::new_append(writer).unwrap();
        let options = zip::write::SimpleFileOptions::default();
        for (name, data) in [
            ("assets/parity.txt", b"\xef\xbb\xbfApi api api".to_vec()),
            (
                "assets/large.txt",
                "x".repeat(2 * 1024 * 1024 + 1).into_bytes(),
            ),
            ("res/xml/broken.xml", vec![3, 0, 8, 0, 255, 255]),
            ("assets/broken.xml", vec![3, 0, 8, 0, 255, 255]),
        ] {
            zip.start_file(name, options).unwrap();
            zip.write_all(&data).unwrap();
        }
        zip.finish().unwrap();
        let mut engine = NativeEngine::start().unwrap();
        let project = engine.open(file.path()).unwrap();
        let cancel = AtomicBool::new(false);
        let mut args = json!({"query":"api","case_sensitive":false,"scope":"resources","resource_prefix":"assets/parity.txt","limit":1});
        let mut starts = Vec::new();
        for _ in 0..5 {
            let page = search_hits(&mut engine, &project, &args, &cancel).unwrap();
            starts.extend(
                page["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|item| item["start"].as_u64().unwrap()),
            );
            if page["exhausted"] == true {
                break;
            }
            args["resume_cursor"] = page["next_cursor"].clone();
        }
        assert_eq!(starts, [0, 4, 8]);
        let page = search_hits(
            &mut engine,
            &project,
            &json!({"query":"absent","scope":"resources","resource_prefix":"assets/large.txt"}),
            &cancel,
        )
        .unwrap();
        assert_eq!(page["truncated_resources"], 1);
        assert_eq!(page["page_status"]["complete"], false);
        assert_eq!(page["page_error_count"], 1);
        let page = search_hits(
            &mut engine,
            &project,
            &json!({"query":"absent","scope":"resources","resource_prefix":"res/xml/broken.xml"}),
            &cancel,
        )
        .unwrap();
        assert_eq!(page["page_error_count"], 1);
        assert_eq!(page["page_status"]["complete"], false);
        let page = search_hits(
            &mut engine,
            &project,
            &json!({"query":"absent","scope":"resources","resource_prefix":"assets/broken.xml"}),
            &cancel,
        )
        .unwrap();
        assert_eq!(page["page_error_count"], 1);
        assert_eq!(page["page_status"]["complete"], false);
    }
    #[test]
    fn cached_parallel_source_pages_equal_serial_reference_for_hits_and_classes() {
        use crate::engine::DecompilerEngine;
        let mut engine = NativeEngine::start().unwrap();
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/navigation.apk");
        let project = engine.open(&path).unwrap();
        let cancel = AtomicBool::new(false);
        let mut expected = Vec::new();
        for name in &project.classes {
            let code = engine.decompile_search_cancellable(name, &cancel).unwrap();
            for range in crate::resource_search::literal_ranges(&code.source, &code.links, " ") {
                expected.push((
                    name.clone(),
                    code.source[..range.start].chars().count() as u64,
                    code.source[..range.end].chars().count() as u64,
                    code.source_hash.clone(),
                ));
            }
        }
        assert!(expected.len() > 100);
        for pagination in ["hits", "classes"] {
            let mut args = json!({"query":" ","limit":if pagination=="hits" {7}else{2},"pagination":pagination});
            let mut actual = Vec::new();
            for _ in 0..expected.len() + 1 {
                let page = search_hits(&mut engine, &project, &args, &cancel).unwrap();
                assert_eq!(page["errors"], json!([]));
                actual.extend(page["items"].as_array().unwrap().iter().map(|hit| {
                    (
                        hit["class_id"].as_str().unwrap().to_owned(),
                        hit["start"].as_u64().unwrap(),
                        hit["end"].as_u64().unwrap(),
                        hit["source_hash"].as_str().unwrap().to_owned(),
                    )
                }));
                if page["exhausted"] == true {
                    break;
                }
                // Exercise the existing client continuation contract as well as new cursor tests.
                args["offset"] = page["next_offset"].clone();
                args["source_offset"] = page["next_source_offset"].clone();
            }
            assert_eq!(actual, expected, "{pagination}");
        }
    }
    #[test]
    fn source_matching_and_resource_options_validate_without_identifier_filters() {
        let args = json!({"instance_id":"i","project_id":"p","query":"API","regex":true,"case_sensitive":false,"scope":"all","resource_prefix":"assets/"});
        super::super::validate("search_code", &args).unwrap();
        let mut changed = args.clone();
        changed["mode"] = json!("identifiers");
        assert!(
            super::super::validate("search_code", &changed)
                .unwrap_err()
                .to_string()
                .contains("Source options require mode=source")
        );
        let mut changed = args;
        changed["match_mode"] = json!("exact");
        assert!(
            super::super::validate("search_code", &changed)
                .unwrap_err()
                .to_string()
                .contains("Identifier filters")
        );
    }
    #[test]
    fn page_messages_require_cursor_exhaustion_and_every_pages_coverage() {
        for (empty, more, errors) in [
            (true, true, false),
            (false, true, false),
            (true, false, true),
            (true, false, false),
        ] {
            let status = page_status(empty, more, errors);
            assert_eq!(status["complete"], !more && !errors);
            assert_eq!(status["completion_scope"], "page");
            let message = status["message"].as_str().unwrap();
            for clause in [
                "exhausted=true",
                "every page's errors/truncation",
                "coverage diagnostics",
                "complete is page-local",
                "earlier gaps remain",
            ] {
                assert!(message.contains(clause), "{message}");
            }
            assert!(
                message.split_whitespace().count() <= 50,
                "page guidance stays compact: {message}"
            );
            if more {
                assert!(message.contains("next_cursor as resume_cursor"));
                assert!(message.contains("pinned IDs/filters unchanged"));
                assert!(message.contains("next_offset and next_source_offset"));
            }
        }
        assert!(
            page_status(true, true, false)["message"]
                .as_str()
                .unwrap()
                .contains("search remains incomplete")
        );
        let failed_page = page_status(true, false, true);
        let clean_final_page = page_status(true, false, false);
        assert_eq!(failed_page["complete"], false);
        assert_eq!(clean_final_page["complete"], true);
        assert!(
            clean_final_page["message"]
                .as_str()
                .unwrap()
                .contains("earlier gaps remain")
        );
    }
    #[test]
    fn oversized_text_and_unknown_resources_report_incomplete_coverage() {
        use crate::engine::DecompilerEngine;
        use std::io::Write;
        let file = tempfile::Builder::new().suffix(".apk").tempfile().unwrap();
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/navigation.apk");
        std::fs::copy(path, file.path()).unwrap();
        let writer = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(file.path())
            .unwrap();
        let mut zip = zip::ZipWriter::new_append(writer).unwrap();
        let large = "x".repeat(crate::apk::MAX_PREVIEW_BYTES as usize + 1);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for name in [
            "assets/oversized.txt",
            "assets/oversized.unknown",
            "assets/oversized.png",
        ] {
            zip.start_file(name, options).unwrap();
            zip.write_all(large.as_bytes()).unwrap();
        }
        zip.start_file("res/xml/oversized.xml", options).unwrap();
        zip.write_all(b"<root>").unwrap();
        zip.write_all(large.as_bytes()).unwrap();
        zip.write_all(b"</root>").unwrap();
        zip.finish().unwrap();
        let mut engine = NativeEngine::start().unwrap();
        let project = engine.open(file.path()).unwrap();
        let cancel = AtomicBool::new(false);
        for prefix in ["assets/oversized.txt", "assets/oversized.unknown"] {
            let page = search_hits(
                &mut engine,
                &project,
                &json!({"query":"absent","scope":"resources","resource_prefix":prefix}),
                &cancel,
            )
            .unwrap();
            assert_eq!(page["items"], json!([]));
            assert_eq!(page["exhausted"], true);
            assert_eq!(page["page_status"]["complete"], false);
            assert_eq!(page["page_error_count"], 1);
            assert_eq!(page["skipped_binary_resources"], 0);
            let error = page["errors"][0]["error"].as_str().unwrap();
            assert!(
                error.contains("coverage incomplete") && error.contains("archive preview limit"),
                "{error}"
            );
        }
        let binary = search_hits(
            &mut engine,
            &project,
            &json!({"query":"absent","scope":"resources","resource_prefix":"assets/oversized.png"}),
            &cancel,
        )
        .unwrap();
        assert_eq!(binary["skipped_binary_resources"], 1);
        assert_eq!(binary["page_error_count"], 0);
        let xml=search_hits(&mut engine,&project,&json!({"query":"absent","scope":"resources","resource_prefix":"res/xml/oversized.xml"}),&cancel).unwrap();
        assert_eq!(
            xml["page_status"]["complete"], true,
            "decoded XML retains its separate larger bound"
        );
        assert_eq!(xml["page_error_count"], 0);
    }
}
