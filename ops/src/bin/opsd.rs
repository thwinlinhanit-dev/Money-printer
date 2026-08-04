//! `opsd` — local health-ingestion and read-only status daemon (OPS-11).
//! It deliberately exposes no order, mode, or kill-latch-clearing endpoint.

use mp_ops::OpsDaemon;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

/// Extract the bearer-ish token presented in an HTTP request. Accepts
/// `Authorization: Bearer <t>` or `X-Opsd-Token: <t>`. Returns `None` when the
/// request carries neither (and the caller decides whether that is allowed).
fn request_token(request: &str) -> Option<&str> {
    const AUTHORIZATION: &str = "authorization: bearer ";
    const X_OPSD_TOKEN: &str = "x-opsd-token: ";
    for line in request.lines() {
        let line = line.trim();
        let lower = line.to_ascii_lowercase();
        if lower.starts_with(AUTHORIZATION) {
            return Some(line[AUTHORIZATION.len()..].trim());
        }
        if lower.starts_with(X_OPSD_TOKEN) {
            return Some(line[X_OPSD_TOKEN.len()..].trim());
        }
    }
    None
}

/// Whether `request` may proceed. With no configured token (or an empty one)
/// every request passes — safe only because `main` refuses a non-loopback bind
/// without a token. With a token configured, the request must present exactly
/// that token (08-04 audit: the previous docs claimed a token that didn't exist).
fn authorized(request: &str, configured: Option<&str>) -> bool {
    match configured {
        None | Some("") => true,
        Some(expected) => request_token(request) == Some(expected),
    }
}

fn respond(stream: &mut TcpStream, status: &str, body: &str) -> std::io::Result<()> {
    write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
}

fn handle(stream: &mut TcpStream, daemon: &mut OpsDaemon, token: Option<&str>) -> std::io::Result<()> {
    let mut request = [0u8; 4096];
    let len = stream.read(&mut request)?;
    let request = String::from_utf8_lossy(&request[..len]);
    if !authorized(&request, token) {
        return respond(stream, "401 Unauthorized", r#"{"error":"unauthorized"}"#);
    }
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
    let token = std::env::var("MP_OPSD_TOKEN").ok();

    let listener = TcpListener::bind(&bind)?;
    let bound = listener.local_addr()?;
    // Fail closed (08-04 audit): an operator who widens the bind beyond loopback
    // must supply MP_OPSD_TOKEN, or opsd refuses to start rather than silently
    // exposing unauthenticated /status and POST /beat. Env-supplied, never
    // committed (PD-2).
    let has_token = matches!(token.as_deref(), Some(t) if !t.is_empty());
    if !bound.ip().is_loopback() && !has_token {
        eprintln!("refusing to start opsd on non-loopback {bound} without MP_OPSD_TOKEN");
        return Err("non-loopback bind requires MP_OPSD_TOKEN".into());
    }

    let mut daemon = OpsDaemon::new(data_dir, 30_000_000_000);
    tracing::info!(%bind, token_required = has_token, "opsd started (read-only status and heartbeat ingestion)");
    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                // A slow/never-writing peer must not stall the dead-man loop
                // (08-04 audit): a bounded read timeout lets a bad peer be
                // dropped and the next heartbeat/status request proceed.
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                if let Err(error) = handle(&mut stream, &mut daemon, token.as_deref()) {
                    tracing::warn!(%error, "opsd request failed");
                }
            }
            Err(error) => tracing::warn!(%error, "opsd accept failed"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{authorized, request_token};

    #[test]
    fn opsd_request_token_parses_bearer_and_x_opsd_token() {
        assert_eq!(
            request_token("GET /status HTTP/1.1\r\nAuthorization: Bearer abc123\r\n"),
            Some("abc123")
        );
        assert_eq!(
            request_token("GET /beat/x HTTP/1.1\r\nX-Opsd-Token:  secret\r\n"),
            Some("secret")
        );
        // A non-auth request carries no token.
        assert_eq!(request_token("GET /status HTTP/1.1\r\nHost: localhost\r\n"), None);
    }

    #[test]
    fn opsd_no_configured_token_allows_all() {
        assert!(authorized("", None));
        assert!(authorized("GET /status HTTP/1.1\r\n", Some("")));
    }

    #[test]
    fn opsd_configured_token_must_match_exactly() {
        let token = Some("s3cr3t");
        assert!(authorized(
            "GET /status HTTP/1.1\r\nAuthorization: Bearer s3cr3t\r\n",
            token
        ));
        assert!(authorized(
            "POST /beat/binance HTTP/1.1\r\nX-Opsd-Token: s3cr3t\r\n",
            token
        ));
        // Wrong token, missing header, and a token embedded mid-header all fail.
        assert!(!authorized(
            "GET /status HTTP/1.1\r\nAuthorization: Bearer nope\r\n",
            token
        ));
        assert!(!authorized("GET /status HTTP/1.1\r\n", token));
    }
}
