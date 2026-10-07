//! Manages one marked block per alias in ~/.ssh/config and leaves the rest of the
//! file alone. Pods get a new IP and port on every start, so the block is rewritten
//! each time a pod comes up.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{Error, Result};
use crate::fsutil;

#[derive(Debug, Clone, PartialEq)]
pub struct HostEntry {
    pub alias: String,
    pub host: String,
    pub port: u16,
    pub identity_file: String,
    /// Shown in the block's comment line, e.g. `offrig-medium (abc123)`.
    pub label: String,
}

fn markers(alias: &str) -> (String, String) {
    (
        format!("# >>> offrig:{alias} >>>"),
        format!("# <<< offrig:{alias} <<<"),
    )
}

pub fn render_block(e: &HostEntry) -> String {
    let (begin, end) = markers(&e.alias);
    [
        begin,
        format!(
            "# {} (managed by offrig; rewritten on every pod start)",
            e.label
        ),
        format!("Host {}", e.alias),
        format!("    HostName {}", e.host),
        format!("    Port {}", e.port),
        "    User root".into(),
        format!("    IdentityFile {}", e.identity_file),
        "    IdentitiesOnly yes".into(),
        "    UserKnownHostsFile ~/.ssh/known_hosts_offrig".into(),
        "    StrictHostKeyChecking accept-new".into(),
        "    ServerAliveInterval 30".into(),
        "    ServerAliveCountMax 4".into(),
        end,
    ]
    .join("\n")
}

/// Byte range of the alias's block, end marker included.
fn find_block(text: &str, alias: &str) -> Option<(usize, usize)> {
    let (begin, end) = markers(alias);
    let start = text.find(&begin)?;
    let end_at = text[start..].find(&end)? + start + end.len();
    Some((start, end_at))
}

/// Replace the alias's block in place, or append it after one blank line.
pub fn upsert(text: &str, e: &HostEntry) -> String {
    let block = render_block(e);
    match find_block(text, &e.alias) {
        Some((s, t)) => format!("{}{}{}", &text[..s], block, &text[t..]),
        None => {
            let head = text.trim_end();
            if head.is_empty() {
                format!("{block}\n")
            } else {
                format!("{head}\n\n{block}\n")
            }
        }
    }
}

pub fn remove(text: &str, alias: &str) -> String {
    match find_block(text, alias) {
        Some((s, t)) => {
            let before = text[..s].trim_end();
            let after = text[t..].trim_start_matches(['\r', '\n']);
            match (before.is_empty(), after.is_empty()) {
                (true, _) => after.to_string(),
                (false, true) => format!("{before}\n"),
                (false, false) => format!("{before}\n\n{after}"),
            }
        }
        None => text.to_string(),
    }
}

/// Whether the alias's block was written for `pod_id` (its label line names the pod).
fn block_names_pod(text: &str, alias: &str, pod_id: &str) -> bool {
    find_block(text, alias).is_some_and(|(s, t)| text[s..t].contains(&format!("({pod_id})")))
}

/// Remove the alias's block from the file, but only if it names `pod_id`. A block that
/// was rewritten for a newer pod, or belongs to another alias (another lane), stays.
/// Returns whether a block was removed.
pub fn remove_for_pod_at(path: &Path, alias: &str, pod_id: &str) -> Result<bool> {
    let old = read_or_empty(path)?;
    if !block_names_pod(&old, alias, pod_id) {
        return Ok(false);
    }
    fsutil::write_atomic(path, remove(&old, alias).as_bytes())?;
    Ok(true)
}

/// [`remove_for_pod_at`] on `~/.ssh/config`: a pod that is gone leaves no block behind
/// pointing at it.
pub fn remove_for_pod(alias: &str, pod_id: &str) -> Result<bool> {
    remove_for_pod_at(&config_path()?, alias, pod_id)
}

pub fn config_path() -> Result<PathBuf> {
    Ok(fsutil::home_ssh_dir()?.join("config"))
}

pub fn known_hosts_path() -> Result<PathBuf> {
    Ok(fsutil::home_ssh_dir()?.join("known_hosts_offrig"))
}

