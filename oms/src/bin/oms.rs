//! `oms doctor` (EXE-10, offline slice). Spec 007 defers venue-connectivity,
//! clock-skew, and key-permission checks to the live adapter (PD-1). This stub
//! runs every check that needs NO venue: state-machine self-test and optional
//! `risk.toml`/`features.toml` TOML validation. Venue checks are reported as
//! SKIPPED, not assumed green — honesty before coverage.

use std::process::ExitCode;

const USAGE: &str = "usage: oms doctor [--config PATH]";

fn main() -> ExitCode {
    let mut config_path: Option<String> = None;
    let mut args = std::env::args().skip(1).peekable();
    while let Some(a) = args.next() {
        match a.as_str() {
            "doctor" => {}
            "--config" => match args.next() {
                Some(p) => config_path = Some(p),
                None => {
                    eprintln!("{USAGE}");
                    return ExitCode::from(2);
                }
            },
            "--help" | "-h" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            _ => {
                eprintln!("{USAGE}");
                return ExitCode::from(2);
            }
        }
    }

    println!("oms doctor (EXE-10 offline slice)");

    match mp_oms::state_machine_self_test() {
        Ok(()) => println!("[ok] state machine: legal transition graph intact"),
        Err(e) => {
            eprintln!("[FAIL] state machine: {e}");
            return ExitCode::FAILURE;
        }
    }

    if let Some(path) = &config_path {
        match std::fs::read_to_string(path) {
            Err(e) => {
                eprintln!("[FAIL] config unreadable ({path}): {e}");
                return ExitCode::FAILURE;
            }
            Ok(src) => match toml::from_str::<toml::Value>(&src) {
                Ok(_) => println!("[ok] config parses as TOML: {path}"),
                Err(e) => {
                    eprintln!("[FAIL] config TOML parse ({path}): {e}");
                    return ExitCode::FAILURE;
                }
            },
        }
    }

    // Venue connectivity, clock skew vs venue, key permissions (trade-only),
    // live recon polling: all need the venue adapter — deferred (spec 007
    // Decisions). Reported as skipped, never as passed (exit still 0: no check
    // actually failed; the skipped checks are owner-gated).
    println!("[skip] venue connectivity / clock skew / key perms / live recon — requires the live venue adapter (PD-1-deferred)");

    println!("doctor: offline checks passed");
    ExitCode::SUCCESS
}