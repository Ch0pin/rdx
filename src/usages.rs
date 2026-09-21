//! Semantic reference collection, cancellable between source owners.
use crate::search::{SearchDocument, SearchHit, SearchTarget};
use rdx::engine::{ClassUsages, NativeEngine};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Instant;

pub enum UsageUpdate {
    Target(String),
    Batch(Vec<SearchHit>),
    Progress(String),
}
#[derive(Default)]
pub struct UsageSummary {
    pub status: String,
    pub errors: Vec<String>,
}

pub fn collect(
    engine: &mut NativeEngine,
    class: &str,
    offset: usize,
    hash: &str,
    cancel: &AtomicBool,
    mut emit: impl FnMut(UsageUpdate),
) -> UsageSummary {
    let started = Instant::now();
    if cancel.load(Ordering::Relaxed) {
        return UsageSummary {
            status: "Cancelled — no references scanned".into(),
            ..Default::default()
        };
    }
    let target = match engine.resolve_usage_target(class, offset, hash) {
        Ok(target) => target,
        Err(error) => {
            return UsageSummary {
                status: "Find usages failed".into(),
                errors: vec![format!("{error:#}")],
            };
        }
    };
    emit(UsageUpdate::Target(target.label));
    collect_owners(
        target.classes,
        cancel,
        started,
        |owner| engine.usages_in_class(&target.id, owner),
        emit,
    )
}

fn collect_owners(
    owners: Vec<String>,
    cancel: &AtomicBool,
    started: Instant,
    mut fetch: impl FnMut(&str) -> anyhow::Result<ClassUsages>,
    mut emit: impl FnMut(UsageUpdate),
) -> UsageSummary {
    let total = owners.len();
    let (mut scanned, mut count, mut retained) = (0, 0, 0);
    let mut limited = false;
    let mut errors = Vec::new();
    for owner in owners {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        match fetch(&owner) {
            Ok(result) => {
                limited |= result.limited;
                if result.limited && errors.len() < 8 {
                    errors.push(format!("{owner}: source or reference limit reached"));
                }
                // Only documents backing actual hits remain owned by the results
                // window. Scanning a nonmatching class consumes no retained budget.
                let cost = if result.occurrences.is_empty() {
                    0
                } else {
                    result.code.as_ref().map_or(0, |code| {
                        code.source.len()
                            + code
                                .links
                                .iter()
                                .map(|l| std::mem::size_of_val(l) + l.label.len())
                                .sum::<usize>()
                    })
                };
                if retained + cost > 32 * 1024 * 1024 {
                    limited = true;
                    if errors.len() < 8 {
                        errors
                            .push("Usage results reached the 32 MiB retained-source limit".into());
                    }
                    break;
                }
                match hits(result) {
                    Ok(mut batch) => {
                        if !batch.is_empty() {
                            retained += cost;
                        }
                        if batch.len() > 1000 - count {
                            limited = true;
                            batch.truncate(1000 - count);
                        }
                        count += batch.len();
                        if !batch.is_empty() {
                            emit(UsageUpdate::Batch(batch));
                        }
                    }
                    Err(error) => {
                        if errors.len() < 8 {
                            errors.push(error);
                        }
                    }
                }
            }
            Err(error) => {
                if errors.len() < 8 {
                    errors.push(format!("{owner}: {error:#}"));
                }
            }
        }
        scanned += 1;
        emit(UsageUpdate::Progress(format!(
            "Finding references: {scanned}/{total} classes · {count} usages"
        )));
        if count >= 1000 {
            limited |= scanned < total;
            break;
        }
    }
    let state = if cancel.load(Ordering::Relaxed) {
        "Cancelled — partial results"
    } else if limited || !errors.is_empty() {
        "Partial usage results"
    } else {
        "Usages complete"
    };
    UsageSummary {
        status: format!(
            "{state}: {count} references · {scanned}/{total} classes · {:.2} s",
            started.elapsed().as_secs_f64()
        ),
        errors,
    }
}

