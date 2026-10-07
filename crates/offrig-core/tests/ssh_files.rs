//! The ssh config file edits on temp files, and the banner probe on loopback sockets.

mod support;

use std::io::Write;
use std::time::Duration;

use offrig_core::error::Error;
use offrig_core::remote;
use offrig_core::sshconfig::{self, HostEntry};
use support::{dead_url, temp};

fn entry(alias: &str, port: u16, label: &str) -> HostEntry {
    HostEntry {
        alias: alias.into(),
        host: "203.0.113.7".into(),
        port,
        identity_file: "~/.ssh/id_ed25519".into(),
        label: label.into(),
    }
}

#[test]
fn applying_writes_once_and_reports_whether_the_file_changed() {
    let d = temp("ssh-apply");
    let path = d.join(".ssh").join("config");
    let e = entry("offrig", 2222, "offrig-medium (p1)");
    assert!(
        sshconfig::apply_at(&path, &e).expect("first"),
        "a new file changes"
    );
    let text = std::fs::read_to_string(&path).expect("read");
    assert!(
        text.contains("Host offrig") && text.contains("Port 2222"),
        "{text}"
    );
    assert!(
        !sshconfig::apply_at(&path, &e).expect("same"),
        "the same endpoint leaves the file alone"
    );
    assert!(
        sshconfig::apply_at(&path, &entry("offrig", 2223, "offrig-medium (p2)")).expect("moved"),
        "a new endpoint rewrites the block"
    );
    let text = std::fs::read_to_string(&path).expect("read");
    assert!(text.contains("Port 2223") && !text.contains("Port 2222"));
    assert_eq!(
        text.matches("Host offrig").count(),
        1,
        "one block per alias"
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_byte_order_mark_is_dropped_and_other_hosts_survive() {
    let d = temp("ssh-bom");
    let path = d.join("config");
    std::fs::write(&path, "\u{feff}Host github.com\n  User git\n").expect("write");
    sshconfig::apply_at(&path, &entry("offrig", 2222, "x (p1)")).expect("apply");
    let bytes = std::fs::read(&path).expect("read");
    assert!(!bytes.starts_with(&[0xEF, 0xBB, 0xBF]), "plain UTF-8 only");
    let text = String::from_utf8(bytes).expect("utf8");
    assert!(text.contains("Host github.com") && text.contains("User git"));
    sshconfig::remove_at(&path, "offrig").expect("remove");
    let text = std::fs::read_to_string(&path).expect("read");
    assert!(
        text.contains("Host github.com") && !text.contains("Host offrig"),
        "{text}"
    );
    // Removing an alias that is not there leaves the file byte for byte.
    let before = std::fs::read(&path).expect("read");
    sshconfig::remove_at(&path, "offrig").expect("nothing to remove");
    assert_eq!(std::fs::read(&path).expect("read"), before);
    // And a file that does not exist is simply empty.
    sshconfig::remove_at(&d.join("absent"), "offrig").expect("absent file");
    assert!(
        !d.join("absent").exists(),
        "nothing is created for a removal"
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn removing_a_pods_block_needs_the_pod_to_match() {
    let d = temp("ssh-pod");
    let path = d.join("config");
    sshconfig::apply_at(&path, &entry("offrig", 2222, "offrig-medium (pod-a)")).expect("apply");
    assert!(
        !sshconfig::remove_for_pod_at(&path, "offrig", "pod-b").expect("other pod"),
        "another pod's block is not ours to remove"
    );
    assert!(
        std::fs::read_to_string(&path)
            .expect("read")
            .contains("Host offrig")
    );
    assert!(sshconfig::remove_for_pod_at(&path, "offrig", "pod-a").expect("own pod"));
    assert!(
        !std::fs::read_to_string(&path)
            .expect("read")
            .contains("Host offrig")
    );
    assert!(
        !sshconfig::remove_for_pod_at(&d.join("absent"), "offrig", "pod-a").expect("absent"),
        "no file, nothing removed"
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn an_unreadable_config_is_an_io_error_and_nothing_is_overwritten() {
    let d = temp("ssh-unreadable");
    // A directory where the config file should be.
    let err = sshconfig::apply_at(&d, &entry("offrig", 1, "x (p)")).expect_err("a directory");
    assert!(matches!(err, Error::Io { .. }), "{err}");
    assert!(d.is_dir());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn ssh_answers_only_to_a_real_ssh_banner() {
    let wait = Duration::from_secs(2);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let serve = std::thread::spawn(move || {
        // First visitor gets an SSH banner, the second an HTTP one.
        for banner in [&b"SSH-2.0-OpenSSH_9.6\r\n"[..], &b"HTTP/1.1 400\r\n"[..]] {
            if let Ok((mut s, _)) = listener.accept() {
                let _ = s.write_all(banner);
            }
        }
    });
    assert!(remote::ssh_answers("127.0.0.1", port, wait), "sshd banner");
    assert!(!remote::ssh_answers("127.0.0.1", port, wait), "not sshd");
    serve.join().expect("server thread");
    // Nothing listening, and a name that does not resolve: simply "not yet".
    let dead = dead_url();
    let dead_port: u16 = dead
        .rsplit(':')
        .next()
        .expect("port")
        .parse()
        .expect("number");
    assert!(!remote::ssh_answers("127.0.0.1", dead_port, wait));
    assert!(!remote::ssh_answers("host.invalid", 22, wait));
}
