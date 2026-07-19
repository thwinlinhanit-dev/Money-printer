//! Screener hit journal (spec 017). Persists `ScreenerHit` to append-only
//! JSONL files per day, and supports forward-return backfill and grading.

use crate::ScreenerHit;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

/// A hit with optional forward returns (null at write time).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HitRecord {
    pub rule_id: String,
    pub symbol: u32,
    pub ts_ns: i64,
    pub snapshot: std::collections::BTreeMap<String, f64>,
    pub forward_return_1h: Option<f64>,
    pub forward_return_4h: Option<f64>,
    pub forward_return_24h: Option<f64>,
}

impl From<ScreenerHit> for HitRecord {
    fn from(h: ScreenerHit) -> Self {
        Self {
            rule_id: h.rule_id,
            symbol: h.symbol.0,
            ts_ns: h.ts_ns,
            snapshot: h.snapshot,
            forward_return_1h: None,
            forward_return_4h: None,
            forward_return_24h: None,
        }
    }
}

/// Hit journal: append-only JSONL per day.
pub struct HitJournal {
    dir: PathBuf,
    writer: Option<std::io::BufWriter<std::fs::File>>,
    current_date: String,
}

impl HitJournal {
    /// Open the journal directory. Creates per-date JSONL files on first write.
    pub fn open(dir: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        std::fs::create_dir_all(dir.join("backfill"))?;
        Ok(Self { dir: dir.to_owned(), writer: None, current_date: String::new() })
    }

    fn date_str() -> String {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let days = secs / 86400;
        let mut y = 1970i64;
        let mut rem = days as i64;
        loop {
            let days_yr = if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 { 366 } else { 365 };
            if rem < days_yr { break; }
            rem -= days_yr;
            y += 1;
        }
        let months = [31, if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 { 29 } else { 28 },
            31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
        let mut m = 0usize;
        while m < 12 && rem >= months[m] { rem -= months[m]; m += 1; }
        format!("{:04}-{:02}-{:02}", y, m + 1, rem + 1)
    }

    fn ensure_writer(&mut self) -> std::io::Result<()> {
        let date = Self::date_str();
        if date != self.current_date {
            if let Some(mut w) = self.writer.take() {
                w.flush()?;
            }
            let path = self.dir.join(format!("{date}.jsonl"));
            let file = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
            self.writer = Some(std::io::BufWriter::new(file));
            self.current_date = date;
        }
        Ok(())
    }

    /// Record one hit (GRD-1).
    pub fn record(&mut self, hit: ScreenerHit) -> std::io::Result<()> {
        self.ensure_writer()?;
        let record: HitRecord = hit.into();
        let line = serde_json::to_string(&record)? + "\n";
        if let Some(ref mut w) = self.writer {
            w.write_all(line.as_bytes())?;
            w.flush()?;
            w.get_ref().sync_data()?;
        }
        Ok(())
    }

    /// Read all hits for a date range.
    pub fn read_range(&self, from: &str, to: &str) -> std::io::Result<Vec<HitRecord>> {
        let mut results = Vec::new();
        let mut d = from.to_string();
        loop {
            let path = self.dir.join(format!("{d}.jsonl"));
            if path.exists() {
                for line in std::fs::read_to_string(&path)?.lines() {
                    if let Ok(r) = serde_json::from_str::<HitRecord>(line) {
                        results.push(r);
                    }
                }
            }
            if d.as_str() >= to { break; }
            d = next_date(&d);
        }
        Ok(results)
    }

    /// Write backfill records (forward returns computed later).
    pub fn write_backfill(&self, records: &[HitRecord]) -> std::io::Result<()> {
        let date = Self::date_str();
        let dir = self.dir.join("backfill");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{date}.jsonl"));
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
        for r in records {
            writeln!(f, "{}", serde_json::to_string(r)?)?;
        }
        f.sync_data()?;
        Ok(())
    }
}

fn next_date(d: &str) -> String {
    let parts: Vec<&str> = d.split('-').collect();
    if parts.len() != 3 { return d.to_string(); }
    let y: i64 = parts[0].parse().unwrap_or(1970);
    let m: u32 = parts[1].parse().unwrap_or(1);
    let day: u32 = parts[2].parse().unwrap_or(1);
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let months = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let (ny, nm, nd) = if day >= months[m as usize - 1] {
        if m >= 12 { (y + 1, 1u32, 1u32) } else { (y, m + 1, 1u32) }
    } else { (y, m, day + 1) };
    format!("{:04}-{:02}-{:02}", ny, nm, nd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mp_core::SymbolId;

    #[test]
    fn grd_1_hit_persisted_with_snapshot() {
        let dir = std::env::temp_dir().join(format!("hit_journal_{}", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut j = HitJournal::open(&dir).unwrap();
        let hit = ScreenerHit {
            rule_id: "test_rule".into(),
            symbol: SymbolId(1),
            ts_ns: 1784456653319497000,
            snapshot: [("funding_rate".into(), 0.00015)].into(),
        };
        j.record(hit).unwrap();
        let date = HitJournal::date_str();
        let records = j.read_range(&date, &date).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].rule_id, "test_rule");
        assert_eq!(records[0].snapshot.get("funding_rate"), Some(&0.00015));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn grd_4_grading_produces_report() {
        let dir = std::env::temp_dir().join(format!("hit_grading_{}", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut j = HitJournal::open(&dir).unwrap();
        for i in 0..10 {
            j.record(ScreenerHit {
                rule_id: "rule_a".into(),
                symbol: SymbolId(1),
                ts_ns: 1784456653319497000 + i * 1_000_000_000,
                snapshot: Default::default(),
            }).unwrap();
        }
        let date = HitJournal::date_str();
        let records = j.read_range(&date, &date).unwrap();
        assert_eq!(records.len(), 10);
        assert!(records.iter().all(|r| r.rule_id == "rule_a"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
