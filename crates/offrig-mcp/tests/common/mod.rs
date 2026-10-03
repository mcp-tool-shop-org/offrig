//! Shared by the end-to-end tests: a tiny HTTP server standing in for RunPod or the
//! pod's Ollama, and a fresh temp project directory.
#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::sync::{Arc, Mutex};

pub type Hits = Arc<Mutex<Vec<String>>>;

/// A small HTTP server standing in for RunPod. `handler(route, body, nth)`.
pub fn mock(
    handler: impl Fn(&str, &str, usize) -> (u16, String) + Send + 'static,
) -> (String, Hits) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let url = format!("http://{}", listener.local_addr().expect("addr"));
    let hits: Hits = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&hits);
    std::thread::spawn(move || {
        for stream in listener.incoming().map_while(Result::ok) {
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            if reader.read_line(&mut line).is_err() {
                continue;
            }
            let mut parts = line.split_whitespace();
            let method = parts.next().unwrap_or("").to_string();
            let path = parts
                .next()
                .unwrap_or("")
                .split('?')
                .next()
                .unwrap_or("")
                .to_string();
            let route = format!("{method} {path}");
            let mut len = 0usize;
            loop {
                let mut h = String::new();
                if reader.read_line(&mut h).is_err() || h == "\r\n" || h.is_empty() {
                    break;
                }
                if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; len];
            let _ = reader.read_exact(&mut body);
            let body = String::from_utf8_lossy(&body).to_string();
            let nth = {
                let mut l = log.lock().expect("hits");
                let n = l.iter().filter(|h| **h == route).count();
                l.push(route.clone());
                n
            };
            let (status, text) = handler(&route, &body, nth);
            let resp = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                text.len()
            );
            let _ = reader.get_mut().write_all(resp.as_bytes());
        }
    });
    (url, hits)
}

pub fn count(h: &Hits, route: &str) -> usize {
    h.lock()
        .expect("hits")
        .iter()
        .filter(|x| *x == route)
        .count()
}

pub fn temp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("offrig-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("temp dir");
    d
}
