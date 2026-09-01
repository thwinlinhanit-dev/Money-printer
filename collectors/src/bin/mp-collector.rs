/// 24/7 live collector.  One process owns one `(venue, symbol)` recording;
/// cross-venue observations must run as separate processes and merge only at
/// replay (spec 024).
///
/// Run:
///   cargo run -p mp-collectors --features live-ws --bin mp-collector -- --symbol BTCUSDT
#[cfg(feature = "live-ws")]
mod inner {
    use mp_collectors::binutil::{self, InstanceLock, PidFile};
    use mp_collectors::ws::{endpoints, WsEndpoint, WsTransport};
    use mp_collectors::{
        Backoff, BackpressurePolicy, BinanceNormalizer, Collector, CollectorConfig, DriveOutcome,
        HyperliquidNormalizer, Normalizer, RateBudget, Staleness, TeeTransport, Transport,
    };
    use mp_core::log::EventLogWriter;
    use mp_core::{
        EventEnvelope, EventProvenance, InstrumentKind, MarketEvent, SnapshotSource, StatusKind,
        SymbolMeta, Venue,
    };
    use serde::Deserialize;
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    /// Seconds a stream may stay silent before the watchdog forces a
    /// reconnect (COL-2). Depth@100ms + markPrice@1s mean anything quiet
    /// longer than this is a dead subscription, not a slow venue.
    const STALE_AFTER_NS: i64 = 15_000_000_000;

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct FileConfig {
        venue: String,
        symbol: String,
        #[serde(default = "default_data_dir")]
        data_dir: String,
        #[serde(default = "default_channel_capacity")]
        channel_capacity: usize,
        #[serde(default)]
        backpressure: Option<String>,
        /// COL-25/27: `"ws"` (default) or `"rest"`. REST mode ingests trades
        /// from `GET /fapi/v1/aggTrades` and drops WS aggTrade frames at the
        /// normalizer — the documented recovery path when fstream silently
        /// drops the trade stream (spec 024 incident 2026-08-04).
        #[serde(default)]
        trade_source: Option<String>,
        /// COL-28: `"ws"` (default) or `"rest"`. REST mode ingests mark price
        /// and funding from `GET /fapi/v1/premiumIndex` and drops WS
        /// markPriceUpdate frames at the normalizer — the recovery path when
        /// fstream silently drops the markPrice stream (spec 024 incident
        /// 2026-08-04). Independent of `trade_source` so either stream can be
        /// restored without disturbing the other.
        #[serde(default)]
        mark_source: Option<String>,
        /// COL-29: `"ws"` (default) or `"rest"`. REST mode ingests
        /// liquidations from `GET /fapi/v1/allForceOrders` (order-id dedup +
        /// update-time resume) and drops WS forceOrder frames at the
        /// normalizer — the recovery path when fstream silently drops the
        /// forceOrder stream (spec 024 incident 2026-08-04; forceOrder had NO
        /// REST fallback before this). Independent of `trade_source`/
        /// `mark_source` so each stream can be restored separately.
        #[serde(default)]
        liq_source: Option<String>,
        /// Egress proxy for the WS connection (spec 024 2026-08-04):
        /// `http://host:port` (HTTP CONNECT) or `socks5://host:port`.  The
        /// `MP_WS_PROXY` env var overrides this when set.  The proxy only
        /// carries bytes — TLS is terminated against the venue, not the proxy.
        #[serde(default)]
        proxy: Option<String>,
        /// Deribit options recorder (spec 031 OPT-7): margin currency, e.g.
        /// "BTC" or "ETH". Used as the record's symbol + instrument universe.
        #[serde(default)]
        currency: Option<String>,
        /// Deribit (spec 031 OPT-7): also subscribe ticker channels (mark IV
        /// + greeks at record). Default off — bounds volume.
        #[serde(default)]
        record_ticker: Option<bool>,
        /// Deribit (spec 031 OPT-7): instrument subscription filter bounds.
        #[serde(default)]
        instrument_filter: Option<DeribitFilterConfig>,
        /// Deribit (spec 031 OPT-3): capture raw frames verbatim pre-parse to
        /// `{data_dir}/raw/deribit/{date}/frames.ndjson`.
        #[serde(default = "default_true")]
        raw_capture: bool,
        /// HIP-3 TradFi-synthetic coins for the hyperliquid venue (spec 030
        /// MAC-1), e.g. ["xyz:XYZ100", "xyz:SP500"]. Recorded through the
        /// existing hyperliquid normalizer with `asset_class: tradfi_synthetic`
        /// metadata (InstrumentKind::TradFiSynthetic).
        #[serde(default)]
        hip3_symbols: Option<Vec<String>>,
        /// Swing focus (spec 035 SWG-1 / spec 036 SLQ-D): `true` = subscribe
        /// ONLY the streams a higher-timeframe (daily/4h) strategy needs —
        /// trades (OHLCV bar derivation), funding/mark/OI, and liquidations —
        /// and DROP the L2 order-book depth stream, the dominant disk
        /// consumer. Swing features must never depend on L2 or tick-tape
        /// input (spec 035 Non-Goals), so the book stream is pure overhead for
        /// a swing-only setup. Defaults `false` so full Phase-0 capture
        /// (incl. `book`) stays the default and the promotion gate is
        /// unaffected unless explicitly opted out.
        #[serde(default)]
        swing_only: Option<bool>,
        /// Free-tier VPS tuning (docs/ZERO_COST_MODE.md): reconnect backoff
        /// base in milliseconds. Default 250ms (COL-1 full-jitter). Free-tier
        /// VPS with shared bandwidth benefits from a slightly higher base to
        /// avoid hammering during transient network blips.
        #[serde(default)]
        backoff_base_ms: Option<u64>,
        /// Free-tier VPS tuning: reconnect backoff cap in milliseconds.
        /// Default 30000ms (30s). Free-tier may need a higher cap to ride out
        /// longer network outages without exhausting retry budget.
        #[serde(default)]
        backoff_cap_ms: Option<u64>,
    }

    /// Deribit instrument filter (spec 031 Decisions: near-expiry subset).
    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct DeribitFilterConfig {
        #[serde(default = "default_max_instruments")]
        max_instruments: usize,
        #[serde(default = "default_expiry_window_days")]
        expiry_window_days: u64,
    }

    fn default_data_dir() -> String {
        "data".to_owned()
    }
    fn default_channel_capacity() -> usize {
        10_000
    }
    fn default_true() -> bool {
        true
    }
    fn default_max_instruments() -> usize {
        200
    }
    fn default_expiry_window_days() -> u64 {
        45
    }

    fn config_from_args(args: &[String]) -> Result<FileConfig, String> {
        if let Some(path) = binutil::flag(args, "--config") {
            let text = std::fs::read_to_string(&path)
                .map_err(|error| format!("read collector config {path}: {error}"))?;
            let config: FileConfig = toml::from_str(&text)
                .map_err(|error| format!("parse collector config {path}: {error}"))?;
            if config.symbol.is_empty() || config.venue.is_empty() || config.channel_capacity == 0 {
                return Err(
                    "collector config requires non-empty venue/symbol and channel_capacity > 0"
                        .into(),
                );
            }
            // MP_WS_PROXY env override applies on the config path too.
            if let Ok(proxy) = std::env::var("MP_WS_PROXY") {
                return Ok(FileConfig {
                    proxy: Some(proxy),
                    ..config
                });
            }
            return Ok(config);
        }
        Ok(FileConfig {
            venue: binutil::flag(args, "--venue").unwrap_or_else(|| "binance".to_string()),
            symbol: binutil::flag(args, "--symbol").unwrap_or_else(|| "BTCUSDT".to_string()),
            data_dir: default_data_dir(),
            channel_capacity: default_channel_capacity(),
            backpressure: None,
            trade_source: binutil::flag(args, "--trade-source"),
            mark_source: binutil::flag(args, "--mark-source"),
            liq_source: binutil::flag(args, "--liq-source"),
            proxy: std::env::var("MP_WS_PROXY").ok(),
            currency: binutil::flag(args, "--currency"),
            record_ticker: None,
            instrument_filter: None,
            raw_capture: default_true(),
            hip3_symbols: binutil::flag(args, "--hip3-symbols").map(|s| {
                s.split(',')
                    .map(|c| c.trim().to_owned())
                    .filter(|c| !c.is_empty())
                    .collect()
            }),
            swing_only: binutil::flag(args, "--swing-only").map(|v| v == "true" || v == "1"),
        })
    }

