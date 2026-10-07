//! The SSH tunnel: local `127.0.0.1:<tunnel_port>` to the pod's loopback Ollama.
//! The child `ssh` is killed when the `Tunnel` is dropped.

use std::io::{BufRead, BufReader};
use std::net::{SocketAddr, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::config::{LOCAL_OLLAMA_PORT, REMOTE_OLLAMA_PORT};
use crate::error::{Error, Result};
use crate::proc;

pub struct Tunnel {
    child: Child,
    pub alias: String,
    pub local_port: u16,
    stderr: Arc<Mutex<Vec<String>>>,
}

pub fn port_open(port: u16) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok()
}

pub fn ssh_args(alias: &str, local_port: u16) -> Vec<String> {
    vec![
        "-N".into(),
        "-T".into(),
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        "ExitOnForwardFailure=yes".into(),
        "-o".into(),
        "ServerAliveInterval=15".into(),
        "-o".into(),
        "ServerAliveCountMax=3".into(),
        "-o".into(),
        "ConnectTimeout=15".into(),
        "-L".into(),
        forward_spec(local_port),
        alias.into(),
    ]
}

/// The `-L` spec offrig passes; an `ssh` whose command line carries it is offrig's.
pub fn forward_spec(local_port: u16) -> String {
    format!("127.0.0.1:{local_port}:127.0.0.1:{REMOTE_OLLAMA_PORT}")
}

/// A process listening on a local port: what the orphan check looks at.
#[derive(Debug, Clone, PartialEq)]
pub struct Listener {
    pub pid: u32,
    pub name: String,
    pub command_line: String,
}

/// The machine's processes, as far as the orphan check needs them. The real one asks
/// the OS; tests pass a fake, so no test ever probes or kills a real process.
pub trait Processes {
    /// Who listens on 127.0.0.1:`port`, if anyone.
    fn listener(&self, port: u16) -> Result<Option<Listener>>;
    fn kill(&self, pid: u32) -> Result<()>;
}

/// The real processes.
pub struct System;

#[cfg(windows)]
impl Processes for System {
    fn listener(&self, port: u16) -> Result<Option<Listener>> {
        let script = format!(
            "$c = Get-NetTCPConnection -LocalAddress 127.0.0.1 -LocalPort {port} -State Listen -ErrorAction SilentlyContinue | Select-Object -First 1; \
             if (-not $c) {{ 'none'; exit }}; \
             $p = Get-CimInstance Win32_Process -Filter \"ProcessId=$($c.OwningProcess)\"; \
             \"$($p.ProcessId)`t$($p.Name)`t$($p.CommandLine)\""
        );
        let mut cmd = Command::new("powershell");
        cmd.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
        let out = proc::run_with_timeout(
            &mut cmd,
            Duration::from_secs(30),
            "checking the tunnel port",
        )?;
        let mut f = out.stdout.trim().splitn(3, '\t');
        let (Some(pid), Some(name), cmdline) = (
            f.next().and_then(|v| v.trim().parse::<u32>().ok()),
            f.next(),
            f.next().unwrap_or(""),
        ) else {
            return Ok(None);
        };
        Ok(Some(Listener {
            pid,
            name: name.trim().to_string(),
            command_line: cmdline.trim().to_string(),
        }))
    }

    fn kill(&self, pid: u32) -> Result<()> {
        let mut cmd = Command::new("powershell");
        cmd.args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &format!("Stop-Process -Id {pid} -Force"),
        ]);
        proc::run_with_timeout(
            &mut cmd,
            Duration::from_secs(30),
            "stopping an orphan tunnel",
        )?;
        Ok(())
    }
}

#[cfg(not(windows))]
impl Processes for System {
    fn listener(&self, _port: u16) -> Result<Option<Listener>> {
        Ok(None)
    }

    fn kill(&self, _pid: u32) -> Result<()> {
        Ok(())
    }
}

