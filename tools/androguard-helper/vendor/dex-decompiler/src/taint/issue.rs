//! Issues and traces (SAPP-style presentation data).

use serde::{Deserialize, Serialize};

use crate::detectors::mas::{enrich_mas, MasLink};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TraceFrame {
    pub class_name: String,
    pub method_name: String,
    /// Instruction offset within the method (method-relative), if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<u32>,
    pub kind: String,
    pub description: String,
    /// Recovered dest URL or `extra:<key>` / feature breadcrumb.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<String>,
    /// `field:Class.field` breadcrumb when the hop is heap-sensitive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// Port label (`return`, `arg:0`, `this`) when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<String>,
    /// Model features (e.g. `user-controlled`, `via-extra:url`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Issue {
    pub rule_code: u32,
    pub rule_name: String,
    #[serde(default)]
    pub description: String,
    pub source_kind: String,
    pub sink_kind: String,
    /// Method where source and sink traces meet (trace root).
    pub callable: String,
    pub trace: Vec<TraceFrame>,
    /// OWASP MASWE weaknesses (native enrichment from detectors/mas.rs).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub maswe: Vec<MasLink>,
    /// OWASP MASVS controls with docs URLs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub masvs: Vec<MasLink>,
    /// OWASP MASTG Knowledge articles.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mastg_know: Vec<MasLink>,
    /// OWASP MASTG Best Practices.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mastg_best: Vec<MasLink>,
}

impl Issue {
    /// Attach MASWE → MASVS → MASTG links using the shared vuln-detector catalog.
    pub fn enrich_mas_links(&mut self) {
        let category = format!("taint:{}", self.sink_kind);
        let mas = enrich_mas(
            &category,
            &self.rule_name,
            &self.description,
            &format!("{} → {}", self.source_kind, self.sink_kind),
            "",
            None,
            Some(&self.rule_name),
        );
        self.maswe = mas.maswe;
        self.masvs = mas.masvs;
        self.mastg_know = mas.mastg_know;
        self.mastg_best = mas.mastg_best;
    }
}
