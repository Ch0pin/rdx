//! Permissions: load AOSP permissions by API level (mirrors apkparser.permissions).

use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

use crate::error::{Error, Result};

/// AOSP permissions for an API level: permission name -> { protectionLevel, label, description }.
pub type PermissionsMap = HashMap<String, PermissionInfo>;

#[derive(Debug, Clone, Deserialize)]
pub struct PermissionInfo {
    #[serde(rename = "protectionLevel")]
    pub protection_level: Option<String>,
    pub label: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PermissionsFile {
    permissions: Option<HashMap<String, PermissionInfo>>,
}

/// Load permissions JSON for the given API level from a directory containing permissions_XX.json.
/// Falls back to nearest available level (e.g. 28 -> 27 if 27 exists).
pub fn load_permissions(base_path: &Path, api_level: u32) -> Result<PermissionsMap> {
    let mut path = base_path.join(format!("permissions_{}.json", api_level));
    if !path.exists() {
        let levels = available_levels(base_path)?;
        if levels.is_empty() {
            return Ok(HashMap::new());
        }
        let api_level = if api_level > *levels.iter().max().unwrap() {
            *levels.iter().max().unwrap()
        } else if api_level < *levels.iter().min().unwrap() {
            *levels.iter().min().unwrap()
        } else {
            *levels
                .iter()
                .filter(|&&l| l < api_level)
                .max()
                .unwrap_or(&levels[0])
        };
        path = base_path.join(format!("permissions_{}.json", api_level));
    }
    let data = std::fs::read_to_string(&path).map_err(Error::Io)?;
    let file: PermissionsFile = serde_json::from_str(&data)
        .map_err(|e| Error::Parse(format!("permissions JSON: {}", e)))?;
    Ok(file.permissions.unwrap_or_default())
}

fn available_levels(base_path: &Path) -> Result<Vec<u32>> {
    let mut levels = Vec::new();
    let dir = std::fs::read_dir(base_path).map_err(Error::Io)?;
    for entry in dir {
        let entry = entry.map_err(Error::Io)?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("permissions_") && name.ends_with(".json") {
            if let Ok(n) = name
                .trim_start_matches("permissions_")
                .trim_end_matches(".json")
                .parse::<u32>()
            {
                levels.push(n);
            }
        }
    }
    levels.sort();
    Ok(levels)
}

/// Permissions helper: holds permission list and optional AOSP lookup (by API level).
pub struct Permissions {
    pub permissions: Vec<String>,
    pub aosp: PermissionsMap,
}

impl Permissions {
    pub fn new(permissions: Vec<String>, aosp: PermissionsMap) -> Self {
        Self { permissions, aosp }
    }

    pub fn get_requested_aosp_permissions(&self) -> Vec<&str> {
        self.permissions
            .iter()
            .filter(|p| self.aosp.contains_key(*p))
            .map(String::as_str)
            .collect()
    }

    pub fn get_requested_third_party_permissions(&self) -> Vec<&str> {
        self.permissions
            .iter()
            .filter(|p| !self.aosp.contains_key(*p))
            .map(String::as_str)
            .collect()
    }
}