fn read_or_empty(path: &Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(t) => Ok(t.strip_prefix('\u{feff}').map(str::to_string).unwrap_or(t)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(Error::io(format!("reading {}", path.display()), e)),
    }
}

/// Write the alias's block into ~/.ssh/config and forget any old host key for the
/// endpoint (RunPod reuses ip:port pairs across pods with new host keys).
pub fn apply(e: &HostEntry) -> Result<()> {
    // Forget the old key only when the block changed (a new pod or endpoint). An
    // unchanged endpoint keeps its pinned key, so a swapped host still fails ssh.
    if apply_at(&config_path()?, e)? {
        forget_host_key(&e.host, e.port);
    }
    Ok(())
}

/// Returns whether the file changed.
pub fn apply_at(path: &Path, e: &HostEntry) -> Result<bool> {
    let old = read_or_empty(path)?;
    let new = upsert(&old, e);
    if new == old {
        return Ok(false);
    }
    // OpenSSH on Windows misreads a BOM; write plain UTF-8.
    fsutil::write_atomic(path, new.as_bytes())?;
    Ok(true)
}

pub fn remove_at(path: &Path, alias: &str) -> Result<()> {
    let old = read_or_empty(path)?;
    let new = remove(&old, alias);
    if new != old {
        fsutil::write_atomic(path, new.as_bytes())?;
    }
    Ok(())
}

