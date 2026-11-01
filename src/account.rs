//! The account commands: adding, listing, aliasing, switching, and mapping.

use anyhow::{Context, Result, bail};
use serde_json::json;
use std::fs;
use std::path::Path;

use crate::claude;
use crate::clock;
use crate::codex;
use crate::login::{self, Login};
use crate::mappings;
use crate::oauth;
use crate::paths;
use crate::store::{self, Account, CredentialsFile, Provider, Roster, StoreLock};
use crate::usage;

const FRESHEN_BUFFER_MS: i64 = 10 * 60 * 1000;

pub fn add(token: Option<String>, email: Option<String>, alias: Option<String>) -> Result<()> {
    let _lock = StoreLock::acquire()?;
    if let Some(token) = token {
        add_from_token(&token, email, alias)
    } else {
        add_from_live_login(alias)
    }
}

pub fn add_codex(alias: Option<String>) -> Result<()> {
    let _lock = StoreLock::acquire()?;
    let home = codex::Home::current();
    let auth = home.auth()?.context("no Codex login found — log in with `codex login` first")?;
    let identity = home
        .identity()?
        .context("the Codex login has no ChatGPT tokens — API key logins have no accounts to switch between")?;

    let mut roster = Roster::load()?;
    let number = match roster.find_by_identity(&identity.email, &identity.account_id) {
        Some(existing) => {
            println!("Updating stored credentials for {}.", existing.email);
            existing.number
        }
        None => {
            let number = roster.next_number();
            roster.accounts.push(Account {
                number,
                provider: Provider::Codex,
                email: identity.email.clone(),
                uuid: identity.user_id,
                organization_uuid: identity.account_id,
                organization_name: format!("ChatGPT {}", identity.plan),
                alias: None,
                added: clock::timestamp(),
            });
            println!("Added account {number}: {}.", identity.email);
            number
        }
    };

    store::write_codex_auth(number, &auth)?;
    apply_alias(&mut roster, number, alias)?;
    roster.save()
}

pub fn list() -> Result<()> {
    let roster = Roster::load()?;
    if roster.accounts.is_empty() {
        println!("No accounts yet — log into Claude Code and run `{} account add claude`, or into Codex and run `account add codex`.", paths::invoked_as());
        return Ok(());
    }

    let bars = usage::Bars::for_stdout();
    let now = clock::now_seconds();
    for report in usage::of_every_account()? {
        let account = &report.account;
        let marker = if report.active { "*" } else { " " };
        let alias = match &account.alias {
            Some(alias) => format!(" ({alias})"),
            None => String::new(),
        };
        println!(
            "{marker} {}. {} [{}]{alias}",
            account.number,
            account.email,
            organization_tag(account)
        );
        match (&report.reading, &report.failed) {
            (Some(reading), failed) => {
                for row in bars.rows(&reading.usage, now) {
                    println!("     {row}");
                }
                match (usage::aged(reading, now), failed) {
                    (Some(how_old), None) => println!("     as of {how_old} ago"),
                    (Some(how_old), Some(failed)) => println!("     as of {how_old} ago; the endpoint didn't answer just now: {failed}"),
                    (None, Some(failed)) => println!("     the endpoint didn't answer just now: {failed}"),
                    (None, None) => {}
                }
            }
            (None, Some(failed)) => println!("     usage unknown: {failed}"),
            (None, None) => {}
        }
    }
    Ok(())
}

pub fn set_alias(identifier: &str, alias: &str) -> Result<()> {
    let alias = store::validate_alias(alias)?;
    let _lock = StoreLock::acquire()?;
    let mut roster = Roster::load()?;
    let number = roster.find(identifier)?.number;
    roster.ensure_alias_is_free(&alias, number)?;
    roster.find_mut(identifier)?.alias = Some(alias.clone());
    roster.save()?;
    println!("Account {number} is now '{alias}'.");
    Ok(())
}

pub fn remove(identifier: &str) -> Result<()> {
    let _lock = StoreLock::acquire()?;
    let mut roster = Roster::load()?;
    let account = roster.find(identifier)?.clone();

    store::remove_credentials(account.number)?;
    store::remove_config(account.number)?;
    roster.accounts.retain(|it| it.number != account.number);
    roster.pools.values_mut().for_each(|members| members.retain(|it| *it != account.number));
    roster.pools.retain(|_, members| !members.is_empty());
    if roster.active_account_number == Some(account.number) {
        roster.active_account_number = None;
    }
    roster.save()?;

    println!("Removed account {}: {}.", account.number, account.email);
    Ok(())
}

pub fn switch(identifier: &str) -> Result<()> {
    let _lock = StoreLock::acquire()?;
    let mut roster = Roster::load()?;
    let target = roster.find(identifier)?.clone();

    let default_login = Login::default_of(target.provider);
    if default_login.live_account(&roster)?.is_some_and(|it| it.number == target.number) {
        println!("Already on {}.", target.email);
        return Ok(());
    }

    login::warn_if_live_elsewhere(&target, &roster, &default_login)?;
    default_login.switch_to(&mut roster, &target)?;
    println!("Switched to {}.", target.email);
    Ok(())
}

