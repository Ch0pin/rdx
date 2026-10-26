//! Project-owned disk postings. Fingerprints select candidates; raw DEX verifies each hit.
use super::{Catalog, check_cancel};
use crate::native_xrefs::{self, CallSite};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, File},
    io::{BufReader, BufWriter, Read, Write},
    path::PathBuf,
    sync::{
        Arc, Mutex, TryLockError,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

const SHARDS: usize = 32;
const RECORD: usize = 20;
fn fingerprint(s: &str) -> u64 {
    u64::from_le_bytes(Sha256::digest(s.as_bytes())[..8].try_into().unwrap())
}
struct Directory(PathBuf);
impl Directory {
    fn new() -> Result<Self> {
        let mut nonce = [0u8; 16];
        getrandom::fill(&mut nonce).map_err(|e| anyhow::anyhow!("{e}"))?;
        let path = std::env::temp_dir().join(format!(
            "rdx-invokes-{}-{:032x}",
            std::process::id(),
            u128::from_le_bytes(nonce)
        ));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&path)?;
        Ok(Self(path))
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Clone, Default)]
pub(crate) struct Cache {
    index: Arc<Mutex<Option<Arc<Index>>>>,
    state: Arc<AtomicUsize>,
    scanned: Arc<AtomicUsize>,
    total: Arc<AtomicUsize>,
    sites: Arc<AtomicUsize>,
    pub(crate) batches: Arc<Mutex<BTreeMap<String, Value>>>,
}
impl Cache {
    pub(crate) fn status(&self) -> Value {
        json!({"state":match self.state.load(Ordering::Relaxed) {1=>"building",2=>"ready",_=>"not_built"},
            "scanned_classes":self.scanned.load(Ordering::Relaxed),"total_classes":self.total.load(Ordering::Relaxed),
            "invoke_sites":self.sites.load(Ordering::Relaxed),"storage":"project_owned_disk_shards",
            "active_batches":*self.batches.lock().unwrap()})
    }
    pub(super) fn get(&self, catalog: &Catalog<'_>) -> Result<(Arc<Index>, bool)> {
        let mut slot = loop {
            check_cancel(catalog.cancel)?;
            match self.index.try_lock() {
                Ok(slot) => break slot,
                Err(TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(5)),
                Err(_) => anyhow::bail!("Invoke index unavailable; reopen project"),
            }
        };
        if let Some(index) = slot.as_ref() {
            return Ok((index.clone(), true));
        }
        self.state.store(1, Ordering::Relaxed);
        self.scanned.store(0, Ordering::Relaxed);
        self.sites.store(0, Ordering::Relaxed);
        self.total.store(catalog.classes.len(), Ordering::Relaxed);
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| Index::build(catalog, self)))
                .map_err(|_| anyhow::anyhow!("Invoke index build panicked; retry request"))
                .and_then(|r| r);
        match result {
            Ok(index) => {
                self.state.store(2, Ordering::Relaxed);
                let index = Arc::new(index);
                *slot = Some(index.clone());
                Ok((index, false))
            }
            Err(e) => {
                self.state.store(0, Ordering::Relaxed);
                Err(e)
            }
        }
    }
}

