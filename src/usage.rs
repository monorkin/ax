//! How full each stored account is — its session, weekly and Fable limits —
//! for `ax account list`, and for a program built on ax to report.
//!
//! The usage endpoint answers 429 when it is asked too often, and an
//! embedding program may already be asking every minute. So every answer is
//! kept, per account, and when asking fails the last one is given instead,
//! with how old it is. A reading is only ever what the endpoint said; ax
//! never makes one up.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::path::PathBuf;

use crate::account;
use crate::claude;
use crate::clock;
use crate::fsutil;
use crate::oauth::{self, Usage, Window};
use crate::paths;
use crate::store::{self, Account, CredentialsFile, Roster, StoreLock};

/// What the endpoint said about one account, and when.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Reading {
    pub usage: Usage,
    /// Seconds since the epoch.
    pub taken_at: i64,
}

pub struct Report {
    pub account: Account,
    /// The account Claude Code is logged in as right now.
    pub active: bool,
    /// The newest reading there is: taken just now, or kept from before.
    pub reading: Option<Reading>,
    /// Why there is no reading from just now, when there isn't.
    pub failed: Option<String>,
}

/// Every stored account, asked now.
pub fn of_every_account() -> Result<Vec<Report>> {
    let roster = Roster::load()?;
    let active = account::active_account_number(&roster)?;
    Ok(roster
        .accounts
        .iter()
        .map(|it| report_on(it, Some(it.number) == active))
        .collect())
}

fn report_on(account: &Account, active: bool) -> Report {
    match read(account, active) {
        Ok(reading) => Report { account: account.clone(), active, reading: Some(reading), failed: None },
        Err(error) => Report {
            account: account.clone(),
            active,
            reading: kept().remove(&account.number.to_string()),
            failed: Some(format!("{error:#}")),
        },
    }
}

/// The live login's credentials for the account in use — Claude Code
/// rotates them in place, so the store's copy may be spent — and the
/// store's, freshened, for the rest.
fn read(account: &Account, active: bool) -> Result<Reading> {
    let credentials = if active {
        claude::live_credentials()?.context("no live credentials to ask with")?
    } else {
        freshened_credentials(account)?
    };
    let usage = oauth::fetch_usage(&credentials.oauth.access_token)?;
    Ok(remember(account.number, usage))
}

pub(crate) fn freshened_credentials(account: &Account) -> Result<CredentialsFile> {
    let _lock = StoreLock::acquire()?;
    let mut credentials = store::read_credentials(account)?;
    account::freshen(account, &mut credentials)?;
    Ok(credentials)
}

/// Keeps a reading the endpoint gave, for when it next says no. Losing it
/// costs a stale number, so a failure to keep it is not a failure here.
pub fn remember(number: u32, usage: Usage) -> Reading {
    let reading = Reading { usage, taken_at: clock::now_seconds() };
    let mut all = kept();
    all.insert(number.to_string(), reading.clone());
    let _ = fsutil::write_json_atomically(&kept_path(), &all);
    reading
}

fn kept() -> BTreeMap<String, Reading> {
    fsutil::read_json(&kept_path())
        .ok()
        .and_then(|it| serde_json::from_value(it).ok())
        .unwrap_or_default()
}

fn kept_path() -> PathBuf {
    paths::data_dir().join("usage.json")
}

