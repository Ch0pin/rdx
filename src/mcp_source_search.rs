//! Generated-source search paginated by hits, with a bounded resumable scan.
use super::{NativeEngine, Project, required};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

/// Empty pages with remaining work must never read as an APK-wide negative.
pub(super) fn page_status(empty: bool, more: bool, errors: bool) -> Value {
    json!({"complete":!more && !errors,
        "message":if more && empty {"No matches on this page; the scan is incomplete. Follow next_offset and next_source_offset, or use mode=identifiers for method/type/field names."}
            else if more {"More source remains; follow both continuation offsets."}
            else if errors {"Scan finished with class errors; absence is inconclusive."}
            else {"Scan complete."}})
}

/// `offset` remains a filtered-class cursor; `limit` bounds returned occurrences.
/// A time boundary returns the next untouched class so empty pages are resumable.
pub(super) fn search_hits(
    engine: &mut NativeEngine,
    project: &Project,
    args: &Value,
    cancel: &AtomicBool,
) -> Result<Value> {
    search_with(&project.classes, args, cancel, |class| {
        engine.decompile_search_cancellable(class, cancel)
    })
}

fn search_with(
    project_classes: &[String],
    args: &Value,
    cancel: &AtomicBool,
    mut render: impl FnMut(&str) -> Result<crate::engine::DecompiledCode>,
) -> Result<Value> {
    let query = required(args, "query")?;
    ensure!(
        !query.is_empty() && query.len() <= 4096,
        "Query must contain 1..4096 bytes"
    );
    let package = args["package_prefix"]
        .as_str()
        .unwrap_or("")
        .trim_end_matches('.');
    let classes: Vec<_> = project_classes
        .iter()
        .filter(|class| {
            package.is_empty()
                || class.as_str() == package
                || class
                    .strip_prefix(package)
                    .is_some_and(|tail| tail.starts_with('.'))
        })
        .collect();
    let offset = args["offset"].as_u64().unwrap_or(0) as usize;
    let limit = args["limit"].as_u64().unwrap_or(100) as usize;
    let source_offset = args["source_offset"].as_u64().unwrap_or(0) as usize;
    ensure!(
        (1..=500).contains(&limit) && offset <= classes.len(),
        "Invalid search page"
    );
    ensure!(
        offset < classes.len() || source_offset == 0,
        "Source offset without a class"
    );
    let started = Instant::now();
    let mut items = Vec::new();
    let mut errors = Vec::new();
    let mut error_count = 0usize;
    let mut scanned = 0;
    let mut next = None;
    let mut next_source = 0;
    let mut boundary = "exhausted";
    'classes: for (index, class) in classes.iter().enumerate().skip(offset) {
        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        // A single class render is cooperatively cancellable by the request token.
        if scanned > 0 && started.elapsed() >= Duration::from_secs(20) {
            next = Some(index);
            boundary = "time_budget";
            break;
        }
        scanned += 1;
        let code = match render(class) {
            Ok(code) => code,
            Err(error) => {
                ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
                error_count += 1;
                if errors.len() < 100 {
                    let message: String = format!("{error:#}").chars().take(4096).collect();
                    errors.push(json!({"class_id":class,"error":message}));
                }
                continue;
            }
        };
        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        let start_char = if index == offset { source_offset } else { 0 };
        let start_byte = if start_char == code.source.chars().count() {
            code.source.len()
        } else {
            code.source
                .char_indices()
                .nth(start_char)
                .map(|(index, _)| index)
                .context("Invalid source_offset")?
        };
        for matched in crate::resource_search::literal_ranges(&code.source, &code.links, query)
            .filter(|range| range.start >= start_byte)
        {
            ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
            let byte = matched.start;
            let start = code.source[..byte].chars().count();
            let end = start + code.source[matched].chars().count();
            let line = code.source[..byte]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count()
                + 1;
            let snippet: String = code.source[byte..]
                .chars()
                .take(200)
                .take_while(|ch| *ch != '\n')
                .collect();
            items.push(json!({"class_id":class,"source_hash":code.source_hash,"start":start,"end":end,"line":line,"snippet":snippet,"format":"java-or-mixed-dex"}));
            if items.len() == limit {
                next = Some(index);
                next_source = end;
                boundary = "hit_limit";
                break 'classes;
            }
        }
    }
    Ok(
        json!({"items":items,"pagination":"hits","scope":"generated Java and mixed DEX; case-sensitive literal plus exact numeric aliases for resolved resource links; resource files excluded","offset":offset,"limit":limit,"scanned_classes":scanned,"total_classes":classes.len(),"page_status":page_status(items.is_empty(),next.is_some(),error_count>0),"errors":errors,"next_offset":next,"next_source_offset":next.map(|_| next_source),"exhausted":next.is_none(),"scan_boundary":boundary,"page_error_count":error_count,"errors_truncated":error_count > errors.len(),"scan_budget_ms":20000,"elapsed_ms":started.elapsed().as_millis()}),
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
        assert_eq!(page["errors"].as_array().unwrap().len(), 100);
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
}
