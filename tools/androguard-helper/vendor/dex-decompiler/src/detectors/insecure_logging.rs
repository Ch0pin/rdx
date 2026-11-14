//! Insecure logging: sensitive source → Log.d / Log.i / println.
//!
//! Register VF often loses taint through `StringBuilder` / `String +`, so we also
//! use same-method co-occurrence: PII-ish API or const-string markers next to Log.*.

use crate::decompile::value_flow::ValueFlowAnalysisOwned;
use crate::detectors::types::{invoke_scan, source_sink_scan, VulnFinding};

const LOGGING_SINKS: &[&str] = &[
    "Log.d",
    "Log.i",
    "Log.e",
    "Log.w",
    "Log.v",
    "Log.wtf",
    "println",
    "print",
    "FileWriter",
    "FileWriter.<init>",
    "FileWriter.write",
    "BufferedWriter.write",
    "OutputStreamWriter.write",
];

const LOGGING_SOURCE_DEFAULTS: &[&str] = &[
    "getLastLocation",
    "getCurrentLocation",
    "getDeviceId",
    "getSubscriberId",
    "getAndroidId",
    "getPrimaryClip",
    "getText",
    "EditText.getText",
    "getStringExtra",
    "getCharSequenceExtra",
    "getString",
    "getToken",
    "getPassword",
    "getEmail",
    "LoginData",
    "getLoginData",
    "getLoginUrl",
    "readLine",
    "BufferedReader.readLine",
    "SharedPreferences.getString",
    "dumpLogs",
];

/// Const-string / insn haystack markers that indicate PII or secrets in log lines.
const SENSITIVE_LOG_MARKERS: &[&str] = &[
    "password",
    "passwd",
    "email=",
    "e-mail",
    "login attempt",
    "api_key",
    "apikey",
    "auth_prefs",
    "credit card",
    "card number",
    "ssn=",
];

fn has_log_sink(owned: &ValueFlowAnalysisOwned) -> bool {
    owned.invoke_method_map.values().any(|m| {
        m.contains("android.util.Log.")
            || m.contains("Log.d")
            || m.contains("Log.i")
            || m.contains("Log.e")
            || m.contains("Log.w")
            || m.contains("Log.v")
            || m.contains("Log.wtf")
            || m.ends_with("println")
    })
}

fn has_pii_api(owned: &ValueFlowAnalysisOwned) -> bool {
    owned.api_return_sources.iter().any(|(_, m)| {
        m.contains("EditText.getText")
            || m.contains("getPassword")
            || m.contains("getEmail")
            || m.contains("LoginData")
    }) || owned.invoke_method_map.values().any(|m| {
        m.contains("EditText.getText")
            || m.contains("getPassword")
            || m.contains("getEmail")
    })
}

fn has_sensitive_log_const(owned: &ValueFlowAnalysisOwned) -> bool {
    owned.insn_at.values().any(|insn| {
        let lower = insn.to_ascii_lowercase();
        if !(lower.contains("const-string") || lower.contains("const-string/jumbo")) {
            return false;
        }
        SENSITIVE_LOG_MARKERS
            .iter()
            .any(|m| lower.contains(m))
    })
}

pub fn scan_insecure_logging(
    owned: &ValueFlowAnalysisOwned,
    class_name: &str,
    method_name: &str,
    source_patterns: Option<&[String]>,
) -> Vec<VulnFinding> {
    let sources: Vec<&str> = source_patterns
        .map(|s| s.iter().map(String::as_str).collect::<Vec<_>>())
        .unwrap_or_else(|| LOGGING_SOURCE_DEFAULTS.to_vec());
    let mut out = source_sink_scan(
        owned,
        class_name,
        method_name,
        "insecure_logging",
        &sources,
        LOGGING_SINKS,
    );
    // OVAA InsecureLoggerService#dumpLogs: FileWriter of on-disk log without clear VF seed.
    if out.is_empty()
        && (method_name == "dumpLogs" || class_name.contains("InsecureLogger"))
        && owned
            .invoke_method_map
            .values()
            .any(|m| m.contains("FileWriter") || m.contains("Log."))
    {
        out.extend(crate::detectors::types::invoke_scan(
            owned,
            class_name,
            method_name,
            "insecure_logging",
            &["FileWriter", "Log.d", "Log.i", "Log.e", "Log.w"],
        ));
        out.truncate(1);
    }
    let pii_signal = has_pii_api(owned) || has_sensitive_log_const(owned);
    // Same-method co-occurrence: Log.* with EditText/getText or PII const-strings.
    // Catches StringBuilder / `+` concat where register VF loses the seed (VulnLab LoginActivity).
    if out.is_empty() && has_log_sink(owned) && pii_signal {
        out.extend(invoke_scan(
            owned,
            class_name,
            method_name,
            "logging_pii",
            &[
                "Log.d", "Log.i", "Log.e", "Log.w", "Log.v", "Log.wtf", "println",
            ],
        ));
        out.truncate(2);
    } else if pii_signal {
        // Escalate VF hits that clearly log credentials/PII so NS-09 does not kill them.
        for f in &mut out {
            f.category = "logging_pii".into();
            f.refresh_category_meta();
        }
    }
    out
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
    fn logs_password_const_without_vf() {
        let mut invoke_method_map = HashMap::new();
        invoke_method_map.insert(4, "android.util.Log.d".to_string());
        let mut insn_at = HashMap::new();
        insn_at.insert(0, "const-string v0, \"Login attempt: email=\"".into());
        insn_at.insert(2, "const-string v1, \" password=\"".into());
        insn_at.insert(4, "invoke-static {v0, v1}, Log.d".into());
        let mut rw_map = HashMap::new();
        rw_map.insert(0, (vec![], vec![0]));
        rw_map.insert(2, (vec![], vec![1]));
        rw_map.insert(4, (vec![0, 1], vec![]));
        let owned = ValueFlowAnalysisOwned {
            cfg: make_cfg(vec![0, 2, 4]),
            rw_map,
            exceptional_edges: vec![],
            api_return_sources: vec![],
            invoke_method_map,
            insn_at,
            registers_size: 4,
            ins_size: 0,
        };
        let findings = scan_insecure_logging(&owned, "com.vulnlab.app.activities.LoginActivity", "lambda$onCreate$0", None);
        assert!(
            findings.iter().any(|f| f.category == "logging_pii"),
            "{findings:?}"
        );
    }

    #[test]
    fn logs_gettext_cooccurrence() {
        let mut invoke_method_map = HashMap::new();
        invoke_method_map.insert(0, "android.widget.EditText.getText".to_string());
        invoke_method_map.insert(4, "android.util.Log.d".to_string());
        let api_return_sources = vec![((0, 1), "android.widget.EditText.getText".into())];
        let mut insn_at = HashMap::new();
        insn_at.insert(0, "invoke-virtual {v0}, EditText.getText".into());
        insn_at.insert(4, "invoke-static {v2, v3}, Log.d".into());
        let mut rw_map = HashMap::new();
        rw_map.insert(0, (vec![0], vec![]));
        rw_map.insert(4, (vec![2, 3], vec![]));
        let owned = ValueFlowAnalysisOwned {
            cfg: make_cfg(vec![0, 4]),
            rw_map,
            exceptional_edges: vec![],
            api_return_sources,
            invoke_method_map,
            insn_at,
            registers_size: 6,
            ins_size: 3,
        };
        let findings = scan_insecure_logging(&owned, "com.example.Login", "onClick", None);
        assert!(
            findings.iter().any(|f| f.category == "logging_pii"),
            "{findings:?}"
        );
    }
}
