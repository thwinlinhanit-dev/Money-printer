//! `opsd` — local health-ingestion and read-only status daemon (OPS-11).
//! It deliberately exposes no order, mode, or kill-latch-clearing endpoint.

use mp_ops::OpsDaemon;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as i64
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == name)
        .and_then(|index| args.get(index + 1).cloned())
}

fn respond(stream: &mut TcpStream, status: &str, body: &str) -> std::io::Result<()> {
    write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
}

fn handle(stream: &mut TcpStream, daemon: &mut OpsDaemon) -> std::io::Result<()> {
    let mut request = [0u8; 4096];
    let len = stream.read(&mut request)?;
    let request = String::from_utf8_lossy(&request[..len]);
    let Some(line) = request.lines().next() else {
        return respond(stream, "400 Bad Request", r#"{"error":"empty request"}"#);
    };
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let path = parts.next().unwrap_or_default();
    let now = now_ns();
    daemon.ingest_heartbeat_files(now);
    for alert in daemon.check_alerts(now) {
        tracing::warn!(id = %alert.id, detail = %alert.detail, "opsd alert");
    }
    match (method, path) {
        ("GET", "/status") => {
            let body = serde_json::to_string(&daemon.status(now))
                .unwrap_or_else(|_| r#"{"error":"status serialization"}"#.into());
            respond(stream, "200 OK", &body)
        }
        ("POST", path) if path.starts_with("/beat/") => {
            let process = path.trim_start_matches("/beat/");
            if process.is_empty() || process.contains('/') {
                return respond(stream, "400 Bad Request", r#"{"error":"invalid process"}"#);
            }
            daemon.beat(process, now);
            respond(stream, "204 No Content", "")
        }
        _ => respond(stream, "404 Not Found", r#"{"error":"not found"}"#),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    let args: Vec<String> = std::env::args().collect();
    let bind = flag(&args, "--bind").unwrap_or_else(|| "127.0.0.1:9180".into());
    let data_dir = PathBuf::from(flag(&args, "--data-dir").unwrap_or_else(|| "data".into()));
    let listener = TcpListener::bind(&bind)?;
    let mut daemon = OpsDaemon::new(data_dir, 30_000_000_000);
    tracing::info!(%bind, "opsd started (read-only status and heartbeat ingestion)");
    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                if let Err(error) = handle(&mut stream, &mut daemon) {
                    tracing::warn!(%error, "opsd request failed");
                }
            }
            Err(error) => tracing::warn!(%error, "opsd accept failed"),
        }
    }
    Ok(())
}
