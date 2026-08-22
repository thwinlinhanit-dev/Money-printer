//! `mp-query` — read-time analytics transforms over the corpus (spec 003
//! §Analytics). The Cryexc/OpenMarket compute-on-read pattern: raw points are
//! stored once (raw logs → cold Parquet) and research views are DERIVED on
//! demand.
//!
//! Subcommands:
//!   mp-query footprint --cold <root> --venue <v> --symbol <s> --date <d>
//!       [--interval-secs N] [--bucket-usd B] [--json]
//!     Per-interval bars with the order-flow split (footprint totals); with
//!     --bucket-usd, the block-bucketed (interval × price) footprint grid.
//!     Reads cold Parquet via the Dataset reader (STO-4).
//!
//!   mp-query oiwa --logs <a.log> [--logs <b.log> ...] [--interval-secs N]
//!       [--json]
//!     OI-weighted funding series (OpenMarket GROUP_BY_TYPE_OPEN_INTEREST_
//!     WEIGHTED_AVG) over raw logs, merged exactly like the materializer
//!     (load_logs_merged, MAT-5). Default interval 28_800s (8h funding cycle).
//!
//!   mp-query carry --logs <a.log> [--logs <b.log> ...] [--interval-secs N]
//!       [--json]
//!     Funding-carry series: per (interval, venue, symbol) the OI-weighted
//!     funding rate paired with the mark-vs-oracle basis in bps — the two
//!     legs of the funding-carry trade. Default interval 3_600s (Hyperliquid
//!     funds hourly).
//!
//!   mp-query liq --logs <a.log> [--logs <b.log> ...] [--interval-secs N]
//!       [--json]
//!     Liquidation-stress series: per (interval, venue, symbol) the signed
//!     notional sums of liquidation events (buy = shorts flushed, sell = longs
//!     dumped). Intervals line up with `carry` on `interval_ts_ns` so the
//!     positioning legs join. Default interval 3_600s.
//!
//!   mp-query bars --logs <a.log> [--interval-secs N] [--json]
//!     OHLCV + order-flow bars from ONE log (a venue-symbol-day file): open/
//!     high/low/close/vwap + aggressor buy/sell volume + trade count per
//!     interval (terminal Slice 1 price pane; exactly one --logs — bars from a
//!     multi-symbol merge would mix symbols).
//!
//!   mp-query dom --logs <a.log> [--from <ts_ns>] [--to <ts_ns>]
//!       [--every-ms M] [--levels N] [--max-samples M] [--json]
//!     Sampled top-N order-book ladders from ONE log (terminal Slice 1 DOM
//!     pane): the book is warmed from the log head, then the ladder is sampled
//!     at `from`, `from + every`, … up to `to`. `stale: true` samples mark
//!     sequence holes — the viewer must show "no trusted book", not a stale
//!     ladder (EVT-9). Defaults: full day, 5s samples, 5 levels, 300 samples.
//!
//!   mp-query merge --logs <a.log> [--logs <b.log> ...] --out <merged.log>
//!     Merge multi-day logs into ONE event log in the canonical order (MAT-5/
//!     EVT-5/EVT-8, symbol ids re-interned onto a shared table) — the sim's
//!     backtest/wf replay input. Deterministic: same input set, byte-identical
//!     output (no wall clock, PD-3).
//!
//! Exit 0 on success, 2 on any error. Deterministic: no wall clock (PD-3),
//! pure aggregation over the given inputs.

use mp_core::Venue;
use mp_storage::analytics::{self, FootprintBar, FootprintBucketRow, OiwaSeries};
use mp_storage::{load_logs_merged, stream_logs_merged, Dataset};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn all_flags(args: &[String], name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == name {
            if let Some(v) = args.get(i + 1) {
                out.push(v.clone());
                i += 1;
            }
        }
        i += 1;
    }
    out
}

fn usage() -> ! {
    eprintln!(
        "usage: mp-query footprint --cold <root> --venue <v> --symbol <s> --date <YYYY-MM-DD> \
         [--interval-secs N] [--bucket-usd B] [--json]\n\
         \x20      mp-query oiwa --logs <event.log> [--logs <more.log> ...] \
         [--interval-secs N] [--json]\n\
         \x20      mp-query carry --logs <event.log> [--logs <more.log> ...] \
         [--interval-secs N] [--json]\n\
         \x20      mp-query liq --logs <event.log> [--logs <more.log> ...] \
         [--interval-secs N] [--json]\n\
         \x20      mp-query bars --logs <event.log> [--interval-secs N] [--json]\n\
         \x20      mp-query dom --logs <event.log> [--from <ts_ns>] [--to <ts_ns>] \
         [--every-ms M] [--levels N] [--max-samples M] [--json]\n\
         \x20      mp-query merge --logs <event.log> [--logs <more.log> ...] \
         --out <merged.log>"
    );
    std::process::exit(2)
}

/// Exit-2 helper for a missing required flag, typed as the error value.
fn missing(what: &str) -> String {
    eprintln!("mp-query: missing required --{what}");
    std::process::exit(2)
}