/// Whether `l` is the tunnel of exactly this lane: an `ssh` whose command line carries
/// this lane's forward (`-L 127.0.0.1:<port>:...`) and ends with this lane's alias.
/// Another lane's tunnel differs in one or both, and so is never this lane's to kill.
pub fn is_lane_tunnel(l: &Listener, alias: &str, local_port: u16) -> bool {
    let unquote = |t: &str| t.trim_matches(['"', '\'']).to_string();
    let name = l.name.to_ascii_lowercase();
    if name != "ssh" && name != "ssh.exe" {
        return false;
    }
    let tokens: Vec<String> = l.command_line.split_whitespace().map(unquote).collect();
    let spec = forward_spec(local_port);
    tokens.contains(&spec) && tokens.last().is_some_and(|t| t == alias)
}

/// A tunnel left behind by a crashed run still holds the port. Kill it, but only if
/// the listener is an `ssh` carrying this lane's exact forward spec and alias.
/// Returns whether one was reclaimed.
pub fn reclaim_orphan_with(procs: &dyn Processes, alias: &str, local_port: u16) -> Result<bool> {
    match procs.listener(local_port)? {
        Some(l) if is_lane_tunnel(&l, alias, local_port) => {
            procs.kill(l.pid)?;
            std::thread::sleep(Duration::from_millis(500));
            Ok(true)
        }
        _ => Ok(false),
    }
}

pub fn reclaim_orphan(alias: &str, local_port: u16) -> Result<bool> {
    reclaim_orphan_with(&System, alias, local_port)
}

impl Tunnel {
    /// Start the tunnel and wait until its local port accepts connections.
    pub fn start(alias: &str, local_port: u16, timeout: Duration) -> Result<Self> {
        Self::start_with(&System, alias, local_port, timeout)
    }