pub(super) struct Index {
    directory: Directory,
    names: Vec<String>,
    lengths: Vec<u64>,
}
impl Index {
    fn build(catalog: &Catalog<'_>, progress: &Cache) -> Result<Self> {
        let directory = Directory::new()?;
        let mut writers = (0..SHARDS * 2)
            .map(|i| {
                File::create(directory.0.join(i.to_string()))
                    .map(|f| BufWriter::with_capacity(65536, f))
            })
            .collect::<std::io::Result<Vec<_>>>()?;
        let mut names = Vec::new();
        let mut targets = HashMap::new();
        for (ci, (name, class)) in catalog.classes.iter().enumerate() {
            check_cancel(catalog.cancel)?;
            names.push(name.clone());
            for (mi, method) in class.methods.iter().enumerate() {
                for site in native_xrefs::method_call_sites(class, method, catalog.cancel)? {
                    let key = (Arc::as_ptr(&class.symbols) as usize, site.method_index);
                    if targets.len() >= 262144 {
                        targets.clear();
                    }
                    let callee = *targets
                        .entry(key)
                        .or_insert_with(|| fingerprint(&catalog.canonical(&site.callee)));
                    let caller = fingerprint(&site.caller);
                    for (reverse, hash) in [(false, caller), (true, callee)] {
                        let shard = (hash as usize % SHARDS) + if reverse { SHARDS } else { 0 };
                        let mut record = [0u8; RECORD];
                        record[..8].copy_from_slice(&hash.to_le_bytes());
                        record[8..12].copy_from_slice(&u32::try_from(ci)?.to_le_bytes());
                        record[12..16].copy_from_slice(&u32::try_from(mi)?.to_le_bytes());
                        record[16..20].copy_from_slice(&u32::try_from(site.pc)?.to_le_bytes());
                        writers[shard].write_all(&record)?;
                    }
                    progress.sites.fetch_add(1, Ordering::Relaxed);
                }
            }
            progress.scanned.store(ci + 1, Ordering::Relaxed);
        }
        for writer in &mut writers {
            writer.flush()?;
        }
        let lengths = writers
            .iter()
            .map(|w| w.get_ref().metadata().map(|m| m.len()))
            .collect::<std::io::Result<Vec<_>>>()?;
        drop(writers);
        check_cancel(catalog.cancel)?;
        Ok(Self {
            directory,
            names,
            lengths,
        })
    }
    pub(super) fn neighbors(
        &self,
        catalog: &Catalog<'_>,
        target: &str,
        reverse: bool,
    ) -> Result<Vec<CallSite>> {
        let hash = fingerprint(target);
        let shard = (hash as usize % SHARDS) + if reverse { SHARDS } else { 0 };
        let file = File::open(self.directory.0.join(shard.to_string()))?;
        ensure!(
            file.metadata()?.len() == self.lengths[shard],
            "Invoke index shard changed; reopen project"
        );
        let mut reader = BufReader::with_capacity(65536, file);
        let mut result = vec![];
        for n in 0..self.lengths[shard] / RECORD as u64 {
            if n % 1024 == 0 {
                check_cancel(catalog.cancel)?;
            }
            let mut record = [0u8; RECORD];
            reader.read_exact(&mut record)?;
            if u64::from_le_bytes(record[..8].try_into().unwrap()) != hash {
                continue;
            }
            let ci = u32::from_le_bytes(record[8..12].try_into().unwrap()) as usize;
            let mi = u32::from_le_bytes(record[12..16].try_into().unwrap()) as usize;
            let pc = u32::from_le_bytes(record[16..20].try_into().unwrap()) as usize;
            let class = self
                .names
                .get(ci)
                .and_then(|n| catalog.class(n))
                .context("Invalid invoke index class")?;
            let method = class
                .methods
                .get(mi)
                .context("Invalid invoke index method")?;
            let site =
                native_xrefs::call_site_at(class, method, pc)?.context("Invalid indexed invoke")?;
            let actual = if reverse {
                catalog.canonical(&site.callee)
            } else {
                site.caller.clone()
            };
            if actual == target {
                result.push(site);
            }
        }
        check_cancel(catalog.cancel)?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::class;
    use super::*;
    use std::sync::atomic::AtomicBool;
    fn fixtures() -> Vec<crate::native_dex::DexClass> {
        vec![
            class(
                "app.One",
                None,
                vec![(
                    "run",
                    vec![],
                    vec!["api.Parent.call()V", "api.Child.call()V"],
                )],
            ),
            class(
                "app.Two",
                None,
                vec![("run", vec![], vec!["api.Other.call()V"])],
            ),
            class("api.Parent", None, vec![("call", vec![], vec![])]),
            class("api.Child", Some("api.Parent"), vec![]),
        ]
    }
    #[test]
    fn index_reuses_exact_dex_sites_and_inherited_aliases() {
        let classes = fixtures();
        let cancel = AtomicBool::new(false);
        let catalog = Catalog::new(&classes, &cancel).unwrap();
        let cache = Cache::default();
        let (index, cached) = cache.get(&catalog).unwrap();
        assert!(!cached);
        let (again, cached) = cache.get(&catalog).unwrap();
        assert!(cached && Arc::ptr_eq(&index, &again));
        let hits = index
            .neighbors(&catalog, "api.Parent.call()V", true)
            .unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].pc, 1);
        assert_eq!(hits[1].pc, 5);
        assert_eq!(hits[1].callee, "api.Child.call()V");
        assert_eq!(
            index
                .neighbors(&catalog, "api.Other.call()V", true)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            index
                .neighbors(&catalog, "app.One.run()V", false)
                .unwrap()
                .len(),
            2
        );
        assert_eq!(cache.status()["state"], "ready");
        assert_eq!(cache.status()["scanned_classes"], 4);
        assert_eq!(cache.status()["invoke_sites"], 3);
        cancel.store(true, Ordering::Relaxed);
        assert!(
            index
                .neighbors(&catalog, "api.Parent.call()V", true)
                .is_err()
        );
    }
    #[test]
    fn fingerprint_candidates_are_verified_against_exact_target() {
        let classes = fixtures();
        let cancel = AtomicBool::new(false);
        let catalog = Catalog::new(&classes, &cancel).unwrap();
        let cache = Cache::default();
        let mut index = Index::build(&catalog, &cache).unwrap();
        let target = "api.Parent.call()V";
        let hash = fingerprint(target);
        let shard = hash as usize % SHARDS + SHARDS;
        // Simulate a hash collision pointing at app.Two's invoke of api.Other.
        let ci = index.names.iter().position(|s| s == "app.Two").unwrap() as u32;
        let mut bytes = Vec::new();
        bytes.extend(hash.to_le_bytes());
        bytes.extend(ci.to_le_bytes());
        bytes.extend(0u32.to_le_bytes());
        bytes.extend(1u32.to_le_bytes());
        fs::write(index.directory.0.join(shard.to_string()), bytes).unwrap();
        index.lengths[shard] = 20;
        assert!(index.neighbors(&catalog, target, true).unwrap().is_empty());
        fs::write(index.directory.0.join(shard.to_string()), []).unwrap();
        assert!(index.neighbors(&catalog, target, true).is_err());
    }
    #[test]
    fn waiting_for_shared_build_is_cancellable_without_blocking_progress() {
        let cache = Cache::default();
        let held = cache.index.lock().unwrap();
        let cancel = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let (tx, rx) = std::sync::mpsc::channel();
            let cache = &cache;
            let cancel = &cancel;
            let waiting = scope.spawn(move || {
                let classes = fixtures();
                let catalog = Catalog::new(&classes, cancel).unwrap();
                tx.send(()).unwrap();
                cache.get(&catalog).err().unwrap().to_string()
            });
            rx.recv_timeout(Duration::from_secs(2)).unwrap();
            assert_eq!(cache.status()["state"], "not_built");
            cancel.store(true, Ordering::Relaxed);
            assert!(waiting.join().unwrap().contains("cancelled"));
        });
        drop(held);
        cancel.store(false, Ordering::Relaxed);
        let classes = fixtures();
        assert!(cache.get(&Catalog::new(&classes, &cancel).unwrap()).is_ok());
    }
    #[test]
    fn cancelled_build_is_retryable_and_files_follow_project_lifetime() {
        let classes = fixtures();
        let cancel = AtomicBool::new(true);
        let cache = Cache::default();
        let failed = Catalog::new(&classes, &cancel).and_then(|c| cache.get(&c));
        assert!(failed.is_err());
        cancel.store(false, Ordering::Relaxed);
        let catalog = Catalog::new(&classes, &cancel).unwrap();
        let (index, _) = cache.get(&catalog).unwrap();
        let path = index.directory.0.clone();
        assert!(path.exists());
        drop(index);
        drop(cache);
        assert!(!path.exists());
    }
}