pub fn map(directory: &Path, identifier: &str) -> Result<()> {
    let roster = Roster::load()?;
    let account = roster.find(identifier)?;
    let key = mappings::set(directory, account.provider, &account.email, &account.organization_uuid)?;
    println!("Mapped {key} to {} for {}.", account.email, account.provider.program());
    Ok(())
}

pub fn unmap(directory: &Path, tool: Option<Provider>) -> Result<()> {
    let mut removed = false;
    for provider in Provider::ALL {
        if tool.is_none_or(|it| it == provider) {
            removed |= mappings::remove(directory, provider)?;
        }
    }
    if removed {
        println!("Mapping removed.");
    } else {
        println!("No mapping for that directory.");
    }
    Ok(())
}

pub fn list_mappings() -> Result<()> {
    let mut any = false;
    for provider in Provider::ALL {
        for (directory, mapping) in mappings::all(provider)? {
            any = true;
            println!("{directory} → {} [{}]", mapping.email, provider.program());
        }
    }
    if !any {
        println!("No mappings yet — add one with `{} map ./dir --to <account>`.", paths::invoked_as());
    }
    Ok(())
}

/// Switch the default Claude Code login to `target`. The caller holds the
/// store lock.
pub fn switch_to(roster: &mut Roster, target: &Account) -> Result<()> {
    Login::default_of(Provider::Claude).switch_to(roster, target)
}

pub fn active_account_number(roster: &Roster) -> Result<Option<u32>> {
    if let Some(live) = claude::live_identity()? {
        Ok(roster
            .find_by_identity(&live.email, &live.organization_uuid)
            .map(|it| it.number))
    } else {
        Ok(None)
    }
}

pub fn freshen(account: &Account, credentials: &mut CredentialsFile) -> Result<()> {
    if credentials.oauth.expires_within(FRESHEN_BUFFER_MS)
        && credentials.oauth.refresh_token.is_some()
    {
        oauth::refresh(&mut credentials.oauth)
            .with_context(|| format!("could not refresh the token for {}", account.email))?;
        store::write_credentials(account.number, credentials)?;
    }
    Ok(())
}

fn add_from_live_login(alias: Option<String>) -> Result<()> {
    let identity = claude::live_identity()?
        .context("no Claude Code login found — log in with `claude` first")?;
    let credentials =
        claude::live_credentials()?.context("no credentials found — log in with `claude` first")?;
    if credentials.oauth.access_token.is_empty() {
        bail!("the live credentials are wiped — log in with `claude` again first");
    }
    let config_text = fs::read_to_string(paths::global_config_path())?;

    let mut roster = Roster::load()?;
    let number = match roster.find_by_identity(&identity.email, &identity.organization_uuid) {
        Some(existing) => {
            println!("Updating stored credentials for {}.", existing.email);
            existing.number
        }
        None => {
            let number = roster.next_number();
            roster.accounts.push(Account {
                number,
                provider: Provider::Claude,
                email: identity.email.clone(),
                uuid: identity.uuid,
                organization_uuid: identity.organization_uuid,
                organization_name: identity.organization_name,
                alias: None,
                added: crate::clock::timestamp(),
            });
            println!("Added account {number}: {}.", identity.email);
            number
        }
    };

    store::write_credentials(number, &credentials)?;
    store::write_config(number, &config_text)?;
    roster.active_account_number = Some(number);
    apply_alias(&mut roster, number, alias)?;
    roster.save()
}

fn add_from_token(token: &str, email: Option<String>, alias: Option<String>) -> Result<()> {
    let mut roster = Roster::load()?;
    let number = roster.next_number();
    let email = email.unwrap_or_else(|| format!("setup-token-{number}@token.local"));
    if roster.find_by_identity(&email, "").is_some() {
        bail!("an account with the email {email} already exists");
    }

    let credentials = CredentialsFile {
        oauth: oauth::credentials_for_setup_token(token)?,
        extra: serde_json::Map::new(),
    };
    let config = json!({
        "oauthAccount": {
            "emailAddress": email,
            "accountUuid": "",
            "organizationUuid": null,
            "organizationName": null,
        }
    });

    store::write_credentials(number, &credentials)?;
    store::write_config(number, &serde_json::to_string_pretty(&config)?)?;
    roster.accounts.push(Account {
        number,
        provider: Provider::Claude,
        email: email.clone(),
        uuid: String::new(),
        organization_uuid: String::new(),
        organization_name: String::new(),
        alias: None,
        added: crate::clock::timestamp(),
    });
    apply_alias(&mut roster, number, alias)?;
    roster.save()?;

    println!("Added account {number}: {email}.");
    Ok(())
}

fn apply_alias(roster: &mut Roster, number: u32, alias: Option<String>) -> Result<()> {
    if let Some(alias) = alias {
        let alias = store::validate_alias(&alias)?;
        roster.ensure_alias_is_free(&alias, number)?;
        roster.find_mut(&number.to_string())?.alias = Some(alias);
    }
    Ok(())
}

fn organization_tag(account: &Account) -> &str {
    if account.organization_name.is_empty() {
        "personal"
    } else {
        &account.organization_name
    }
}