/// The three limits, as they are shown, in this order.
pub fn limits_of(usage: &Usage) -> [(&'static str, Option<&Window>); 3] {
    [("session", usage.five_hour.as_ref()), ("week", usage.seven_day.as_ref()), ("Fable", usage.fable.as_ref())]
}

/// Every account in a few lines of plain text, for a person or a model
/// reading it: which one is in use, each limit and when it resets, and how
/// old the numbers are when they couldn't be had just now.
pub fn described(reports: &[Report], now: i64) -> String {
    let mut lines = Vec::new();
    for report in reports {
        let in_use = if report.active { ", in use" } else { "" };
        let mut line = format!("{} ({}){in_use}: ", report.account.email, name_of(&report.account));
        match &report.reading {
            Some(reading) => {
                let limits: Vec<String> = limits_of(&reading.usage)
                    .iter()
                    .map(|(name, window)| match window {
                        Some(window) => format!("{name} {:.0}%{}", window.utilization, resets(window, now, " (resets in ", ")")),
                        None => format!("{name} not reported"),
                    })
                    .collect();
                line.push_str(&limits.join(", "));
                if report.failed.is_some() {
                    line.push_str(&format!("; as of {} ago, the endpoint didn't answer just now", clock::span(now - reading.taken_at)));
                }
            }
            None => line.push_str(&format!("unknown; {}", report.failed.as_deref().unwrap_or("no reading"))),
        }
        lines.push(line);
    }
    lines.join("\n")
}

fn name_of(account: &Account) -> &str {
    match (&account.alias, account.organization_name.is_empty()) {
        (Some(alias), _) => alias,
        (None, true) => "personal",
        (None, false) => &account.organization_name,
    }
}

fn resets(window: &Window, now: i64, before: &str, after: &str) -> String {
    match window.resets_at.as_deref().and_then(clock::epoch_seconds_of) {
        Some(at) => format!("{before}{}{after}", clock::span(at - now)),
        None => String::new(),
    }
}

/// How the bars are drawn: in colour on a terminal, in two weights of line
/// anywhere else.
pub struct Bars {
    pub coloured: bool,
    pub width: usize,
}

const BAR: char = '━';
const BAR_EMPTY_UNCOLOURED: char = '─';
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RED: &str = "\x1b[31m";
const DIM: &str = "\x1b[2m";
const PLAIN: &str = "\x1b[0m";

impl Bars {
    pub fn for_stdout() -> Bars {
        let coloured = std::io::stdout().is_terminal()
            && std::env::var_os("NO_COLOR").is_none()
            && std::env::var("TERM").is_ok_and(|it| it != "dumb");
        Bars { coloured, width: 30 }
    }

    /// One line per limit: its name, a bar filled as far as it is used, the
    /// percentage and when it resets.
    pub fn rows(&self, usage: &Usage, now: i64) -> Vec<String> {
        limits_of(usage)
            .iter()
            .map(|(name, window)| match window {
                Some(window) => format!(
                    "{name:<8} {}  {:>3.0}%{}",
                    self.bar(window.utilization),
                    window.utilization,
                    resets(window, now, "  resets in ", "")
                ),
                None => format!("{name:<8} {}     —", self.bar(0.0)),
            })
            .collect()
    }

    fn bar(&self, percent: f64) -> String {
        let filled = ((percent.clamp(0.0, 100.0) / 100.0) * self.width as f64).round() as usize;
        let empty = self.width - filled;
        if self.coloured {
            let colour = if percent >= 90.0 {
                RED
            } else if percent >= 70.0 {
                YELLOW
            } else {
                GREEN
            };
            format!("{colour}{}{DIM}{}{PLAIN}", BAR.to_string().repeat(filled), BAR.to_string().repeat(empty))
        } else {
            format!("{}{}", BAR.to_string().repeat(filled), BAR_EMPTY_UNCOLOURED.to_string().repeat(empty))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_790_094_000; // 2026-09-22T16:20:00Z

    fn window(utilization: f64, resets_at: Option<&str>) -> Option<Window> {
        Some(Window { utilization, resets_at: resets_at.map(String::from) })
    }

    fn usage() -> Usage {
        Usage {
            five_hour: window(42.0, Some("2026-09-22T19:32:00+00:00")),
            seven_day: window(95.0, Some("2026-09-24T20:20:00Z")),
            fable: None,
        }
    }

    #[test]
    fn a_bar_is_filled_as_far_as_the_limit_is_used() {
        let plain = Bars { coloured: false, width: 10 };
        let rows = plain.rows(&usage(), NOW);
        assert_eq!(rows[0], "session  ━━━━──────   42%  resets in 3h 12m");
        assert_eq!(rows[1], "week     ━━━━━━━━━━   95%  resets in 2d 4h");
        assert_eq!(rows[2], "Fable    ──────────     —", "a limit the account doesn't report is shown empty");

        let coloured = Bars { coloured: true, width: 10 };
        let rows = coloured.rows(&usage(), NOW);
        assert!(rows[0].contains(&format!("{GREEN}━━━━{DIM}━━━━━━{PLAIN}")), "one line, the used part in colour");
        assert!(rows[1].contains(RED), "past 90% is red");
    }

    #[test]
    fn every_account_is_described_in_a_line_with_how_old_its_numbers_are() {
        let account = |number: u32, email: &str, alias: Option<&str>| Account {
            number,
            email: email.to_string(),
            uuid: String::new(),
            organization_uuid: String::new(),
            organization_name: "37signals".to_string(),
            alias: alias.map(String::from),
            added: String::new(),
        };
        let reports = [
            Report { account: account(1, "anna@example.com", Some("work")), active: true, reading: Some(Reading { usage: usage(), taken_at: NOW }), failed: None },
            Report {
                account: account(2, "anna2@example.com", None),
                active: false,
                reading: Some(Reading { usage: usage(), taken_at: NOW - 12 * 60 }),
                failed: Some("http status: 429".to_string()),
            },
            Report { account: account(3, "anna3@example.com", None), active: false, reading: None, failed: Some("http status: 429".to_string()) },
        ];

        assert_eq!(
            described(&reports, NOW),
            "anna@example.com (work), in use: session 42% (resets in 3h 12m), week 95% (resets in 2d 4h), Fable not reported\n\
             anna2@example.com (37signals): session 42% (resets in 3h 12m), week 95% (resets in 2d 4h), Fable not reported; as of 12m ago, the endpoint didn't answer just now\n\
             anna3@example.com (37signals): unknown; http status: 429"
        );
    }
}
