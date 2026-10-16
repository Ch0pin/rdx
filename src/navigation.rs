use rdx::apk::Entry;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum AssetGrouping {
    #[default]
    Folder,
    FileType,
}

impl AssetGrouping {
    pub fn label(self) -> &'static str {
        match self {
            Self::Folder => "Folder",
            Self::FileType => "File type",
        }
    }
}

fn asset_type(path: &str) -> &'static str {
    let filename = path.rsplit('/').next().unwrap_or(path);
    let extension = filename.rsplit_once('.').map_or("", |(_, ext)| ext);
    match extension.to_ascii_lowercase().as_str() {
        "json" => "JSON",
        "js" | "mjs" | "cjs" => "JavaScript",
        "xml" => "XML",
        "html" | "htm" => "HTML",
        "css" => "CSS",
        "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "svg" | "avif" | "ico" => "Images",
        "ttf" | "otf" | "woff" | "woff2" => "Fonts",
        "tflite" | "lite" | "onnx" | "pb" => "Models",
        "dex" => "DEX",
        "lottie" | "riv" => "Animations",
        "mp3" | "wav" | "ogg" | "m4a" | "aac" | "flac" => "Audio",
        "mp4" | "webm" | "mov" => "Video",
        "txt" | "md" | "csv" | "properties" => "Text",
        _ => "Other",
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Class(String),
    File(usize),
    ManifestSummary(usize),
}

#[derive(Default)]
pub struct Node {
    pub children: BTreeMap<String, Node>,
    pub targets: Vec<Target>,
    pub count: usize,
    pub source_path: Option<String>,
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
    } else if let Some(rest) = path.strip_prefix("splits/") {
        ("Splits", rest)
    } else if let Some(rest) = path.strip_prefix("container/") {
        ("Container", rest)
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

pub fn build(classes: &[String], entries: &[Entry], filter: &str, grouping: AssetGrouping) -> Node {
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
        if group == "Assets" && grouping == AssetGrouping::FileType {
            parts.push(asset_type(path));
        }
        parts.extend(path.split('/'));
        root.insert(&parts, Target::File(entry.index));
        // Keep the actual archive path for tooltips, independent of virtual groups.
        let mut leaf = &mut root;
        for part in parts.iter().take(63) {
            leaf = leaf.children.get_mut(*part).unwrap();
        }
        if parts.len() > 63 {
            leaf = leaf.children.get_mut(&parts[63..].join("/")).unwrap();
        }
        leaf.source_path = Some(entry.path.clone());
    }
    root
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn asset_grouping_preserves_identity_folders_and_search() {
        let entries: Vec<_> = [
            "assets/a/config.JSON",
            "assets/b/config.JSON",
            "assets/model.tflite",
            "assets/font.ttf",
            "assets/code.js",
            "assets/picture.png",
            "assets/a/no-extension",
            "res/raw/config.JSON",
            "splits/feature/assets/config.JSON",
        ]
        .into_iter()
        .enumerate()
        .map(|(index, path)| Entry {
            index,
            path: path.into(),
            size: 0,
            compressed_size: 0,
        })
        .collect();
        let folders = build(&[], &entries, "", AssetGrouping::Folder);
        assert_eq!(folders.children["Assets"].count, 7);
        assert_eq!(
            folders.children["Assets"].children["a"].children["config.JSON"].targets,
            [Target::File(0)]
        );
        let grouped = build(&[], &entries, "", AssetGrouping::FileType);
        let assets = &grouped.children["Assets"];
        assert_eq!(assets.count, 7);
        assert_eq!(assets.children["JSON"].count, 2);
        for (folder, index) in [("a", 0), ("b", 1)] {
            let leaf = &assets.children["JSON"].children[folder].children["config.JSON"];
            assert_eq!(leaf.targets, [Target::File(index)]);
            assert_eq!(
                leaf.source_path.as_deref(),
                Some(entries[index].path.as_str())
            );
        }
        for kind in ["Models", "Fonts", "JavaScript", "Images", "Other"] {
            assert_eq!(assets.children[kind].count, 1);
        }
        assert_eq!(
            grouped.children["Resources"].children["raw"].children["config.JSON"].targets,
            [Target::File(7)]
        );
        assert_eq!(grouped.children["Splits"].children["feature"].children["assets"].children["config.JSON"].targets, [Target::File(8)]);
        let filtered = build(&[], &entries, "ASSETS/B/CONFIG", AssetGrouping::FileType);
        assert_eq!(filtered.count, 1);
        assert_eq!(
            filtered.children["Assets"].children["JSON"].children["b"].children["config.JSON"]
                .targets,
            [Target::File(1)]
        );
    }
    #[test]
    fn split_paths_keep_their_apk_origin_in_the_tree() {
        assert_eq!(
            category("splits/feature_chat/AndroidManifest.xml"),
            ("Splits", "feature_chat/AndroidManifest.xml")
        );
        assert_eq!(
            category("splits/config.en/res/values/strings.xml"),
            ("Splits", "config.en/res/values/strings.xml")
        );
        assert_eq!(
            category("container/Android/obb/main.1.obb"),
            ("Container", "Android/obb/main.1.obb")
        );
    }
    #[test]
    fn preserves_asset_folders_and_unifies_packages() {
        let entries = vec![];
        let root = build(
            &["com.example.One".into(), "com.example.Two".into()],
            &entries,
            "",
            AssetGrouping::Folder,
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
            AssetGrouping::Folder,
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
        let root = build(&[], &entries, "", AssetGrouping::Folder);
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
