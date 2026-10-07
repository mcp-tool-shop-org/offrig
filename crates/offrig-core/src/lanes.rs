//! Lanes: what keeps two projects' side-cars from colliding on one account.
//!
//! Three things were global and each collided: the one SSH alias `offrig` (a second
//! project's launch re-pointed it at its own pod), the one tunnel port 11435 (a second
//! project's tunnel start reclaimed the first one's), and pod names `offrig-<profile>`
//! with no project in them (a side-car took another project's pod for its own).
//!
//! A *lane* is one project's own alias, tunnel port and pod-name tag. The registry
//! `<config dir>/lanes.toml` maps a canonical project path to its lane. A lane is
//! allocated first-free the first time a project's side-car starts, then kept: the same
//! project gets the same lane for as long as the file lives. The CLI, the egui app and
//! the Zed wiring stay on the *plain lane* (alias `offrig`, port 11435, pods
//! `offrig-<profile>`), which is not in the registry and is never allocated.
//!
//! Allocation is safe against two side-cars starting at once: it runs under a lock file
//! created with `create_new`, and the registry is replaced atomically.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

use crate::config::{Config, LOCAL_OLLAMA_PORT, config_dir};
use crate::error::{Error, Result};
use crate::fsutil;
use crate::store::{Plan, Store};

pub const LANES_FILE: &str = "lanes.toml";
const LOCK_FILE: &str = "lanes.lock";
/// First lane port. The range sits well above the local Ollama (11434) and the plain
/// lane (11435, runner 11436), so no lane can ever be either.
pub const LANE_PORT_BASE: u16 = 11500;
/// Lane ports are two apart: the lane's own port, and the one above it for the
/// handoff runner's second tunnel.
pub const LANE_PORT_STEP: u16 = 2;
/// How many lanes the range holds.
pub const LANE_COUNT: u16 = 64;
/// First default side-car port: the port a project's shell-driven side-car listens on
/// (loopback HTTP, in front of `offrig-mcp`). Lane `i` gets `SIDECAR_PORT_BASE + i`.
/// The range 11700..=11763 sits above every tunnel and runner port a lane can have
/// (11500..=11627), the plain lane (11435, 11436) and the local Ollama (11434), so a
/// side-car port can never be a tunnel port. It replaces the one machine-wide default
/// (11439) that two projects shared (issue #11).
pub const SIDECAR_PORT_BASE: u16 = 11700;
pub const MAX_TAG_LEN: usize = 24;
/// Tags that would collide with names offrig already uses: staging pods are
/// `offrig-stage-<profile>` and the staging alias is `<alias>-stage`.
const RESERVED_TAGS: [&str; 2] = ["stage", "offrig"];

/// One lane. `tag` is `None` for the plain lane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lane {
    pub tag: Option<String>,
    pub ssh_alias: String,
    pub tunnel_port: u16,
}

impl Lane {
    /// The plain lane of a config: the CLI's, the app's and Zed's.
    pub fn plain(base: &Config) -> Self {
        Lane {
            tag: None,
            ssh_alias: base.ssh_alias.clone(),
            tunnel_port: base.tunnel_port,
        }
    }

    /// The runner's own tunnel port, so it never shares the side-car's.
    pub fn runner_port(&self) -> u16 {
        self.tunnel_port + 1
    }

    pub fn label(&self) -> &str {
        self.tag.as_deref().unwrap_or("plain")
    }

    /// The default port of this project's side-car, derived from the lane's slot the
    /// same way its tunnel port is: lane `i` (tunnel port `11500 + 2i`) gets
    /// `11700 + i`. Two projects have two lanes, so two different ports by default.
    /// `None` for the plain lane, which has no side-car of its own.
    pub fn sidecar_port(&self) -> Option<u16> {
        self.tag.as_ref()?;
        let slot = self.tunnel_port.checked_sub(LANE_PORT_BASE)? / LANE_PORT_STEP;
        (slot < LANE_COUNT).then(|| SIDECAR_PORT_BASE + slot)
    }
}

