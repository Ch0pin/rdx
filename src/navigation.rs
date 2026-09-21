use rdx::apk::Entry;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Class(String),
    File(usize),
}

#[derive(Default)]
pub struct Node {
    pub children: BTreeMap<String, Node>,
    pub targets: Vec<Target>,
    pub count: usize,
}

impl Node {
    fn insert(&mut self, path: &[&str], target: Target) {
        // Bound recursive construction, rendering, and drop for deeply nested names.
        if path.len() > 64 {
            let tail = path[63..].join("/");
            let mut bounded = path[..63].to_vec();
            bounded.push(&tail);
            self.insert(&bounded, target);
            return;
        }
        self.count += 1;
        match path.split_first() {
            Some((name, rest)) => self
                .children
                .entry((*name).into())
                .or_default()
                .insert(rest, target),
            None => self.targets.push(target),
        }
    }
}

pub fn category(path: &str) -> (&str, &str) {
    if path == "AndroidManifest.xml" {
        ("Manifest", path)
    } else if let Some(rest) = path.strip_prefix("assets/") {
        ("Assets", rest)
    } else if let Some(rest) = path.strip_prefix("res/") {
        ("Resources", rest)
    } else if let Some(rest) = path.strip_prefix("lib/") {
        ("Libraries", rest)
    } else if let Some(rest) = path.strip_prefix("META-INF/") {
        ("Signatures & metadata", rest)
    } else if path.ends_with(".dex") && !path.contains('/') {
        ("DEX bytecode", path)
    } else if path == "resources.arsc" {
        ("Resources", path)
    } else {
        ("Other files", path)
    }
}

pub fn build(classes: &[String], entries: &[Entry], filter: &str) -> Node {
    let mut root = Node::default();
    let filter = filter.to_lowercase();
    for class in classes {
        if !filter.is_empty() && !class.to_lowercase().contains(&filter) {
            continue;
        }
        let mut parts = vec!["Classes"];
        parts.extend(class.split('.'));
        root.insert(&parts, Target::Class(class.clone()));
    }
    for entry in entries {
        if !filter.is_empty() && !entry.path.to_lowercase().contains(&filter) {
            continue;
        }
        let (group, path) = category(&entry.path);
        let mut parts = vec![group];
        parts.extend(path.split('/'));
        root.insert(&parts, Target::File(entry.index));
    }
    root
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_asset_folders_and_unifies_packages() {
        let entries = vec![];
        let root = build(
            &["com.example.One".into(), "com.example.Two".into()],
            &entries,
            "",
        );
        assert_eq!(root.count, 2);
        assert_eq!(
            root.children["Classes"].children["com"].children["example"].count,
            2
        );
        let filtered = build(
            &["com.example.One".into(), "com.example.Two".into()],
            &entries,
            "ONE",
        );
        assert_eq!(filtered.count, 1);
    }
    #[test]
    fn deep_paths_have_bounded_display_depth() {
        let path = vec!["nested"; 10_000].join("/");
        let entries = [Entry {
            index: 0,
            path,
            size: 0,
            compressed_size: 0,
        }];
        let root = build(&[], &entries, "");
        let mut node = &root;
        let mut depth = 0;
        while let Some(child) = node.children.values().next() {
            node = child;
            depth += 1;
        }
        assert!(depth <= 64);
        assert_eq!(node.targets, vec![Target::File(0)]);
    }
    #[test]
    fn each_entry_has_one_category() {
        for (path, expected) in [
            ("AndroidManifest.xml", "Manifest"),
            ("classes2.dex", "DEX bytecode"),
            ("res/layout/main.xml", "Resources"),
            ("resources.arsc", "Resources"),
            ("lib/arm64-v8a/liba.so", "Libraries"),
            ("META-INF/MANIFEST.MF", "Signatures & metadata"),
            ("root.txt", "Other files"),
        ] {
            assert_eq!(category(path).0, expected);
        }
    }
}
