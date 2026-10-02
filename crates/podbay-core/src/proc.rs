//! Child-process helpers: no console window on Windows, and a wall-clock timeout,
//! which `std::process` does not provide.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::error::{Error, Result};

/// Keep a GUI app from flashing a console window for every ssh call.
pub fn quiet(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

pub struct Output {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    pub fn success(&self) -> bool {
        self.status == Some(0)
    }
}

/// Run `cmd` to completion or kill it at `timeout`.
pub fn run_with_timeout(cmd: &mut Command, timeout: Duration, what: &str) -> Result<Output> {
    let mut child = quiet(cmd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::io(format!("starting {what}"), e))?;
    let out_reader = drain(child.stdout.take());
    let err_reader = drain(child.stderr.take());
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s.code(),
            Ok(None) if Instant::now() >= deadline => {
                kill(&mut child);
                return Err(Error::Timeout(format!("{what} ({}s)", timeout.as_secs())));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(Error::io(format!("waiting for {what}"), e)),
        }
    };
    Ok(Output {
        status,
        stdout: out_reader.join().unwrap_or_default(),
        stderr: err_reader.join().unwrap_or_default(),
    })
}

fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(mut p) = pipe {
            let mut buf = Vec::new();
            let _ = p.read_to_end(&mut buf);
            s = String::from_utf8_lossy(&buf).into_owned();
        }
        s
    })
}

pub fn kill(child: &mut Child) {
    // Already-exited children make kill fail; the wait reaps either way.
    let _ = child.kill();
    let _ = child.wait();
}
