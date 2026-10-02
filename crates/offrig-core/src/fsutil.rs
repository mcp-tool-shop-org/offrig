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

pub fn home_ssh_dir() -> Result<PathBuf> {
    dirs::home_dir()
        .map(|h| h.join(".ssh"))
        .ok_or_else(|| Error::Config("no home directory".into()))
}