    /// SIGTERM/Ctrl+C handler per spec 019 COL-19: a dedicated
    /// `tokio::signal::ctrl_c()` future flips this flag; the (synchronous)
    /// poll loop checks it each iteration, flushes, and exits with status 0.
    /// One-shot install; repeated calls return the same flag.
    fn shutdown_flag() -> Arc<AtomicBool> {
        static FLAG: std::sync::OnceLock<Arc<AtomicBool>> = std::sync::OnceLock::new();
        FLAG.get_or_init(|| {
            let flag = Arc::new(AtomicBool::new(false));
            let f = flag.clone();
            std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build();
                if let Ok(rt) = rt {
                    rt.block_on(async move {
                        if tokio::signal::ctrl_c().await.is_ok() {
                            f.store(true, Ordering::SeqCst);
                        }
                    });
                }
            });
            flag
        })
        .clone()
    }

    fn hl_coin(symbol: &str) -> String {
        symbol
            .trim_end_matches("USDT")
            .trim_end_matches("USD")
            .trim_end_matches("PERP")
            .to_string()
    }

    /// Open (append-only, W-6) the verbatim raw-frame capture file for a
    /// connection: `{dir}/{date}/frames.ndjson` (spec 031 OPT-3).
    fn open_raw_capture_file(dir: &Path, symbol: &str) -> io::Result<File> {
        let day = dir.join(binutil::utc_date_str());
        std::fs::create_dir_all(&day)?;
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(day.join(format!("{symbol}_frames.ndjson")))
    }

    /// Hyperliquid channel set for one coin. Full set = trades + snapshot-only
    /// l2Book + activeAssetCtx (mark/funding/OI). HIP-3 TradFi-synthetic coins
    /// get the same channels (spec 030 MAC-1 — zero new code paths). In
    /// `swing_only` mode the L2 order-book stream (`l2Book`) is dropped —
    /// swing features never depend on it (spec 035 Non-Goals / spec 036
    /// SLQ-D); trades + activeAssetCtx cover OHLCV, funding, mark, and OI.
    fn hl_channels(coin: &str, swing_only: bool) -> Vec<String> {
        let trades = format!(
            r#"{{"method":"subscribe","subscription":{{"type":"trades","coin":"{coin}"}}}}"#
        );
        let ctx = format!(
            r#"{{"method":"subscribe","subscription":{{"type":"activeAssetCtx","coin":"{coin}"}}}}"#
        );
        if swing_only {
            vec![trades, ctx]
        } else {
            let book = format!(
                r#"{{"method":"subscribe","subscription":{{"type":"l2Book","coin":"{coin}"}}}}"#
            );
            vec![trades, book, ctx]
        }
    }

    pub(super) fn subscribe_for(
        venue: &str,
        symbol: &str,
        hip3_symbols: &[String],
        swing_only: bool,
    ) -> Vec<String> {
        match venue {
            // Bybit v5 linear perp topics (public, no credentials):
            //   publicTrade.{symbol}  trades
            //   orderbook.50.{symbol}  book (depth 50; the audit gate requires
            //     the `book` stream on every recording — spec 024)
            //   tickers.{symbol}  funding/mark_price/open_interest
            //   allLiquidation.{symbol}  liquidation (COL-29, the real liq
            //     source) — the v5 topic; the legacy `liquidation.` topic is
            //     DEAD (bybit rejects the whole subscribe with "handler not
            //     found" — proven live 2026-08-13), so never regress to it.
            //
            // In `swing_only` mode the `orderbook.50.{symbol}` topic is
            // dropped (swing never needs L2 depth — spec 035 Non-Goals); the
            // rest are kept: publicTrade (OHLCV bars), tickers (funding/mark/
            // OI), allLiquidation (daily liq aggregates).
            "bybit" if swing_only => vec![format!(
                r#"{{"op":"subscribe","args":["publicTrade.{symbol}","tickers.{symbol}","allLiquidation.{symbol}"]}}"#
            )],
            "bybit" => vec![format!(
                r#"{{"op":"subscribe","args":["publicTrade.{symbol}","orderbook.50.{symbol}","tickers.{symbol}","allLiquidation.{symbol}"]}}"#
            )],
            // Binance uses combined-stream URL (streams baked into path) — no
            // SUBSCRIBE frame needed. Empty here on purpose.
            "binance" => vec![],
            "hyperliquid" => {
                let coin = hl_coin(symbol);
                let mut subs = hl_channels(&coin, swing_only);
                for h in hip3_symbols {
                    // HIP-3 coins carry the dex prefix in their venue name
                    // (e.g. "xyz:XYZ100") — used verbatim in the subscription.
                    subs.extend(hl_channels(h, swing_only));
                }
                subs
            }
            "okx" => vec![format!(
                r#"{{"op":"subscribe","args":[{{"channel":"trades","instId":"{symbol}"}}]}}"#
            )],
            // Deribit subscriptions are computed dynamically in `Stream::new`
            // (instrument discovery — spec 031).
            "deribit" => vec![],
            _ => vec![],
        }
    }

    /// Public WS URL for a venue. Binance is special: futures combined-stream
    /// URL with symbol streams embedded (reliable; avoids SUBSCRIBE race on /ws).
    /// `swing_only` drops the `depth@100ms` stream — swing never needs L2 depth
    /// (spec 035 Non-Goals).
    pub(super) fn endpoint_for(
        venue: &str,
        symbol: &str,
        swing_only: bool,
    ) -> Result<String, String> {
        Ok(match venue {
            "bybit" => endpoints::BYBIT_LINEAR.to_string(),
            "binance" => {
                let s = symbol.to_lowercase();
                // depth@100ms = incremental depth update stream (U/u/pu continuity, Spec 020).
                // markPrice@1s carries mark + funding rate. forceOrder = liqs.
                // OI remains REST-only on Binance.
                let depth = format!("{s}@depth@100ms/");
                format!(
                    "{base}?streams={s}@aggTrade/{s}@markPrice@1s/{}{s}@forceOrder",
                    if swing_only { "" } else { &depth },
                    base = endpoints::BINANCE_FUTURES_COMBINED
                )
            }
            "okx" => endpoints::OKX_PUBLIC.to_string(),
            "hyperliquid" => endpoints::HYPERLIQUID.to_string(),
            "deribit" => endpoints::DERIBIT.to_string(),
            other => return Err(format!("unsupported venue: {other}")),
        })
    }

    fn parse_venue(s: &str) -> Result<Venue, String> {
        Ok(match s {
            "bybit" => Venue::Bybit,
            "binance" => Venue::BinanceFutures,
            "okx" => Venue::Okx,
            "hyperliquid" => Venue::Hyperliquid,
            "deribit" => Venue::Deribit,
            other => return Err(format!("unsupported venue: {other}")),
        })
    }

    /// Per-stream construction options (kept small — one process owns one
    /// recording, so most venues use defaults).
    #[derive(Debug, Clone, Default)]
    struct StreamOpts {
        /// HIP-3 TradFi-synthetic coins for the hyperliquid venue (spec 030
        /// MAC-1); recorded with `InstrumentKind::TradFiSynthetic` metadata.
        hip3_symbols: Vec<String>,
        /// Swing focus (spec 035 SWG-1 / spec 036 SLQ-D): drop the L2
        /// order-book depth stream (see `subscribe_for`/`endpoint_for`).
        swing_only: bool,
        /// Deribit margin currency (spec 031 OPT-7), e.g. "BTC".
        deribit_currency: Option<String>,
        /// Deribit instrument subscription filter (OPT-7).
        #[cfg(feature = "live-http")]
        deribit_filter: mp_collectors::deribit::rest::InstrumentFilter,
        /// Deribit: also subscribe ticker channels (OPT-7; default off).
        deribit_record_ticker: bool,
        /// Deribit raw-frame capture dir (OPT-3); `None` = no verbatim capture.
        raw_capture_dir: Option<PathBuf>,
        /// Free-tier VPS tuning: reconnect backoff base/cap in ms (COL-1).
        backoff_base_ms: u64,
        backoff_cap_ms: u64,
    }

    struct Stream {
        name: String,
        symbol: String,
        endpoint: WsEndpoint,
        venue: Venue,
        /// For Binance streams, the symbol name used to seed the book via REST.
        binance_symbol: Option<String>,
        collector: Collector<Box<dyn Normalizer>>,
        transport: Option<Box<dyn Transport>>,
        backoff: Backoff,
        connection_id: u64,
        channel_capacity: usize,
        backpressure: BackpressurePolicy,
        /// COL-2 staleness watchdog (last `recv_ts_ns` of any valid event).
        staleness: Staleness,
        /// Deribit verbatim raw-frame capture dir (spec 031 OPT-3/COL-9).
        raw_capture_dir: Option<PathBuf>,
        /// COL-21 REST rate budget for snapshot reseeds (futures weights/min).
        #[cfg(feature = "live-http")]
        rest_budget: RateBudget,
        /// COL-1/08-04: next `recv_ts_ns` before which a failed depth reseed is
        /// NOT retried (a persistent failure must not busy-spin the loop).
        #[cfg(feature = "live-http")]
        next_reseed_at_ns: i64,
    }

    impl Stream {
        #[allow(clippy::too_many_arguments)]
        fn new(
            name: String,
            venue_str: &str,
            symbol: &str,
            seed: u64,
            channel_capacity: usize,
            backpressure: BackpressurePolicy,
            proxy: Option<String>,
            opts: StreamOpts,
        ) -> Result<Self, String> {
            let backoff_base = opts.backoff_base_ms;
            let backoff_cap = opts.backoff_cap_ms;
            let venue = parse_venue(venue_str)?;
            let url = endpoint_for(venue_str, symbol, opts.swing_only)?;
            let mut subscribe =
                subscribe_for(venue_str, symbol, &opts.hip3_symbols, opts.swing_only);

            // Deribit (spec 031): discover the near-expiry option instrument
            // set via public REST and build the channel list (book + trades +
            // optional ticker). Requires the live-http feature.
            #[cfg(feature = "live-http")]
            if venue_str == "deribit" {
                let currency = opts
                    .deribit_currency
                    .as_deref()
                    .unwrap_or("BTC")
                    .to_uppercase();
                let instruments =
                    mp_collectors::deribit::rest::discover_option_instruments_blocking(
                        &currency,
                        &opts.deribit_filter,
                        binutil::now_ns(),
                    )?;
                if instruments.is_empty() {
                    return Err(format!(
                        "deribit instrument discovery returned no instruments for {currency}"
                    ));
                }
                tracing::info!(
                    venue = %venue_str,
                    currency = %currency,
                    instruments = instruments.len(),
                    "deribit instrument universe discovered"
                );
                let mut channels: Vec<String> = Vec::new();
                for instr in &instruments {
                    channels.push(format!("book.{instr}.10.100ms"));
                    channels.push(format!("trades.{instr}.100ms"));
                    if opts.deribit_record_ticker {
                        channels.push(format!("ticker.{instr}.100ms"));
                    }
                }
                subscribe = vec![serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "public/subscribe",
                    "params": { "channels": channels },
                })
                .to_string()];
            }
            #[cfg(not(feature = "live-http"))]
            if venue_str == "deribit" {
                return Err(
                    "deribit venue requires the live-http feature (instrument discovery)".into(),
                );
            }

            // Binance embeds streams in the URL (empty subscribe is OK);
            // Deribit subscribes via the computed JSON-RPC frame above.
            if subscribe.is_empty() && venue_str != "binance" && venue_str != "deribit" {
                return Err(format!("no subscribe frames for {venue_str}"));
            }

            // Build the normalizer once so HIP-3 symbol seeding (spec 030
            // MAC-1: asset_class = tradfi_synthetic metadata) survives into
            // the collector.
            let mut normalizer = mp_collectors::normalizer_for(venue);
            if venue_str == "hyperliquid" && !opts.hip3_symbols.is_empty() {
                if let Some(hl) = normalizer
                    .as_any_mut()
                    .and_then(|a| a.downcast_mut::<HyperliquidNormalizer>())
                {
                    for h in &opts.hip3_symbols {
                        let coin = h.clone();
                        hl.symbols_mut().intern(Venue::Hyperliquid, &coin, |id| {
                            SymbolMeta::new(
                                id,
                                Venue::Hyperliquid,
                                &coin,
                                "",
                                "",
                                InstrumentKind::TradFiSynthetic,
                                f64::NAN,
                                f64::NAN,
                                f64::NAN,
                            )
                        });
                    }
                }
            }

            // Proactive rotation (spec 024 amendment 2026-08-25): Hyperliquid's
            // edge kills every WS after ~2h48m–2h57m of wall time regardless of
            // traffic (measured Aug 22–25); the surprise close can stall silently
            // for tens of seconds and dirties the day's scorecard with a
            // stale_bursts finding — the sole remaining Phase-0 promotion
            // blocker. Rotate at 2h15m + jitter, comfortably under the observed
            // minimum kill (2h48m), so reconnects are scheduled non-events.
            let max_connection_age = if venue_str == "hyperliquid" {
                Some(Duration::from_secs(2 * 3600 + 15 * 60))
            } else {
                None
            };

            Ok(Self {
                name,
                symbol: symbol.to_owned(),
                endpoint: WsEndpoint::new(url, subscribe)
                    .with_proxy(proxy)
                    .with_max_connection_age(max_connection_age),
                venue,
                binance_symbol: if venue_str == "binance" {
                    Some(symbol.to_string())
                } else {
                    None
                },
                collector: Collector::new(normalizer, CollectorConfig::default()),
                transport: None,
                // COL-1 full-jitter backoff (configurable via backoff_base_ms / backoff_cap_ms)
                backoff: Backoff::new(backoff_base, backoff_cap, seed),
                connection_id: 0,
                channel_capacity,
                backpressure,
                staleness: Staleness::new(STALE_AFTER_NS),
                raw_capture_dir: opts.raw_capture_dir,
                #[cfg(feature = "live-http")]
                rest_budget: RateBudget::binance_futures(binutil::now_ns()),
                #[cfg(feature = "live-http")]
                next_reseed_at_ns: 0,
            })
        }

        fn provenance(
            &self,
            body: &MarketEvent,
            snapshot_source: SnapshotSource,
        ) -> EventProvenance {
            let stream = match body {
                MarketEvent::Trade { .. } | MarketEvent::TradeWithAddr { .. } => "trade",
                MarketEvent::BookDelta { .. } | MarketEvent::BookSnapshot { .. } => "book",
                MarketEvent::Funding { .. } => "funding",
                MarketEvent::MarkPrice { .. } => "mark_price",
                MarketEvent::OpenInterest { .. } => "open_interest",
                MarketEvent::Liquidation { .. } => "liquidation",
                MarketEvent::IndexPrice { .. } => "index_price",
                MarketEvent::Status { .. } => "status",
                MarketEvent::WhalePosition { .. } => "whale_positions",
                MarketEvent::MacroPoint { .. } => "macro",
                MarketEvent::OptionTrade { .. } => "option_trades",
                MarketEvent::OptionBook { .. } => "option_books",
                MarketEvent::OptionTicker { .. } => "option_tickers",
                MarketEvent::NetflowSnapshot { .. } => "netflow",
            };
            EventProvenance {
                stream: stream.to_owned(),
                subscription: self.endpoint.url.clone(),
                connection_id: self.connection_id,
                snapshot_source,
            }
        }

        fn stamp(&self, events: &mut [EventEnvelope], snapshot_source: SnapshotSource) {
            for event in events {
                let source = if matches!(event.body, MarketEvent::BookSnapshot { .. }) {
                    snapshot_source
                } else {
                    SnapshotSource::None
                };
                event.provenance = self.provenance(&event.body, source);
            }
        }

        fn ensure_connected(&mut self, seed_buf: &mut Vec<EventEnvelope>) {
            if self.transport.is_some() {
                return;
            }
            match WsTransport::connect_with_policy(
                self.endpoint.clone(),
                self.channel_capacity,
                self.backpressure,
            ) {
                Ok(t) => {
                    tracing::info!(stream = %self.name, venue = ?self.venue, "connected");
                    self.backoff.reset();
                    self.connection_id = self.connection_id.saturating_add(1);
                    // Deribit (spec 031 OPT-3/COL-9): tee raw frames verbatim
                    // pre-parse for re-normalization after venue/schema drift.
                    let transport: Box<dyn Transport> = if let Some(dir) = &self.raw_capture_dir {
                        match open_raw_capture_file(dir, &self.symbol) {
                            Ok(sink) => Box::new(TeeTransport::new(t, sink)),
                            Err(e) => {
                                tracing::warn!(
                                    stream = %self.name,
                                    error = %e,
                                    "raw capture unavailable; continuing without it"
                                );
                                Box::new(t)
                            }
                        }
                    } else {
                        Box::new(t)
                    };
                    self.transport = Some(transport);

                    // Seed Binance book from REST immediately after WS connect,
                    // BEFORE any depthUpdate messages are processed (COL-22).
                    // Synthetic-seeding from the first delta was removed spec 020:
                    // without a successful REST seed, depth deltas drop until
                    // the reseed loop below fetches one.
                    #[cfg(feature = "live-http")]
                    if let Some(sym) = self.binance_symbol.clone() {
                        let now_ns = mp_collectors::binance::wall_now_ns();
                        let before = seed_buf.len();
                        let norm = self.collector.normalizer_mut();
                        if let Some(bn) = norm
                            .as_any_mut()
                            .and_then(|a| a.downcast_mut::<BinanceNormalizer>())
                        {
                            match mp_collectors::binance::inject_rest_depth_seed_budgeted(
                                bn,
                                &sym,
                                now_ns,
                                seed_buf,
                                Some(&mut self.rest_budget),
                            ) {
                                Ok(()) => {}
                                Err(e) => tracing::warn!(
                                    stream = %self.name,
                                    error = %e,
                                    "REST depth seed failed; depth deltas drop until a reseed succeeds"
                                ),
                            }
                        }
                        self.stamp(&mut seed_buf[before..], SnapshotSource::Rest);
                    }
                }
                Err(e) => {
                    let delay = self.backoff.next_delay_ms();
                    tracing::warn!(
                        stream = %self.name,
                        error = %e,
                        delay_ms = delay,
                        "connect failed"
                    );
                    std::thread::sleep(Duration::from_millis(delay));
                }
            }
        }

        /// COL-2 watchdog check: if this stream produced no valid events inside
        /// `STALE_AFTER_NS`, emit `Status::Stale` and drop the transport so the
        /// loop reconnects. Returns true when it forced a reconnect.
        fn check_stale(&mut self, out: &mut Vec<EventEnvelope>, now_recv_ns: i64) -> bool {
            // Report the ACTUAL observed silence, not just the static threshold:
            // a seconds past the 15s bar (loop stall, no recv loss) and a
            // minutes-long feed outage look identical here until the silence is
            // measured — this is the operator-facing root-cause signal that
            // separates a false-firing watchdog from a real data hole (spec
            // 024/COL-2, postponed diagnostics).
            let silences = self.staleness.stale_silences(now_recv_ns);
            if silences.is_empty() {
                return false;
            }
            let worst_silence_ns = silences
                .iter()
                .map(|(_, s)| *s)
                .max()
                .unwrap_or(STALE_AFTER_NS);
            let topics: Vec<&str> = silences.iter().map(|(t, _)| t.as_str()).collect();
            tracing::warn!(stream = %self.name, symbol = %self.symbol, "stream stale; reconnecting (COL-2)");
            // Reset the observation so we don't emit one Stale per iteration.
            self.staleness.observe(&self.symbol, now_recv_ns);
            let symbol = self
                .collector
                .normalizer()
                .symbols()
                .lookup(self.venue, &self.symbol)
                .unwrap_or(mp_core::SymbolId(0));
            let stale = EventEnvelope::new(
                self.venue,
                symbol,
                now_recv_ns,
                now_recv_ns,
                0,
                MarketEvent::Status {
                    kind: StatusKind::Stale,
                    detail: format!(
                        "no valid event for {} ms (threshold {} ms; {})",
                        worst_silence_ns / 1_000_000,
                        STALE_AFTER_NS / 1_000_000,
                        topics.join(", "),
                    ),
                },
            );
            let provenance = self.provenance(&stale.body, SnapshotSource::None);
            out.push(stale.with_provenance(provenance));
            self.collector.normalizer_mut().reset_books();
            self.transport = None;
            true
        }

        /// Poll once; appends events to `out`. Returns true if work happened.
        /// `now_recv_ns` is the wall clock stamped at the loop edge (the only
        /// place a collector may read the OS clock — recv stamping).
        fn poll(&mut self, out: &mut Vec<EventEnvelope>, now_recv_ns: i64) -> bool {
            self.ensure_connected(out);
            // COL-24: a pu gap asks the driver to re-seed from REST. Do it on
            // this same loop iteration (no sleep inline) so trades keep flowing.
            #[cfg(feature = "live-http")]
            if let Some(sym) = self.binance_symbol.clone() {
                // audit 08-04 (COL-1 spirit): once a reseed fails, don't retry it
                // every ~50ms loop iteration — wait out the backoff window first.
                if now_recv_ns >= self.next_reseed_at_ns {
                    let norm = self.collector.normalizer_mut();
                    if let Some(bn) = norm
                        .as_any_mut()
                        .and_then(|a| a.downcast_mut::<BinanceNormalizer>())
                    {
                        if bn.needs_reseed() {
                            let before = out.len();
                            match mp_collectors::binance::reseed_if_needed(
                                bn,
                                &sym,
                                now_recv_ns, // loop-edge clock, not a fresh read
                                out,
                                Some(&mut self.rest_budget),
                            ) {
                                Ok(true) => {
                                    self.stamp(&mut out[before..], SnapshotSource::Rest);
                                }
                                Ok(false) => {}
                                Err(e) => {
                                    // Bounded retry: schedule the next attempt a
                                    // couple seconds out instead of busy-spinning.
                                    self.next_reseed_at_ns = now_recv_ns + 2_000_000_000;
                                    tracing::warn!(
                                        stream = %self.name,
                                        error = %e,
                                        "depth re-seed failed; book stays desynced until next attempt"
                                    );
                                }
                            }
                        }
                    }
                }
            }
            let before = out.len();
            let (outcome, metrics) = {
                let Some(t) = self.transport.as_mut() else {
                    // Disconnected right now: still tick the staleness watchdog —
                    // a silently dead socket is exactly the case COL-2 covers.
                    self.check_stale(out, now_recv_ns);
                    return !out.is_empty();
                };
                let outcome = self.collector.drive(t.as_mut(), out);
                (outcome, t.take_metrics())
            };
            let new_events = &mut out[before..];
            self.stamp(new_events, SnapshotSource::WebSocket);
            // COL-2: note valid events for the staleness watchdog. The ws
            // transport stamps recv_ts_ns at socket read; fall back to the loop
            // edge when an event has none (REST-injected events use 0).
            for ev in new_events.iter() {
                let ts = if ev.recv_ts_ns > 0 {
                    ev.recv_ts_ns
                } else {
                    now_recv_ns
                };
                self.staleness.observe(&self.symbol, ts);
            }
            // COL-2: stream silent past the threshold ⇒ declare stale, force
            // reconnect (book untrusted from here until a fresh seed).
            if self.check_stale(out, now_recv_ns) {
                return true;
            }
            if metrics.dropped_frames > 0 {
                // Conservatively invalidate the book after *any* frame loss:
                // a dropped depth update is indistinguishable from a dropped
                // trade here, and a stale book is worse than no book (BKP-3).
                self.collector.normalizer_mut().reset_books();
                let symbol = self
                    .collector
                    .normalizer()
                    .symbols()
                    .lookup(self.venue, &self.symbol)
                    .unwrap_or(mp_core::SymbolId(0));
                let now_ns = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos() as i64;
                let status = EventEnvelope::new(
                    self.venue,
                    symbol,
                    now_ns,
                    now_ns,
                    0,
                    MarketEvent::Status {
                        kind: mp_core::StatusKind::BackpressureDrop {
                            dropped: metrics.dropped_frames,
                        },
                        detail: format!("queue_high_water={}", metrics.queue_high_water),
                    },
                );
                let provenance = self.provenance(&status.body, SnapshotSource::None);
                out.push(status.with_provenance(provenance));
            }
            match outcome {
                DriveOutcome::Exhausted => out.len() > before,
                DriveOutcome::Disconnected | DriveOutcome::ParseFailureLimit => {
                    tracing::warn!(stream = %self.name, ?outcome, "reconnecting");
                    self.transport = None;
                    let delay = self.backoff.next_delay_ms();
                    std::thread::sleep(Duration::from_millis(delay));
                    true
                }
            }
        }
    }

    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        let args: Vec<String> = std::env::args().collect();
        // CONV-18: every binary supports --version and --check-config.
        if binutil::has_flag(&args, "--version") {
            binutil::version_exit();
        }
        let config = config_from_args(&args)?;
        if binutil::has_flag(&args, "--check-config") {
            binutil::check_config_exit();
        }
        let venue = config.venue;
        let symbol = config.symbol;
        let backpressure = match config.backpressure.as_deref() {
            Some(value) => BackpressurePolicy::from_toml(value)
                .ok_or_else(|| format!("invalid backpressure policy: {value}"))?,
            None => BackpressurePolicy::default(),
        };
        let trade_source = match config.trade_source.as_deref() {
            None | Some("ws") => "ws",
            Some("rest") => "rest",
            Some(other) => {
                return Err(format!(
                    "invalid trade_source {other:?}: expected \"ws\" or \"rest\" (COL-25)"
                )
                .into())
            }
        };
        let mark_source = match config.mark_source.as_deref() {
            None | Some("ws") => "ws",
            Some("rest") => "rest",
            Some(other) => {
                return Err(format!(
                    "invalid mark_source {other:?}: expected \"ws\" or \"rest\" (COL-28)"
                )
                .into())
            }
        };
        let liq_source = match config.liq_source.as_deref() {
            None | Some("ws") => "ws",
            Some("rest") => "rest",
            Some(other) => {
                return Err(format!(
                    "invalid liq_source {other:?}: expected \"ws\" or \"rest\" (COL-29)"
                )
                .into())
            }
        };
        #[cfg(not(feature = "live-http"))]
        if trade_source == "rest" || mark_source == "rest" || liq_source == "rest" {
            return Err(
                "trade_source=rest/mark_source=rest/liq_source=rest requires the live-http feature (COL-25/28/29)"
                    .into(),
            );
        }

        let _ = rustls::crypto::ring::default_provider().install_default();

        if let Some(ref proxy) = config.proxy {
            tracing::info!(proxy = %proxy, "routing WS through egress proxy (MP_WS_PROXY/config.proxy)");
        }
        // HIP-3 (spec 030 MAC-1) + Deribit (spec 031) stream options.
        let hip3_symbols = config.hip3_symbols.unwrap_or_default();
        if !hip3_symbols.is_empty() && venue != "hyperliquid" {
            return Err("--hip3-symbols requires the hyperliquid venue (spec 030 MAC-1)".into());
        }
        let deribit_currency = config.currency.clone();
        if deribit_currency.is_some() && venue != "deribit" {
            return Err("--currency/currency requires the deribit venue (spec 031 OPT-7)".into());
        }
        let record_ticker = config.record_ticker.unwrap_or(false);
        if record_ticker && venue != "deribit" {
            return Err("record_ticker requires the deribit venue (spec 031 OPT-7)".into());
        }
        let swing_only = config.swing_only.unwrap_or(false);
        if swing_only && venue == "deribit" {
            return Err(
                "swing_only is not meaningful for the deribit venue (options only; spec 035 SWG-1)"
                    .into(),
            );
        }
        if swing_only {
            tracing::info!(
                venue = %venue,
                symbol = %symbol,
                "swing-only mode: L2 order-book depth stream dropped (spec 035 SWG-1; OHLCV from trade tape)"
            );
        }
        #[cfg(feature = "live-http")]
        let deribit_filter = mp_collectors::deribit::rest::InstrumentFilter {
            max_instruments: config
                .instrument_filter
                .as_ref()
                .map(|f| f.max_instruments)
                .unwrap_or(default_max_instruments()),
            expiry_window_days: config
                .instrument_filter
                .as_ref()
                .map(|f| f.expiry_window_days)
                .unwrap_or(default_expiry_window_days()),
        };
        let raw_capture_dir = if venue == "deribit" && config.raw_capture {
            Some(Path::new(&config.data_dir).join("raw").join("deribit"))
        } else {
            None
        };
        let backoff_base = config.backoff_base_ms.unwrap_or(250);
        let backoff_cap = config.backoff_cap_ms.unwrap_or(30_000);
        let mut streams = vec![Stream::new(
            "primary".into(),
            &venue,
            &symbol,
            1,
            config.channel_capacity,
            backpressure,
            config.proxy.clone(),
            StreamOpts {
                hip3_symbols: hip3_symbols.clone(),
                swing_only,
                deribit_currency: deribit_currency.clone(),
                #[cfg(feature = "live-http")]
                deribit_filter,
                deribit_record_ticker: record_ticker,
                raw_capture_dir: raw_capture_dir.clone(),
                backoff_base_ms,
                backoff_cap_ms,
            },
        )?];

        if trade_source == "rest" {
            // COL-27: trades come exclusively from the REST poller below; WS
            // aggTrade frames are dropped at the normalizer so a degraded
            // trade stream can never churn the collector via staleness
            // reconnects (spec 024 incident 2026-08-04).
            let norm = streams[0].collector.normalizer_mut();
            if let Some(bn) = norm
                .as_any_mut()
                .and_then(|a| a.downcast_mut::<BinanceNormalizer>())
            {
                bn.set_suppress_ws_trades(true);
            } else {
                return Err("trade_source=rest requires a Binance normalizer (COL-27)".into());
            }
            tracing::info!(symbol = %symbol, "trade source: REST aggTrades (WS aggTrade suppressed)");
        }

        if mark_source == "rest" {
            // COL-28: mark/funding come exclusively from the REST premiumIndex
            // poller below; WS markPriceUpdate frames are dropped at the
            // normalizer (same rationale as COL-27 — a degraded or restored
            // WS mark stream must never double-record).
            let norm = streams[0].collector.normalizer_mut();
            if let Some(bn) = norm
                .as_any_mut()
                .and_then(|a| a.downcast_mut::<BinanceNormalizer>())
            {
                bn.set_suppress_ws_mark_price(true);
            } else {
                return Err("mark_source=rest requires a Binance normalizer (COL-28)".into());
            }
            tracing::info!(symbol = %symbol, "mark source: REST premiumIndex (WS markPriceUpdate suppressed)");
        }

        // COL-29: Binance allForceOrders is a USER_DATA endpoint, so the REST
        // liq leg is credential-gated. Read the credentials once here; the
        // gate below validates them and the poll loop reuses the bindings
        // (env never changes mid-run — PD-3 reads only at the edge). Plain
        // env reads, so they compile in every feature set; only the poll loop
        // that uses them is live-http-gated.
        let binance_liq_api_key: Option<String> = std::env::var("MP_BINANCE_API_KEY").ok();
        let binance_liq_api_secret: Option<String> = std::env::var("MP_BINANCE_API_SECRET").ok();
        if liq_source == "rest" {
            // COL-29: liquidations come exclusively from the REST allForceOrders
            // poller below; WS forceOrder frames are dropped at the normalizer
            // (same rationale as COL-27/28 — a degraded or restored WS liq
            // stream must never double-record). allForceOrders is a USER_DATA
            // endpoint (404s without credentials while every public fapi
            // endpoint answers — verified 2026-08-13), so the leg is
            // dead-until-creds, never a silent 404 loop: without both env vars
            // the collector refuses to start.
            let Some(api_key) = &binance_liq_api_key else {
                return Err(
                    "liq_source=rest requires MP_BINANCE_API_KEY + MP_BINANCE_API_SECRET: \
                     GET /fapi/v1/allForceOrders is a USER_DATA endpoint, not public market \
                     data (spec 024 COL-29). Wired but dead until credentials exist — \
                     create a read-only futures API key and set both env vars."
                        .into(),
                );
            };
            let Some(api_secret) = &binance_liq_api_secret else {
                return Err(
                    "liq_source=rest requires MP_BINANCE_API_KEY + MP_BINANCE_API_SECRET: \
                     GET /fapi/v1/allForceOrders is a USER_DATA endpoint, not public market \
                     data (spec 024 COL-29). Wired but dead until credentials exist."
                        .into(),
                );
            };
            let norm = streams[0].collector.normalizer_mut();
            if let Some(bn) = norm
                .as_any_mut()
                .and_then(|a| a.downcast_mut::<BinanceNormalizer>())
            {
                bn.set_suppress_ws_liquidations(true);
            } else {
                return Err("liq_source=rest requires a Binance normalizer (COL-29)".into());
            }
            tracing::info!(
                symbol = %symbol,
                "liq source: REST allForceOrders (signed USER_DATA; WS forceOrder suppressed)"
            );
            let _ = (api_key, api_secret);
        }

        let raw_dir = Path::new(&config.data_dir).join("raw");
        std::fs::create_dir_all(&raw_dir)?;
        // Held for process lifetime — prevents dual-writer log corruption.
        let _instance_lock = InstanceLock::acquire(&raw_dir, &format!("{venue}_{symbol}"))?;
        tracing::info!(venue = %venue, symbol = %symbol, "instance lock acquired");
        // COL-18/19: PID file for systemd/monitoring; removed on clean exit.
        let _pid_file = PidFile::write(&raw_dir, &venue)?;
        let shutdown = shutdown_flag();

        let mut current_date = String::new();
        let mut log_writer: Option<EventLogWriter> = None;
        let mut last_symbol_count: usize = 0;
        // Running recv clock of the last appended frame. Appends are made
        // recv-monotonic (spec 024 2026-08-04): REST-injected events (OI
        // polls, depth reseeds) can otherwise regress the clock by their HTTP
        // round-trip, which mp-audit flags as recv_time_reversal and which
        // used to make every recording DIRTY. Reset per log file.
        let mut last_written_recv_ns: i64 = 0;
        #[cfg(feature = "live-http")]
        let mut last_oi_poll = std::time::Instant::now();
        #[cfg(feature = "live-http")]
        let oi_poll_interval = Duration::from_secs(30);
        // COL-25: REST aggTrades poll cadence + fromId watermark (inclusive
        // resume point; dedup skips the overlap). `0` = no watermark yet: the
        // first poll fetches the most recent window, establishing it.
        #[cfg(feature = "live-http")]
        let mut last_trade_poll = std::time::Instant::now();
        #[cfg(feature = "live-http")]
        let trade_poll_interval = Duration::from_secs(2);
        #[cfg(feature = "live-http")]
        let mut last_trade_watermark: u64 = 0;
        // COL-28: REST premiumIndex poll cadence. The WS markPrice@1s stream
        // carries mark+funding every second; REST premiumIndex is weight-1 and
        // the values move slowly (funding rate updates ~8h, mark tracks index
        // closely), so 15s is a faithful degraded cadence that the audit's
        // 120s global max_gap absorbs (trades/depth keep the clock hot).
        #[cfg(feature = "live-http")]
        let mut last_mark_poll = std::time::Instant::now();
        #[cfg(feature = "live-http")]
        let mark_poll_interval = Duration::from_secs(15);
        // COL-29: REST allForceOrders poll cadence. Liquidations are sparse
        // (dozens/hour at most on BTC), so 10s is a faithful cadence; the
        // update-time watermark resumes loss-free across polls and dedups by
        // order id. Weight 20 per request (symbol-scoped endpoint).
        #[cfg(feature = "live-http")]
        let mut last_liq_poll = std::time::Instant::now();
        #[cfg(feature = "live-http")]
        let liq_poll_interval = Duration::from_secs(10);
        #[cfg(feature = "live-http")]
        let mut last_liq_order_id: u64 = 0;
        #[cfg(feature = "live-http")]
        let mut last_liq_time_ns: i64 = 0;

        let heartbeat_path = raw_dir.join(format!("mp-collector-{venue}-{symbol}.heartbeat"));
        let mut last_heartbeat = std::time::Instant::now();
        let heartbeat_interval = Duration::from_secs(15);

        loop {
            let mut event_buffer = Vec::new();
            let mut any = false;

            if last_heartbeat.elapsed() >= heartbeat_interval || current_date.is_empty() {
                last_heartbeat = std::time::Instant::now();
                let ts_sec = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                let content = format!(
                    "ts={ts_sec} pid={} venue={venue} symbol={symbol}\n",
                    std::process::id()
                );
                let _ = std::fs::write(&heartbeat_path, content);
            }

            #[cfg(feature = "live-http")]
            if venue == "binance"
                && (last_oi_poll.elapsed() >= oi_poll_interval || current_date.is_empty())
            {
                last_oi_poll = std::time::Instant::now();
                // COL-21: the OI poll shares the stream's REST budget so
                // snapshot reseeds and OI fetches together respect the venue limit.
                if !streams[0].rest_budget.try_take(binutil::now_ns(), 1.0) {
                    tracing::debug!("OI poll skipped: REST rate budget empty (COL-21)");
                } else if let Ok(oi_body) =
                    mp_collectors::binance::fetch_open_interest_blocking(&symbol)
                {
                    let now_ns = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_nanos() as i64;
                    let sym_id = streams[0]
                        .collector
                        .normalizer()
                        .symbols()
                        .lookup(Venue::BinanceFutures, &symbol)
                        .unwrap_or(mp_core::SymbolId(0));
                    let event = EventEnvelope::new(
                        Venue::BinanceFutures,
                        sym_id,
                        now_ns,
                        now_ns,
                        0,
                        oi_body,
                    );
                    let provenance = streams[0].provenance(&event.body, SnapshotSource::None);
                    event_buffer.push(event.with_provenance(provenance));
                    any = true;
                }
            }

            // COL-25/26: REST aggTrades as the trade source (fstream silently
            // drops the aggTrade WS stream from datacenter egress — spec 024
            // incident 2026-08-04). The fromId watermark resumes loss-free;
            // a jump in ids is surfaced as Status::GapDetected, never hidden.
            #[cfg(feature = "live-http")]
            if venue == "binance"
                && trade_source == "rest"
                && (last_trade_poll.elapsed() >= trade_poll_interval || current_date.is_empty())
            {
                last_trade_poll = std::time::Instant::now();
                if !streams[0].rest_budget.try_take(binutil::now_ns(), 2.0) {
                    tracing::debug!("trade poll skipped: REST rate budget empty (COL-21)");
                } else if let Ok(batch) = mp_collectors::binance::fetch_agg_trades_blocking(
                    &symbol,
                    if last_trade_watermark == 0 {
                        None
                    } else {
                        Some(last_trade_watermark.saturating_add(1))
                    },
                ) {
                    let recv_ns = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_nanos() as i64;
                    let (fresh, missing) = mp_collectors::binance::advance_trade_watermark(
                        &mut last_trade_watermark,
                        batch,
                    );
                    if missing > 0 {
                        tracing::warn!(symbol = %symbol, missing, "REST aggTrades watermark gap (COL-26)");
                        let sym_id = streams[0]
                            .collector
                            .normalizer()
                            .symbols()
                            .lookup(Venue::BinanceFutures, &symbol)
                            .unwrap_or(mp_core::SymbolId(0));
                        let status = EventEnvelope::new(
                            Venue::BinanceFutures,
                            sym_id,
                            recv_ns,
                            recv_ns,
                            0,
                            MarketEvent::Status {
                                kind: StatusKind::GapDetected,
                                detail: format!("rest aggTrades watermark gap: {missing} trades"),
                            },
                        );
                        let provenance = streams[0].provenance(&status.body, SnapshotSource::None);
                        event_buffer.push(status.with_provenance(provenance));
                        any = true;
                    }
                    if !fresh.is_empty() {
                        let before = event_buffer.len();
                        let norm = streams[0].collector.normalizer_mut();
                        if let Some(bn) = norm
                            .as_any_mut()
                            .and_then(|a| a.downcast_mut::<BinanceNormalizer>())
                        {
                            mp_collectors::binance::apply_agg_trades(
                                bn,
                                &symbol,
                                &fresh,
                                recv_ns,
                                &mut event_buffer,
                            );
                            streams[0].stamp(&mut event_buffer[before..], SnapshotSource::None);
                            any = true;
                        }
                    }
                }
            }

            // COL-28: REST premiumIndex as the mark/funding source (fstream
            // silently drops the markPrice WS stream from this egress — spec
            // 024 incident 2026-08-04). Emits the same MarkPrice + Funding
            // bodies the WS branch would, stamped at poll time.
            #[cfg(feature = "live-http")]
            if venue == "binance"
                && mark_source == "rest"
                && (last_mark_poll.elapsed() >= mark_poll_interval || current_date.is_empty())
            {
                last_mark_poll = std::time::Instant::now();
                if !streams[0].rest_budget.try_take(binutil::now_ns(), 1.0) {
                    tracing::debug!("mark poll skipped: REST rate budget empty (COL-21)");
                } else if let Ok(pi) = mp_collectors::binance::fetch_premium_index_blocking(&symbol)
                {
                    let recv_ns = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_nanos() as i64;
                    let before = event_buffer.len();
                    let norm = streams[0].collector.normalizer_mut();
                    if let Some(bn) = norm
                        .as_any_mut()
                        .and_then(|a| a.downcast_mut::<BinanceNormalizer>())
                    {
                        mp_collectors::binance::apply_premium_index(
                            bn,
                            &symbol,
                            &pi,
                            recv_ns,
                            &mut event_buffer,
                        );
                        streams[0].stamp(&mut event_buffer[before..], SnapshotSource::None);
                        any = true;
                    }
                }
            }

            // COL-29: REST allForceOrders as the liquidation source (fstream
            // silently drops the forceOrder WS stream from this egress — spec
            // 024 incident 2026-08-04; previously the one Binance stream with
            // NO fallback). Emits the same Liquidation bodies the WS branch
            // would, stamped at poll time. The update-time watermark resumes
            // loss-free; order-id dedup skips the overlap window.
            #[cfg(feature = "live-http")]
            if venue == "binance"
                && liq_source == "rest"
                && (last_liq_poll.elapsed() >= liq_poll_interval || current_date.is_empty())
            {
                last_liq_poll = std::time::Instant::now();
                if !streams[0].rest_budget.try_take(binutil::now_ns(), 20.0) {
                    tracing::debug!("liq poll skipped: REST rate budget empty (COL-21)");
                } else if let (Some(api_key), Some(api_secret)) =
                    (&binance_liq_api_key, &binance_liq_api_secret)
                {
                    if let Ok(batch) = mp_collectors::binance::fetch_force_orders_blocking(
                        &symbol,
                        // Resume at the last update time seen; the first poll takes
                        // only the recent window (the endpoint defaults to a 7-day
                        // lookback — a live leg should not backfill weeks).
                        if last_liq_time_ns > 0 {
                            Some(last_liq_time_ns)
                        } else {
                            Some(binutil::now_ns() - 3_600_000_000_000)
                        },
                        api_key,
                        api_secret,
                    ) {
                        let recv_ns = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_nanos() as i64;
                        let (fresh, skipped) =
                            mp_collectors::binance::advance_force_order_watermark(
                                &mut last_liq_order_id,
                                batch,
                            );
                        if let Some(last) = fresh.last() {
                            last_liq_time_ns = last.exch_ts_ns;
                        }
                        if skipped > 0 {
                            tracing::debug!(symbol = %symbol, skipped, "allForceOrders overlap dedup");
                        }
                        if !fresh.is_empty() {
                            let before = event_buffer.len();
                            let norm = streams[0].collector.normalizer_mut();
                            if let Some(bn) = norm
                                .as_any_mut()
                                .and_then(|a| a.downcast_mut::<BinanceNormalizer>())
                            {
                                mp_collectors::binance::apply_force_orders(
                                    bn,
                                    &symbol,
                                    &fresh,
                                    recv_ns,
                                    &mut event_buffer,
                                );
                                streams[0].stamp(&mut event_buffer[before..], SnapshotSource::None);
                                any = true;
                            }
                        }
                    }
                }
            }

            // Loop-edge wall clock: recv stamping + watchdog timers only
            // (PD-3: never a decision/feature value).
            let loop_now_ns = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as i64;
            for s in &mut streams {
                if s.poll(&mut event_buffer, loop_now_ns) {
                    any = true;
                }
            }

            if !event_buffer.is_empty() {
                any = true;
                let date = binutil::utc_date_str();
                if date != current_date {
                    if let Some(ref mut w) = log_writer {
                        let _ = w.flush();
                    }
                    let log_path = raw_dir.join(format!("{date}_{venue}_{symbol}.log"));
                    tracing::info!(path = %log_path.display(), "rotating log file");
                    let (w, truncated) = EventLogWriter::open(&log_path)?;
                    if truncated {
                        tracing::warn!(path = %log_path.display(), "recovered torn tail");
                    }
                    log_writer = Some(w);
                    current_date = date;
                    last_symbol_count = 0;
                    last_written_recv_ns = 0;
                }

                if let Some(ref mut w) = log_writer {
                    let symbols = streams[0].collector.normalizer().symbols();
                    if symbols.len() != last_symbol_count {
                        w.write_symbols(symbols.metas())?;
                        last_symbol_count = symbols.len();
                    }
                    // recv-monotonic append boundary (spec 024 2026-08-04):
                    // sort + clamp this batch so the log never regresses.
                    last_written_recv_ns =
                        mp_collectors::monotonicize(&mut event_buffer, last_written_recv_ns);
                    for ev in &event_buffer {
                        w.append(ev)?;
                    }
                    let _ = w.flush();
                }
            }

            // COL-19: graceful shutdown — the current event_buffer is already
            // flushed above; sync_data via the writer's shutdown hook (honours
            // FsyncPolicy::on_sigterm), PID/lock freed by their Drops.
            if shutdown.load(Ordering::SeqCst) {
                tracing::info!(venue = %venue, symbol = %symbol, "SIGTERM/Ctrl+C received; flushing and exiting (COL-19)");
                if let Some(ref mut w) = log_writer {
                    w.sync_on_shutdown()?;
                }
                return Ok(());
            }

            if !any {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

fn main() {
    #[cfg(not(feature = "live-ws"))]
    {
        eprintln!("Error: 'live-ws' feature required.");
        eprintln!(
            "  cargo run -p mp-collectors --features live-ws --bin mp-collector -- --symbol BTCUSDT"
        );
        std::process::exit(1);
    }

    #[cfg(feature = "live-ws")]
    {
        // --trace-file <path>: durable append-only tracing log (COL-29). The
        // watchdog spawns detached (UseShellExecute) so stderr is lost; a
        // file sink keeps the freeze/diagnosis evidence after a respawn.
        // Append-only + per-day path (watchdog names it with the date), so a
        // respawn never erases the previous process's last lines.
        let args: Vec<String> = std::env::args().collect();
        match mp_collectors::binutil::flag(&args, "--trace-file") {
            Some(path) => {
                match mp_collectors::binutil::SharedLogFile::open(std::path::Path::new(&path)) {
                    Ok(sink) => {
                        // Plain-text file sink (LOG-1): ANSI forced off via the
                        // shared builder so trace files stay machine-parseable
                        // (2026-08-15 investigation had to strip ESC codes).
                        tracing::subscriber::set_global_default(
                            mp_collectors::binutil::trace_subscriber(sink),
                        )
                        .expect("tracing already initialized");
                    }
                    Err(e) => {
                        eprintln!(
                            "warning: cannot open --trace-file {path}: {e}; tracing to stderr"
                        );
                        tracing_subscriber::fmt()
                            .with_env_filter(mp_collectors::binutil::tracing_filter())
                            .init();
                    }
                }
            }
            None => {
                tracing_subscriber::fmt()
                    .with_env_filter(mp_collectors::binutil::tracing_filter())
                    .init();
            }
        }
        if let Err(e) = inner::run() {
            tracing::error!(error = %e, "collector failed");
            std::process::exit(1);
        }
    }
}

#[cfg(all(test, feature = "live-ws"))]
mod tests {
    use super::inner::endpoint_for;
    use super::inner::subscribe_for;

    /// COL-29 regression (2026-08-13, proven live): the bybit v5 subscribe
    /// must use the `allLiquidation.` topic — the legacy `liquidation.` topic
    /// is dead and bybit rejects the WHOLE subscribe frame ("error:handler not
    /// found"), which silently starves every bybit stream. The normalizer
    /// accepts both topic names, so this must be pinned at the subscribe
    /// boundary, not the parse boundary.
    #[test]
    fn col_29_bybit_subscribe_uses_all_liquidation_topic() {
        let frames = subscribe_for("bybit", "BTCUSDT", &[], false);
        assert_eq!(frames.len(), 1);
        let f = &frames[0];
        assert!(
            f.contains(r#""allLiquidation.BTCUSDT""#),
            "subscribe must use the live allLiquidation topic, got: {f}"
        );
        assert!(
            !f.contains(r#""liquidation.BTCUSDT""#),
            "subscribe must NOT use the dead liquidation topic, got: {f}"
        );
        for t in [
            "publicTrade.BTCUSDT",
            "orderbook.50.BTCUSDT",
            "tickers.BTCUSDT",
            "allLiquidation.BTCUSDT",
        ] {
            assert!(
                f.contains(&format!("\"{t}\"")),
                "bybit subscribe missing topic {t}: {f}"
            );
        }
    }

    /// Swing-only (spec 035 SWG-1 / spec 036 SLQ-D): dropping the L2
    /// order-book depth stream must NOT break the streams a higher-timeframe
    /// strategy needs (trades, funding/mark via tickers, liq), and must
    /// remove `orderbook.50` from the bybit frame.
    #[test]
    fn swg_1_bybit_subscribe_drops_order_book_keeps_swing_streams() {
        let frames = subscribe_for("bybit", "BTCUSDT", &[], true);
        assert_eq!(frames.len(), 1);
        let f = &frames[0];
        assert!(
            !f.contains("orderbook.50.BTCUSDT"),
            "swing-only bybit subscribe must drop orderbook.50, got: {f}"
        );
        for t in [
            "publicTrade.BTCUSDT",
            "tickers.BTCUSDT",
            "allLiquidation.BTCUSDT",
        ] {
            assert!(
                f.contains(&format!("\"{t}\"")),
                "swing-only bybit subscribe must keep {t}: {f}"
            );
        }
    }

    /// Swing-only hyperliquid: the `l2Book` channel must be dropped while
    /// `trades` and `activeAssetCtx` (mark/funding/OI) are kept.
    #[test]
    fn swg_1_hyperliquid_drops_l2_book_keeps_swing_channels() {
        let full = subscribe_for("hyperliquid", "BTC", &[], false);
        let full_str = full.join("\n");
        assert!(
            full_str.contains(r#""type":"l2Book","coin":"BTC""#),
            "full: {full_str}"
        );
        let swing = subscribe_for("hyperliquid", "BTC", &[], true);
        let swing_str = swing.join("\n");
        assert!(
            !swing_str.contains(r#""type":"l2Book""#),
            "swing-only hyperliquid must drop l2Book, got: {swing_str}"
        );
        assert!(
            swing_str.contains(r#""type":"trades""#),
            "missing trades: {swing_str}"
        );
        assert!(
            swing_str.contains(r#""type":"activeAssetCtx""#),
            "missing activeAssetCtx (mark/funding/OI): {swing_str}"
        );
    }

    /// Swing-only Binance: the combined-stream URL must drop the depth@100ms
    /// stream while keeping aggTrade, markPrice@1s, and forceOrder.
    #[test]
    fn swg_1_binance_endpoint_drops_depth_keeps_swing_streams() {
        let full = endpoint_for("binance", "BTCUSDT", false).unwrap();
        assert!(
            full.contains("depth@100ms"),
            "full URL expected depth@100ms: {full}"
        );
        let swing = endpoint_for("binance", "BTCUSDT", true).unwrap();
        assert!(
            !swing.contains("depth@100ms"),
            "swing-only binance URL must drop depth@100ms: {swing}"
        );
        for t in ["aggTrade", "markPrice@1s", "forceOrder"] {
            assert!(
                swing.contains(t),
                "swing-only binance URL lost {t}: {swing}"
            );
        }
    }
}
