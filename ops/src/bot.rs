//! Telegram bot command surface (OPS-3) — the deterministic core. Parsing,
//! the owner-id allowlist, confirm flows for `/kill` and `/flatten`, command
//! journaling, and the kill-latch writes all live here, clock-injected and
//! testable offline. The Telegram HTTP transport is a thin binary-edge loop
//! that feeds `(user_id, text, now_ns)` in and sends `BotReply.text` back —
//! it contains no decision logic.
//!
//! Safety: `/kill` and `/flatten` write the latch FILE the risk gate reads
//! (RG-10) — never an RPC to oms — and they are the only state-changing
//! commands. There is no command that loosens anything (PD-1: risk-off only).

use crate::latch::{KillLatch, LatchScope};
use mp_core::Venue;

/// The exact command surface (spec 009). Anything else is `Unknown`.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Status,
    Positions,
    Kill(KillScope),
    Flatten,
    Silence { alert_id: String, duration: String },
    Funnel,
    Report,
    Unknown(String),
}

/// `/kill` scope argument.
#[derive(Debug, Clone, PartialEq)]
pub enum KillScope {
    Global,
    Venue(Venue),
    Strategy(String),
}

fn venue_of(s: &str) -> Option<Venue> {
    Some(match s.to_ascii_lowercase().as_str() {
        "bybit" => Venue::Bybit,
        "okx" => Venue::Okx,
        "binance" | "binancefutures" => Venue::BinanceFutures,
        "coinbase" => Venue::Coinbase,
        "kraken" | "krakenfutures" => Venue::KrakenFutures,
        "hyperliquid" => Venue::Hyperliquid,
        _ => return None,
    })
}

/// Parse one message into a [`Command`].
pub fn parse(text: &str) -> Command {
    let mut it = text.split_whitespace();
    match it.next() {
        Some("/status") => Command::Status,
        Some("/positions") => Command::Positions,
        Some("/kill") => match it.next() {
            Some("GLOBAL") => Command::Kill(KillScope::Global),
            Some(arg) => match venue_of(arg) {
                Some(v) => Command::Kill(KillScope::Venue(v)),
                None => Command::Kill(KillScope::Strategy(arg.to_string())),
            },
            None => Command::Unknown("/kill needs a scope".into()),
        },
        Some("/flatten") => Command::Flatten,
        Some("/silence") => match (it.next(), it.next()) {
            (Some(id), Some(dur)) => Command::Silence {
                alert_id: id.to_string(),
                duration: dur.to_string(),
            },
            _ => Command::Unknown("/silence needs <id> <duration>".into()),
        },
        Some("/funnel") => Command::Funnel,
        Some("/report") => Command::Report,
        other => Command::Unknown(other.unwrap_or("").to_string()),
    }
}

/// What the bot answers, plus any latch it decided to write.
#[derive(Debug, Clone)]
pub struct BotReply {
    pub text: String,
    /// A latch to persist via [`Bot::write_latch`] (only /kill and /flatten
    /// after their confirm flow produce one).
    pub latch: Option<KillLatch>,
}

/// A pending confirmation (kill: one confirm; flatten: two).
#[derive(Debug, Clone, PartialEq)]
enum PendingConfirm {
    Kill(KillScope),
    Flatten { confirms_left: u8 },
}

/// The bot state machine. All commands are journaled (OPS-3); the journal is
/// the caller's append-only sink.
pub struct Bot {
    owner_id: i64,
    pending: Option<PendingConfirm>,
    journal: Vec<String>,
}

impl Bot {
    /// Single-owner allowlist: exactly one Telegram user id may command.
    pub fn new(owner_id: i64) -> Self {
        Bot {
            owner_id,
            pending: None,
            journal: Vec::new(),
        }
    }

    /// Every accepted (and every refused) command line, `ts|user|text|verdict`.
    pub fn journal(&self) -> &[String] {
        &self.journal
    }

    fn log(&mut self, ts: i64, user: i64, text: &str, verdict: &str) {
        self.journal.push(format!("{ts}|{user}|{text}|{verdict}"));
    }

