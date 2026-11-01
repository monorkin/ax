//! A login is one place a tool reads its account from: Claude Code's or
//! Codex's default, or a pool's own folder. One account at a time is live in
//! it, and switching swaps which.

use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;

use crate::account;
use crate::claude;
use crate::clock;
use crate::codex;
use crate::locks::{ConfigLock, CredentialsLock};
use crate::oauth::{self, Usage};
use crate::paths;
use crate::session;
use crate::store::{self, Account, Provider, Roster};

const FRESHEN_BUFFER_SECONDS: i64 = 10 * 60;

#[derive(Clone)]
pub struct Login {
    pub provider: Provider,
    /// A pool's folder, or None for the tool's default login.
    pub directory: Option<PathBuf>,
}

impl Login {
    pub fn default_of(provider: Provider) -> Login {
        Login { provider, directory: None }
    }

    pub fn of_pool(pool: &str, provider: Provider) -> Login {
        let directory = paths::data_dir().join("pools").join(pool).join(provider.program());
        Login { provider, directory: Some(directory) }
    }

    pub fn live_account(&self, roster: &Roster) -> Result<Option<Account>> {
        let identity = match self.provider {
            Provider::Claude => self.claude_home().identity()?.map(|it| (it.email, it.organization_uuid)),
            Provider::Codex => self.codex_home().identity()?.map(|it| (it.email, it.account_id)),
        };
        Ok(identity.and_then(|(email, organization)| roster.find_by_identity(&email, &organization).cloned()))
    }

    /// What the endpoint says about whoever is live here, with their live
    /// tokens: the tool rotates those in place, so the store's may be spent.
    pub fn usage(&self) -> Result<Usage> {
        match self.provider {
            Provider::Claude => {
                let credentials = self.claude_home().credentials()?.context("no live credentials to ask with")?;
                oauth::fetch_usage(&credentials.oauth.access_token)
            }
            Provider::Codex => {
                let auth = self.codex_home().auth()?.context("no live credentials to ask with")?;
                codex::fetch_usage(&auth)
            }
        }
    }

    /// Backs up the outgoing account into its slot and makes `target` live.
    /// The caller holds the store lock. Any token freshening happens before
    /// Claude Code's locks — never hold them across the network.
    pub fn switch_to(&self, roster: &mut Roster, target: &Account) -> Result<()> {
        if target.provider != self.provider {
            bail!("{} is not a {} account", target.email, self.provider.program());
        }
        take_back_newest(target, roster)?;
        match self.provider {
            Provider::Claude => self.switch_claude_to(roster, target),
            Provider::Codex => self.switch_codex_to(roster, target),
        }
    }

    pub fn claude_home(&self) -> claude::Home {
        match &self.directory {
            Some(directory) => claude::Home::at(directory),
            None => claude::Home::current(),
        }
    }

    pub fn codex_home(&self) -> codex::Home {
        match &self.directory {
            Some(directory) => codex::Home::at(directory),
            None => codex::Home::current(),
        }
    }

    fn switch_claude_to(&self, roster: &mut Roster, target: &Account) -> Result<()> {
        let mut credentials = store::read_credentials(target)?;
        let config = store::read_config(target)?;
        account::freshen(target, &mut credentials)?;

        let home = self.claude_home();
        let _credentials_lock = CredentialsLock::acquire_in(&home.config_home)?;
        let _config_lock = ConfigLock::acquire_for(&home.global_config)?;

        self.back_up_outgoing_claude_login(roster, &home)?;
        home.activate_credentials(&credentials)?;
        home.splice_oauth_account(&config)?;

        if self.directory.is_none() {
            roster.active_account_number = Some(target.number);
            roster.save()?;
        }
        Ok(())
    }