fn hits(mut result: ClassUsages) -> Result<Vec<SearchHit>, String> {
    if result.occurrences.is_empty() {
        return Ok(Vec::new());
    }
    let Some(code) = result.code else {
        return if result.occurrences.is_empty() {
            Ok(Vec::new())
        } else {
            Err("Usage source is unavailable".into())
        };
    };
    let doc = Arc::new(SearchDocument {
        target: SearchTarget::Class(result.class.clone()),
        name: result.class,
        source: code.source,
        syntax: "java".into(),
        links: code.links,
        source_hash: Some(code.source_hash),
        metadata_complete: true,
    });
    result.occurrences.sort_by_key(|reference| reference.start);
    let source = &doc.source;
    let total_chars = source.chars().count();
    let (mut byte_cursor, mut char_cursor, mut line, mut row_start) = (0, 0, 1, 0);
    let mut output = Vec::new();
    for reference in result.occurrences {
        if reference.start >= reference.end || reference.end > total_chars {
            return Err("Worker returned an invalid usage range".into());
        }
        let byte = byte_cursor
            + source[byte_cursor..]
                .char_indices()
                .nth(reference.start - char_cursor)
                .map_or(source.len() - byte_cursor, |(i, _)| i);
        for (i, _) in source[byte_cursor..byte].match_indices('\n') {
            line += 1;
            row_start = byte_cursor + i + 1;
        }
        byte_cursor = byte;
        char_cursor = reference.start;
        let before: Vec<_> = source[row_start..byte].chars().rev().take(61).collect();
        let mut preview = if before.len() > 60 {
            "…".to_owned()
        } else {
            String::new()
        };
        preview.extend(before.iter().take(60).rev());
        let mut preview = preview.trim_start().to_owned();
        let start = preview.len();
        let tail: String = source[byte..]
            .chars()
            .take_while(|c| *c != '\n')
            .take(180)
            .collect();
        let end = start
            + tail
                .char_indices()
                .nth(reference.end - reference.start)
                .map_or(tail.len(), |(i, _)| i);
        preview.push_str(&tail);
        output.push(SearchHit {
            document: doc.clone(),
            start: reference.start,
            end: reference.end,
            line,
            kind: reference.enclosing,
            preview,
            preview_match: start..end,
        });
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rdx::engine::{DecompiledCode, UsageOccurrence};

    fn source_result(owner: &str, bytes: usize, matched: bool) -> ClassUsages {
        ClassUsages {
            class: owner.into(),
            code: Some(DecompiledCode {
                source: "x".repeat(bytes),
                source_hash: "test".into(),
                links: vec![],
                definitions: vec![],
            }),
            occurrences: if matched {
                vec![UsageOccurrence {
                    start: 0,
                    end: 1,
                    enclosing: "method".into(),
                }]
            } else {
                vec![]
            },
            limited: false,
        }
    }

    #[test]
    fn nonmatching_sources_do_not_exhaust_retained_budget_or_hide_late_matches() {
        let owners: Vec<_> = (0..41).map(|i| i.to_string()).collect();
        let mut hits = Vec::new();
        let summary = collect_owners(
            owners,
            &AtomicBool::new(false),
            Instant::now(),
            |owner| {
                Ok(source_result(
                    owner,
                    if owner == "40" { 1 } else { 1024 * 1024 },
                    owner == "40",
                ))
            },
            |update| {
                if let UsageUpdate::Batch(batch) = update {
                    hits.extend(batch);
                }
            },
        );
        assert!(
            summary
                .status
                .starts_with("Usages complete: 1 references · 41/41"),
            "{}",
            summary.status
        );
        assert!(summary.errors.is_empty());
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].document.name, "40");
    }

    #[test]
    fn actual_retained_sources_remain_bounded() {
        let mut batches = 0;
        let summary = collect_owners(
            vec!["one".into(), "two".into()],
            &AtomicBool::new(false),
            Instant::now(),
            |owner| Ok(source_result(owner, 17 * 1024 * 1024, true)),
            |update| {
                if matches!(update, UsageUpdate::Batch(_)) {
                    batches += 1;
                }
            },
        );
        assert_eq!(batches, 1);
        assert!(
            summary
                .status
                .starts_with("Partial usage results: 1 references")
        );
        assert!(
            summary
                .errors
                .iter()
                .any(|e| e.contains("retained-source limit"))
        );
    }

    #[test]
    fn cancellation_stops_between_nonmatching_sources() {
        let cancel = AtomicBool::new(false);
        let mut fetched = 0;
        let summary = collect_owners(
            vec!["one".into(), "two".into()],
            &cancel,
            Instant::now(),
            |owner| {
                fetched += 1;
                cancel.store(true, Ordering::Relaxed);
                Ok(source_result(owner, 1, false))
            },
            |_| {},
        );
        assert_eq!(fetched, 1);
        assert!(
            summary
                .status
                .starts_with("Cancelled — partial results: 0 references · 1/2")
        );
    }
    #[test]
    fn usage_preview_preserves_exact_unicode_range_and_shared_source() {
        let source = "// 🦀\n  café(); café();";
        let occurrences = source
            .match_indices("café")
            .map(|(byte, _)| {
                let start = source[..byte].chars().count();
                UsageOccurrence {
                    start,
                    end: start + 4,
                    enclosing: "Example.run() void".into(),
                }
            })
            .collect();
        let batch = hits(ClassUsages {
            class: "Example".into(),
            code: Some(DecompiledCode {
                source: source.into(),
                source_hash: "hash".into(),
                links: vec![],
                definitions: vec![],
            }),
            occurrences,
            limited: false,
        })
        .unwrap();
        assert_eq!(batch.len(), 2);
        assert!(Arc::ptr_eq(&batch[0].document, &batch[1].document));
        for hit in batch {
            assert_eq!(hit.line, 2);
            assert_eq!(&hit.preview[hit.preview_match], "café");
        }
    }
}
