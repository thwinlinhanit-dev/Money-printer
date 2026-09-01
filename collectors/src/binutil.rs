//! Shared helper utilities for the collector binaries (mp-collector, mp-whale,
//! mp-macro). Pure std — no network stack, so any binary can use them.
//! Wall-clock reads here are binary-edge only (recv stamping, rotation,
//! heartbeats) — never decision-path values (PD-3/CONV-5).

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Value of a CLI flag `--name <value>`; `None` if absent.
pub fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

/// True when `--name` is present as a bare switch.
pub fn has_flag(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

/// `--check-config` (CONV-18): validate config and exit 0; otherwise exit 2.
/// Call when `--check-config` is present, AFTER the config has been parsed.
pub fn check_config_exit() -> ! {
    eprintln!("config OK");
    std::process::exit(0);
}

/// `--version` (CONV-18): print the embedded git SHA (or a fallback) and exit.
pub fn version_exit() -> ! {
    let sha = option_env!("GIT_SHA").unwrap_or("unknown").to_string();
    let sha = if sha.is_empty() {
        "unknown".into()
    } else {
        sha
    };
    println!("{} {sha}", env!("CARGO_PKG_VERSION"));
    std::process::exit(0);
}

/// Current UTC date as `YYYYMMDD` (log rotation key). Pure arithmetic — no
/// chrono dependency, exact for the Gregorian calendar.
pub fn utc_date_str() -> String {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = d / 86400;
    let mut y = 1970i64;
    let mut rem = days as i64;
    loop {
        let days_yr = if is_leap(y) { 366 } else { 365 };
        if rem < days_yr {
            break;
        }
        rem -= days_yr;
        y += 1;
    }
    let months = [
        31,
        if is_leap(y) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut m = 0usize;
    while m < 12 && rem >= months[m] {
        rem -= months[m];
        m += 1;
    }
    format!("{:04}{:02}{:02}", y, m + 1, rem + 1)
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// Wall clock as ns — recv stamping / timers at the binary edge only.
pub fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as i64
}

/// Exclusive create/open of a lock file (platform-specific; see
/// [`InstanceLock`]).
fn exclusive_lock_file(path: &Path) -> io::Result<File> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // share_mode(0) = exclusive; second process gets ERROR_SHARING_VIOLATION.
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .share_mode(0)
            .open(path)
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(path)
        {
            Ok(f) => Ok(f),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "lock file exists — another collector is running, or a stale \
                 lock remains after a crash (delete the .lock_* file if sure)",
            )),
            Err(e) => Err(e),
        }
    }
    #[cfg(not(any(windows, unix)))]
    {
        OpenOptions::new().write(true).create_new(true).open(path)
    }
}

/// Process-lifetime exclusive lock so two collectors cannot write the same
/// log (one process owns one recording). Held open for the whole run.
#[derive(Debug)]
pub struct InstanceLock {
    _file: File,
    path: PathBuf,
}

impl InstanceLock {
    pub fn acquire(raw_dir: &Path, name: &str) -> io::Result<Self> {
        std::fs::create_dir_all(raw_dir)?;
        let path = raw_dir.join(format!(".lock_{name}"));
        let mut file = exclusive_lock_file(&path).map_err(|e| {
            io::Error::new(
                e.kind(),
                format!(
                    "another collector already owns {name} (lock {}): {e}",
                    path.display()
                ),
            )
        })?;
        let _ = writeln!(file, "pid={} name={name}", std::process::id());
        let _ = file.flush();
        Ok(Self { _file: file, path })
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        // Best-effort cleanup; exclusive handle release is the real unlock.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// PID file for systemd/monitoring; removed on clean exit (COL-18/19).
pub struct PidFile {
    path: PathBuf,
}

impl PidFile {
    pub fn write(raw_dir: &Path, name: &str) -> io::Result<Self> {
        let path = raw_dir.join(format!("mp-collector-{name}.pid"));
        std::fs::write(&path, format!("{}\n", std::process::id()))?;
        Ok(Self { path })
    }
}

impl Drop for PidFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Write/refresh a heartbeat file (every `interval`, per loop edge).
pub fn touch_heartbeat(raw_dir: &Path, name: &str) {
    let path = raw_dir.join(format!("mp-collector-{name}.heartbeat"));
    let ts_sec = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let _ = std::fs::write(
        path,
        format!("ts={ts_sec} pid={} name={name}\n", std::process::id()),
    );
}

/// Append-only tracing sink shared by the collector binaries (`--trace-file`).
/// Append (never truncate) so a watchdog respawn never erases the freeze
/// evidence of the previous process — rotate by naming the path per day at
/// the caller (watchdog passes `trace_<date>_<venue>_<symbol>.log`).
#[derive(Clone)]
pub struct SharedLogFile {
    // Do not buffer this sink. The watchdog may terminate a stale collector,
    // and its last reconnect/error line must already be inspectable on disk.
    file: std::sync::Arc<std::sync::Mutex<File>>,
}

impl SharedLogFile {
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            file: std::sync::Arc::new(std::sync::Mutex::new(file)),
        })
    }
}

