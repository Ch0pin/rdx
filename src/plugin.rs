use crate::transport::Transport;
use anyhow::{Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutableBase {
    #[default]
    Environment,
    Application,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plugin {
    pub protocol: u32,
    pub id: String,
    pub name: String,
    pub executable: String,
    #[serde(default)]
    pub executable_base: ExecutableBase,
    #[serde(default)]
    pub args: Vec<String>,
    pub capability: String,
    #[serde(skip)]
    pub directory: PathBuf,
}
impl Plugin {
    /// Reading a manifest never executes it. The GUI requires explicit enablement.
    pub fn load(path: &Path) -> Result<Self> {
        let path = path.canonicalize()?;
        ensure!(
            std::fs::metadata(&path)?.len() <= 65536,
            "Plugin manifest exceeds 64 KiB"
        );
        let mut plugin: Self = serde_json::from_slice(&std::fs::read(&path)?)?;
        ensure!(plugin.protocol == 1, "Unsupported plugin protocol");
        ensure!(
            plugin.capability == "source.analyze",
            "Unsupported plugin capability"
        );
        ensure!(
            !plugin.id.is_empty() && !plugin.executable.is_empty(),
            "Plugin ID and executable are required"
        );
        plugin.directory = path.parent().unwrap().into();
        Ok(plugin)
    }
    fn resolve_executable(&self) -> Result<PathBuf> {
        if self.executable_base == ExecutableBase::Application {
            ensure!(
                Path::new(&self.executable).components().count() == 1
                    && !self.executable.contains(['/', '\\'])
                    && self.executable != "."
                    && self.executable != "..",
                "Application plugin executable must be a bare filename"
            );
            let mut filename = self.executable.clone();
            if cfg!(windows) && !filename.ends_with(".exe") {
                filename.push_str(".exe");
            }
            let current = std::env::current_exe()?;
            let directory = current
                .parent()
                .ok_or_else(|| anyhow::anyhow!("Application executable has no directory"))?;
            let adjacent = directory.join(&filename);
            if adjacent.is_file() {
                return Ok(adjacent);
            }
            // Cargo integration tests live in target/<profile>/deps; binaries are one level up.
            if directory.file_name().is_some_and(|name| name == "deps") {
                let target = directory.parent().unwrap().join(&filename);
                if target.is_file() {
                    return Ok(target);
                }
            }
            anyhow::bail!(
                "Native plugin {} is missing beside the application; build with cargo build --bins or install the bundled plugin binary",
                filename
            );
        }
        if Path::new(&self.executable).components().count() > 1 {
            Ok(self.directory.join(&self.executable))
        } else {
            Ok(PathBuf::from(&self.executable))
        }
    }

    pub fn analyze(&self, class: &str, source: &str) -> Result<Value> {
        let executable = self.resolve_executable()?;
        let mut command = Command::new(executable);
        command.args(&self.args).current_dir(&self.directory);
        let mut worker = Transport::spawn(&mut command)?;
        worker.request(json!({"method":"source.analyze","class":class,"source":source}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn example_manifest_is_valid() {
        let plugin = Plugin::load(
            &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("plugins/source-stats/plugin.json"),
        )
        .unwrap();
        assert_eq!(plugin.capability, "source.analyze");
    }
}
