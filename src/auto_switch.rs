//! `ax auto-switch`: watch the active account's usage and rotate to the
//! account with the most quota left before a rate limit hits.
//!
//! The decision, once per tick: read the active account's 5-hour and 7-day
//! utilization; below the threshold, do nothing. At or above it, pick the
//! candidate with the most headroom — but never one that is itself over the
//! threshold, and, unless the active account is fully spent, only one that
//! beats it by a hysteresis margin, so two accounts hovering at the line
//! never ping-pong. A cooldown (persisted, so separate runs share it) spaces
//! proactive switches out; a fully spent account bypasses it.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::account;
use crate::claude;
use crate::fsutil;
use crate::oauth;
use crate::paths;
use crate::store::{self, Account, Roster, StoreLock};

const COOLDOWN_SECONDS: u64 = 300;
const HYSTERESIS_PERCENT: f64 = 10.0;

pub fn run(threshold: f64, interval: u64, once: bool) -> Result<()> {
    loop {
        match tick(threshold) {
            Ok(outcome) => println!("{outcome}"),
            Err(error) => eprintln!("check failed: {error:#}"),
        }
        if once {
            return Ok(());
        }
        thread::sleep(Duration::from_secs(interval));
    }
}

/// One look at the active account's usage, switching if it's time. Returns
/// what happened in a sentence; `run` prints it, and a program embedding ax
/// can log it instead.
pub fn tick(threshold: f64) -> Result<String> {
    let mut roster = Roster::load()?;
    let Some(active_number) = account::active_account_number(&roster)? else {
        return Ok("the current login is not a stored account — nothing to watch".to_string());
    };
    let active = roster.find(&active_number.to_string())?.clone();

    let credentials =
        claude::live_credentials()?.context("no live credentials to check usage with")?;
    let usage = oauth::fetch_usage(&credentials.oauth.access_token)?;
    let utilization = usage
        .utilization()
        .context("the usage API returned no windows")?;

    if utilization < threshold {
        return Ok(format!(
            "{} is at {utilization:.0}% — staying put",
            active.email
        ));
    }

    let fully_spent = utilization >= 100.0;
    if !fully_spent && within_cooldown()? {
        return Ok(format!(
            "{} is at {utilization:.0}%, but a switch just happened — waiting out the cooldown",
            active.email
        ));
    }

    let headroom = 100.0 - utilization;
    match best_candidate(&roster, &active, threshold, headroom, fully_spent)? {
        Some((target, target_headroom)) => {
            let _lock = StoreLock::acquire()?;
            account::switch_to(&mut roster, &target)?;
            record_switch()?;
            Ok(format!(
                "{} hit {utilization:.0}% — switched to {} ({target_headroom:.0}% headroom)",
                active.email, target.email
            ))
        }
        None => Ok(format!(
            "{} is at {utilization:.0}%, but no other account has meaningfully more headroom",
            active.email
        )),
    }
}

fn best_candidate(
    roster: &Roster,
    active: &Account,
    threshold: f64,
    active_headroom: f64,
    active_fully_spent: bool,
) -> Result<Option<(Account, f64)>> {
    let mut best: Option<(Account, f64)> = None;
    for candidate in &roster.accounts {
        if candidate.number == active.number {
            continue;
        }
        let Some(headroom) = headroom_of(candidate) else {
            continue;
        };
        if headroom <= 0.0 || 100.0 - headroom >= threshold {
            continue;
        }
        if !active_fully_spent && headroom - active_headroom < HYSTERESIS_PERCENT {
            continue;
        }
        if best
            .as_ref()
            .is_none_or(|(_, best_headroom)| headroom > *best_headroom)
        {
            best = Some((candidate.clone(), headroom));
        }
    }
    Ok(best)
}

fn headroom_of(candidate: &Account) -> Option<f64> {
    let credentials = freshened_credentials(candidate)?;
    match oauth::fetch_usage(&credentials.oauth.access_token) {
        Ok(usage) => usage.utilization().map(|it| 100.0 - it),
        Err(error) => {
            eprintln!("skipping {}: {error:#}", candidate.email);
            None
        }
    }
}

fn freshened_credentials(candidate: &Account) -> Option<store::CredentialsFile> {
    let _lock = StoreLock::acquire().ok()?;
    let mut credentials = store::read_credentials(candidate).ok()?;
    match account::freshen(candidate, &mut credentials) {
        Ok(()) => Some(credentials),
        Err(error) => {
            eprintln!("skipping {}: {error:#}", candidate.email);
            None
        }
    }
}

#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct State {
    last_switch_at: u64,
}

fn within_cooldown() -> Result<bool> {
    Ok(now() - load_state()?.last_switch_at < COOLDOWN_SECONDS)
}

fn record_switch() -> Result<()> {
    fsutil::write_json_atomically(
        &state_path(),
        &State {
            last_switch_at: now(),
        },
    )
}

fn load_state() -> Result<State> {
    let path = state_path();
    if path.exists() {
        let contents = fs::read_to_string(&path)?;
        Ok(serde_json::from_str(&contents).unwrap_or_default())
    } else {
        Ok(State::default())
    }
}

fn state_path() -> PathBuf {
    paths::data_dir().join("auto_switch_state.json")
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before the Unix epoch")
        .as_secs()
}
