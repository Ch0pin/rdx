//! Line-oriented regex search in one resolved method document.
use super::*;
use crate::engine::MethodSourceFormat;
use crate::search::{CompiledQuery, SearchQuery};

const MAX_LINE_BYTES: usize = 256 * 1024;
const MAX_SCAN_BYTES: usize = 4 * 1024 * 1024;
const MAX_SCAN_LINES: usize = 20_000;
const MAX_PREVIEW_CHARS: usize = 512;
const MAX_OUTPUT_BYTES: usize = 512 * 1024;
const SCAN_TIME: Duration = Duration::from_millis(750);

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Position {
    byte: usize,
    chars: usize,
    first_line: usize,
    next_line: usize,
    examined_lines: usize,
    skipped_oversized_lines: usize,
    terminal_line_consumed: bool,
}

fn compile(args: &Value) -> Result<CompiledQuery> {
    CompiledQuery::new(SearchQuery {
        text: required(args, "query")?.into(),
        regex: true,
        case_sensitive: args["case_sensitive"].as_bool().unwrap_or(true),
        ..Default::default()
    })
    .map_err(anyhow::Error::msg)
}

pub(super) fn validate(args: &Value) -> Result<()> {
    ensure!(
        args.get("start_line").is_none() || args.get("resume_cursor").is_none(),
        "Use start_line or resume_cursor"
    );
    if args.get("method_id").is_some() {
        ensure!(
            ["class_name", "method_name", "descriptor"]
                .iter()
                .all(|key| args.get(*key).is_none()),
            "Use either method_id or class_name plus method_name and optional descriptor"
        );
    }
    compile(args).map(|_| ())
}

fn line_text(raw: &str) -> &str {
    raw.strip_suffix('\n')
        .map_or(raw, |line| line.strip_suffix('\r').unwrap_or(line))
}

fn has_empty_final_line(source: &str) -> bool {
    source.is_empty() || source.ends_with('\n')
}

fn lines(source: &str, byte: usize, terminal_line_consumed: bool) -> impl Iterator<Item = &str> {
    source[byte..]
        .split_inclusive('\n')
        .chain((!terminal_line_consumed && has_empty_final_line(source)).then_some(""))
}

fn advance(position: &mut Position, raw: &str) {
    position.byte += raw.len();
    position.chars += raw.chars().count();
    position.next_line += 1;
    position.terminal_line_consumed = raw.is_empty();
}

fn begin(source: &str, line: usize, cancel: &AtomicBool) -> Result<Position> {
    ensure!(line >= 1, "start_line must be one-based");
    let mut position = Position {
        first_line: line,
        next_line: 1,
        ..Default::default()
    };
    for raw in lines(source, 0, false).take(line - 1) {
        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        advance(&mut position, raw);
    }
    ensure!(
        position.next_line == line,
        "start_line is beyond the method document"
    );
    Ok(position)
}

struct Continuation {
    session: [u8; 32],
    binding: Value,
}
impl Continuation {
    fn seal(&self, payload: &str) -> Result<String> {
        let mut hash = Sha256::new();
        hash.update(b"rdx-method-source-search-v1");
        hash.update(self.session);
        hash.update(serde_json::to_vec(&self.binding)?);
        hash.update([0]);
        hash.update(payload.as_bytes());
        hash.update(self.session);
        Ok(format!("{:x}", hash.finalize()))
    }
    fn encode(&self, position: &Position) -> Result<String> {
        let payload = serde_json::to_string(position)?;
        Ok(format!("{}:{payload}", self.seal(&payload)?))
    }
    fn decode(&self, cursor: &str, source: &str) -> Result<Position> {
        let (seal, payload) = cursor
            .split_once(':')
            .context("Invalid method search cursor")?;
        ensure!(
            cursor.len() <= 1024 && self.seal(payload)? == seal,
            "STALE_CURSOR: method source, project or search parameters changed; restart the search"
        );
        let position: Position = serde_json::from_str(payload)?;
        ensure!(
            source.is_char_boundary(position.byte)
                && (position.byte == 0 || source.as_bytes()[position.byte - 1] == b'\n'),
            "Invalid method search cursor line boundary"
        );
        Ok(position)
    }
}

