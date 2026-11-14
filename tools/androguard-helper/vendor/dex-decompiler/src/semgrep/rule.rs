//! Semgrep-compatible rule schema (YAML subset) plus native match hints.

use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Severity as in Semgrep YAML.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Severity {
    Info,
    Warning,
    Error,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Info => "INFO",
            Severity::Warning => "WARNING",
            Severity::Error => "ERROR",
        }
    }
}

impl Default for Severity {
    fn default() -> Self {
        Severity::Warning
    }
}

/// How the native engine should match this rule on DEX / SSA value-flow.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeKind {
    /// Tainted returns matching `sources` flow into invokes matching `sinks`.
    SourceSink,
    /// Any invoke whose method ref contains one of `methods`.
    Invoke,
    /// Method whose name equals `method_name` and body invokes one of `methods`.
    MethodInvoke,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NativeMatch {
    pub kind: NativeKind,
    #[serde(default)]
    pub sources: Vec<String>,
    #[serde(default)]
    pub sinks: Vec<String>,
    #[serde(default)]
    pub methods: Vec<String>,
    /// Exact method name for `MethodInvoke` (e.g. `onReceivedSslError`).
    #[serde(default)]
    pub method_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct RuleMetadata {
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub vuln_class: Option<String>,
    #[serde(default)]
    pub chain_tag: Option<String>,
}

/// One Semgrep-style rule (Java/XML patterns + optional native DEX match).
#[derive(Debug, Clone, Deserialize)]
pub struct SemgrepRule {
    pub id: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub severity: Severity,
    #[serde(default)]
    pub languages: Vec<String>,
    /// Single pattern string.
    #[serde(default)]
    pub pattern: Option<String>,
    /// Semgrep regex over the whole source (Java or XML text).
    #[serde(default, rename = "pattern-regex")]
    pub pattern_regex: Option<String>,
    /// Semgrep `patterns` / `pattern-either` tree (loosely parsed).
    #[serde(default)]
    pub patterns: Option<serde_yaml::Value>,
    /// Top-level `pattern-either:` (common in Semgrep rules).
    #[serde(default, rename = "pattern-either")]
    pub pattern_either: Option<serde_yaml::Value>,
    #[serde(default)]
    pub metadata: RuleMetadata,
    #[serde(default)]
    pub native: Option<NativeMatch>,
}

#[derive(Debug, Deserialize)]
struct RulesFile {
    rules: Vec<SemgrepRule>,
}

/// Load rules from a YAML file (`rules: [...]`).
pub fn load_rules_from_yaml_file(path: &Path) -> Result<Vec<SemgrepRule>, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {}", path.display(), e))?;
    load_rules_from_str(&text).map_err(|e| format!("{}: {}", path.display(), e))
}

/// Load all `*.yml` / `*.yaml` rule files from a directory (non-recursive).
pub fn load_rules_from_dir(dir: &Path) -> Result<Vec<SemgrepRule>, String> {
    if !dir.is_dir() {
        return Err(format!("not a directory: {}", dir.display()));
    }
    let mut rules = Vec::new();
    let mut errors = Vec::new();
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("read_dir {}: {}", dir.display(), e))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .map(|e| e == "yml" || e == "yaml")
                .unwrap_or(false)
        })
        .collect();
    paths.sort();
    for path in paths {
        match load_rules_from_yaml_file(&path) {
            Ok(mut rs) => rules.append(&mut rs),
            Err(e) => errors.push(e),
        }
    }
    if rules.is_empty() && !errors.is_empty() {
        return Err(errors.join("; "));
    }
    if !errors.is_empty() {
        // Soft-fail: keep successfully parsed files; log via stderr in CLI if needed.
        eprintln!(
            "warning: {} Semgrep rule file(s) failed to parse under {}",
            errors.len(),
            dir.display()
        );
        for e in &errors {
            eprintln!("  {}", e);
        }
    }
    Ok(rules)
}

/// Parse Semgrep-style YAML rules from a string.
pub fn load_rules_from_str(yaml: &str) -> Result<Vec<SemgrepRule>, String> {
    let file: RulesFile = serde_yaml::from_str(yaml).map_err(|e| format!("parse YAML: {}", e))?;
    Ok(file.rules)
}