/// The port of the `i`th lane.
pub fn lane_port(i: u16) -> u16 {
    LANE_PORT_BASE + i * LANE_PORT_STEP
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Record {
    /// The canonical project path (see [`project_key`]).
    project: String,
    tag: String,
    ssh_alias: String,
    tunnel_port: u16,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    lane: Vec<Record>,
}

impl Record {
    fn lane(&self) -> Lane {
        Lane {
            tag: Some(self.tag.clone()),
            ssh_alias: self.ssh_alias.clone(),
            tunnel_port: self.tunnel_port,
        }
    }
}

/// A canonical, comparable form of a project path: absolute, symlinks resolved, `/`
/// separators, no trailing slash, lower case on Windows.
pub fn project_key(project: &Path) -> String {
    let p = std::fs::canonicalize(project)
        .or_else(|_| std::path::absolute(project))
        .unwrap_or_else(|_| project.to_path_buf());
    let mut s = p.to_string_lossy().replace('\\', "/");
    if let Some(rest) = s.strip_prefix("//?/") {
        s = rest.to_string();
    }
    while s.len() > 1 && s.ends_with('/') && !s.ends_with(":/") {
        s.pop();
    }
    if cfg!(windows) {
        s = s.to_lowercase();
    }
    s
}

/// A DNS- and filesystem-safe slug of a folder name: `[a-z0-9-]`, no edge dashes.
fn slug(name: &str, max: usize) -> String {
    let mut out = String::new();
    for c in name.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let mut out: String = out.chars().take(max).collect();
    while out.ends_with('-') {
        out.pop();
    }
    out
}

fn tag_shape_ok(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= MAX_TAG_LEN
        && !tag.starts_with('-')
        && !tag.ends_with('-')
        && tag
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// FNV-1a, folded to 16 bits: a short, stable disambiguator for a project path.
fn short_hash(key: &str) -> String {
    let mut h: u32 = 0x811c_9dc5;
    for b in key.bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    format!("{:04x}", (h ^ (h >> 16)) & 0xffff)
}

/// The pod names a tag would give: `offrig-<tag>-<profile>` for every profile.
fn pod_names(tag: &str, profiles: &[String]) -> Vec<String> {
    profiles
        .iter()
        .map(|p| format!("offrig-{tag}-{p}"))
        .collect()
}

/// Everything an allocation must not collide with besides other lanes.
struct Avoid {
    aliases: Vec<String>,
    /// Ports that are taken, each with the one above it (the runner's).
    ports: Vec<u16>,
    /// Profile names, for pod-name collisions.
    profiles: Vec<String>,
}

impl Avoid {
    fn of(base: &Config) -> Self {
        Avoid {
            aliases: vec![base.ssh_alias.clone(), format!("{}-stage", base.ssh_alias)],
            ports: vec![base.tunnel_port, base.tunnel_port + 1, LOCAL_OLLAMA_PORT],
            profiles: base.profiles.iter().map(|p| p.name.clone()).collect(),
        }
    }
}

fn tag_free(tag: &str, records: &[Record], avoid: &Avoid) -> bool {
    if !tag_shape_ok(tag) || RESERVED_TAGS.contains(&tag) || tag.ends_with("-stage") {
        return false;
    }
    let alias = format!("offrig-{tag}");
    if avoid.aliases.contains(&alias)
        || records.iter().any(|r| r.tag == tag || r.ssh_alias == alias)
    {
        return false;
    }
    let mine = pod_names(tag, &avoid.profiles);
    // Not a plain-lane pod name (`offrig-<profile>`)...
    if mine
        .iter()
        .any(|n| avoid.profiles.iter().any(|p| *n == format!("offrig-{p}")))
    {
        return false;
    }
    // ...and not another lane's pod name (`offrig-a-b-c` from tag `a` + profile `b-c`
    // and from tag `a-b` + profile `c`).
    !records.iter().any(|r| {
        pod_names(&r.tag, &avoid.profiles)
            .iter()
            .any(|n| mine.contains(n))
    })
}

fn port_free(port: u16, records: &[Record], avoid: &Avoid) -> bool {
    let clash = |p: u16| avoid.ports.contains(&p) || avoid.ports.contains(&(p + 1));
    port != LOCAL_OLLAMA_PORT
        && port + 1 != LOCAL_OLLAMA_PORT
        && !clash(port)
        && !records.iter().any(|r| r.tunnel_port == port)
}

/// Check a registry as read from disk. A hand-edited file that would give a lane the
/// local Ollama's port, share a name or port between lanes, or leave the lane range is
/// refused, never repaired.
fn validate(records: &[Record]) -> Result<()> {
    let bad = |why: String| Err(Error::Config(format!("{LANES_FILE}: {why}")));
    for (i, r) in records.iter().enumerate() {
        if !tag_shape_ok(&r.tag) {
            return bad(format!(
                "tag {:?} must be 1-{MAX_TAG_LEN} of a-z, 0-9, -",
                r.tag
            ));
        }
        if r.ssh_alias != format!("offrig-{}", r.tag) {
            return bad(format!(
                "lane {} must use the alias offrig-{}, not {:?}",
                r.tag, r.tag, r.ssh_alias
            ));
        }
        let top = lane_port(LANE_COUNT - 1);
        if r.tunnel_port == LOCAL_OLLAMA_PORT
            || r.tunnel_port < LANE_PORT_BASE
            || r.tunnel_port > top
            || !(r.tunnel_port - LANE_PORT_BASE).is_multiple_of(LANE_PORT_STEP)
        {
            return bad(format!(
                "lane {} port {} is outside the lane range {LANE_PORT_BASE}..={top} (steps of {LANE_PORT_STEP}; never the local Ollama's {LOCAL_OLLAMA_PORT})",
                r.tag, r.tunnel_port
            ));
        }
        for o in &records[i + 1..] {
            if o.tag == r.tag
                || o.ssh_alias == r.ssh_alias
                || o.tunnel_port == r.tunnel_port
                || o.project == r.project
            {
                return bad(format!(
                    "lanes {} and {} share a project, tag, alias or port",
                    r.tag, o.tag
                ));
            }
        }
    }
    Ok(())
}

/// The registry file and its lock.
#[derive(Debug, Clone)]
pub struct Registry {
    dir: PathBuf,
    lock_wait: Duration,
    lock_stale: Duration,
}

struct LockGuard(PathBuf);

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

impl Registry {
    /// A registry in `dir` (the config directory, or a temp dir in tests).
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Registry {
            dir: dir.into(),
            lock_wait: Duration::from_secs(10),
            lock_stale: Duration::from_secs(30),
        }
    }

    /// The registry in offrig's config directory.
    pub fn open_default() -> Result<Self> {
        Ok(Self::at(config_dir()?))
    }

    pub fn with_lock_timing(mut self, wait: Duration, stale: Duration) -> Self {
        self.lock_wait = wait;
        self.lock_stale = stale;
        self
    }

    pub fn path(&self) -> PathBuf {
        self.dir.join(LANES_FILE)
    }

    fn lock(&self) -> Result<LockGuard> {
        std::fs::create_dir_all(&self.dir)
            .map_err(|e| Error::io(format!("creating {}", self.dir.display()), e))?;
        let path = self.dir.join(LOCK_FILE);
        let deadline = Instant::now() + self.lock_wait;
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return Ok(LockGuard(path)),
                // On Windows a lock that is being deleted answers PermissionDenied.
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::PermissionDenied
                    ) =>
                {
                    // A side-car that died holding the lock must not wedge every other.
                    let age = std::fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| SystemTime::now().duration_since(t).ok());
                    if age.is_some_and(|a| a > self.lock_stale) {
                        let _ = std::fs::remove_file(&path);
                        continue;
                    }
                    if Instant::now() >= deadline {
                        return Err(Error::Timeout(format!(
                            "the lane registry lock {}",
                            path.display()
                        )));
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => return Err(Error::io(format!("locking {}", path.display()), e)),
            }
        }
    }

    fn read(&self) -> Result<Vec<Record>> {
        let path = self.path();
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(Error::io(format!("reading {}", path.display()), e)),
        };
        let file: File =
            toml::from_str(&text).map_err(|e| Error::Config(format!("{}: {e}", path.display())))?;
        validate(&file.lane)?;
        Ok(file.lane)
    }

    fn write(&self, records: &[Record]) -> Result<()> {
        let file = File {
            lane: records.to_vec(),
        };
        let mut text = String::from(
            "# offrig lanes: one entry per project, allocated by its side-car. Do not edit.\n",
        );
        text.push_str(&toml::to_string_pretty(&file).map_err(|e| Error::Config(e.to_string()))?);
        fsutil::write_atomic(&self.path(), text.as_bytes())
    }

    /// The project's lane: the one it already has, or the first free tag, alias and
    /// port. `base` is the plain config, whose own names and ports are avoided.
    pub fn lane_for_project(&self, project: &Path, base: &Config) -> Result<Lane> {
        let key = project_key(project);
        let _lock = self.lock()?;
        let mut records = self.read()?;
        if let Some(r) = records.iter().find(|r| r.project == key) {
            return Ok(r.lane());
        }
        let avoid = Avoid::of(base);
        let name = Path::new(&key)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let stem = {
            let s = slug(&name, MAX_TAG_LEN);
            if s.is_empty() {
                "project".to_string()
            } else {
                s
            }
        };
        let hash = short_hash(&key);
        let tag = std::iter::once(stem.clone())
            .chain(std::iter::once(format!(
                "{}-{hash}",
                slug(&stem, MAX_TAG_LEN - 5)
            )))
            .chain((2..100).map(|n| {
                let tail = format!("-{hash}-{n}");
                format!("{}{tail}", slug(&stem, MAX_TAG_LEN - tail.len()))
            }))
            .find(|t| tag_free(t, &records, &avoid))
            .ok_or_else(|| Error::Config(format!("could not find a free lane tag for {name:?}")))?;
        let port = (0..LANE_COUNT)
            .map(lane_port)
            .find(|p| port_free(*p, &records, &avoid))
            .ok_or_else(|| {
                Error::Config(format!(
                    "all {LANE_COUNT} lanes are in use; remove an unused entry from {}",
                    self.path().display()
                ))
            })?;
        let rec = Record {
            project: key,
            ssh_alias: format!("offrig-{tag}"),
            tag,
            tunnel_port: port,
        };
        records.push(rec.clone());
        validate(&records)?;
        self.write(&records)?;
        Ok(rec.lane())
    }

    /// The lane this project already has, without allocating one.
    pub fn find_project(&self, project: &Path) -> Result<Option<Lane>> {
        if !self.path().exists() {
            return Ok(None);
        }
        let key = project_key(project);
        let _lock = self.lock()?;
        Ok(self
            .read()?
            .into_iter()
            .find(|r| r.project == key)
            .map(|r| r.lane()))
    }

    /// The lane with this tag, if the registry has one.
    pub fn find_tag(&self, tag: &str) -> Result<Option<Lane>> {
        let _lock = self.lock()?;
        Ok(self
            .read()?
            .into_iter()
            .find(|r| r.tag == tag)
            .map(|r| r.lane()))
    }

    /// As [`Registry::all`], but a registry that does not exist yet means no lanes and is
    /// left alone: no folder and no lock file are created just to look (status uses this).
    pub fn all_if_present(&self) -> Result<Vec<(String, Lane)>> {
        if self.path().exists() {
            self.all()
        } else {
            Ok(Vec::new())
        }
    }

    /// Every lane with the project path it belongs to.
    pub fn all(&self) -> Result<Vec<(String, Lane)>> {
        let _lock = self.lock()?;
        Ok(self
            .read()?
            .into_iter()
            .map(|r| (r.project.clone(), r.lane()))
            .collect())
    }
}