fn run() -> Result<ExitCode, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let sub = args.first().map(|s| s.as_str()).unwrap_or_else(|| usage());
    match sub {
        "footprint" => {
            let cold = flag(&args, "--cold").unwrap_or_else(|| "data/cold".into());
            let venue = flag(&args, "--venue").ok_or_else(|| missing("venue"))?;
            let symbol = flag(&args, "--symbol").ok_or_else(|| missing("symbol"))?;
            let date = flag(&args, "--date").ok_or_else(|| missing("date"))?;
            let interval = flag(&args, "--interval-secs")
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(60)
                * 1_000_000_000;
            let bucket_usd = flag(&args, "--bucket-usd").and_then(|s| s.parse::<f64>().ok());
            let json = args.iter().any(|a| a == "--json");

            let venue = Venue::from_slug(&venue)
                .ok_or_else(|| format!("mp-query: unknown venue slug '{venue}'"))?;
            let ds = Dataset::open(Path::new(&cold));
            let events = ds
                .trades_day(venue, &symbol, &date)
                .map_err(|e| format!("read cold {venue:?}/{symbol}/{date}: {e}"))?;
            match bucket_usd {
                Some(b) => {
                    let rows: Vec<FootprintBucketRow> =
                        analytics::footprint_buckets(&events, interval, b);
                    emit(&rows, json, "mp-query footprint (bucketed)");
                }
                None => {
                    let bars: Vec<FootprintBar> = analytics::footprint_bars(&events, interval);
                    emit(&bars, json, "mp-query footprint");
                }
            }
        }
        "oiwa" => {
            let logs: Vec<PathBuf> = all_flags(&args, "--logs")
                .into_iter()
                .map(PathBuf::from)
                .collect();
            if logs.is_empty() {
                return Err("mp-query oiwa: at least one --logs <file> is required".into());
            }
            let interval = flag(&args, "--interval-secs")
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(28_800)
                * 1_000_000_000;
            let json = args.iter().any(|a| a == "--json");
            let loaded = load_logs_merged(&logs).map_err(|e| format!("mp-query oiwa: {e}"))?;
            let series: Vec<OiwaSeries> = analytics::oiwa_series(&loaded.events, interval);
            emit(&series, json, "mp-query oiwa");
        }
        "carry" => {
            let logs: Vec<PathBuf> = all_flags(&args, "--logs")
                .into_iter()
                .map(PathBuf::from)
                .collect();
            if logs.is_empty() {
                return Err("mp-query carry: at least one --logs <file> is required".into());
            }
            let interval = flag(&args, "--interval-secs")
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(3_600)
                * 1_000_000_000;
            let json = args.iter().any(|a| a == "--json");
            let loaded = load_logs_merged(&logs).map_err(|e| format!("mp-query carry: {e}"))?;
            let pts = analytics::carry_series(&loaded.events, interval);
            // Resolve numeric symbol ids to names via the shared symbol table.
            #[derive(serde::Serialize)]
            struct CarryOut {
                interval_ts_ns: i64,
                venue: String,
                symbol: String,
                funding_rate: f64,
                total_oi: f64,
                oi_unit: String,
                mark: f64,
                index: f64,
                basis_bps: f64,
            }
            let out: Vec<CarryOut> = pts
                .into_iter()
                .map(|p| CarryOut {
                    interval_ts_ns: p.interval_ts_ns,
                    venue: p.venue,
                    symbol: loaded
                        .symbols
                        .get(mp_core::SymbolId(p.symbol))
                        .map(|m| m.venue_symbol.clone())
                        .unwrap_or_else(|| p.symbol.to_string()),
                    funding_rate: p.funding_rate,
                    total_oi: p.total_oi,
                    oi_unit: p.oi_unit,
                    mark: p.mark,
                    index: p.index,
                    basis_bps: p.basis_bps,
                })
                .collect();
            emit(&out, json, "mp-query carry");
        }
        "liq" => {
            let logs: Vec<PathBuf> = all_flags(&args, "--logs")
                .into_iter()
                .map(PathBuf::from)
                .collect();
            if logs.is_empty() {
                return Err("mp-query liq: at least one --logs <file> is required".into());
            }
            let interval = flag(&args, "--interval-secs")
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(3_600)
                * 1_000_000_000;
            let json = args.iter().any(|a| a == "--json");
            let loaded = load_logs_merged(&logs).map_err(|e| format!("mp-query liq: {e}"))?;
            let pts = analytics::liq_series(&loaded.events, interval);
            #[derive(serde::Serialize)]
            struct LiqOut {
                interval_ts_ns: i64,
                venue: String,
                symbol: String,
                buy_notional: f64,
                sell_notional: f64,
                net_notional: f64,
                n_buy: u64,
                n_sell: u64,
            }
            let out: Vec<LiqOut> = pts
                .into_iter()
                .map(|p| LiqOut {
                    interval_ts_ns: p.interval_ts_ns,
                    venue: p.venue,
                    symbol: loaded
                        .symbols
                        .get(mp_core::SymbolId(p.symbol))
                        .map(|m| m.venue_symbol.clone())
                        .unwrap_or_else(|| p.symbol.to_string()),
                    buy_notional: p.buy_notional,
                    sell_notional: p.sell_notional,
                    net_notional: p.net_notional,
                    n_buy: p.n_buy,
                    n_sell: p.n_sell,
                })
                .collect();
            emit(&out, json, "mp-query liq");
        }
        "bars" => {
            let logs: Vec<PathBuf> = all_flags(&args, "--logs")
                .into_iter()
                .map(PathBuf::from)
                .collect();
            if logs.len() != 1 {
                return Err(
                    "mp-query bars: exactly one --logs <file> is required (a venue-symbol-day \
                     file); multi-symbol bars would mix symbols"
                        .into(),
                );
            }
            let interval = flag(&args, "--interval-secs")
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(60)
                * 1_000_000_000;
            let json = args.iter().any(|a| a == "--json");
            let loaded = load_logs_merged(&logs).map_err(|e| format!("mp-query bars: {e}"))?;
            let bars = analytics::footprint_bars(&loaded.events, interval);
            #[derive(serde::Serialize)]
            struct BarOut {
                interval_ts_ns: i64,
                open: f64,
                high: f64,
                low: f64,
                close: f64,
                vwap: f64,
                buy_vol: f64,
                sell_vol: f64,
                n_trades: u64,
            }
            let out: Vec<BarOut> = bars
                .into_iter()
                .map(|b| BarOut {
                    interval_ts_ns: b.bucket_ts_ns,
                    open: b.open,
                    high: b.high,
                    low: b.low,
                    close: b.close,
                    vwap: b.vwap,
                    buy_vol: b.buy_vol,
                    sell_vol: b.sell_vol,
                    n_trades: b.n_trades,
                })
                .collect();
            emit(&out, json, "mp-query bars");
        }
        "dom" => {
            let logs: Vec<PathBuf> = all_flags(&args, "--logs")
                .into_iter()
                .map(PathBuf::from)
                .collect();
            if logs.len() != 1 {
                return Err(
                    "mp-query dom: exactly one --logs <file> is required (a venue-symbol-day \
                     file)"
                        .into(),
                );
            }
            let from = flag(&args, "--from").and_then(|s| s.parse::<i64>().ok());
            let to = flag(&args, "--to").and_then(|s| s.parse::<i64>().ok());
            let every_ns = flag(&args, "--every-ms")
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(5_000)
                * 1_000_000;
            let levels = flag(&args, "--levels")
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(5);
            let max_samples = flag(&args, "--max-samples")
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(300);
            let json = args.iter().any(|a| a == "--json");
            let loaded = load_logs_merged(&logs).map_err(|e| format!("mp-query dom: {e}"))?;
            let (from, to) = match (from, to) {
                (Some(f), Some(t)) => (f, t),
                _ => {
                    let (Some(f), Some(t)) = (
                        loaded.events.first().map(|e| e.recv_ts_ns),
                        loaded.events.last().map(|e| e.recv_ts_ns),
                    ) else {
                        emit(&Vec::<analytics::DomSnapshot>::new(), json, "mp-query dom");
                        return Ok(ExitCode::SUCCESS);
                    };
                    (f, t)
                }
            };
            let snaps =
                analytics::dom_series(&loaded.events, from, to, every_ns, levels, max_samples);
            emit(&snaps, json, "mp-query dom");
        }
        "merge" => {
            let logs: Vec<PathBuf> = all_flags(&args, "--logs")
                .into_iter()
                .map(PathBuf::from)
                .collect();
            if logs.is_empty() {
                return Err("mp-query merge: at least one --logs <file> is required".into());
            }
            let out = flag(&args, "--out").ok_or_else(|| missing("out"))?;
            // Eager loader (MAT-5/EVT-5/EVT-8): reads each log's symbols after
            // streaming its events, so legacy schema-1 logs whose symbol frame
            // follows the first event still remap correctly. The streaming
            // loader needs the table up front and fails on those logs.
            let loaded = load_logs_merged(&logs).map_err(|e| format!("mp-query merge: {e}"))?;
            let mut writer = mp_core::log::EventLogWriter::open(Path::new(&out))
                .map_err(|e| format!("mp-query merge: open {out}: {e}"))?
                .0;
            writer
                .write_symbols(loaded.symbols.metas())
                .map_err(|e| format!("mp-query merge: write symbols: {e}"))?;
            for ev in loaded.events {
                writer
                    .append(&ev)
                    .map_err(|e| format!("mp-query merge: append: {e}"))?;
            }
            writer
                .flush()
                .map_err(|e| format!("mp-query merge: flush: {e}"))?;
            println!("mp-query merge: {} events -> {out}", loaded.events_read);
        }
        _ => usage(),
    }
    Ok(ExitCode::SUCCESS)
}

fn emit<T: serde::Serialize>(rows: &[T], json: bool, label: &str) {
    if json {
        match serde_json::to_string_pretty(rows) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                eprintln!("{label}: json serialize failed: {e}");
                std::process::exit(1);
            }
        }
    } else {
        for row in rows {
            println!("{}", serde_json::to_string(row).unwrap_or_default());
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(2)
        }
    }
}