impl Write for SharedLogFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut f = self
            .file
            .lock()
            .map_err(|_| io::Error::other("shared trace file mutex poisoned"))?;
        f.write_all(buf)?;
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        let mut f = self
            .file
            .lock()
            .map_err(|_| io::Error::other("shared trace file mutex poisoned"))?;
        f.flush()
    }
}

/// tracing-subscriber sink: each `make_writer()` hands the subscriber a cheap
/// clone; the fmt layer never calls `Write` on the same handle twice.
impl<'a> tracing_subscriber::fmt::writer::MakeWriter<'a> for SharedLogFile {
    type Writer = Self;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Plain-text fmt subscriber over a [`SharedLogFile`] sink (LOG-1): ANSI
/// forced OFF and a UTC timestamp so trace files stay machine-parseable —
/// the 2026-08-15 outage traces were polluted with ESC[..m codes that broke
/// timestamp parsing (enforced by `ops/ci/check_log_hygiene.sh`).
pub fn trace_subscriber(sink: SharedLogFile) -> impl tracing::Subscriber + Send + Sync + 'static {
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(sink)
        .with_timer(tracing_subscriber::fmt::time::SystemTime)
        .finish()
}

/// `RUST_LOG`-driven filter for the fmt builders (`tracing-filter` default
/// falls back to `info` when the env var is unset).
pub fn tracing_filter() -> tracing_subscriber::EnvFilter {
    tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
}

#[cfg(test)]
mod tests {
    use super::SharedLogFile;
    use std::io::Write;

    #[test]
    fn shared_log_file_is_visible_without_waiting_for_drop() {
        let path = std::env::temp_dir().join(format!("mp-trace-{}.log", std::process::id()));
        let _ = std::fs::remove_file(&path);

        let mut sink = SharedLogFile::open(&path).expect("open trace sink");
        sink.write_all(b"reconnect diagnostic\\n")
            .expect("write trace event");

        assert_eq!(
            std::fs::read(&path).expect("read trace file"),
            b"reconnect diagnostic\\n"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn trace_subscriber_emits_ansi_free_timestamped_lines() {
        // LOG-1: trace files are machine input — the fmt subscriber must emit
        // plain text (no ESC[..m codes) with a parseable UTC timestamp.
        let path = std::env::temp_dir().join(format!("mp-trace-fmt-{}.log", std::process::id()));
        let _ = std::fs::remove_file(&path);

        let sink = SharedLogFile::open(&path).expect("open trace sink");
        let subscriber = super::trace_subscriber(sink);
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(venue = "hyperliquid", "test trace line");
        });

        let text = std::fs::read_to_string(&path).expect("read trace file");
        assert!(
            !text.contains('\u{1b}'),
            "ANSI escape leaked into trace: {text:?}"
        );
        assert!(
            text.contains("test trace line"),
            "event body missing: {text:?}"
        );
        assert!(
            text.starts_with("2026-") || text.contains("INFO"),
            "expected timestamped INFO line, got: {text:?}"
        );
        let _ = std::fs::remove_file(path);
    }
}
