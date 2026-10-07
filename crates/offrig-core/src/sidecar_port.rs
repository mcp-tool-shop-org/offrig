//! The port a project's shell-driven side-car listens on (issue #11).
//!
//! `offrig-mcp` itself speaks MCP over stdio. A shell driver can hold one open behind a
//! loopback HTTP port so a whole session shares one process (the launch job and the
//! tunnel live inside it). That port used to be one number for the whole machine, so a
//! second project's driver, or any other program, could take it and the first side-car
//! went dark without a word. Now the default is per project, from the project's lane
//! ([`Lane::sidecar_port`]), and a taken port is reported with whoever holds it.
//!
//! `OFFRIG_SIDECAR_PORT` still overrides the default.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

use crate::config::LOCAL_OLLAMA_PORT;
use crate::error::{Error, Result};
use crate::lanes::{LANE_COUNT, LANE_PORT_BASE, LANE_PORT_STEP, Lane};

/// The environment variable that overrides the per-project default.
pub const PORT_ENV: &str = "OFFRIG_SIDECAR_PORT";

/// The ports an override may not take: the local Ollama, the plain lane's tunnel and
/// runner, and every lane's tunnel and runner ports (a side-car there would break a
/// tunnel or be killed as an orphan one).
fn reserved(port: u16) -> Option<&'static str> {
    let lanes_top = LANE_PORT_BASE + LANE_COUNT * LANE_PORT_STEP;
    match port {
        0..=1023 => Some("a privileged port"),
        LOCAL_OLLAMA_PORT => Some("the local Ollama's port"),
        11435 | 11436 => Some("the offrig tunnel's port"),
        p if (LANE_PORT_BASE..lanes_top).contains(&p) => Some("a project lane's tunnel port range"),
        _ => None,
    }
}

/// The side-car port for a project: `override_` (the value of `OFFRIG_SIDECAR_PORT`) when
/// given, else the lane's default. An override that is not a port, or is one offrig
/// already uses, is refused rather than silently ignored.
pub fn resolve(lane: &Lane, override_: Option<&str>) -> Result<u16> {
    if let Some(v) = override_.map(str::trim).filter(|v| !v.is_empty()) {
        let port: u16 = v.parse().map_err(|_| {
            Error::Config(format!(
                "{PORT_ENV}={v:?} is not a port number (1024 to 65535)"
            ))
        })?;
        if let Some(why) = reserved(port) {
            return Err(Error::Config(format!(
                "{PORT_ENV}={port} is {why}; pick another port"
            )));
        }
        return Ok(port);
    }
    lane.sidecar_port().ok_or_else(|| {
        Error::Config("the plain lane has no side-car port; a side-car belongs to a project".into())
    })
}

/// What answers on a loopback port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Holder {
    /// Nothing is listening.
    Free,
    /// A program is listening and is not an offrig side-car (or did not say).
    Other,
    /// An offrig side-car is listening, serving this project.
    Sidecar { project: String },
}

/// Ask what holds `port` on loopback, without side effects: a refused connection is
/// [`Holder::Free`]; otherwise a request the side-car driver answers without touching
/// its tools (as a project it cannot be serving) tells an offrig side-car from any other
/// program. The driver's reply names the project it serves.
pub fn probe(port: u16) -> Holder {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let Ok(mut s) = TcpStream::connect_timeout(&addr, Duration::from_millis(500)) else {
        return Holder::Free;
    };
    let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = s.set_write_timeout(Some(Duration::from_secs(2)));
    // An `expect_project` that no project matches: the driver refuses before it calls
    // any tool, so this changes nothing in the side-car.
    let body = r#"{"name":"__probe__","args":{},"expect_project":"\u0000offrig-probe"}"#;
    let req = format!(
        "POST / HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    if s.write_all(req.as_bytes()).is_err() {
        return Holder::Other;
    }
    let mut buf = Vec::new();
    let _ = s.take(64 * 1024).read_to_end(&mut buf);
    let text = String::from_utf8_lossy(&buf);
    let json = text.split_once("\r\n\r\n").map_or("", |(_, b)| b);
    match serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| v["sidecar_project"].as_str().map(str::to_string))
    {
        Some(project) => Holder::Sidecar { project },
        None => Holder::Other,
    }
}

