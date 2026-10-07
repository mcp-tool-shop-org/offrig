//! Issue #11: each project's shell-driven side-car has its own default port, derived from
//! its lane, and a taken port is reported with the project that holds it.

mod common;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Command, Output};

use common::temp;

fn run(
    project: &std::path::Path,
    cfg: &std::path::Path,
    args: &[&str],
    port: Option<&str>,
) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_offrig-mcp"));
    c.arg("--sidecar-port")
        .args(args)
        .arg("--project")
        .arg(project)
        .env("OFFRIG_CONFIG_DIR", cfg)
        .env_remove("OFFRIG_SIDECAR_PORT");
    if let Some(p) = port {
        c.env("OFFRIG_SIDECAR_PORT", p);
    }
    c.output().expect("run offrig-mcp")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).trim().to_string()
}

#[test]
fn two_projects_get_two_stable_default_ports_in_their_own_range() {
    let root = temp("sidecar-ports");
    let cfg = root.join("cfg");
    let (a, b) = (root.join("aspire-si"), root.join("ai-jam-sessions"));
    std::fs::create_dir_all(&a).expect("a");
    std::fs::create_dir_all(&b).expect("b");

    let pa = stdout(&run(&a, &cfg, &[], None));
    let pb = stdout(&run(&b, &cfg, &[], None));
    assert_eq!(pa, "11700", "first project, first lane");
    assert_eq!(pb, "11701", "a second project never shares it");
    assert_ne!(pa, "11439", "no longer the machine-wide default");
    // Stable: asking again, from a fresh process, gives the same answer.
    assert_eq!(stdout(&run(&a, &cfg, &[], None)), pa);
    assert_eq!(stdout(&run(&b, &cfg, &[], None)), pb);
    // It is a lane's port, never a tunnel port: the registry holds the tunnel ports
    // 11500 and 11502 for the same two projects.
    let registry = std::fs::read_to_string(cfg.join("lanes.toml")).expect("lanes.toml");
    assert!(registry.contains("tunnel_port = 11500"), "{registry}");
    assert!(registry.contains("tunnel_port = 11502"), "{registry}");
    assert!(
        !registry.contains("1170"),
        "derived, not stored: {registry}"
    );

    // OFFRIG_SIDECAR_PORT still overrides; a reserved or malformed value is refused.
    assert_eq!(stdout(&run(&a, &cfg, &[], Some("12345"))), "12345");
    for bad in ["11435", "11500", "nope"] {
        let o = run(&a, &cfg, &[], Some(bad));
        assert!(!o.status.success(), "{bad}");
        assert!(stderr(&o).contains("OFFRIG_SIDECAR_PORT"), "{}", stderr(&o));
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn check_names_the_port_and_the_project_that_holds_it() {
    let root = temp("sidecar-check");
    let cfg = root.join("cfg");
    let proj = root.join("proj");
    std::fs::create_dir_all(&proj).expect("proj");

    // Free: --check prints the port and succeeds.
    let free = {
        let l = TcpListener::bind("127.0.0.1:0").expect("bind");
        l.local_addr().expect("addr").port().to_string()
    };
    let o = run(&proj, &cfg, &["--check"], Some(&free));
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(stdout(&o), free);

    // Held by an offrig side-car for another project: exit 1, port and project named.
    let l = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = l.local_addr().expect("addr").port().to_string();
    std::thread::spawn(move || {
        for mut s in l.incoming().map_while(Result::ok) {
            let mut seen = [0u8; 4096];
            let _ = s.read(&mut seen);
            let body =
                r#"{"is_error":true,"body":"refused","sidecar_project":"E:/work/other-project"}"#;
            let _ = s.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            );
        }
    });
    let o = run(&proj, &cfg, &["--check"], Some(&port));
    assert_eq!(o.status.code(), Some(1));
    assert!(stdout(&o).is_empty(), "nothing on stdout when taken");
    let err = stderr(&o);
    assert!(
        err.contains(&format!("side-car port {port} is taken")),
        "{err}"
    );
    assert!(err.contains("E:/work/other-project"), "{err}");

    // Held by something that is not an offrig side-car: still refused, no project.
    let l = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = l.local_addr().expect("addr").port().to_string();
    std::thread::spawn(move || {
        for s in l.incoming().map_while(Result::ok) {
            drop(s);
        }
    });
    let o = run(&proj, &cfg, &["--check"], Some(&port));
    assert_eq!(o.status.code(), Some(1));
    let err = stderr(&o);
    assert!(
        err.contains(&port) && err.contains("another program"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&root);
}
