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

/// A offrig tunnel left behind by a crashed run still holds the port. Kill it, but
/// only if the listener is an `ssh` carrying offrig's exact forward spec.
/// Returns whether one was reclaimed.
#[cfg(windows)]
pub fn reclaim_orphan(local_port: u16) -> Result<bool> {
    let spec = forward_spec(local_port);
    let script = format!(
        "$c = Get-NetTCPConnection -LocalAddress 127.0.0.1 -LocalPort {local_port} -State Listen -ErrorAction SilentlyContinue | Select-Object -First 1; \
         if (-not $c) {{ 'none'; exit }}; \
         $p = Get-CimInstance Win32_Process -Filter \"ProcessId=$($c.OwningProcess)\"; \
         if ($p.Name -eq 'ssh.exe' -and $p.CommandLine -like '*{spec}*') {{ Stop-Process -Id $p.ProcessId -Force; 'killed' }} else {{ \"other:$($p.Name)\" }}"
    );
    let mut cmd = Command::new("powershell");
    cmd.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
    let out = proc::run_with_timeout(
        &mut cmd,
        Duration::from_secs(30),
        "checking the tunnel port",
    )?;
    match out.stdout.trim() {
        "killed" => {
            std::thread::sleep(Duration::from_millis(500));
            Ok(true)
        }
        _ => Ok(false),
    }
}

#[cfg(not(windows))]
pub fn reclaim_orphan(_local_port: u16) -> Result<bool> {
    Ok(false)
}

impl Tunnel {
    /// Start the tunnel and wait until its local port accepts connections.
    pub fn start(alias: &str, local_port: u16, timeout: Duration) -> Result<Self> {
        if local_port == LOCAL_OLLAMA_PORT {
            return Err(Error::Guard(format!(
                "refusing to tunnel on {local_port}: that is the local Ollama port"
            )));
        }
        if port_open(local_port) {
            reclaim_orphan(local_port)?;
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

    #[test]
    fn refuses_a_port_someone_else_holds() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = l.local_addr().expect("addr").port();
        let err = Tunnel::start("offrig", port, Duration::from_secs(1))
            .err()
            .expect("must refuse");
        assert!(err.to_string().contains("already in use"));
    }
}
