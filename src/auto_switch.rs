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
//!
//! Each login is decided on its own: Claude Code's and Codex's default logins,
//! or a pool's, which rotates only among the pool's members. An account live
//! in another login is never a candidate, since two copies of a refresh token
//! log each other out.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::fsutil;
use crate::login::{self, Login};
use crate::paths;
use crate::store::{Account, Provider, Roster, StoreLock};
use crate::usage;

const COOLDOWN_SECONDS: u64 = 300;
const HYSTERESIS_PERCENT: f64 = 10.0;

pub fn run(threshold: f64, interval: u64, once: bool) -> Result<()> {
    watch(threshold, interval, once, None)
}

pub fn watch(threshold: f64, interval: u64, once: bool, pool: Option<&str>) -> Result<()> {
    loop {
        match scopes_for(pool) {
            Ok(scopes) => {
                for scope in &scopes {
                    match tick_in(threshold, scope) {
                        Ok(outcome) => println!("{outcome}"),
                        Err(error) => eprintln!("check failed: {error:#}"),
                    }
                }
            }
            Err(error) if pool.is_some() => return Err(error),
            Err(error) => eprintln!("check failed: {error:#}"),
        }
        if once {
            return Ok(());
        }
        thread::sleep(Duration::from_secs(interval));
    }
}

/// One look at the active Claude Code account's usage, switching if it's
/// time. Returns what happened in a sentence; a program embedding ax can log
/// it.
pub fn tick(threshold: f64) -> Result<String> {
    tick_in(threshold, &Scope::default_login(Provider::Claude))
}

pub fn tick_in(threshold: f64, scope: &Scope) -> Result<String> {
    match decide_in(threshold, scope)? {
        Decision::Stay(why) => Ok(why),
        Decision::Switch(switch) => switch.carry_out(),
    }
}

/// A login to watch, and the pool it may rotate within, if it's kept to one.
pub struct Scope {
    login: Login,
    pool: Option<String>,
}

impl Scope {
    pub fn default_login(provider: Provider) -> Scope {
        Scope { login: Login::default_of(provider), pool: None }
    }

    pub fn of_pool(pool: &str, provider: Provider) -> Scope {
        Scope { login: Login::of_pool(pool, provider), pool: Some(pool.to_string()) }
    }
}

/// Claude Code's default login always; the others only for what's stored.
fn scopes_for(pool: Option<&str>) -> Result<Vec<Scope>> {
    let roster = Roster::load()?;
    let mut scopes = Vec::new();
    for provider in Provider::ALL {
        match pool {
            Some(pool) => {
                if !roster.members_of(pool, provider)?.is_empty() {
                    scopes.push(Scope::of_pool(pool, provider));
                }
            }
            None => {
                if provider == Provider::Claude || roster.accounts.iter().any(|it| it.provider == provider) {
                    scopes.push(Scope::default_login(provider));
                }
            }
        }
    }
    Ok(scopes)
}

/// What a tick would do, without doing it yet: a program embedding ax can
/// wind down what runs on the active account before it carries out a switch.
pub enum Decision {
    Stay(String),
    Switch(Box<Switch>),
}

pub struct Switch {
    login: Login,
    from: Account,
    to: Account,
    utilization: f64,
    headroom: f64,
}

impl Switch {
    pub fn to(&self) -> &str {
        &self.to.email
    }

    pub fn from(&self) -> &str {
        &self.from.email
    }

    /// The roster is read again: a wind-down takes minutes, and whatever
    /// changed meanwhile is kept.
    pub fn carry_out(self) -> Result<String> {
        let _lock = StoreLock::acquire()?;
        let mut roster = Roster::load()?;
        let target = roster.find(&self.to.number.to_string())?.clone();
        self.login.switch_to(&mut roster, &target)?;
        record_switch(&self.login)?;
        Ok(format!(
            "{} hit {:.0}% — switched to {} ({:.0}% headroom)",
            self.from.email, self.utilization, self.to.email, self.headroom
        ))
    }
}

pub fn decide(threshold: f64) -> Result<Decision> {
    decide_in(threshold, &Scope::default_login(Provider::Claude))
}

