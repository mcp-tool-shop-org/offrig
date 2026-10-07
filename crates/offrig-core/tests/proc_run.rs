//! Child-process helpers, using this test binary itself as the child so nothing
//! outside the repository is run and it works the same on every platform.

use std::process::Command;
use std::time::{Duration, Instant};

use offrig_core::error::Error;
use offrig_core::proc;

/// Not a test: the child the other tests start. It does what the environment asks.
#[test]
fn helper_child() {
    match std::env::var("OFFRIG_HELPER").as_deref() {
        Ok("sleep") => std::thread::sleep(Duration::from_secs(60)),
        Ok("noisy") => {
            println!("to-stdout");
            eprintln!("to-stderr");
        }
        Ok("fail") => std::process::exit(3),
        _ => {}
    }
}

fn helper(mode: &str) -> Command {
    let mut cmd = Command::new(std::env::current_exe().expect("current exe"));
    cmd.args(["--exact", "helper_child", "--nocapture", "--test-threads=1"])
        .env("OFFRIG_HELPER", mode);
    cmd
}

#[test]
fn output_and_exit_status_are_captured() {
    let out = proc::run_with_timeout(&mut helper("noisy"), Duration::from_secs(60), "helper")
        .expect("runs");
    assert!(out.success());
    assert_eq!(out.status, Some(0));
    assert!(out.stdout.contains("to-stdout"), "{}", out.stdout);
    assert!(out.stderr.contains("to-stderr"), "{}", out.stderr);

    let failed = proc::run_with_timeout(&mut helper("fail"), Duration::from_secs(60), "helper")
        .expect("runs");
    assert!(!failed.success());
    assert_eq!(failed.status, Some(3));
}

#[test]
fn a_command_that_outlives_its_timeout_is_killed() {
    let started = Instant::now();
    let err = proc::run_with_timeout(&mut helper("sleep"), Duration::from_millis(300), "sleeper")
        .err()
        .expect("too slow");
    assert!(
        matches!(&err, Error::Timeout(m) if m.contains("sleeper")),
        "{err}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "it did not wait out the sleep"
    );
}

#[test]
fn a_command_that_cannot_start_is_an_io_error() {
    let mut cmd = Command::new("offrig-no-such-program-anywhere");
    let err = proc::run_with_timeout(&mut cmd, Duration::from_secs(5), "ghost")
        .err()
        .expect("no such program");
    assert!(
        matches!(&err, Error::Io { what, .. } if what.contains("ghost")),
        "{err}"
    );
}

#[test]
fn kill_stops_a_running_child_and_is_harmless_on_a_finished_one() {
    let mut cmd = helper("sleep");
    let mut child = proc::quiet(&mut cmd).spawn().expect("spawn");
    proc::kill(&mut child);
    assert!(
        child.try_wait().expect("status").is_some(),
        "it has been reaped"
    );
    proc::kill(&mut child);
}
