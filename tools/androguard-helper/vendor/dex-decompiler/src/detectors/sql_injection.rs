//! SQL injection: user / Intent / Uri input → rawQuery / execSQL / query.
//!
//! S4: when the sink lives in a ContentProvider (or query/insert/update/delete
//! entry), retag as `provider_sql_injection` (Samsung Billing IAPService-style).
//!
//! ContentProvider `selection` / URI path segments often reach `rawQuery` via
//! `StringBuilder` / `+`, which drops register VF — co-occurrence covers that.

use crate::decompile::value_flow::ValueFlowAnalysisOwned;
use crate::detectors::types::{invoke_scan, source_sink_scan, VulnFinding};

const SQL_SOURCES: &[&str] = &[
    "getStringExtra",
    "getCharSequenceExtra",
    "getText",
    "getData",
    "getDataString",
    "getQueryParameter",
    "getLastPathSegment",
    "getPathSegments",
    "getEncodedPath",
    "getPath",
    "Uri.getQuery",
    "getQuery",
];

const SQL_SINKS: &[&str] = &[
    "rawQuery",
    "execSQL",
    "compileStatement",
    "SQLiteDatabase.query",
    "SQLiteQueryBuilder",
];

const RAW_SQL_SINKS: &[&str] = &["rawQuery", "execSQL", "compileStatement"];

fn is_provider_class(class_name: &str) -> bool {
    let cls = class_name.to_lowercase();
    // Nested SQLiteOpenHelper / *DbHelper often live under *ContentProvider$… — not IPC.
    if cls.contains("dbhelper")
        || cls.contains("sqliteopenhelper")
        || cls.contains("openhelper")
    {
        return false;
    }
    cls.contains("contentprovider")
        || (cls.ends_with("provider") && !cls.contains("fileprovider"))
        || cls.contains(".providers.")
}

/// ContentProvider IPC entries only — not helper onCreate CREATE TABLE execSQL.
fn is_provider_sql_entry(class_name: &str, method_name: &str) -> bool {
    let meth = method_name.to_lowercase();
    matches!(
        meth.as_str(),
        "query" | "insert" | "update" | "delete" | "call"
    ) && is_provider_class(class_name)
}

fn has_raw_sql_sink(owned: &ValueFlowAnalysisOwned) -> bool {
    owned
        .invoke_method_map
        .values()
        .any(|m| RAW_SQL_SINKS.iter().any(|s| m.contains(s)))
}

pub fn scan_sql_injection(
    owned: &ValueFlowAnalysisOwned,
    class_name: &str,
    method_name: &str,
) -> Vec<VulnFinding> {
    let mut findings = source_sink_scan(
        owned,
        class_name,
        method_name,
        "sql_injection",
        SQL_SOURCES,
        SQL_SINKS,
    );

    // Provider IPC entry that builds SQL into rawQuery/execSQL (VulnLab / Samsung IAP).
    // Entry-param → String concat → rawQuery often loses VF; treat as provider SQLi.
    if is_provider_sql_entry(class_name, method_name) && has_raw_sql_sink(owned) {
        if findings.is_empty() {
            findings.extend(invoke_scan(
                owned,
                class_name,
                method_name,
                "provider_sql_injection",
                RAW_SQL_SINKS,
            ));
            findings.truncate(2);
        }
        for f in &mut findings {
            f.category = "provider_sql_injection".into();
            f.refresh_category_meta();
        }
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
    fn provider_query_retag() {
        let mut rw_map = HashMap::new();
        rw_map.insert(0, (vec![], vec![0]));
        rw_map.insert(2, (vec![0], vec![]));
        let mut invoke_method_map = HashMap::new();
        invoke_method_map.insert(
            2,
            "android.database.sqlite.SQLiteDatabase.rawQuery".to_string(),
        );
        let mut insn_at = HashMap::new();
        insn_at.insert(0, "move-result-object v0".into());
        insn_at.insert(2, "invoke-virtual {v1, v0}, rawQuery".into());
        let owned = ValueFlowAnalysisOwned {
            cfg: make_cfg(vec![0, 2]),
            rw_map,
            exceptional_edges: vec![],
            api_return_sources: vec![((0, 0), "android.net.Uri.getQueryParameter".into())],
            invoke_method_map,
            insn_at,
            registers_size: 0,
            ins_size: 0,
        };
        let findings = scan_sql_injection(&owned, "com.example.IapProvider", "query");
        assert!(
            findings
                .iter()
                .any(|f| f.category == "provider_sql_injection"),
            "{findings:?}"
        );
    }

    #[test]
    fn provider_rawquery_without_vf() {
        let mut invoke_method_map = HashMap::new();
        invoke_method_map.insert(
            8,
            "android.database.sqlite.SQLiteDatabase.rawQuery".to_string(),
        );
        let mut insn_at = HashMap::new();
        insn_at.insert(8, "invoke-virtual {v0, v1, v2}, rawQuery".into());
        let mut rw_map = HashMap::new();
        rw_map.insert(8, (vec![0, 1, 2], vec![]));
        let owned = ValueFlowAnalysisOwned {
            cfg: make_cfg(vec![8]),
            rw_map,
            exceptional_edges: vec![],
            api_return_sources: vec![],
            invoke_method_map,
            insn_at,
            registers_size: 10,
            ins_size: 6,
        };
        let findings = scan_sql_injection(
            &owned,
            "com.vulnlab.app.providers.VulnContentProvider",
            "query",
        );
        assert!(
            findings
                .iter()
                .any(|f| f.category == "provider_sql_injection"),
            "{findings:?}"
        );
    }

    #[test]
    fn helper_oncreate_not_provider_sqli() {
        let mut invoke_method_map = HashMap::new();
        invoke_method_map.insert(
            4,
            "android.database.sqlite.SQLiteDatabase.execSQL".to_string(),
        );
        let mut insn_at = HashMap::new();
        insn_at.insert(4, "invoke-virtual {v0, v1}, execSQL".into());
        let mut rw_map = HashMap::new();
        rw_map.insert(4, (vec![0, 1], vec![]));
        let owned = ValueFlowAnalysisOwned {
            cfg: make_cfg(vec![4]),
            rw_map,
            exceptional_edges: vec![],
            api_return_sources: vec![],
            invoke_method_map,
            insn_at,
            registers_size: 4,
            ins_size: 1,
        };
        let findings = scan_sql_injection(
            &owned,
            "com.vulnlab.app.providers.VulnContentProvider$VulnDbHelper",
            "onCreate",
        );
        assert!(
            findings
                .iter()
                .all(|f| f.category != "provider_sql_injection"),
            "{findings:?}"
        );
    }
}
