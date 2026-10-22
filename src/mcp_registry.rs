//! Conservative cleanup of local MCP discovery records.
//!
//! A failed TCP connection is not evidence that its GUI has exited. Only prune
//! records with an owner PID that the operating system confirms is absent.
//! A reused PID is deliberately retained: it may leave a stale record, but can
//! never cause a live registration to be removed on that basis.
use std::{fs, path::Path};

/// Forget one explicitly selected discovery record. The caller must verify the
/// instance identity and refuse reachable instances before invoking this helper.
/// This removes only the registration file; it never signals its owner.
pub(super) fn forget(path: &Path, original: &[u8]) -> anyhow::Result<()> {
    anyhow::ensure!(
        fs::symlink_metadata(path)?.file_type().is_file(),
        "Registration must be a regular file"
    );
    anyhow::ensure!(
        fs::read(path)? == original,
        "Registration changed during forget; retry discovery"
    );
    fs::remove_file(path)?;
    Ok(())
}

pub(super) fn prune_if_dead(path: &Path, original: &[u8], owner_pid: Option<u32>) -> bool {
    let Some(pid) = owner_pid else {
        // Older records do not contain process identity.
        return false;
    };
    if !process_is_absent(pid) {
        return false;
    }
    // Retain symlinks and records replaced since discovery read them.
    if !fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_file())
        || fs::read(path).map_or(true, |bytes| bytes != original)
        || !process_is_absent(pid)
    {
        return false;
    }
    fs::remove_file(path).is_ok()
}

#[cfg(unix)]
fn process_is_absent(pid: u32) -> bool {
    // kill(0, 0) and negative PIDs address process groups, not one owner.
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    // SAFETY: signal 0 checks existence/permission without delivering a signal.
    (unsafe { libc::kill(pid, 0) }) == -1
        && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

#[cfg(not(unix))]
fn process_is_absent(_pid: u32) -> bool {
    // An unsupported existence probe is an unknown state, never proof of exit.
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDir(std::path::PathBuf);
    impl TestDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "rdx-registry-{}",
                super::super::random_id().unwrap()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn live_unreachable_and_legacy_records_are_retained() {
        let directory = TestDir::new();
        let path = directory.path().join("instance.json");
        let bytes = br#"{"address":"127.0.0.1:1"}"#;
        fs::write(&path, bytes).unwrap();
        assert!(!prune_if_dead(&path, bytes, Some(std::process::id())));
        assert!(!prune_if_dead(&path, bytes, None));
        assert!(!prune_if_dead(&path, bytes, Some(0)));
        assert_eq!(fs::read(path).unwrap(), bytes);
    }

    #[test]
    fn explicit_forget_removes_only_the_unchanged_selected_record() {
        let directory = TestDir::new();
        let selected = directory.path().join("selected.json");
        let other = directory.path().join("other.json");
        fs::write(&selected, b"selected").unwrap();
        fs::write(&other, b"other").unwrap();
        assert!(forget(&selected, b"old contents").is_err());
        assert_eq!(fs::read(&selected).unwrap(), b"selected");
        forget(&selected, b"selected").unwrap();
        assert!(!selected.exists());
        assert_eq!(fs::read(other).unwrap(), b"other");
    }

    #[cfg(unix)]
    #[test]
    fn explicit_forget_refuses_symlink_and_preserves_target() {
        let directory = TestDir::new();
        let target = directory.path().join("target.json");
        let link = directory.path().join("link.json");
        fs::write(&target, b"target").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(forget(&link, b"target").is_err());
        assert!(fs::symlink_metadata(link).unwrap().file_type().is_symlink());
        assert_eq!(fs::read(target).unwrap(), b"target");
    }

    #[cfg(unix)]
    #[test]
    fn exited_owner_is_pruned_but_replaced_record_is_retained() {
        let directory = TestDir::new();
        let path = directory.path().join("instance.json");
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .spawn()
            .unwrap();
        let pid = child.id();
        child.wait().unwrap();
        let original = b"original record";
        fs::write(&path, b"replacement record").unwrap();
        assert!(!prune_if_dead(&path, original, Some(pid)));
        assert!(path.exists());
        fs::write(&path, original).unwrap();
        assert!(prune_if_dead(&path, original, Some(pid)));
        assert!(!path.exists());
    }
}