fn excerpt(line: usize, text: &str, focus: Option<(usize, usize)>) -> Value {
    let start = focus.map_or(0, |(start, _)| start.saturating_sub(80));
    let mut chars = text.chars().skip(start);
    let preview: String = chars.by_ref().take(MAX_PREVIEW_CHARS).collect();
    let more = chars.next().is_some();
    json!({"line":line,"column":start+1,"text":preview,"truncated_before":start>0,
        "truncated_after":more,"contains_full_match":focus.map(|(_,end)| end <= start.saturating_add(MAX_PREVIEW_CHARS))})
}

fn context_window(
    source: &str,
    position: &Position,
    raw: &str,
    context_lines: usize,
    focus: (usize, usize),
) -> Vec<Value> {
    let mut result: Vec<_> = source[..position.byte]
        .rsplit_terminator('\n')
        .take(context_lines)
        .enumerate()
        .map(|(index, text)| {
            excerpt(
                position.next_line - index - 1,
                text.strip_suffix('\r').unwrap_or(text),
                None,
            )
        })
        .collect();
    result.reverse();
    result.push(excerpt(position.next_line, line_text(raw), Some(focus)));
    result.extend(
        lines(source, position.byte + raw.len(), raw.is_empty())
            .take(context_lines)
            .enumerate()
            .map(|(index, text)| excerpt(position.next_line + index + 1, line_text(text), None)),
    );
    result
}

fn search_page(
    source: &str,
    mut position: Position,
    continuation: &Continuation,
    matcher: &CompiledQuery,
    args: &Value,
    cancel: &AtomicBool,
) -> Result<Value> {
    let limit = args["limit"].as_u64().unwrap_or(50) as usize;
    let context_lines = args["context_lines"].as_u64().unwrap_or(2) as usize;
    ensure!(
        (1..=100).contains(&limit) && context_lines <= 8,
        "Invalid method search page limits"
    );
    let mut items = Vec::new();
    let mut bytes = 0;
    let mut skipped_examples = Vec::new();
    let first_page_line = position.next_line;
    let first_page_byte = position.byte;
    let started = Instant::now();
    let mut boundary = "source_end";
    for raw in lines(source, position.byte, position.terminal_line_consumed) {
        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        if items.len() == limit {
            boundary = "hit_limit";
            break;
        }
        if position.next_line > first_page_line {
            if position.byte - first_page_byte >= MAX_SCAN_BYTES
                || position.next_line - first_page_line >= MAX_SCAN_LINES
            {
                boundary = "scan_budget";
                break;
            }
            if started.elapsed() >= SCAN_TIME {
                boundary = "time_budget";
                break;
            }
        }
        let text = line_text(raw);
        if text.len() > MAX_LINE_BYTES {
            position.skipped_oversized_lines += 1;
            if skipped_examples.len() < 8 {
                skipped_examples.push(json!({"line":position.next_line,"bytes":text.len()}));
            }
        } else {
            if let Some(matched) = matcher.find_text(text) {
                let start = text[..matched.start()].chars().count();
                let end = start + matched.as_str().chars().count();
                let row = json!({"line":position.next_line,"column":start+1,"end_column":end+1,
                    "source_start":position.chars+start,"source_end":position.chars+end,"empty_match":matched.is_empty(),
                    "window":context_window(source,&position,raw,context_lines,(start,end))});
                let size = serde_json::to_vec(&row)?.len() + 1;
                if size + bytes > MAX_OUTPUT_BYTES {
                    ensure!(
                        !items.is_empty(),
                        "Method search window exceeds output budget"
                    );
                    boundary = "output_budget";
                    break;
                }
                bytes += size;
                items.push(row);
            }
            position.examined_lines += 1;
        }
        advance(&mut position, raw);
    }
    ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
    let done = position.byte == source.len()
        && (!has_empty_final_line(source) || position.terminal_line_consumed);
    let complete = done && position.first_line == 1 && position.skipped_oversized_lines == 0;
    let cursor = if done {
        None
    } else {
        Some(continuation.encode(&position)?)
    };
    Ok(
        json!({"items":items,"line_offset":first_page_line,"next_line":(!done).then_some(position.next_line),"next_cursor":cursor,
        "scan_complete":done,"coverage_complete":complete,"page_boundary":boundary,"line_bytes_limit":MAX_LINE_BYTES,
        "coverage":{"scope":"rendered_method_text","first_line":position.first_line,"scanned_through_line":position.next_line-1,
            "examined_lines":position.examined_lines,"skipped_oversized_lines":position.skipped_oversized_lines,
            "page_skipped_line_examples":skipped_examples},
        "location_units":{"line":"one-based","column":"one-based Unicode scalar","source_offset":"zero-based Unicode scalar","end":"exclusive"}}),
    )
}