impl SemgrepRule {
    /// True if this rule applies to Java/Kotlin source (DEX decompilation).
    pub fn applies_to_java(&self) -> bool {
        if self.languages.is_empty() {
            return true;
        }
        self.languages.iter().any(|l| {
            let l = l.to_ascii_lowercase();
            l == "java" || l == "kotlin" || l == "generic"
        })
    }

    /// True if this rule applies to XML (manifest / resources).
    pub fn applies_to_xml(&self) -> bool {
        self.languages.iter().any(|l| {
            let l = l.to_ascii_lowercase();
            l == "xml" || l == "generic"
        })
    }

    /// Collect positive pattern strings (OR candidates from pattern-either; leaves from patterns).
    pub fn pattern_strings(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(p) = &self.pattern {
            out.push(p.clone());
        }
        if let Some(v) = &self.patterns {
            collect_patterns(v, &mut out, false);
        }
        if let Some(v) = &self.pattern_either {
            collect_patterns(v, &mut out, false);
        }
        out
    }

    /// Collect `pattern-regex` strings (top-level and nested).
    pub fn pattern_regexes(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(r) = &self.pattern_regex {
            out.push(r.clone());
        }
        if let Some(v) = &self.patterns {
            collect_regexes(v, &mut out);
        }
        if let Some(v) = &self.pattern_either {
            collect_regexes(v, &mut out);
        }
        out
    }

    /// Whether `patterns:` should be treated as AND (Semgrep default) vs OR (`pattern-either`).
    pub fn patterns_are_conjunction(&self) -> bool {
        self.patterns.is_some() && self.pattern_either.is_none() && self.pattern.is_none()
    }
}

fn collect_patterns(v: &serde_yaml::Value, out: &mut Vec<String>, skip_negatives: bool) {
    let _ = skip_negatives;
    match v {
        serde_yaml::Value::Sequence(seq) => {
            for item in seq {
                collect_patterns(item, out, skip_negatives);
            }
        }
        serde_yaml::Value::Mapping(map) => {
            for (k, val) in map {
                let key = k.as_str().unwrap_or("");
                if key == "pattern" {
                    if let Some(s) = val.as_str() {
                        if is_usable_match_pattern(s) {
                            out.push(s.to_string());
                        }
                    }
                } else if key == "pattern-inside" || key == "pattern-not-inside" {
                    // Structural constraints only. Collecting `$M(...){ ... }` as an OR
                    // candidate makes the rule match every method body.
                    continue;
                } else if key == "pattern-not"
                    || key == "pattern-not-regex"
                    || key == "metavariable-regex"
                    || key == "metavariable-pattern"
                    || key == "metavariable-comparison"
                    || key == "metavariable-name"
                    || key == "focus-metavariable"
                {
                    // Negations / metavariable constraints: ignored in the native subset.
                    continue;
                } else if key == "pattern-either" || key == "patterns" {
                    collect_patterns(val, out, skip_negatives);
                } else {
                    // Unknown nested keys — do not scrape strings (avoids `$DURATION` from
                    // metavariable-comparison values matching every identifier).
                    continue;
                }
            }
        }
        serde_yaml::Value::String(s) => {
            if is_usable_match_pattern(s) {
                out.push(s.clone());
            }
        }
        _ => {}
    }
}

/// Patterns that are too broad / not meaningful as standalone matches in our subset.
fn is_usable_match_pattern(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() {
        return false;
    }
    // Bare metavariable or comparison scraped from metavariable-comparison.
    if t.starts_with('$') && !t.contains('(') && !t.contains('.') && !t.contains(' ') {
        return false;
    }
    if t.starts_with('$') && (t.contains('>') || t.contains('<') || t.contains("==")) {
        return false;
    }
    // Universal method-body wrapper used with pattern-inside.
    let compact: String = t.chars().filter(|c| !c.is_whitespace()).collect();
    if compact == "$M(...){...}" || compact == "$F(...){...}" || compact == "$METHOD(...){...}" {
        return false;
    }
    true
}

