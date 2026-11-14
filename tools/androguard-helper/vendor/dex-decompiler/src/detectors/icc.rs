//! Inter-component (ICC) intent edges: putExtra(key) + launch with explicit target.
//!
//! Links const-key extras to an explicit component (`setClass` / `setClassName` /
//! `setComponent`) in the same method that calls `startActivity` / `startService` /
//! `sendBroadcast`. Cross-component getExtra matching is done in AndroHunt
//! (`icc::link_extra_readers`).

use crate::decompile::value_flow::ValueFlowAnalysisOwned;
use crate::detectors::types::{invoke_scan, VulnFinding};

const LAUNCH_SINKS: &[&str] = &[
    "startActivity",
    "startActivityForResult",
    "startService",
    "startForegroundService",
    "bindService",
    "sendBroadcast",
    "sendOrderedBroadcast",
];

const TARGET_SETTERS: &[&str] = &[
    "setClassName",
    "setClass",
    "setComponent",
    "Intent.setClassName",
    "Intent.setClass",
    "Intent.setComponent",
    "ComponentName.<init>",
];

fn const_strings(owned: &ValueFlowAnalysisOwned) -> Vec<String> {
    let mut out = Vec::new();
    for insn in owned.insn_at.values() {
        let lower = insn.as_str();
        if !(lower.contains("const-string") || lower.contains("const-string/jumbo")) {
            continue;
        }
        // const-string vN, "..."
        if let Some(start) = lower.find('"') {
            if let Some(end) = lower[start + 1..].find('"') {
                let s = &lower[start + 1..start + 1 + end];
                if !s.is_empty() && s.len() < 200 {
                    out.push(s.to_string());
                }
            }
        }
    }
    out
}

fn has_put_extra(owned: &ValueFlowAnalysisOwned) -> bool {
    owned
        .invoke_method_map
        .values()
        .any(|m| m.contains("putExtra") || m.contains("putString"))
}

fn has_launch(owned: &ValueFlowAnalysisOwned) -> bool {
    owned
        .invoke_method_map
        .values()
        .any(|m| LAUNCH_SINKS.iter().any(|s| m.contains(s)))
}

fn has_target_setter(owned: &ValueFlowAnalysisOwned) -> bool {
    owned
        .invoke_method_map
        .values()
        .any(|m| TARGET_SETTERS.iter().any(|s| m.contains(s)))
}

fn looks_like_class_name(s: &str) -> bool {
    s.contains('.')
        && !s.contains(' ')
        && s.chars().next().is_some_and(|c| c.is_ascii_lowercase() || c == 'L')
        && s.chars().any(|c| c.is_ascii_uppercase() || c == '/')
}

fn looks_like_extra_key(s: &str) -> bool {
    !looks_like_class_name(s)
        && !s.starts_with("http")
        && !s.contains('/')
        && s.len() >= 2
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
}

/// Scan one method for ICC launch-with-extras (explicit target preferred).
pub fn scan_icc_extra(
    owned: &ValueFlowAnalysisOwned,
    class_name: &str,
    method_name: &str,
) -> Vec<VulnFinding> {
    if !has_put_extra(owned) || !has_launch(owned) {
        return Vec::new();
    }
    let strings = const_strings(owned);
    let keys: Vec<String> = strings
        .iter()
        .filter(|s| looks_like_extra_key(s))
        .cloned()
        .collect();
    let targets: Vec<String> = strings
        .iter()
        .filter(|s| looks_like_class_name(s))
        .cloned()
        .collect();

    // Require either an explicit target setter or at least one class-like const.
    if !has_target_setter(owned) && targets.is_empty() {
        return Vec::new();
    }
    if keys.is_empty() {
        return Vec::new();
    }

    let mut findings = invoke_scan(owned, class_name, method_name, "icc_extra_flow", LAUNCH_SINKS);
    findings.truncate(2);
    for f in &mut findings {
        f.category = "icc_extra_flow".into();
        f.refresh_category_meta();
        f.message = format!(
            "{}\nicc: keys={:?} targets={:?} target_setter={}",
            f.message,
            keys.iter().take(8).collect::<Vec<_>>(),
            targets.iter().take(4).collect::<Vec<_>>(),
            has_target_setter(owned)
        );
        f.problem = format!(
            "putExtra keys {:?} launched toward {:?}",
            keys.iter().take(6).collect::<Vec<_>>(),
            targets.iter().take(3).collect::<Vec<_>>()
        );
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decompile::cfg::{BlockEnd, CfgBlock, MethodCfg};
    use std::collections::{HashMap, HashSet};

    fn make_cfg(instruction_offsets: Vec<u32>) -> MethodCfg {
        let block = CfgBlock {
            start_offset: *instruction_offsets.first().unwrap_or(&0),
            end_offset: instruction_offsets.last().copied().unwrap_or(0) + 2,
            end: BlockEnd::Exit,
            instruction_offsets: instruction_offsets.clone(),
        };
        let mut block_by_start = HashMap::new();
        block_by_start.insert(block.start_offset, 0);
        MethodCfg {
            blocks: vec![block],
            block_by_start,
            loop_headers: HashSet::new(),
            entry: 0,
            folded_const_offsets: HashSet::new(),
        }
    }

    #[test]
    fn detects_putextra_launch_with_target() {
        let mut invoke_method_map = HashMap::new();
        invoke_method_map.insert(2, "android.content.Intent.putExtra".into());
        invoke_method_map.insert(4, "android.content.Intent.setClassName".into());
        invoke_method_map.insert(6, "android.app.Activity.startActivity".into());
        let mut insn_at = HashMap::new();
        insn_at.insert(0, "const-string v0, \"url\"".into());
        insn_at.insert(1, "const-string v1, \"com.example.TargetActivity\"".into());
        insn_at.insert(2, "invoke-virtual {v2, v0, v3}, putExtra".into());
        insn_at.insert(4, "invoke-virtual {v2, v4, v1}, setClassName".into());
        insn_at.insert(6, "invoke-virtual {v5, v2}, startActivity".into());
        let mut rw_map = HashMap::new();
        rw_map.insert(2, (vec![2, 0, 3], vec![]));
        rw_map.insert(4, (vec![2, 4, 1], vec![]));
        rw_map.insert(6, (vec![5, 2], vec![]));
        let owned = ValueFlowAnalysisOwned {
            cfg: make_cfg(vec![0, 1, 2, 4, 6]),
            rw_map,
            exceptional_edges: vec![],
            api_return_sources: vec![],
            invoke_method_map,
            insn_at,
            registers_size: 8,
            ins_size: 2,
        };
        let findings = scan_icc_extra(&owned, "com.example.A", "onCreate");
        assert!(
            findings.iter().any(|f| f.category == "icc_extra_flow"),
            "{findings:?}"
        );
    }
}