/// A side-car's view of lanes: the plain config, the lane registry, and this project's
/// lane. The lane is allocated the first time something needs it (planning a session),
/// not when the side-car starts, so a side-car registered for every project does not
/// claim a lane in each folder it is merely started in.
#[derive(Debug, Clone)]
pub struct LaneCtx {
    /// The config as loaded: the plain lane.
    pub base: Config,
    pub project: PathBuf,
    pub registry: Registry,
    own: Arc<Mutex<Option<Lane>>>,
}

impl LaneCtx {
    pub fn new(base: Config, project: &Path, registry: Registry) -> Self {
        LaneCtx {
            base,
            project: project.to_path_buf(),
            registry,
            own: Arc::new(Mutex::new(None)),
        }
    }

    /// This project's lane, allocated now if it has none.
    pub fn own(&self) -> Result<Lane> {
        let mut g = self.own.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(l) = g.as_ref() {
            return Ok(l.clone());
        }
        let lane = self.registry.lane_for_project(&self.project, &self.base)?;
        *g = Some(lane.clone());
        Ok(lane)
    }

    /// This project's lane if it already has one; allocates nothing.
    pub fn own_if_allocated(&self) -> Result<Option<Lane>> {
        let mut g = self.own.lock().unwrap_or_else(PoisonError::into_inner);
        if g.is_none() {
            *g = self.registry.find_project(&self.project)?;
        }
        Ok(g.clone())
    }