pub fn decide_in(threshold: f64, scope: &Scope) -> Result<Decision> {
    let roster = Roster::load()?;
    let Some(active) = scope.login.live_account(&roster)? else {
        return Ok(Decision::Stay(match &scope.pool {
            Some(pool) => format!("pool {pool} has no {} login yet — start one with `{} run --pool {pool}`", scope.login.provider.program(), paths::invoked_as()),
            None => "the current login is not a stored account — nothing to watch".to_string(),
        }));
    };

    // Through usage, so this and everything else that asks keep to one
    // pace: a watch that runs every minute doesn't earn a 429 here
    let answer = usage::answer_for(active.number, || scope.login.usage());
    let Some(reading) = answer.reading else {
        let why = answer.failed.unwrap_or_else(|| "no reading yet".to_string());
        return Ok(Decision::Stay(format!("nothing to go on — staying put ({why})")));
    };
    let utilization = reading
        .usage
        .utilization()
        .context("the usage API returned no windows")?;

    if utilization < threshold {
        return Ok(Decision::Stay(format!(
            "{} is at {utilization:.0}% — staying put",
            active.email
        )));
    }

    let fully_spent = utilization >= 100.0;
    if !fully_spent && within_cooldown(&scope.login)? {
        return Ok(Decision::Stay(format!(
            "{} is at {utilization:.0}%, but a switch just happened — waiting out the cooldown",
            active.email
        )));
    }

    let headroom = 100.0 - utilization;
    match best_candidate(&roster, scope, &active, threshold, headroom, fully_spent)? {
        Some((to, headroom)) => Ok(Decision::Switch(Box::new(Switch {
            login: scope.login.clone(),
            from: active,
            to,
            utilization,
            headroom,
        }))),
        None => Ok(Decision::Stay(format!(
            "{} is at {utilization:.0}%, but no other account has meaningfully more headroom",
            active.email
        ))),
    }
}

/// For a pool login nobody has used yet: the member with the most left that
/// isn't live anywhere else.
pub fn emptiest_member(pool: &str, provider: Provider) -> Result<Option<Account>> {
    let roster = Roster::load()?;
    let login = Login::of_pool(pool, provider);
    let mut best: Option<(Account, f64)> = None;
    for member in roster.members_of(pool, provider)? {
        if login::live_elsewhere(member, &roster, &login)? {
            continue;
        }
        let headroom = headroom_of(member, &roster).unwrap_or(-1.0);
        if best.as_ref().is_none_or(|(_, best_headroom)| headroom > *best_headroom) {
            best = Some((member.clone(), headroom));
        }
    }
    Ok(best.map(|(member, _)| member))
}

fn best_candidate(
    roster: &Roster,
    scope: &Scope,
    active: &Account,
    threshold: f64,
    active_headroom: f64,
    active_fully_spent: bool,
) -> Result<Option<(Account, f64)>> {
    let candidates: Vec<&Account> = match &scope.pool {
        Some(pool) => roster.members_of(pool, scope.login.provider)?,
        None => roster.accounts.iter().filter(|it| it.provider == scope.login.provider).collect(),
    };

    let mut best: Option<(Account, f64)> = None;
    for candidate in candidates {
        if candidate.number == active.number || login::live_elsewhere(candidate, roster, &scope.login)? {
            continue;
        }
        let Some(headroom) = headroom_of(candidate, roster) else {
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

fn headroom_of(candidate: &Account, roster: &Roster) -> Option<f64> {
    let answer = usage::answer_for(candidate.number, || usage::read(candidate, roster));
    if let Some(failed) = &answer.failed {
        eprintln!("{}: {failed}", candidate.email);
    }
    answer.reading.and_then(|it| it.usage.utilization()).map(|it| 100.0 - it)
}

#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct State {
    last_switch_at: u64,
}

fn within_cooldown(login: &Login) -> Result<bool> {
    Ok(now() - load_state(login)?.last_switch_at < COOLDOWN_SECONDS)
}

fn record_switch(login: &Login) -> Result<()> {
    fsutil::write_json_atomically(
        &state_path(login),
        &State {
            last_switch_at: now(),
        },
    )
}

fn load_state(login: &Login) -> Result<State> {
    let path = state_path(login);
    if path.exists() {
        let contents = fs::read_to_string(&path)?;
        Ok(serde_json::from_str(&contents).unwrap_or_default())
    } else {
        Ok(State::default())
    }
}

fn state_path(login: &Login) -> PathBuf {
    match &login.directory {
        Some(directory) => directory.with_extension("auto_switch_state.json"),
        None if login.provider == Provider::Claude => paths::data_dir().join("auto_switch_state.json"),
        None => paths::data_dir().join(format!("auto_switch_state-{}.json", login.provider.program())),
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before the Unix epoch")
        .as_secs()
}