fn collect_regexes(v: &serde_yaml::Value, out: &mut Vec<String>) {
    match v {
        serde_yaml::Value::Sequence(seq) => {
            for item in seq {
                collect_regexes(item, out);
            }
        }
        serde_yaml::Value::Mapping(map) => {
            for (k, val) in map {
                let key = k.as_str().unwrap_or("");
                if key == "pattern-regex" {
                    if let Some(s) = val.as_str() {
                        out.push(s.to_string());
                    }
                } else if key != "metavariable-regex" {
                    collect_regexes(val, out);
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod count_tests {
    use super::*;

    #[test]
    fn parse_rule_bundles() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        for (label, rel) in [
            ("all.yml", "rules/semgrep/android/all.yml"),
            ("general.yml", "rules/semgrep/android/general.yml"),
        ] {
            let path = root.join(rel);
            let t = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("{label} ({}): {e}", path.display()));
            let rules = load_rules_from_str(&t).unwrap_or_else(|e| panic!("{label}: {e}"));
            println!("{label}: OK {} rules", rules.len());
            assert!(!rules.is_empty(), "{label} empty");
        }
        // Optional sibling droid2web copies (local monorepo only — not on CI).
        for (label, rel) in [
            ("web-mastg", "../droid2web/web/rules/semgrep-mastg.yml"),
            ("web-all", "../droid2web/web/rules/semgrep-all.yml"),
            ("web-mobhunt", "../droid2web/web/rules/semgrep-mobhunt.yml"),
        ] {
            let path = root.join(rel);
            if !path.is_file() {
                println!("{label}: skipped (no {})", path.display());
                continue;
            }
            let t = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("{label} ({}): {e}", path.display()));
            let rules = load_rules_from_str(&t).unwrap_or_else(|e| panic!("{label}: {e}"));
            println!("{label}: OK {} rules", rules.len());
            assert!(!rules.is_empty(), "{label} empty");
        }
        let embedded = load_rules_from_str(crate::semgrep::ANDROID_ALL_RULES_YAML)
            .expect("embedded all.yml must parse");
        assert!(
            embedded.len() >= 70,
            "expected full MobHunt+MASTG set, got {}",
            embedded.len()
        );
        let dir = root.join("rules/semgrep/android/mastg");
        let rules = load_rules_from_dir(&dir).unwrap();
        println!("mastg dir: OK {} rules", rules.len());
        assert!(rules.len() >= 60, "mastg dir too small: {}", rules.len());
    }

    #[test]
    fn noisy_mastg_rules_do_not_collect_universal_patterns() {
        let bio = load_rules_from_str(include_str!(
            "../../rules/semgrep/android/mastg/mastg-android-biometric-validity-duration.yml"
        ))
        .unwrap();
        let pats = bio[0].pattern_strings();
        assert!(
            pats.iter().all(|p| p.contains("setUserAuthentication")),
            "unexpected biometric patterns: {pats:?}"
        );
        assert!(
            !pats
                .iter()
                .any(|p| p.trim() == "$DURATION" || (p.contains('$') && p.contains('>'))),
            "metavariable-comparison leaked: {pats:?}"
        );

        let non_rand = load_rules_from_str(include_str!(
            "../../rules/semgrep/android/mastg/mastg-android-non-random-use.yml"
        ))
        .unwrap();
        let pats = non_rand[0].pattern_strings();
        assert!(
            !pats.iter().any(|p| p.contains("$M(...)")),
            "pattern-inside wrapper leaked: {pats:?}"
        );
        assert!(pats
            .iter()
            .any(|p| p.contains("Date") || p.contains("currentTimeMillis")));

        let entropy = load_rules_from_str(include_str!(
            "../../rules/semgrep/android/mastg/mastg-android-random-apis-insufficient-entropy.yml"
        ))
        .unwrap();
        let pats = entropy[0].pattern_strings();
        assert!(
            !pats.iter().any(|p| p.contains("$M(...)")),
            "pattern-inside wrapper leaked: {pats:?}"
        );
        assert!(pats
            .iter()
            .any(|p| p.contains("random") || p.contains("Random")));
    }
}
