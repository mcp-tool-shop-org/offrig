//! Small file helpers shared by the modules that edit user files.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// Write via a sibling temp file and a rename, so a crash never leaves half a file.
/// Bytes are written as given (no BOM is added).
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| Error::io(format!("creating {}", dir.display()), e))?;
    }
    let tmp = path.with_extension("offrig-tmp");
    std::fs::write(&tmp, bytes).map_err(|e| Error::io(format!("writing {}", tmp.display()), e))?;
    std::fs::rename(&tmp, path).map_err(|e| Error::io(format!("replacing {}", path.display()), e))
}

/// Copy `path` to `<path>.offrig.bak` before offrig first changes it, and keep that
/// first copy: it is the state to restore if offrig is removed.
pub fn backup_once(path: &Path) -> Result<Option<PathBuf>> {
    if !path.is_file() {
        return Ok(None);
    }
    let mut name = path.as_os_str().to_owned();
    name.push(".offrig.bak");
    let bak = PathBuf::from(name);
    if !bak.exists() {
        std::fs::copy(path, &bak)
            .map_err(|e| Error::io(format!("backing up {}", path.display()), e))?;
    }
    Ok(Some(bak))
}

/// Tests point the binaries at a temp home so nothing touches the real `~/.ssh` or
/// Zed settings. Debug builds only: a release binary ignores it, like the mock RunPod.
#[cfg(debug_assertions)]
pub fn test_home() -> Option<PathBuf> {
    std::env::var_os("OFFRIG_TEST_HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

#[cfg(not(debug_assertions))]
pub fn test_home() -> Option<PathBuf> {
    None
}

pub fn home_ssh_dir() -> Result<PathBuf> {
    if let Some(h) = test_home() {
        return Ok(h.join(".ssh"));
    }
    dirs::home_dir()
        .map(|h| h.join(".ssh"))
        .ok_or_else(|| Error::Config("no home directory".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("offrig-fsutil-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("temp dir");
        d
    }

    #[test]
    fn write_atomic_creates_parents_replaces_and_leaves_no_temp_file() {
        let dir = temp("atomic");
        let target = dir.join("a").join("b").join("settings.json");
        write_atomic(&target, b"first").expect("create with parents");
        assert_eq!(std::fs::read(&target).expect("read"), b"first");
        write_atomic(&target, b"\xEF\xBB\xBFsecond").expect("replace");
        assert_eq!(
            std::fs::read(&target).expect("read"),
            b"\xEF\xBB\xBFsecond",
            "bytes are written as given"
        );
        assert!(!target.with_extension("offrig-tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_atomic_reports_where_it_failed() {
        let dir = temp("atomic-fail");
        // A file where a directory is needed.
        let blocker = dir.join("file");
        std::fs::write(&blocker, b"x").expect("write");
        let err = write_atomic(&blocker.join("child").join("f"), b"y").expect_err("no dir");
        assert!(matches!(err, Error::Io { .. }), "{err}");
        assert!(err.to_string().contains("creating"), "{err}");
        // A directory where the file should go: the rename cannot replace it.
        let as_dir = dir.join("occupied");
        std::fs::create_dir_all(&as_dir).expect("dir");
        let err = write_atomic(&as_dir, b"y").expect_err("target is a directory");
        assert!(matches!(err, Error::Io { .. }), "{err}");
        assert!(as_dir.is_dir(), "the original directory is untouched");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn backup_once_keeps_the_first_copy_only() {
        let dir = temp("backup");
        let f = dir.join("config");
        assert_eq!(
            backup_once(&f).expect("missing"),
            None,
            "nothing to back up"
        );
        std::fs::write(&f, b"original").expect("write");
        let bak = backup_once(&f).expect("first").expect("a backup path");
        assert!(bak.to_string_lossy().ends_with("config.offrig.bak"));
        assert_eq!(std::fs::read(&bak).expect("read"), b"original");
        std::fs::write(&f, b"edited").expect("write");
        assert_eq!(backup_once(&f).expect("second"), Some(bak.clone()));
        assert_eq!(
            std::fs::read(&bak).expect("read"),
            b"original",
            "the first copy is the one to restore"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