    /// [`Tunnel::start`] with the process view injected.
    pub fn start_with(
        procs: &dyn Processes,
        alias: &str,
        local_port: u16,
        timeout: Duration,
    ) -> Result<Self> {
        if local_port == LOCAL_OLLAMA_PORT {
            return Err(Error::Guard(format!(
                "refusing to tunnel on {local_port}: that is the local Ollama port"
            )));
        }
        if port_open(local_port) {
            reclaim_orphan_with(procs, alias, local_port)?;
        }
        if port_open(local_port) {
            return Err(Error::Guard(format!(
                "127.0.0.1:{local_port} is already in use by another process; offrig will not share it"
            )));
        }
        let mut cmd = Command::new("ssh");
        cmd.args(ssh_args(alias, local_port))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = proc::quiet(&mut cmd)
            .spawn()
            .map_err(|e| Error::io("starting the ssh tunnel", e))?;
        let stderr = Arc::new(Mutex::new(Vec::new()));
        if let Some(pipe) = child.stderr.take() {
            let sink = Arc::clone(&stderr);
            std::thread::spawn(move || {
                for line in BufReader::new(pipe)
                    .lines()
                    .map_while(std::result::Result::ok)
                {
                    // A poisoned log only loses diagnostics; keep collecting.
                    let mut v = sink
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    v.push(line);
                    if v.len() > 200 {
                        v.remove(0);
                    }
                }
            });
        }
        let mut t = Tunnel {
            child,
            alias: alias.to_string(),
            local_port,
            stderr,
        };
        let deadline = Instant::now() + timeout;
        loop {
            if let Ok(Some(status)) = t.child.try_wait() {
                return Err(Error::Ssh(format!(
                    "tunnel exited ({status}): {}",
                    t.last_error()
                )));
            }
            if port_open(local_port) {
                return Ok(t);
            }
            if Instant::now() >= deadline {
                let msg = t.last_error();
                t.stop();
                return Err(Error::Timeout(format!("tunnel to {alias} ({msg})")));
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    pub fn last_error(&self) -> String {
        let v = self
            .stderr
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        v.iter()
            .rev()
            .find(|l| !l.trim().is_empty())
            .cloned()
            .unwrap_or_default()
    }

    pub fn stop(&mut self) {
        proc::kill(&mut self.child);
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forwards_loopback_to_loopback_and_fails_hard() {
        let a = ssh_args("offrig", 11435);
        assert!(a.contains(&"127.0.0.1:11435:127.0.0.1:11434".to_string()));
        assert!(a.contains(&"ExitOnForwardFailure=yes".to_string()));
        assert_eq!(a.last().map(String::as_str), Some("offrig"));
    }

    #[test]
    fn refuses_local_ollama_port() {
        let err = Tunnel::start("offrig", 11434, Duration::from_secs(1))
            .err()
            .expect("must refuse");
        assert!(matches!(err, Error::Guard(_)));
    }

    /// Processes that exist only in the test: one listener, and a record of kills.
    struct Fake {
        listener: Option<Listener>,
        killed: std::sync::Mutex<Vec<u32>>,
    }

    impl Fake {
        fn holding(command_line: &str) -> Self {
            Fake::with(Some(Listener {
                pid: 4242,
                name: "ssh.exe".into(),
                command_line: command_line.into(),
            }))
        }

        fn with(listener: Option<Listener>) -> Self {
            Fake {
                listener,
                killed: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn kills(&self) -> Vec<u32> {
            self.killed.lock().expect("kills").clone()
        }
    }

    impl Processes for Fake {
        fn listener(&self, _port: u16) -> Result<Option<Listener>> {
            Ok(self.listener.clone())
        }

        fn kill(&self, pid: u32) -> Result<()> {
            self.killed.lock().expect("kills").push(pid);
            Ok(())
        }
    }

    fn ssh_line(alias: &str, port: u16) -> String {
        format!(
            "\"C:\\Windows\\System32\\OpenSSH\\ssh.exe\" {}",
            ssh_args(alias, port).join(" ")
        )
    }

    #[test]
    fn refuses_a_port_someone_else_holds() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = l.local_addr().expect("addr").port();
        // A fake process view: the real one is never asked about a real process.
        let other = Fake::with(Some(Listener {
            pid: 7,
            name: "something.exe".into(),
            command_line: "something.exe --serve".into(),
        }));
        let err = Tunnel::start_with(&other, "offrig", port, Duration::from_secs(1))
            .err()
            .expect("must refuse");
        assert!(err.to_string().contains("already in use"));
        assert!(
            other.kills().is_empty(),
            "someone else's process is never killed"
        );
    }

    #[test]
    fn a_tunnel_of_another_lane_is_never_reclaimed() {
        // Same port, different alias: another project's tunnel on a port this lane
        // was (wrongly) pointed at.
        let theirs = Fake::holding(&ssh_line("offrig-aspire-si", 11500));
        assert!(!reclaim_orphan_with(&theirs, "offrig-ai-jam-sessions", 11500).expect("reclaim"));
        // Same alias, different port.
        assert!(!reclaim_orphan_with(&theirs, "offrig-aspire-si", 11502).expect("reclaim"));
        // The plain lane's tunnel is not a project lane's, and the reverse.
        let plain = Fake::holding(&ssh_line("offrig", 11435));
        assert!(!reclaim_orphan_with(&plain, "offrig-aspire-si", 11435).expect("reclaim"));
        assert!(!reclaim_orphan_with(&theirs, "offrig", 11500).expect("reclaim"));
        // An alias that merely ends the same way is another alias.
        let longer = Fake::holding(&ssh_line("x-offrig", 11435));
        assert!(!reclaim_orphan_with(&longer, "offrig", 11435).expect("reclaim"));
        assert!(theirs.kills().is_empty() && plain.kills().is_empty() && longer.kills().is_empty());
    }

    #[test]
    fn this_lanes_own_orphan_is_reclaimed() {
        let mine = Fake::holding(&ssh_line("offrig-aspire-si", 11500));
        assert!(reclaim_orphan_with(&mine, "offrig-aspire-si", 11500).expect("reclaim"));
        assert_eq!(mine.kills(), [4242]);
        let plain = Fake::holding(&ssh_line("offrig", 11435));
        assert!(reclaim_orphan_with(&plain, "offrig", 11435).expect("reclaim"));
        assert_eq!(plain.kills(), [4242]);
    }

    #[test]
    fn only_ssh_processes_are_ever_reclaimed() {
        let imposter = Fake::with(Some(Listener {
            pid: 9,
            name: "notssh.exe".into(),
            command_line: ssh_line("offrig", 11435),
        }));
        assert!(!reclaim_orphan_with(&imposter, "offrig", 11435).expect("reclaim"));
        assert!(imposter.kills().is_empty());
        assert!(!reclaim_orphan_with(&Fake::with(None), "offrig", 11435).expect("reclaim"));
    }
}
