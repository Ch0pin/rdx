//! One parsed project shared by interactive and service consumers.
use crate::engine::{DecompilerEngine, NativeEngine, Project};
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone)]
pub struct ProjectSnapshot {
    pub(crate) engine: NativeEngine,
    pub(crate) project: Arc<Project>,
    pub(crate) identity: Arc<InputIdentity>,
}

pub(crate) struct InputIdentity {
    pub path: PathBuf,
    pub hash: String,
    pub metadata: fs::Metadata,
}

impl ProjectSnapshot {
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_with(path, |engine| engine.open(path))
    }

    /// Open once, allowing the GUI to reuse its prepared archive and report progress.
    /// Clones share DEX, hierarchy, resources, archive ownership and project indexes.
    pub fn open_with(
        path: &Path,
        open: impl FnOnce(&mut NativeEngine) -> Result<Project>,
    ) -> Result<Self> {
        let path = path.canonicalize().context("Cannot resolve project path")?;
        let metadata = fs::metadata(&path)?;
        let hash = digest(&path)?;
        let mut engine = NativeEngine::start()?;
        let project = open(&mut engine)?;
        ensure!(
            engine.opened_path() == Some(path.as_path()),
            "Loaded project path does not match its identity"
        );
        let snapshot = Self {
            engine,
            project: Arc::new(project),
            identity: Arc::new(InputIdentity {
                path,
                hash,
                metadata,
            }),
        };
        snapshot.verify_current()?;
        Ok(snapshot)
    }

    pub fn engine(&self) -> &NativeEngine {
        &self.engine
    }
    pub fn project(&self) -> &Arc<Project> {
        &self.project
    }

    /// Revalidate the original identity without reopening or rebuilding the project.
    pub(crate) fn verify_current(&self) -> Result<()> {
        let identity = &self.identity;
        let current = fs::metadata(&identity.path)?;
        ensure!(
            current.len() == identity.metadata.len()
                && current.modified()? == identity.metadata.modified()?
                && digest(&identity.path)? == identity.hash,
            "PROJECT_CHANGED: APK changed since loading; reopen the project"
        );
        let after = fs::metadata(&identity.path)?;
        ensure!(
            after.len() == current.len() && after.modified()? == current.modified()?,
            "PROJECT_CHANGED: APK changed during identity verification; reopen the project"
        );
        Ok(())
    }
}

fn digest(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