    fn back_up_outgoing_claude_login(&self, roster: &Roster, home: &claude::Home) -> Result<()> {
        let Some(outgoing) = self.live_account(roster)? else {
            return Ok(());
        };
        let Some(credentials) = home.credentials()? else {
            return Ok(());
        };
        if credentials.oauth.access_token.is_empty() {
            return Ok(());
        }

        store::write_credentials(outgoing.number, &credentials)?;
        store::write_config(outgoing.number, &fs::read_to_string(&home.global_config)?)
    }

    fn switch_codex_to(&self, roster: &Roster, target: &Account) -> Result<()> {
        let mut auth = store::read_codex_auth(target)?;
        freshen_codex(target, &mut auth)?;

        let home = self.codex_home();
        if let (Some(outgoing), Some(live)) = (self.live_account(roster)?, home.auth()?) {
            store::write_codex_auth(outgoing.number, &live)?;
        }
        home.write_auth(&auth)
    }

    fn folder(&self) -> PathBuf {
        match self.provider {
            Provider::Claude => self.claude_home().config_home,
            Provider::Codex => self.codex_home().directory,
        }
    }
}

pub fn freshen_codex(account: &Account, auth: &mut Value) -> Result<()> {
    let expiring = codex::expires_at(auth).is_some_and(|it| it - clock::now_seconds() < FRESHEN_BUFFER_SECONDS);
    if expiring {
        codex::refresh(auth).with_context(|| format!("could not refresh the token for {}", account.email))?;
        store::write_codex_auth(account.number, auth)?;
    }
    Ok(())
}

/// Every login an account could be live in right now.
fn logins_for(provider: Provider, roster: &Roster) -> Vec<Login> {
    let mut logins = vec![Login::default_of(provider)];
    for (pool, numbers) in &roster.pools {
        let has_member = roster.accounts.iter().any(|it| it.provider == provider && numbers.contains(&it.number));
        if has_member {
            logins.push(Login::of_pool(pool, provider));
        }
    }
    logins
}

pub fn where_live(account: &Account, roster: &Roster) -> Result<Option<Login>> {
    for login in logins_for(account.provider, roster) {
        if login.live_account(roster)?.is_some_and(|it| it.number == account.number) {
            return Ok(Some(login));
        }
    }
    Ok(None)
}

pub fn live_elsewhere(account: &Account, roster: &Roster, here: &Login) -> Result<bool> {
    let folder = here.folder();
    for login in logins_for(account.provider, roster) {
        if login.folder() != folder && login.live_account(roster)?.is_some_and(|it| it.number == account.number) {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn warn_if_live_elsewhere(account: &Account, roster: &Roster, here: &Login) -> Result<()> {
    if live_elsewhere(account, roster, here)? {
        eprintln!(
            "warning: {} is also live in another login — refreshing its token in one logs the other out",
            account.email
        );
    }
    Ok(())
}

/// A tool rotates the tokens of a login in place, leaving the store's copy
/// spent. This copies a newer generation back from the account's own profile
/// or a pool login it's live in. The caller holds the store lock.
pub fn take_back_newest(account: &Account, roster: &Roster) -> Result<()> {
    let mut places = vec![session::profile_directory(account)];
    for login in logins_for(account.provider, roster) {
        if login.directory.is_some() && login.live_account(roster)?.is_some_and(|it| it.number == account.number) {
            places.push(login.folder());
        }
    }

    for place in places {
        match account.provider {
            Provider::Claude => {
                if let Ok(Some(copy)) = claude::Home::at(&place).credentials()
                    && let Ok(stored) = store::read_credentials(account)
                    && copy.oauth.expires_at > stored.oauth.expires_at
                {
                    store::write_credentials(account.number, &copy)?;
                }
            }
            Provider::Codex => {
                if let Ok(Some(copy)) = codex::Home::at(&place).auth()
                    && let Ok(stored) = store::read_codex_auth(account)
                    && codex::expires_at(&copy) > codex::expires_at(&stored)
                {
                    store::write_codex_auth(account.number, &copy)?;
                }
            }
        }
    }
    Ok(())
}