pub(super) fn query(
    engine: &mut NativeEngine,
    project: &Project,
    args: &Value,
    cancel: &AtomicBool,
) -> Result<Value> {
    let matcher = compile(args)?;
    let id =
        crate::method_lookup::normalize(&review::member_selector(engine, project, args, true)?)?;
    let requested_format = MethodSourceFormat::parse(args["format"].as_str().unwrap_or("auto"))?;
    let index = engine.search_index();
    let _permit = index
        .source_page(|| cancel.load(Ordering::Relaxed))
        .context("CANCELLED")?;
    let (report, cache_hit, cache_retained) =
        engine.method_search_document(&id, requested_format, cancel)?;
    let owner = crate::method_lookup::owner(&id)?;
    let class = engine
        .dex_class(owner)
        .context("Method declaring class unavailable")?;
    let method = crate::method_lookup::resolve(class, &id)?;
    let body_available = method.code.is_some();
    let source = &report.source;
    let hash = &report.source_hash;
    check_source_hash(args, hash)?;
    let continuation = Continuation {
        session: index.identity().map_err(anyhow::Error::msg)?,
        binding: json!({"method_id":id,"source_hash":hash,"query":args["query"],"requested_format":requested_format.as_str(),
            "case_sensitive":args["case_sensitive"].as_bool().unwrap_or(true)}),
    };
    let position = if let Some(cursor) = args["resume_cursor"].as_str() {
        continuation.decode(cursor, source)?
    } else {
        let line = usize::try_from(args["start_line"].as_u64().unwrap_or(1))?;
        begin(source, line, cancel)?
    };
    let mut result = search_page(source, position, &continuation, &matcher, args, cancel)?;
    result["method_id"] = json!(id);
    result["source_hash"] = json!(hash);
    result["format"] = json!(report.format);
    result["requested_format"] = json!(requested_format.as_str());
    result["source_cache_hit"] = json!(cache_hit);
    result["source_cache_retained"] = json!(cache_retained);
    result["reconstruction_attempted"] = json!(report.reconstruction_attempted);
    result["fallback_reason"] = json!(report.fallback_reason);
    result["body_available"] = json!(body_available);
    result["source_status"] = json!(if !body_available {
        "declaration_only"
    } else {
        match (report.format, requested_format) {
            ("rdx-dex", MethodSourceFormat::Dex) => "requested_dex",
            ("rdx-dex", _) => "dex_fallback",
            ("java", _) => "decompiled",
            _ => "alternate_decompiled",
        }
    });
    result["match_semantics"] = json!(
        "One record per matching logical line. The first regex match on that full line is reported; end coordinates are exclusive."
    );
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cursor() -> Continuation {
        Continuation {
            session: [4; 32],
            binding: json!({"test":"method-search"}),
        }
    }

    #[test]
    fn method_search_preserves_utf8_crlf_and_empty_terminal_line_with_exact_resume() {
        let cancel = AtomicBool::new(false);
        let source = "αβ\r\n🙂xyz\r\n\r\n";
        let args = json!({"query":"β|xyz|^$","limit":1});
        let matcher = compile(&args).unwrap();
        let continuation = cursor();
        let mut position = begin(source, 1, &cancel).unwrap();
        let mut rows = Vec::new();
        loop {
            let page =
                search_page(source, position, &continuation, &matcher, &args, &cancel).unwrap();
            rows.push(page["items"][0].clone());
            if page["scan_complete"] == true {
                assert_eq!(page["coverage_complete"], true);
                break;
            }
            let token = page["next_cursor"].as_str().unwrap();
            assert!(continuation.decode(&format!("{token} "), source).is_err());
            let changed = Continuation {
                session: [4; 32],
                binding: json!({"test":"changed-query"}),
            };
            assert!(changed.decode(token, source).is_err());
            position = continuation.decode(token, source).unwrap();
        }
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0]["source_start"], 1);
        assert_eq!(rows[1]["source_start"], 5);
        assert_eq!(rows[1]["column"], 2);
        assert_eq!(rows[1]["window"][1]["text"], "🙂xyz");
        assert_eq!(rows[2]["empty_match"], true);
        assert_eq!(rows[3]["line"], 4);
        assert_eq!(rows[3]["source_start"], 12);
        let eof = search_page(
            source,
            begin(source, 5, &cancel).unwrap(),
            &continuation,
            &matcher,
            &args,
            &cancel,
        )
        .unwrap();
        assert_eq!(eof["items"], json!([]));
        assert_eq!(eof["scan_complete"], true);
        assert_eq!(eof["coverage_complete"], false);
        assert!(begin(source, 6, &cancel).is_err());
    }

    #[test]
    fn method_search_clips_display_only_and_carries_skipped_line_coverage() {
        let cancel = AtomicBool::new(false);
        let source = format!(
            "{}\n{}needle{}\n",
            "?".repeat(MAX_LINE_BYTES + 1),
            "α".repeat(900),
            "x".repeat(900)
        );
        let args = json!({"query":"needle[x]+","limit":1,"context_lines":1});
        let matcher = compile(&args).unwrap();
        let continuation = cursor();
        let first = search_page(
            &source,
            begin(&source, 1, &cancel).unwrap(),
            &continuation,
            &matcher,
            &args,
            &cancel,
        )
        .unwrap();
        assert_eq!(first["items"][0]["line"], 2);
        assert_eq!(first["items"][0]["column"], 901);
        assert_eq!(first["items"][0]["end_column"], 1807);
        let line = &first["items"][0]["window"][1];
        assert_eq!(line["truncated_before"], true);
        assert_eq!(line["truncated_after"], true);
        assert_eq!(line["contains_full_match"], false);
        assert_eq!(first["coverage"]["skipped_oversized_lines"], 1);
        let token = first["next_cursor"].as_str().unwrap();
        let last = search_page(
            &source,
            continuation.decode(token, &source).unwrap(),
            &continuation,
            &matcher,
            &args,
            &cancel,
        )
        .unwrap();
        assert_eq!(last["scan_complete"], true);
        assert_eq!(last["coverage_complete"], false);
        assert_eq!(last["coverage"]["skipped_oversized_lines"], 1);
        assert_eq!(last["coverage"]["page_skipped_line_examples"], json!([]));
    }

    #[test]
    fn method_search_byte_limited_windows_resume_before_unpublished_hit() {
        let cancel = AtomicBool::new(false);
        let source = format!("hit{}\n", "é".repeat(600)).repeat(120);
        let args = json!({"query":"^hit","limit":100,"context_lines":8});
        let matcher = compile(&args).unwrap();
        let continuation = cursor();
        let mut position = begin(&source, 1, &cancel).unwrap();
        let mut line = 1;
        loop {
            let page =
                search_page(&source, position, &continuation, &matcher, &args, &cancel).unwrap();
            assert!(serde_json::to_vec(&page["items"]).unwrap().len() <= MAX_OUTPUT_BYTES + 2);
            for item in page["items"].as_array().unwrap() {
                assert_eq!(item["line"], line);
                line += 1;
            }
            if page["scan_complete"] == true {
                assert_eq!(page["coverage_complete"], true);
                break;
            }
            assert_eq!(page["page_boundary"], "output_budget");
            assert_eq!(page["next_line"], line);
            position = continuation
                .decode(page["next_cursor"].as_str().unwrap(), &source)
                .unwrap();
        }
        assert_eq!(line, 121);
    }

    #[test]
    fn method_search_uses_source_selectors_format_hash_and_shared_cache() {
        let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/navigation.apk");
        let snapshot = ProjectSnapshot::open(&input).unwrap();
        let cancel = AtomicBool::new(false);
        let mut engine = snapshot.engine.clone();
        let mut args = json!({"instance_id":"i","project_id":"p","signature":"Lsample/Target;->doubleValue(I)I",
            "query":"^","limit":1});
        super::super::validate("search_method_source", &args).unwrap();
        let source = dispatch(
            &mut engine,
            &snapshot.project,
            "get_method_source",
            &json!({"method_id":"sample.Target.doubleValue(I)I"}),
            &cancel,
        )
        .unwrap();
        let first = dispatch(
            &mut engine,
            &snapshot.project,
            "search_method_source",
            &args,
            &cancel,
        )
        .unwrap();
        assert_eq!(first["method_id"], "sample.Target.doubleValue(I)I");
        assert_eq!(
            first["requested_method_id"],
            "Lsample/Target;->doubleValue(I)I"
        );
        assert_eq!(first["format"], "java");
        assert_eq!(first["source_status"], "decompiled");
        assert_eq!(first["source_hash"], source["source_hash"]);
        assert_eq!(first["source_cache_hit"], false);
        assert_eq!(first["source_cache_retained"], true);
        args["resume_cursor"] = first["next_cursor"].clone();
        let second = dispatch(
            &mut snapshot.engine.clone(),
            &snapshot.project,
            "search_method_source",
            &args,
            &cancel,
        )
        .unwrap();
        assert_eq!(second["source_cache_hit"], true);
        assert_eq!(second["items"][0]["line"], 2);
        let mut first_page = args.clone();
        first_page.as_object_mut().unwrap().remove("resume_cursor");
        engine.set_alternate_source_engine(crate::alternate_source::FallbackEngine::RadGuard);
        let switched = dispatch(
            &mut engine,
            &snapshot.project,
            "search_method_source",
            &first_page,
            &cancel,
        )
        .unwrap();
        assert_eq!(switched["source_cache_hit"], false);
        assert_eq!(switched["source_hash"], first["source_hash"]);
        engine.set_alternate_source_engine(crate::alternate_source::FallbackEngine::None);
        let mut named = json!({"instance_id":"i","project_id":"p","class_name":"Target",
            "method_name":"doubleValue","descriptor":"(I)I","query":"return","format":"dex"});
        super::super::validate("search_method_source", &named).unwrap();
        let dex = dispatch(
            &mut engine,
            &snapshot.project,
            "search_method_source",
            &named,
            &cancel,
        )
        .unwrap();
        assert_eq!(dex["format"], "rdx-dex");
        assert_eq!(dex["source_status"], "requested_dex");
        assert_eq!(dex["source_cache_hit"], false);
        assert_eq!(dex["declaring_class_id"], "sample.Target");
        assert_eq!(dex["reconstruction_attempted"], false);
        named["source_hash"] = json!("stale");
        assert!(
            dispatch(
                &mut engine,
                &snapshot.project,
                "search_method_source",
                &named,
                &cancel
            )
            .unwrap_err()
            .to_string()
            .contains("SOURCE_CHANGED")
        );
        named["query"] = json!("[");
        assert!(super::super::validate("search_method_source", &named).is_err());
        let reopened = engine.open(&input).unwrap();
        let result = dispatch(
            &mut engine,
            &reopened,
            "search_method_source",
            &first_page,
            &cancel,
        )
        .unwrap();
        assert_eq!(result["source_cache_hit"], false);
        assert!(
            dispatch(
                &mut engine,
                &reopened,
                "search_method_source",
                &args,
                &cancel
            )
            .unwrap_err()
            .to_string()
            .contains("STALE_CURSOR")
        );
    }
}
