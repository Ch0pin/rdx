//! Package scope filters shared by MCP tools: prefix lists and the
//! `obfuscated-root` pseudo-prefix for short R8/ProGuard package names.
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};

/// Pseudo-prefix selecting classes that live outside any readable package root.
pub(crate) const OBFUSCATED_ROOT: &str = "obfuscated-root";
/// Longest package segment still treated as an obfuscated name.
pub(crate) const OBFUSCATED_SEGMENT_CHARS: usize = 3;
const MAX_ENTRIES: usize = 64;
pub(crate) const OBFUSCATED_ROOT_RULE: &str = "default-package classes and classes whose every package segment has at most three characters, such as wr.b$a, tk.a or a.b.C; a superset heuristic for R8/ProGuard package renaming that also admits short conventional roots such as com.a";

/// Parsed `package_prefix`: zero or more package/class prefixes matched at `.`
/// boundaries, optionally joined by the obfuscated-root rule. Entries combine
/// with OR; an empty scope is unrestricted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PackageScope {
    prefixes: Vec<String>,
    obfuscated_root: bool,
}

impl PackageScope {
    /// Absent, null, `""` and `[]` are unrestricted. A string or an array of
    /// strings lists prefixes; trailing dots are ignored and duplicates merge.
    pub(crate) fn parse(value: &Value) -> Result<Self> {
        let entries: Vec<&str> = match value {
            Value::Null => Vec::new(),
            Value::String(prefix) => vec![prefix.as_str()],
            Value::Array(items) => {
                ensure!(
                    items.len() <= MAX_ENTRIES,
                    "package_prefix accepts at most {MAX_ENTRIES} entries"
                );
                items
                    .iter()
                    .map(|item| {
                        item.as_str()
                            .context("package_prefix entries must be strings")
                    })
                    .collect::<Result<_>>()?
            }
            _ => bail!("package_prefix must be a string or an array of strings"),
        };
        let mut scope = Self::default();
        for entry in entries {
            if entry == OBFUSCATED_ROOT {
                scope.obfuscated_root = true;
                continue;
            }
            let prefix = entry.trim_end_matches('.');
            if prefix.is_empty() {
                ensure!(
                    value.is_string(),
                    "package_prefix entries must not be empty"
                );
                continue;
            }
            ensure!(
                !prefix.contains(|ch: char| ch.is_whitespace() || matches!(ch, ',' | ';')),
                "package_prefix entry {entry:?} is not a dotted package or class name; pass several prefixes as an array"
            );
            if !scope.prefixes.iter().any(|known| known == prefix) {
                scope.prefixes.push(prefix.to_owned());
            }
        }
        Ok(scope)
    }
    pub(crate) fn from_args(args: &Value) -> Result<Self> {
        Self::parse(&args["package_prefix"])
    }
    pub(crate) fn is_unrestricted(&self) -> bool {
        self.prefixes.is_empty() && !self.obfuscated_root
    }
    pub(crate) fn matches(&self, class: &str) -> bool {
        self.is_unrestricted()
            || self
                .prefixes
                .iter()
                .any(|prefix| prefix_matches(prefix, class))
            || (self.obfuscated_root && is_obfuscated_root(class))
    }
    /// Echo of the applied scope for response coverage.
    pub(crate) fn describe(&self) -> Value {
        json!({"prefixes":self.prefixes,"obfuscated_root":self.obfuscated_root,
            "boundary":"each prefix matches the named class itself or classes below it at a '.' boundary; entries combine with OR",
            "obfuscated_root_rule":self.obfuscated_root.then_some(OBFUSCATED_ROOT_RULE)})
    }
}

/// `prefix` matches the class itself or any class below it at a `.` boundary.
pub(crate) fn prefix_matches(prefix: &str, class: &str) -> bool {
    class == prefix
        || class
            .strip_prefix(prefix)
            .is_some_and(|tail| tail.starts_with('.'))
}

/// Default-package classes, or every package segment at most three characters.
pub(crate) fn is_obfuscated_root(class: &str) -> bool {
    match class.rsplit_once('.') {
        None => true,
        Some((package, _)) => package.split('.').all(|segment| {
            !segment.is_empty() && segment.chars().count() <= OBFUSCATED_SEGMENT_CHARS
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn obfuscated_root_rule_admits_short_roots_and_rejects_readable_packages() {
        for class in ["wr.b$a", "tk.a", "a", "a.b.C", "com.a.B", "o0.O$1", "Top"] {
            assert!(is_obfuscated_root(class), "{class}");
        }
        for class in [
            "com.ashleymadison.mobile.a",
            "kotlin.a",
            "androidx.a.b.C",
            "me.leolin.X",
            "sample.Target",
        ] {
            assert!(!is_obfuscated_root(class), "{class}");
        }
    }

    #[test]
    fn scalar_string_keeps_boundary_semantics_and_empty_means_unrestricted() {
        let scope = PackageScope::parse(&json!("app.")).unwrap();
        assert!(scope.matches("app"));
        assert!(scope.matches("app.sub.B"));
        assert!(!scope.matches("application.C"));
        assert!(!scope.matches("APP.D"));
        assert!(!scope.matches("wr.b$a"));
        assert_eq!(scope.describe()["prefixes"], json!(["app"]));
        assert_eq!(scope.describe()["obfuscated_root_rule"], Value::Null);
        for value in [Value::Null, json!(""), json!([])] {
            let scope = PackageScope::parse(&value).unwrap();
            assert!(scope.is_unrestricted());
            assert!(scope.matches("anything.At.All"));
        }
    }

    #[test]
    fn lists_and_pseudo_prefix_combine_with_or() {
        let scope =
            PackageScope::parse(&json!(["app.", "vendor.sdk", OBFUSCATED_ROOT, "app"])).unwrap();
        assert!(!scope.is_unrestricted());
        for class in ["app.A", "vendor.sdk.Client", "wr.b$a", "tk.a", "Top"] {
            assert!(scope.matches(class), "{class}");
        }
        for class in ["vendor.sdkx.Client", "com.example.app.A", "kotlin.jvm.a"] {
            assert!(!scope.matches(class), "{class}");
        }
        let description = scope.describe();
        assert_eq!(description["prefixes"], json!(["app", "vendor.sdk"]));
        assert_eq!(description["obfuscated_root"], true);
        assert_eq!(description["obfuscated_root_rule"], OBFUSCATED_ROOT_RULE);
        let only = PackageScope::parse(&json!(OBFUSCATED_ROOT)).unwrap();
        assert!(only.matches("tk.a") && !only.matches("sample.Target"));
    }

    #[test]
    fn malformed_scopes_are_rejected_with_specific_messages() {
        for (value, message) in [
            (json!(5), "string or an array of strings"),
            (json!([""]), "must not be empty"),
            (json!([5]), "entries must be strings"),
            (json!(["com.a, com.b"]), "pass several prefixes as an array"),
            (json!("com.a com.b"), "pass several prefixes as an array"),
        ] {
            let error = format!("{:#}", PackageScope::parse(&value).unwrap_err());
            assert!(error.contains(message), "{value}: {error}");
        }
        let many: Vec<_> = (0..65).map(|index| format!("p{index}")).collect();
        assert!(PackageScope::parse(&json!(many)).is_err());
    }
}
