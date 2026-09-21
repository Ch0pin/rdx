use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const MAX_SETTINGS_BYTES: u64 = 64 * 1024;
static TEMP_ID: AtomicU64 = AtomicU64::new(0);

pub const STANDARD_PACKAGES: &[&str] = &[
    "android.*",
    "androidx.*",
    "com.android.*",
    "kotlin.*",
    "kotlinx.*",
    "java.*",
    "javax.*",
    "jdk.*",
    "sun.*",
    "com.sun.*",
    "dalvik.*",
    "libcore.*",
];

pub fn default_excluded_packages() -> Vec<String> {
    STANDARD_PACKAGES.iter().map(|s| (*s).to_owned()).collect()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchPreferences {
    pub classes: bool,
    pub methods: bool,
    pub fields: bool,
    pub code: bool,
    pub resources: bool,
    pub comments: bool,
    pub excluded_packages: Vec<String>,
    pub case_sensitive: bool,
    pub regex: bool,
    pub auto_search: bool,
    pub keep_open: bool,
}

impl Default for SearchPreferences {
    fn default() -> Self {
        Self {
            classes: false,
            methods: false,
            fields: false,
            code: true,
            resources: false,
            comments: false,
            excluded_packages: default_excluded_packages(),
            case_sensitive: false,
            regex: false,
            auto_search: false,
            keep_open: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub interface_theme: String,
    pub code_theme: String,
    pub font_size: f32,
    pub code_font: String,
    pub word_wrap: bool,
    pub search_keep_open: bool,
    pub usages_keep_open: bool,
    pub search: SearchPreferences,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            interface_theme: "system".into(),
            code_theme: "ocean".into(),
            font_size: 15.0,
            code_font: "default".into(),
            word_wrap: false,
            search_keep_open: false,
            usages_keep_open: true,
            search: SearchPreferences::default(),
        }
    }
}

impl Settings {
    pub fn normalize(&mut self) {
        if !matches!(self.interface_theme.as_str(), "system" | "light" | "dark") {
            self.interface_theme = "system".into();
        }
        if crate::code_view::CodeTheme::from_preference(&self.code_theme).is_none() {
            self.code_theme = "ocean".into();
        }
        if crate::code_fonts::CodeFont::from_preference(&self.code_font).is_none() {
            self.code_font = "default".into();
        }
        if !self.font_size.is_finite() || !(10.0..=28.0).contains(&self.font_size) {
            self.font_size = 15.0;
        }
    }
}

pub struct SettingsStore {
    path: Result<PathBuf, String>,
}

impl SettingsStore {
    pub fn new() -> Self {
        Self {
            path: config_path(),
        }
    }

    pub fn load(&self) -> Result<Settings, String> {
        let path = self.path.as_ref().map_err(Clone::clone)?;
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Settings::default());
            }
            Err(error) => {
                return Err(format!(
                    "Cannot read settings at {}: {error}",
                    path.display()
                ));
            }
        };
        let mut bytes = Vec::new();
        file.take(MAX_SETTINGS_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("Cannot read settings at {}: {error}", path.display()))?;
        if bytes.len() as u64 > MAX_SETTINGS_BYTES {
            return Err(format!(
                "Settings at {} exceed the 64 KiB limit",
                path.display()
            ));
        }
        let mut value: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("Invalid settings at {}: {error}", path.display()))?;
        let legacy_search = value.get("search").is_none();
        if let Some(search) = value.get_mut("search").and_then(|v| v.as_object_mut())
            && !search.contains_key("excluded_packages")
            && search.get("exclude_standard_packages") == Some(&serde_json::Value::Bool(false))
        {
            search.insert("excluded_packages".into(), serde_json::json!([]));
        }
        let mut settings: Settings = serde_json::from_value(value)
            .map_err(|error| format!("Invalid settings at {}: {error}", path.display()))?;
        if legacy_search {
            settings.search.keep_open = settings.search_keep_open;
        }
        settings.normalize();
        Ok(settings)
    }

    pub fn save(&self, settings: &Settings) -> Result<(), String> {
        let path = self.path.as_ref().map_err(Clone::clone)?;
        let parent = path
            .parent()
            .ok_or_else(|| "Settings path has no parent directory".to_owned())?;
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "Cannot create settings directory {}: {error}",
                parent.display()
            )
        })?;
        let mut normalized = settings.clone();
        normalized.normalize();
        let bytes = serde_json::to_vec_pretty(&normalized)
            .map_err(|error| format!("Cannot encode settings: {error}"))?;
        let (temporary, mut file) = loop {
            let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let temporary = parent.join(format!(".settings-{}-{id}.tmp", std::process::id()));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
            {
                Ok(file) => break (temporary, file),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(format!(
                        "Cannot save settings at {}: {error}",
                        path.display()
                    ));
                }
            }
        };
        let result = (|| {
            file.write_all(&bytes)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            drop(file);
            replace_file(&temporary, path)
        })();
        if let Err(error) = result {
            let _ = fs::remove_file(&temporary);
            return Err(format!(
                "Cannot save settings at {}: {error}",
                path.display()
            ));
        }
        Ok(())
    }
}