/// The error a side-car start prints when its port is taken: names the port, says who
/// holds it, and says how to get a free one. `None` when the port is free.
pub fn taken_message(port: u16, holder: &Holder) -> Option<String> {
    let who = match holder {
        Holder::Free => return None,
        Holder::Other => "another program is listening there".to_string(),
        Holder::Sidecar { project } => {
            format!("an offrig side-car is already serving the project {project} there")
        }
    };
    Some(format!(
        "side-car port {port} is taken: {who}. Stop that first, or set {PORT_ENV} to a free port"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn lane(slot: u16) -> Lane {
        Lane {
            tag: Some(format!("p{slot}")),
            ssh_alias: format!("offrig-p{slot}"),
            tunnel_port: LANE_PORT_BASE + slot * LANE_PORT_STEP,
        }
    }

    /// A listener that answers every request with `reply` as the HTTP body.
    fn fake(reply: &'static str) -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = l.local_addr().expect("addr").port();
        std::thread::spawn(move || {
            for mut s in l.incoming().map_while(std::result::Result::ok) {
                let mut seen = [0u8; 4096];
                let n = s.read(&mut seen).unwrap_or(0);
                assert!(
                    String::from_utf8_lossy(&seen[..n]).contains("expect_project"),
                    "the probe asks as a project nobody serves"
                );
                let r = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                );
                let _ = s.write_all(r.as_bytes());
            }
        });
        port
    }

    #[test]
    fn each_lane_gets_its_own_port_in_its_own_range() {
        let ports: Vec<u16> = (0..LANE_COUNT)
            .map(|i| lane(i).sidecar_port().expect("lane port"))
            .collect();
        assert_eq!(ports[0], crate::lanes::SIDECAR_PORT_BASE);
        assert_eq!(ports[1], crate::lanes::SIDECAR_PORT_BASE + 1);
        let unique: std::collections::HashSet<_> = ports.iter().collect();
        assert_eq!(
            unique.len(),
            ports.len(),
            "two projects never share a default"
        );
        for i in 0..LANE_COUNT {
            let l = lane(i);
            let p = l.sidecar_port().expect("port");
            for other in 0..LANE_COUNT {
                let o = lane(other);
                assert_ne!(p, o.tunnel_port, "lane {i} vs tunnel of {other}");
                assert_ne!(p, o.runner_port(), "lane {i} vs runner of {other}");
            }
            for held in [LOCAL_OLLAMA_PORT, 11435, 11436, 11439] {
                assert_ne!(p, held, "lane {i} vs {held}");
            }
        }
    }

    #[test]
    fn the_plain_lane_and_a_lane_outside_the_range_have_no_side_car_port() {
        let plain = Lane {
            tag: None,
            ssh_alias: "offrig".into(),
            tunnel_port: 11435,
        };
        assert_eq!(plain.sidecar_port(), None);
        let odd = Lane {
            tag: Some("x".into()),
            ssh_alias: "offrig-x".into(),
            tunnel_port: 9000,
        };
        assert_eq!(odd.sidecar_port(), None);
    }

    #[test]
    fn the_env_override_wins_and_a_bad_one_is_refused() {
        let l = lane(3);
        assert_eq!(resolve(&l, None).expect("default"), 11703);
        assert_eq!(resolve(&l, Some("")).expect("blank is unset"), 11703);
        assert_eq!(resolve(&l, Some(" 12345 ")).expect("override"), 12345);
        assert_eq!(
            resolve(&l, Some("11439")).expect("the old shared default is still allowed"),
            11439
        );
        for bad in [
            "abc", "70000", "-1", "11434", "11435", "11436", "80", "11500", "11627",
        ] {
            assert!(resolve(&l, Some(bad)).is_err(), "{bad}");
        }
        let plain = Lane {
            tag: None,
            ssh_alias: "offrig".into(),
            tunnel_port: 11435,
        };
        assert!(resolve(&plain, None).is_err());
        assert_eq!(resolve(&plain, Some("12000")).expect("explicit"), 12000);
    }

    #[test]
    fn a_free_port_is_free_and_says_nothing() {
        let port = {
            let l = TcpListener::bind("127.0.0.1:0").expect("bind");
            l.local_addr().expect("addr").port()
        };
        assert_eq!(probe(port), Holder::Free);
        assert_eq!(taken_message(port, &Holder::Free), None);
    }

    #[test]
    fn an_offrig_side_car_on_the_port_is_named_with_its_project() {
        let port = fake(
            r#"{"is_error":true,"body":"refused: this side-car serves proj-a","sidecar_project":"E:/work/proj-a"}"#,
        );
        let h = probe(port);
        assert_eq!(
            h,
            Holder::Sidecar {
                project: "E:/work/proj-a".into()
            }
        );
        let msg = taken_message(port, &h).expect("taken");
        assert!(msg.contains(&port.to_string()), "{msg}");
        assert!(msg.contains("E:/work/proj-a"), "{msg}");
        assert!(msg.contains(PORT_ENV), "{msg}");
    }

    #[test]
    fn some_other_program_on_the_port_is_reported_as_taken_without_a_project() {
        let port = fake("<html>not offrig</html>");
        let h = probe(port);
        assert_eq!(h, Holder::Other);
        let msg = taken_message(port, &h).expect("taken");
        assert!(
            msg.contains(&port.to_string()) && msg.contains("another program"),
            "{msg}"
        );
        // A listener that accepts and says nothing is also not a side-car.
        let l = TcpListener::bind("127.0.0.1:0").expect("bind");
        let silent = l.local_addr().expect("addr").port();
        std::thread::spawn(move || {
            for s in l.incoming().map_while(std::result::Result::ok) {
                drop(s);
            }
        });
        assert_eq!(probe(silent), Holder::Other);
    }
}
