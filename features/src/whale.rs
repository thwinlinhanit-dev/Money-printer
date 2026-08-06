//! spec 028 → feature engine (004): aggregate whale net positioning + deltas.
//!
//! The spec 028 `WhalePosition` census is now feature-grade — the RES-4
//! event-study gate has passed (WHL-5: `whale_study` grades spec 029's
//! `liq.est_bands` against the recorded real liq prices, spec 028 Decisions
//! 2026-08-05). Each `WhalePosition` event is a FULL snapshot of one
//! address's position on one symbol (the collector polls top-N + watchlist
//! addresses and emits per address/coin), so the aggregate is an
//! address-keyed upsert: net = Σ signed position notional (size × entry),
//! long positive, short negative.
//!
//! Features (per venue; hyperliquid is the only `WhalePosition` source today):
//!   `whale.net.{venue}`   — aggregate signed notional across all recorded
//!                           addresses on this symbol: a positioning census
//!                           gauge (are whales net long or short this coin?).
//!   `whale.delta.{venue}` — change in the aggregate vs the previous reading
//!                           (whale positioning momentum; None until a second
//!                           reading exists — the same shape as `oi.delta`).
//!
//! Staleness: the census is top-N + watchlist, so an address stops appearing
//! when its position is flattened (`clearinghouseState` omits closed
//! positions — no tombstone event) or when it drops off the leaderboard.
//! A position not refreshed within `stale_after_ns` is therefore evicted
//! (lazily, on the next event) — `whale.net` is the CURRENT census, never a
//! graveyard of dead positions. Deterministic: address-keyed `BTreeMap`
//! (CONV-10), pure function of events (PD-3/FEA-2), eviction driven by
//! `recv_ts_ns` already in the stream (as-of discipline). Fail-closed
//! (CONV-8): a position with a non-finite size or entry is skipped ENTIRELY —
//! a corrupt frame must neither move the aggregate nor extend an address's
//! census membership. `leverage` and `liq_price` are not inputs and ignored.

use crate::engine::TickFeature;
use mp_core::{EventEnvelope, MarketEvent, Venue};
use std::collections::BTreeMap;

/// Default position-refresh window (ns): 10 minutes — ~10 top-N polls at the
/// spec 028 60 s cadence. A whale not seen for 10 consecutive polls is out of
/// the current census.
pub const DEFAULT_WHALE_STALE_NS: i64 = 600_000_000_000;

/// Upsert one address's position, evict positions not refreshed within
/// `stale_after_ns` of `now_ns`, and return the aggregate signed notional.
/// Shared by both features so the census semantics live in ONE place.
fn upsert_and_net(
    positions: &mut BTreeMap<String, (i64, f64, f64)>,
    stale_after_ns: i64,
    now_ns: i64,
    address: &str,
    size: f64,
    entry: f64,
) -> f64 {
    // Upsert first (the refreshed address can never be evicted), then evict.
    positions.insert(address.to_owned(), (now_ns, size, entry));
    let cutoff = now_ns - stale_after_ns;
    positions.retain(|_, &mut (ts, _, _)| ts >= cutoff);
    positions.values().map(|&(_, s, e)| s * e).sum()
}

/// `whale.net.{venue}` — aggregate signed position notional across all
/// recorded addresses for one symbol (see module docs).
pub struct WhaleNet {
    venue: Venue,
    /// address → (last_seen_ns, size, entry). Last poll per address wins
    /// (full-snapshot semantics — a new reading REPLACES the position).
    positions: BTreeMap<String, (i64, f64, f64)>,
    stale_after_ns: i64,
}

impl WhaleNet {
    /// Hyperliquid census, default stale window.
    pub fn new(venue: Venue) -> Self {
        Self::with_stale_after(venue, DEFAULT_WHALE_STALE_NS)
    }
    pub fn with_stale_after(venue: Venue, stale_after_ns: i64) -> Self {
        Self {
            venue,
            positions: BTreeMap::new(),
            stale_after_ns: stale_after_ns.max(0),
        }
    }
}

impl TickFeature for WhaleNet {
    fn id(&self) -> String {
        format!("whale.net.{}", self.venue.slug())
    }
    fn on_event(&mut self, ev: &EventEnvelope) -> Option<f64> {
        if ev.venue != self.venue {
            return None;
        }
        if let MarketEvent::WhalePosition {
            address,
            size,
            entry,
            ..
        } = &ev.body
        {
            // CONV-8 fail-closed: a non-finite size or entry cannot price this
            // position, so the event is dropped whole (no partial state, no
            // census-membership refresh).
            if !size.is_finite() || !entry.is_finite() {
                return None;
            }
            return Some(upsert_and_net(
                &mut self.positions,
                self.stale_after_ns,
                ev.recv_ts_ns,
                address,
                *size,
                *entry,
            ));
        }
        None
    }
}

/// `whale.delta.{venue}` — change in [`WhaleNet`] vs the previous reading.
/// None until a second reading exists (first reading is the baseline, exactly
/// like `oi.delta`); an unchanged reading emits 0.0.
pub struct WhaleNetDelta {
    venue: Venue,
    positions: BTreeMap<String, (i64, f64, f64)>,
    last_net: Option<f64>,
    stale_after_ns: i64,
}

impl WhaleNetDelta {
    /// Hyperliquid census, default stale window.
    pub fn new(venue: Venue) -> Self {
        Self::with_stale_after(venue, DEFAULT_WHALE_STALE_NS)
    }
    pub fn with_stale_after(venue: Venue, stale_after_ns: i64) -> Self {
        Self {
            venue,
            positions: BTreeMap::new(),
            last_net: None,
            stale_after_ns: stale_after_ns.max(0),
        }
    }
}

impl TickFeature for WhaleNetDelta {
    fn id(&self) -> String {
        format!("whale.delta.{}", self.venue.slug())
    }
    fn on_event(&mut self, ev: &EventEnvelope) -> Option<f64> {
        if ev.venue != self.venue {
            return None;
        }
        if let MarketEvent::WhalePosition {
            address,
            size,
            entry,
            ..
        } = &ev.body
        {
            if !size.is_finite() || !entry.is_finite() {
                return None;
            }
            let net = upsert_and_net(
                &mut self.positions,
                self.stale_after_ns,
                ev.recv_ts_ns,
                address,
                *size,
                *entry,
            );
            let delta = self.last_net.map(|prev| net - prev);
            self.last_net = Some(net);
            return delta;
        }
        None
    }
}