fn config_path() -> Result<PathBuf, String> {
    fn env_path(name: &str) -> Option<PathBuf> {
        std::env::var_os(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    }
    #[cfg(target_os = "windows")]
    let directory = env_path("APPDATA");
    #[cfg(target_os = "macos")]
    let directory = env_path("HOME").map(|home| home.join("Library/Application Support"));
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let directory = env_path("XDG_CONFIG_HOME")
        .filter(|path| path.is_absolute())
        .or_else(|| env_path("HOME").map(|home| home.join(".config")));
    directory
        .map(|directory| directory.join("rdx/settings.json"))
        .ok_or_else(|| {
            "Cannot locate your user configuration directory; settings cannot be persisted".into()
        })
}

fn replace_file(source: &Path, target: &Path) -> std::io::Result<()> {
    fs::rename(source, target)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TemporaryStore(SettingsStore, PathBuf);
    impl TemporaryStore {
        fn new() -> Self {
            let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let directory =
                std::env::temp_dir().join(format!("rdx-settings-test-{}-{id}", std::process::id()));
            Self(
                SettingsStore {
                    path: Ok(directory.join("settings.json")),
                },
                directory,
            )
        }
    }
    impl Drop for TemporaryStore {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.1);
        }
    }

    #[test]
    fn bundled_font_choices_persist_and_old_settings_keep_default() {
        let store = TemporaryStore::new();
        assert_eq!(store.0.load().unwrap().code_font, "default");
        for font in crate::code_fonts::CodeFont::ALL {
            let settings = Settings {
                code_font: font.preference_key().into(),
                ..Settings::default()
            };
            store.0.save(&settings).unwrap();
            assert_eq!(store.0.load().unwrap().code_font, font.preference_key());
        }
        let mut invalid = Settings {
            code_font: "unavailable".into(),
            ..Settings::default()
        };
        invalid.normalize();
        assert_eq!(invalid.code_font, "default");
    }

    #[test]
    fn settings_round_trip_and_overwrite() {
        let store = TemporaryStore::new();
        assert_eq!(store.0.load().unwrap(), Settings::default());
        let settings = Settings {
            interface_theme: "light".into(),
            code_theme: "solarized_light".into(),
            font_size: 22.0,
            code_font: "jetbrains-mono".into(),
            word_wrap: true,
            search_keep_open: true,
            usages_keep_open: false,
            search: SearchPreferences {
                classes: true,
                methods: true,
                fields: true,
                code: false,
                resources: true,
                comments: true,
                excluded_packages: vec!["custom.library.*".into()],
                case_sensitive: true,
                regex: true,
                auto_search: true,
                keep_open: true,
            },
        };
        store.0.save(&settings).unwrap();
        assert_eq!(store.0.load().unwrap(), settings);
        store.0.save(&Settings::default()).unwrap();
        assert_eq!(store.0.load().unwrap(), Settings::default());
        assert_eq!(fs::read_dir(&store.1).unwrap().count(), 1);
    }

    #[test]
    fn word_wrap_defaults_for_old_settings_and_persists_both_choices() {
        let store = TemporaryStore::new();
        store.0.save(&Settings::default()).unwrap();
        fs::write(store.0.path.as_ref().unwrap(), br#"{"font_size":18.0}"#).unwrap();
        let mut settings = store.0.load().unwrap();
        assert!(!settings.word_wrap);
        for enabled in [true, false] {
            settings.word_wrap = enabled;
            store.0.save(&settings).unwrap();
            assert_eq!(store.0.load().unwrap().word_wrap, enabled);
        }
    }

    #[test]
    fn old_search_settings_migrate_and_partial_preferences_default() {
        let store = TemporaryStore::new();
        store.0.save(&Settings::default()).unwrap();
        let path = store.0.path.as_ref().unwrap();
        fs::write(path, br#"{"search_keep_open":true}"#).unwrap();
        assert!(store.0.load().unwrap().search.keep_open);
        fs::write(
            path,
            br#"{"search_keep_open":true,"search":{"methods":true}}"#,
        )
        .unwrap();
        let loaded = store.0.load().unwrap();
        assert_eq!(
            loaded.search,
            SearchPreferences {
                methods: true,
                ..SearchPreferences::default()
            }
        );
    }

    #[test]
    fn legacy_exclusion_toggle_migrates_without_overriding_editable_list() {
        let store = TemporaryStore::new();
        store.0.save(&Settings::default()).unwrap();
        let path = store.0.path.as_ref().unwrap();
        fs::write(path, br#"{"search":{"exclude_standard_packages":false}}"#).unwrap();
        assert!(store.0.load().unwrap().search.excluded_packages.is_empty());
        fs::write(path, br#"{"search":{"exclude_standard_packages":true}}"#).unwrap();
        assert_eq!(
            store.0.load().unwrap().search.excluded_packages,
            default_excluded_packages()
        );
        fs::write(
            path,
            br#"{"search":{"exclude_standard_packages":false,"excluded_packages":["custom.*"]}}"#,
        )
        .unwrap();
        assert_eq!(
            store.0.load().unwrap().search.excluded_packages,
            vec!["custom.*"]
        );
        fs::write(path, br#"{"search":{"excluded_packages":[]}}"#).unwrap();
        assert!(store.0.load().unwrap().search.excluded_packages.is_empty());
    }
    #[test]
    fn obsolete_java_heap_setting_does_not_replace_native_preferences() {
        let store = TemporaryStore::new();
        store.0.save(&Settings::default()).unwrap();
        let path = store.0.path.as_ref().unwrap();
        fs::write(path, br#"{"heap_mib":4096,"interface_theme":"light","font_size":18,"search":{"case_sensitive":true}}"#).unwrap();
        let restored = store.0.load().unwrap();
        assert_eq!(restored.interface_theme, "light");
        assert_eq!(restored.font_size, 18.0);
        assert!(restored.search.case_sensitive);
        store.0.save(&restored).unwrap();
        assert!(!fs::read_to_string(path).unwrap().contains("heap_mib"));
    }

    #[test]
    fn settings_corrupt_and_oversized_are_errors() {
        let store = TemporaryStore::new();
        store.0.save(&Settings::default()).unwrap();
        let path = store.0.path.as_ref().unwrap();
        fs::write(path, b"not json").unwrap();
        assert!(store.0.load().unwrap_err().contains("Invalid settings"));
        fs::write(path, vec![b' '; MAX_SETTINGS_BYTES as usize + 1]).unwrap();
        assert!(store.0.load().unwrap_err().contains("64 KiB"));
    }

    #[test]
    fn settings_invalid_values_normalize_and_missing_fields_default() {
        let store = TemporaryStore::new();
        store.0.save(&Settings::default()).unwrap();
        let path = store.0.path.as_ref().unwrap();
        fs::write(
            path,
            br#"{"interface_theme":"unknown","code_theme":"unknown","font_size":200,"heap_mib":1}"#,
        )
        .unwrap();
        assert_eq!(store.0.load().unwrap(), Settings::default());
        fs::write(path, br#"{"interface_theme":"dark","future_setting":true}"#).unwrap();
        assert_eq!(
            store.0.load().unwrap(),
            Settings {
                interface_theme: "dark".into(),
                ..Settings::default()
            }
        );
        let mut settings = Settings {
            font_size: f32::NAN,
            ..Settings::default()
        };
        settings.normalize();
        assert_eq!(settings, Settings::default());
    }
}
