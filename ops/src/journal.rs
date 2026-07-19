//! Bot command journal persistence (spec 021).
//!
//! Append-only JSONL per day for bot commands, plus a separate pending.json
//! for confirmation state that survives restart.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

/// One command journal entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandEntry {
    pub ts_ns: i64,
    pub user_id: i64,
    pub text: String,
    pub verdict: String,
}

/// Pending confirmation state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingState {
    pub kind: String,          // "kill" or "flatten"
    pub scope: Option<String>, // kill scope or confirms_left
}

/// Bot command journal.
pub struct CommandJournal {
    dir: PathBuf,
    writer: Option<std::io::BufWriter<std::fs::File>>,
    current_date: String,
    bytes_written: u64,
    max_bytes: u64,
    last_flushed: i64,
}

impl CommandJournal {
    /// Open or create the journal directory. The current day's file is opened
    /// for appending.
    pub fn open(dir: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        std::fs::create_dir_all(dir.join("backfill"))?;
        let mut j = Self {
            dir: dir.to_owned(),
            writer: None,
            current_date: String::new(),
            bytes_written: 0,
            max_bytes: 10 * 1024 * 1024, // 10MB default rotation threshold
            last_flushed: 0,
        };
        j.rotate()?;
        Ok(j)
    }

    /// Write one command entry and fsync before returning (OPS-12).
    pub fn append(&mut self, entry: &CommandEntry) -> std::io::Result<()> {
        let date = utc_date_str();
        if date != self.current_date || self.bytes_written >= self.max_bytes {
            self.rotate()?;
        }
        let line = serde_json::to_string(entry)? + "\n";
        if let Some(ref mut w) = self.writer {
            w.write_all(line.as_bytes())?;
            self.bytes_written += line.len() as u64;
            w.flush()?; // fflush
            // fsync the file handle
            w.get_ref().sync_data()?;
        }
        Ok(())
    }

    fn rotate(&mut self) -> std::io::Result<()> {
        if let Some(mut w) = self.writer.take() {
            w.flush()?;
            w.get_ref().sync_data()?;
        }
        let date = utc_date_str();
        let path = self.dir.join(format!("commands-{date}.jsonl"));
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        self.writer = Some(std::io::BufWriter::new(file));
        self.current_date = date;
        self.bytes_written = 0;
        Ok(())
    }

    /// Read back commands for a date range.
    pub fn read_range(&self, from: &str, to: &str) -> std::io::Result<Vec<CommandEntry>> {
        let mut results = Vec::new();
        let mut d = from.to_string();
        loop {
            let path = self.dir.join(format!("commands-{d}.jsonl"));
            if path.exists() {
                let content = std::fs::read_to_string(&path)?;
                for line in content.lines() {
                    if let Ok(entry) = serde_json::from_str::<CommandEntry>(line) {
                        results.push(entry);
                    }
                }
            }
            if d.as_str() >= to {
                break;
            }
            d = next_date(&d);
        }
        Ok(results)
    }

    /// Persist pending confirmation (OPS-15). Write to temp, rename for atomicity.
    pub fn write_pending(&self, state: &Option<PendingState>) -> std::io::Result<()> {
        let path = self.dir.join("pending.json");
        let tmp = self.dir.join("pending.json.tmp");
        let json = serde_json::to_string(state)?;
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Read pending confirmation on restart.
    pub fn read_pending(&self) -> std::io::Result<Option<PendingState>> {
        let path = self.dir.join("pending.json");
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&path)?;
        serde_json::from_str(&content).map_err(|e| std::io::Error::other(e))
    }

    /// Flush and fsync.
    pub fn flush(&mut self) -> std::io::Result<()> {
        if let Some(ref mut w) = self.writer {
            w.flush()?;
            w.get_ref().sync_data()?;
        }
        Ok(())
    }
}

fn utc_date_str() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = secs / 86400;
    let mut y = 1970i64;
    let mut rem = days as i64;
    loop {
        let days_yr = if is_leap(y) { 366 } else { 365 };
        if rem < days_yr { break; }
        rem -= days_yr;
        y += 1;
    }
    let months = [31, if is_leap(y) { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut m = 0usize;
    while m < 12 && rem >= months[m] {
        rem -= months[m];
        m += 1;
    }
    format!("{:04}-{:02}-{:02}", y, m + 1, rem + 1)
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn next_date(d: &str) -> String {
    // Simple: parse YYYY-MM-DD, add one day
    let parts: Vec<&str> = d.split('-').collect();
    if parts.len() != 3 { return d.to_string(); }
    let y: i64 = parts[0].parse().unwrap_or(1970);
    let m: u32 = parts[1].parse().unwrap_or(1);
    let day: u32 = parts[2].parse().unwrap_or(1);
    let months = [31, if is_leap(y) { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let (ny, nm, nd) = if day >= months[m as usize - 1] {
        if m >= 12 { (y + 1, 1u32, 1u32) } else { (y, m + 1, 1u32) }
    } else {
        (y, m, day + 1)
    };
    format!("{:04}-{:02}-{:02}", ny, nm, nd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn ops_11_command_persisted() {
        let dir = std::env::temp_dir().join(format!("cmd_journal_test_{}", SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut j = CommandJournal::open(&dir).unwrap();
        let entry = CommandEntry {
            ts_ns: 1784456653319497000,
            user_id: 123456789,
            text: "/position BTCUSDT".into(),
            verdict: "ok".into(),
        };
        j.append(&entry).unwrap();
        j.flush().unwrap();
        // Read back
        let date = utc_date_str();
        let entries = j.read_range(&date, &date).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].text, "/position BTCUSDT");
        assert_eq!(entries[0].verdict, "ok");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ops_14_audit_read() {
        let dir = std::env::temp_dir().join(format!("cmd_audit_{}", SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut j = CommandJournal::open(&dir).unwrap();
        for i in 0..5 {
            j.append(&CommandEntry {
                ts_ns: 1784456653319497000 + i,
                user_id: 1,
                text: format!("/status {}", i),
                verdict: "ok".into(),
            }).unwrap();
        }
        j.flush().unwrap();
        let date = utc_date_str();
        let entries = j.read_range(&date, &date).unwrap();
        assert_eq!(entries.len(), 5);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ops_15_pending_survives_restart() {
        let dir = std::env::temp_dir().join(format!("cmd_pending_{}", SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
        let _ = std::fs::remove_dir_all(&dir);
        let j = CommandJournal::open(&dir).unwrap();
        let state = Some(PendingState {
            kind: "kill".into(),
            scope: Some("GLOBAL".into()),
        });
        j.write_pending(&state).unwrap();
        let read = j.read_pending().unwrap();
        assert_eq!(read.as_ref().map(|s| &s.kind), Some(&"kill".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
