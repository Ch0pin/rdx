//! Reverse constructor-discriminator lookup using the existing branch proof.
use crate::{
    engine::{NativeEngine, Project},
    native_engine::disassembly::method_id,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

pub(crate) fn search(
    engine: &NativeEngine,
    project: &Project,
    args: &Value,
    cancel: &AtomicBool,
) -> Result<Value> {
    let owner = args["class_id"].as_str().context("Missing class_id")?;
    let class = engine.dex_class(owner).context("Unknown lambda class")?;
    let case = args["case"]
        .as_i64()
        .and_then(|v| i32::try_from(v).ok())
        .context("case must be a signed 32-bit integer")?;
    let limit = args["limit"].as_u64().unwrap_or(25) as usize;
    ensure!(
        (1..=100).contains(&limit),
        "limit must be 1..100 candidate constructor sites"
    );
    let dispatcher = args["method_id"]
        .as_str()
        .map(crate::method_lookup::normalize)
        .transpose()?;
    if let Some(id) = &dispatcher {
        ensure!(
            class.methods.iter().any(|m| method_id(m) == *id),
            "Dispatcher must be an exact method declared by class_id"
        );
    }
    let binding = json!({"class_id":owner,"case":case,"method_id":dispatcher});
    let mut next = if let Some(encoded) = args["resume_cursor"].as_str() {
        ensure!(encoded.len() <= 8192, "Invalid resume_cursor");
        let cursor: Value = serde_json::from_str(encoded).context("Invalid resume_cursor")?;
        ensure!(
            cursor["query"] == binding,
            "STALE_CURSOR: lambda query changed"
        );
        Some(
            cursor["scan"]
                .as_str()
                .context("Invalid scan cursor")?
                .to_owned(),
        )
    } else {
        None
    };
    let classes: Vec<_> = project
        .classes
        .iter()
        .map(|n| engine.dex_class(n).context("Incomplete project"))
        .collect::<Result<_>>()?;
    let mut items = vec![];
    let mut unresolved = vec![];
    let mut examined = 0;
    let mut complete = false;
    let started = Instant::now();
    let prefix = format!("{owner}.<init>(");
    while examined < limit && started.elapsed() < Duration::from_secs(15) {
        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        let mut request = json!({"query":prefix,"symbol_kind":"method","occurrence":"reference","search_mode":"scan","limit":1});
        if let Some(cursor) = &next {
            request["resume_cursor"] = json!(cursor);
        }
        let page = engine.search_identifiers(&request, cancel)?;
        next = page["next_cursor"].as_str().map(str::to_owned);
        complete = page["coverage_complete"] == true;
        let records = page["items"]
            .as_array()
            .context("Missing constructor scan records")?;
        for record in records {
            if !record["symbol_id"]
                .as_str()
                .is_some_and(|id| id.starts_with(&prefix))
            {
                continue;
            }
            examined += 1;
            let caller = record["method_id"]
                .as_str()
                .context("Constructor caller missing")?;
            let pc = record["code_unit_offset"]
                .as_u64()
                .context("Constructor offset missing")? as usize;
            let proof = crate::lambda_branch::analyze(&classes, caller, pc, &|| {
                cancel.load(Ordering::Relaxed)
            });
            ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
            if proof["status"] != "resolved" {
                unresolved.push(json!({"caller_method_id":caller,"call_site_offset":pc,"reason":proof["reason"]}));
                continue;
            }
            let selected: Vec<_> = proof["dispatchers"].as_array().into_iter().flatten()
                .filter(|d| dispatcher.as_ref().is_none_or(|id| d["method_id"] == *id))
                .filter(|d| d["discriminator"] == case)
                .map(|d| json!({"method_id":d["method_id"],"discriminator":d["discriminator"],"field_id":d["field_id"],"switch_offset":d["switch_offset"],"selected_target_offset":d["selected_target_offset"],"selected_default":d["selected_default"],"constructor_argument_word":d["constructor_argument_word"],"discriminator_source":d["discriminator_source"]})).collect();
            if !selected.is_empty() {
                items.push(json!({"caller_method_id":caller,"call_site_offset":pc,"allocation_offset":proof["allocation_offset"],"constructor_method_id":proof["constructor_method_id"],"dex_entry":record["dex_entry"],"dispatchers":selected,"relationship":"proven_constructor_discriminator"}));
            } else if let Some(id) = &dispatcher
                && !proof["dispatchers"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|d| d["method_id"] == *id)
            {
                unresolved.push(json!({"caller_method_id":caller,"call_site_offset":pc,"reason":"Requested dispatcher has no proven discriminator mapping"}));
            }
        }
        if complete || records.is_empty() {
            break;
        }
    }
    let cursor = next.map(|scan| json!({"query":binding,"scan":scan}).to_string());
    Ok(
        json!({"class_id":owner,"case":case,"method_id":dispatcher,"items":items,
        "unresolved_sites":unresolved,"candidate_sites_examined":examined,"scan_complete":complete,
        "page_proofs_complete":unresolved.is_empty(),"next_cursor":cursor,"offset_unit":"16-bit DEX code units",
        "scope":"Known invoke-direct constructor sites. Constant/move arguments or fixed constructor constants, constructor field stores and dispatcher switch are proven by get_lambda_branch. Unresolved sites must be reviewed; normal constructor completion and no reflection/native discriminator mutation are assumed."}),
    )
}