    /// Handle one incoming message. Non-owner users are refused (and the
    /// refusal is journaled — an unexpected sender is a signal, not noise).
    pub fn handle(&mut self, user_id: i64, text: &str, now_ns: i64) -> BotReply {
        if user_id != self.owner_id {
            self.log(now_ns, user_id, text, "REFUSED non-owner");
            return BotReply {
                text: "not authorized".into(),
                latch: None,
            };
        }

        // Confirm flow: an outstanding confirm consumes "yes" / anything else.
        if let Some(pending) = self.pending.take() {
            return self.handle_confirm(pending, text, now_ns);
        }

        match parse(text) {
            Command::Kill(scope) => {
                self.log(now_ns, user_id, text, "confirm requested");
                self.pending = Some(PendingConfirm::Kill(scope.clone()));
                BotReply {
                    text: format!("confirm kill of {scope:?}? reply 'yes'"),
                    latch: None,
                }
            }
            Command::Flatten => {
                self.log(now_ns, user_id, text, "double-confirm requested");
                self.pending = Some(PendingConfirm::Flatten { confirms_left: 2 });
                BotReply {
                    text: "GLOBAL kill + reduce-only flatten. Reply 'yes' twice.".into(),
                    latch: None,
                }
            }
            Command::Silence { alert_id, duration } => {
                self.log(now_ns, user_id, text, "silenced");
                BotReply {
                    text: format!("silenced {alert_id} for {duration}"),
                    latch: None,
                }
            }
            Command::Status | Command::Positions | Command::Funnel | Command::Report => {
                // Read-only surfaces: the binary edge fills these from state
                // files; the state machine only journals and acknowledges.
                self.log(now_ns, user_id, text, "ok");
                BotReply {
                    text: format!("{text}: (rendered from state by opsd)"),
                    latch: None,
                }
            }
            Command::Unknown(u) => {
                self.log(now_ns, user_id, text, "unknown");
                BotReply {
                    text: format!("unknown command: {u}"),
                    latch: None,
                }
            }
        }
    }

    fn handle_confirm(&mut self, pending: PendingConfirm, text: &str, now_ns: i64) -> BotReply {
        let yes = text.trim().eq_ignore_ascii_case("yes");
        match pending {
            PendingConfirm::Kill(scope) => {
                if !yes {
                    self.log(now_ns, self.owner_id, text, "kill aborted");
                    return BotReply {
                        text: "kill aborted".into(),
                        latch: None,
                    };
                }
                self.log(now_ns, self.owner_id, text, "kill CONFIRMED");
                let latch = match scope {
                    KillScope::Global => KillLatch::global("telegram /kill GLOBAL", now_ns),
                    KillScope::Venue(v) => KillLatch::new("telegram /kill venue", now_ns)
                        .kill(LatchScope::Venue { venue: v }),
                    KillScope::Strategy(id) => KillLatch::new("telegram /kill strategy", now_ns)
                        .kill(LatchScope::Strategy { id }),
                };
                BotReply {
                    text: "kill latched — the gate rejects on next check (RG-10)".into(),
                    latch: Some(latch),
                }
            }
            PendingConfirm::Flatten { confirms_left } => {
                if !yes {
                    self.log(now_ns, self.owner_id, text, "flatten aborted");
                    return BotReply {
                        text: "flatten aborted".into(),
                        latch: None,
                    };
                }
                if confirms_left > 1 {
                    self.log(now_ns, self.owner_id, text, "flatten confirm 1/2");
                    self.pending = Some(PendingConfirm::Flatten {
                        confirms_left: confirms_left - 1,
                    });
                    return BotReply {
                        text: "confirm again to flatten".into(),
                        latch: None,
                    };
                }
                self.log(now_ns, self.owner_id, text, "flatten CONFIRMED 2/2");
                BotReply {
                    text: "GLOBAL kill latched; reduce-only flatten queued".into(),
                    latch: Some(KillLatch::global("telegram /flatten", now_ns)),
                }
            }
        }
    }

    /// Persist a latch where the risk gate reads it (RG-10). Works with oms
    /// down — it is a file write, not an RPC.
    pub fn write_latch(path: &std::path::Path, latch: &KillLatch) -> std::io::Result<()> {
        let json = latch
            .to_json()
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        std::fs::write(path, json)
    }
}