    /// The config for new plans from this project.
    pub fn own_cfg(&self) -> Result<Config> {
        self.base.in_lane(&self.own()?)
    }

    /// The config of a lane by tag: `None` is the plain lane.
    pub fn cfg_for_tag(&self, tag: Option<&str>) -> Result<Config> {
        match tag {
            None => Ok(self.base.clone()),
            Some(t) => {
                let lane = self.registry.find_tag(t)?.ok_or_else(|| {
                    Error::Refused(format!(
                        "the plan's lane {t:?} is not in {}; its pod cannot be reached safely",
                        self.registry.path().display()
                    ))
                })?;
                self.base.in_lane(&lane)
            }
        }
    }

    /// The config a plan runs under: the lane it recorded. A plan that recorded none
    /// and is already committed was made before lanes existed, so it is the plain
    /// lane's (its pod is `offrig-<profile>`); a plan not yet launched takes this
    /// project's lane.
    pub fn cfg_for_plan(&self, store: &Store, plan: &Plan) -> Result<Config> {
        match store.plan_lane(plan.id)? {
            Some(tag) => self.cfg_for_tag(Some(&tag)),
            None if plan.state == "planned" => self.own_cfg(),
            None => self.cfg_for_tag(None),
        }
    }

    /// Record this project's lane on a plan that has none yet (before it launches).
    pub fn adopt(&self, store: &Store, plan: &Plan) -> Result<()> {
        if store.plan_lane(plan.id)?.is_none() && plan.state == "planned" {
            let lane = self.own()?;
            if let Some(tag) = &lane.tag {
                store.set_plan_lane(plan.id, tag)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("offrig-lanes-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("temp dir");
        d
    }

    fn project(root: &Path, name: &str) -> PathBuf {
        let p = root.join("projects").join(name);
        std::fs::create_dir_all(&p).expect("project dir");
        p
    }

    #[test]
    fn looking_at_a_missing_registry_creates_nothing() {
        let d = dir("absent").join("not-yet");
        let reg = Registry::at(&d);
        assert!(reg.all_if_present().expect("none").is_empty());
        assert!(!d.exists(), "no folder or lock was created to look");
        let p = project(&dir("absent-p"), "proj");
        reg.lane_for_project(&p, &Config::default()).expect("lane");
        assert_eq!(reg.all_if_present().expect("one").len(), 1);
    }

    #[test]
    fn a_project_keeps_its_lane_and_a_second_gets_another() {
        let d = dir("stable");
        let reg = Registry::at(d.join("cfg"));
        let base = Config::default();
        let a = reg
            .lane_for_project(&project(&d, "aspire-si"), &base)
            .expect("a");
        assert_eq!(a.tag.as_deref(), Some("aspire-si"));
        assert_eq!(a.ssh_alias, "offrig-aspire-si");
        assert_eq!(a.tunnel_port, LANE_PORT_BASE);
        let b = reg
            .lane_for_project(&project(&d, "ai-jam-sessions"), &base)
            .expect("b");
        assert_eq!(b.tag.as_deref(), Some("ai-jam-sessions"));
        assert_eq!(b.tunnel_port, LANE_PORT_BASE + LANE_PORT_STEP);
        // Stable across restarts: a fresh Registry on the same dir finds the same lane.
        let again = Registry::at(d.join("cfg"))
            .lane_for_project(&project(&d, "aspire-si"), &base)
            .expect("again");
        assert_eq!(again, a);
        assert_eq!(reg.all().expect("all").len(), 2);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn an_allocated_lane_has_a_stable_side_car_port_of_its_own() {
        let d = dir("sidecar-port");
        let reg = Registry::at(d.join("cfg"));
        let base = Config::default();
        let a = reg
            .lane_for_project(&project(&d, "aspire-si"), &base)
            .expect("a");
        let b = reg
            .lane_for_project(&project(&d, "ai-jam-sessions"), &base)
            .expect("b");
        assert_eq!(a.sidecar_port(), Some(SIDECAR_PORT_BASE));
        assert_eq!(b.sidecar_port(), Some(SIDECAR_PORT_BASE + 1));
        let again = Registry::at(d.join("cfg"))
            .lane_for_project(&project(&d, "aspire-si"), &base)
            .expect("again");
        assert_eq!(again.sidecar_port(), a.sidecar_port(), "stable");
        assert_eq!(Lane::plain(&base).sidecar_port(), None);
        // The registry file stores no side-car port: it is derived, so registries
        // written before this existed work unchanged.
        let text = std::fs::read_to_string(reg.path()).expect("lanes.toml");
        assert!(!text.contains("sidecar_port"), "{text}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn same_folder_name_in_two_places_gets_a_disambiguator() {
        let d = dir("twins");
        let reg = Registry::at(d.join("cfg"));
        let base = Config::default();
        let one = project(&d.join("one"), "game");
        let two = project(&d.join("two"), "game");
        let a = reg.lane_for_project(&one, &base).expect("a");
        let b = reg.lane_for_project(&two, &base).expect("b");
        assert_eq!(a.tag.as_deref(), Some("game"));
        let tag = b.tag.expect("tag");
        assert!(tag.starts_with("game-") && tag != "game", "{tag}");
        assert_ne!(a.tunnel_port, b.tunnel_port);
        assert_ne!(a.ssh_alias, b.ssh_alias);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn tags_are_safe_whatever_the_folder_is_called() {
        let d = dir("odd");
        let reg = Registry::at(d.join("cfg"));
        let base = Config::default();
        for name in [
            "My Game (v2)!",
            "UPPER",
            "stage",
            "offrig",
            "x-stage",
            "..weird..",
            "a-very-long-project-folder-name-that-goes-on",
        ] {
            let lane = reg
                .lane_for_project(&project(&d, name), &base)
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            let tag = lane.tag.expect("tag");
            assert!(tag_shape_ok(&tag), "{name} -> {tag}");
            assert!(!RESERVED_TAGS.contains(&tag.as_str()), "{name} -> {tag}");
            assert!(!tag.ends_with("-stage"), "{name} -> {tag}");
            assert_eq!(lane.ssh_alias, format!("offrig-{tag}"));
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn ports_never_include_the_local_ollama_or_the_plain_lane() {
        let top = LANE_COUNT - 1;
        for i in 0..LANE_COUNT {
            let p = lane_port(i);
            assert_ne!(p, LOCAL_OLLAMA_PORT, "lane {i} must never be 11434");
            assert_ne!(p + 1, LOCAL_OLLAMA_PORT);
            for plain in [11435, 11436] {
                assert!(
                    p != plain && p + 1 != plain,
                    "lane {i} hits plain port {plain}"
                );
            }
        }
        assert!(lane_port(0) > LOCAL_OLLAMA_PORT + 2);
        assert!(lane_port(top) < u16::MAX - 1);
    }

    #[test]
    fn a_registry_that_names_the_local_ollama_port_is_refused() {
        let d = dir("guard");
        let reg = Registry::at(&d);
        for port in [LOCAL_OLLAMA_PORT, 11435, 11499, 11501, 9] {
            std::fs::write(
                reg.path(),
                format!(
                    "[[lane]]\nproject = \"/p\"\ntag = \"p\"\nssh_alias = \"offrig-p\"\ntunnel_port = {port}\n"
                ),
            )
            .expect("write");
            let err = reg
                .lane_for_project(&project(&d, "other"), &Config::default())
                .expect_err("must refuse");
            assert!(err.to_string().contains("lane range"), "{port}: {err}");
        }
        // And a Config will not enter a lane on that port even if built by hand.
        let bad = Lane {
            tag: Some("x".into()),
            ssh_alias: "offrig-x".into(),
            tunnel_port: LOCAL_OLLAMA_PORT,
        };
        assert!(Config::default().in_lane(&bad).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_malformed_registry_is_an_error_never_replaced() {
        let d = dir("malformed");
        let reg = Registry::at(&d);
        std::fs::write(reg.path(), "lane = 3").expect("write");
        assert!(
            reg.lane_for_project(&project(&d, "p"), &Config::default())
                .is_err()
        );
        assert_eq!(
            std::fs::read_to_string(reg.path()).expect("read"),
            "lane = 3"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn lanes_avoid_the_plain_configs_own_alias_and_port() {
        let d = dir("avoid");
        let reg = Registry::at(d.join("cfg"));
        let base = Config {
            ssh_alias: "offrig-box".into(),
            tunnel_port: LANE_PORT_BASE,
            ..Config::default()
        };
        let lane = reg
            .lane_for_project(&project(&d, "box"), &base)
            .expect("lane");
        assert_ne!(lane.ssh_alias, "offrig-box");
        assert_eq!(
            lane.tunnel_port,
            LANE_PORT_BASE + 2,
            "the plain port and its runner port are skipped"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_lane_tag_never_makes_another_lanes_or_the_plain_pod_name() {
        let base = Config {
            profiles: ["small", "b-c", "c"]
                .iter()
                .map(|n| {
                    let mut p = Config::default().profiles[0].clone();
                    p.name = (*n).into();
                    p
                })
                .collect(),
            active_profile: "small".into(),
            ..Config::default()
        };
        let avoid = Avoid::of(&base);
        let a = Record {
            project: "/a".into(),
            tag: "a".into(),
            ssh_alias: "offrig-a".into(),
            tunnel_port: lane_port(0),
        };
        // `a-b` + `c` = `offrig-a-b-c` = `a` + `b-c`: refused.
        assert!(!tag_free("a-b", &[a], &avoid));
        // A tag + profile equal to a plain pod name (`offrig-c` ... `b` + `c` = `offrig-b-c`).
        assert!(!tag_free("b", &[], &avoid));
        assert!(tag_free("zed", &[], &avoid));
    }

    #[test]
    fn concurrent_allocations_never_share_a_tag_alias_or_port() {
        let d = dir("race");
        let cfg_dir = d.join("cfg");
        let base = Config::default();
        let n = 24;
        let handles: Vec<_> = (0..n)
            .map(|i| {
                let reg = Registry::at(&cfg_dir);
                let base = base.clone();
                // Half the projects share a folder name, to force disambiguation.
                let p = project(
                    &d.join(format!("root{i}")),
                    if i % 2 == 0 { "same" } else { "x" },
                );
                std::thread::spawn(move || reg.lane_for_project(&p, &base).expect("lane"))
            })
            .collect();
        let lanes: Vec<Lane> = handles
            .into_iter()
            .map(|h| h.join().expect("thread"))
            .collect();
        let tags: HashSet<_> = lanes.iter().map(|l| l.tag.clone()).collect();
        let aliases: HashSet<_> = lanes.iter().map(|l| l.ssh_alias.clone()).collect();
        let ports: HashSet<_> = lanes.iter().map(|l| l.tunnel_port).collect();
        assert_eq!(tags.len(), n);
        assert_eq!(aliases.len(), n);
        assert_eq!(ports.len(), n);
        assert_eq!(Registry::at(&cfg_dir).all().expect("all").len(), n);
        assert!(!cfg_dir.join(LOCK_FILE).exists(), "the lock is released");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn concurrent_allocations_for_one_project_agree() {
        let d = dir("one");
        let cfg_dir = d.join("cfg");
        let p = project(&d, "solo");
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let (reg, p) = (Registry::at(&cfg_dir), p.clone());
                std::thread::spawn(move || {
                    reg.lane_for_project(&p, &Config::default()).expect("lane")
                })
            })
            .collect();
        let lanes: Vec<Lane> = handles.into_iter().map(|h| h.join().expect("t")).collect();
        assert!(lanes.windows(2).all(|w| w[0] == w[1]));
        assert_eq!(Registry::at(&cfg_dir).all().expect("all").len(), 1);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_dead_holders_stale_lock_is_broken_but_a_live_one_is_waited_for() {
        let d = dir("lock");
        let base = Config::default();
        std::fs::create_dir_all(&d).expect("dir");
        let lock = d.join(LOCK_FILE);
        std::fs::write(&lock, "").expect("lock");
        let live =
            Registry::at(&d).with_lock_timing(Duration::from_millis(150), Duration::from_secs(60));
        let err = live
            .lane_for_project(&project(&d, "p"), &base)
            .expect_err("a fresh lock is respected");
        assert!(matches!(err, Error::Timeout(_)), "{err}");
        assert!(lock.exists(), "a live holder's lock is left alone");
        // Age it past the stale limit.
        let old = SystemTime::now() - Duration::from_secs(120);
        std::fs::File::options()
            .write(true)
            .open(&lock)
            .and_then(|f| f.set_modified(old))
            .expect("age the lock");
        let reg =
            Registry::at(&d).with_lock_timing(Duration::from_secs(2), Duration::from_secs(60));
        assert!(reg.lane_for_project(&project(&d, "p"), &base).is_ok());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_runner_port_is_never_another_lanes_port() {
        let d = dir("runner");
        let reg = Registry::at(d.join("cfg"));
        let base = Config::default();
        let lanes: Vec<Lane> = (0..10)
            .map(|i| {
                reg.lane_for_project(&project(&d, &format!("p{i}")), &base)
                    .expect("lane")
            })
            .collect();
        let own: HashSet<u16> = lanes.iter().map(|l| l.tunnel_port).collect();
        for l in &lanes {
            assert!(!own.contains(&l.runner_port()));
            assert_ne!(l.runner_port(), LOCAL_OLLAMA_PORT);
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_lane_config_names_its_pods_and_owns_only_those() {
        let d = dir("names");
        let reg = Registry::at(d.join("cfg"));
        let base = Config::default();
        let lane = reg
            .lane_for_project(&project(&d, "aspire-si"), &base)
            .expect("lane");
        let cfg = base.in_lane(&lane).expect("in lane");
        let small = cfg.profile("small").expect("small");
        assert_eq!(cfg.pod_name(small), "offrig-aspire-si-small");
        assert_eq!(cfg.ssh_alias, "offrig-aspire-si");
        assert_eq!(cfg.tunnel_port, lane.tunnel_port);
        assert_eq!(
            cfg.zed_api_url(),
            format!("http://127.0.0.1:{}/v1", lane.tunnel_port)
        );
        assert!(cfg.owns_pod("offrig-aspire-si-small"));
        // Never the plain lane's pods, nor another lane's.
        assert!(!cfg.owns_pod("offrig-small"));
        assert!(!cfg.owns_pod("offrig-job"));
        assert!(!cfg.owns_pod("offrig-other-small"));
        // The plain lane is unchanged and owns none of the lane's.
        assert_eq!(base.pod_name(small), "offrig-small");
        assert!(base.owns_pod("offrig-small"));
        assert!(!base.owns_pod("offrig-aspire-si-small"));
        assert_eq!(base.ssh_alias, "offrig");
        assert_eq!(base.tunnel_port, 11435);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_plan_runs_in_the_lane_it_recorded_and_old_plans_stay_plain() {
        let d = dir("plans");
        let reg = Registry::at(d.join("cfg"));
        let base = Config::default();
        let ctx = LaneCtx::new(base, &project(&d, "mine"), reg.clone());
        assert!(
            ctx.own_if_allocated().expect("peek").is_none() && !reg.path().exists(),
            "building a context allocates nothing"
        );
        let other = reg
            .lane_for_project(&project(&d, "theirs"), &ctx.base)
            .expect("other");
        let store = Store::open_in_memory().expect("store");
        store.set_budget_cap(50.0).expect("cap");
        let new = |s: &Store| {
            s.create_plan(crate::store::NewPlan {
                profile: "small".into(),
                gpu_count: 1,
                gpu_types: vec![],
                max_hours: 1.0,
                max_price_hr: 0.25,
                note: None,
            })
            .expect("plan")
        };
        // A plan not yet launched takes this project's lane once adopted.
        let planned = new(&store);
        let c = ctx.cfg_for_plan(&store, &planned).expect("cfg");
        assert_eq!(c.lane_tag.as_deref(), Some("mine"));
        ctx.adopt(&store, &planned).expect("adopt");
        assert_eq!(
            store.plan_lane(planned.id).expect("lane").as_deref(),
            Some("mine")
        );
        // A committed plan with no recorded lane predates lanes: plain.
        let old = new(&store);
        let old = store.commit_plan(old.id).expect("commit");
        ctx.adopt(&store, &old)
            .expect("adopt is a no-op once committed");
        assert_eq!(store.plan_lane(old.id).expect("lane"), None);
        let c = ctx.cfg_for_plan(&store, &old).expect("cfg");
        assert_eq!(c.lane_tag, None);
        assert_eq!(c.ssh_alias, "offrig");
        assert_eq!(
            c.pod_name(c.profile("small").expect("small")),
            "offrig-small"
        );
        // A plan recorded on another lane resolves through the registry.
        let third = new(&store);
        store
            .set_plan_lane(third.id, other.tag.as_deref().expect("tag"))
            .expect("set");
        let c = ctx.cfg_for_plan(&store, &third).expect("cfg");
        assert_eq!(c.ssh_alias, other.ssh_alias);
        // A lane the registry does not know is refused, not guessed.
        store.set_plan_lane(third.id, "ghost").expect("set");
        assert!(ctx.cfg_for_plan(&store, &third).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn project_keys_ignore_separators_and_trailing_slashes() {
        let d = dir("keys");
        let p = project(&d, "k");
        let with_slash = PathBuf::from(format!("{}/", p.display()));
        assert_eq!(project_key(&p), project_key(&with_slash));
        let _ = std::fs::remove_dir_all(&d);
    }
}