fn forget_host_key(host: &str, port: u16) {
    if let Ok(kh) = known_hosts_path()
        && kh.is_file()
    {
        // A missing entry makes ssh-keygen exit non-zero; that is not a failure here.
        let _ = crate::proc::quiet(
            Command::new("ssh-keygen")
                .arg("-R")
                .arg(format!("[{host}]:{port}"))
                .arg("-f")
                .arg(&kh),
        )
        .output();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(alias: &str, host: &str, port: u16) -> HostEntry {
        HostEntry {
            alias: alias.into(),
            host: host.into(),
            port,
            identity_file: "~/.ssh/runpod_rustline".into(),
            label: "offrig-medium (abc)".into(),
        }
    }

    #[test]
    fn upsert_appends_then_replaces_in_place() {
        let start = "Host github.com\n    User git\n";
        let once = upsert(start, &entry("offrig", "10.0.0.1", 40001));
        assert!(once.starts_with("Host github.com\n    User git\n\n# >>> offrig:offrig >>>"));
        let twice = upsert(&once, &entry("offrig", "10.0.0.2", 40002));
        assert_eq!(twice.matches("# >>> offrig:offrig >>>").count(), 1);
        assert!(twice.contains("HostName 10.0.0.2") && twice.contains("Port 40002"));
        assert!(!twice.contains("10.0.0.1"));
        assert!(twice.starts_with("Host github.com\n    User git\n\n"));
    }

    #[test]
    fn two_aliases_coexist_and_remove_takes_only_one() {
        let a = upsert("", &entry("offrig", "1.1.1.1", 1));
        let ab = upsert(&a, &entry("offrig-b", "2.2.2.2", 2));
        assert!(ab.contains("Host offrig\n") && ab.contains("Host offrig-b\n"));
        let b_only = remove(&ab, "offrig");
        assert!(!b_only.contains("Host offrig\n"));
        assert!(b_only.contains("Host offrig-b\n"));
        assert_eq!(remove(&b_only, "offrig-b"), "");
    }

    #[test]
    fn each_lane_rewrites_only_its_own_block_and_the_plain_block_is_unchanged() {
        let plain = entry("offrig", "1.1.1.1", 1);
        let before = upsert(
            "Host a
", &plain,
        );
        let a = upsert(&before, &entry("offrig-aspire-si", "2.2.2.2", 2));
        let both = upsert(&a, &entry("offrig-ai-jam-sessions", "3.3.3.3", 3));
        // A second launch by lane aspire-si moves only aspire-si's block.
        let moved = upsert(&both, &entry("offrig-aspire-si", "9.9.9.9", 9));
        assert_eq!(
            moved.matches("# >>> offrig:offrig-aspire-si >>>").count(),
            1
        );
        assert!(moved.contains("HostName 9.9.9.9") && !moved.contains("2.2.2.2"));
        assert!(
            moved.contains("HostName 3.3.3.3"),
            "another lane is untouched"
        );
        assert!(
            moved.contains(&render_block(&plain)),
            "the plain lane's block is byte-for-byte unchanged"
        );
        assert!(render_block(&plain).starts_with(
            "# >>> offrig:offrig >>>
"
        ));
        // Removing one lane leaves the others.
        let gone = remove(&moved, "offrig-aspire-si");
        assert!(!gone.contains("offrig-aspire-si") && gone.contains("Host offrig-ai-jam-sessions"));
        assert!(gone.contains(&render_block(&plain)));
    }

    #[test]
    fn a_pods_block_is_removed_only_for_that_pod_and_only_under_its_own_alias() {
        let dir = std::env::temp_dir().join(format!("offrig-ssh-pod-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("config");
        let plain = HostEntry {
            label: "offrig-job (pod-a)".into(),
            ..entry("offrig", "1.1.1.1", 1)
        };
        let lane = HostEntry {
            label: "offrig-ai-jam-sessions-jam (pod-b)".into(),
            ..entry("offrig-ai-jam-sessions", "2.2.2.2", 2)
        };
        std::fs::write(&path, "Host a\n").expect("write");
        apply_at(&path, &plain).expect("plain");
        apply_at(&path, &lane).expect("lane");
        let both = std::fs::read_to_string(&path).expect("read");

        // A pod that is not the one the block names changes nothing.
        assert!(!remove_for_pod_at(&path, "offrig", "pod-x").expect("other pod"));
        // The lane's pod id under the plain lane's alias changes nothing either.
        assert!(!remove_for_pod_at(&path, "offrig", "pod-b").expect("other lane"));
        // A missing alias or missing file is not an error.
        assert!(!remove_for_pod_at(&path, "offrig-nope", "pod-a").expect("no block"));
        assert!(!remove_for_pod_at(&dir.join("absent"), "offrig", "pod-a").expect("no file"));
        assert_eq!(std::fs::read_to_string(&path).expect("read"), both);

        // The plain lane's own pod removes the plain block and leaves the lane's.
        assert!(remove_for_pod_at(&path, "offrig", "pod-a").expect("removed"));
        let after = std::fs::read_to_string(&path).expect("read");
        assert!(!after.contains("Host offrig\n") && !after.contains("pod-a"));
        assert!(after.contains(&render_block(&lane)));
        assert!(after.starts_with("Host a\n"));
        // A pod id is matched whole: a prefix of it does not name the block.
        assert!(!remove_for_pod_at(&path, "offrig-ai-jam-sessions", "pod").expect("prefix"));
        assert!(remove_for_pod_at(&path, "offrig-ai-jam-sessions", "pod-b").expect("removed"));
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "Host a\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_restores_surrounding_text() {
        let base = "Host a\n    User x\n";
        let with = upsert(base, &entry("offrig", "1.1.1.1", 1));
        assert_eq!(remove(&with, "offrig"), base);
        let trailing = format!("{with}\nHost z\n    User y\n");
        assert_eq!(
            remove(&trailing, "offrig"),
            "Host a\n    User x\n\nHost z\n    User y\n"
        );
    }

    #[test]
    fn alias_prefix_does_not_match_a_longer_alias() {
        let b = upsert("", &entry("offrig-b", "2.2.2.2", 2));
        let both = upsert(&b, &entry("offrig", "1.1.1.1", 1));
        assert_eq!(both.matches("Host ").count(), 2);
    }

    #[test]
    fn file_round_trip_has_no_bom_and_strips_one() {
        let dir = std::env::temp_dir().join(format!("offrig-ssh-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("config");
        std::fs::write(&path, "\u{feff}Host a\n").expect("write");
        assert!(
            apply_at(&path, &entry("offrig", "1.1.1.1", 1)).expect("apply"),
            "first write changes the file"
        );
        assert!(
            !apply_at(&path, &entry("offrig", "1.1.1.1", 1)).expect("apply"),
            "same endpoint is a no-op"
        );
        let bytes = std::fs::read(&path).expect("read");
        assert_ne!(&bytes[..3], b"\xEF\xBB\xBF");
        remove_at(&path, "offrig").expect("remove");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "Host a\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
