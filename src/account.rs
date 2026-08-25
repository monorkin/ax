//! The account commands: adding, listing, aliasing, switching, and mapping.

use anyhow::{Context, Result, bail};
use serde_json::json;
use std::fs;
use std::path::Path;

use crate::claude;
use crate::locks::{ConfigLock, CredentialsLock};
use crate::mappings;
use crate::oauth;
use crate::paths;
use crate::store::{self, Account, CredentialsFile, Roster, StoreLock};

const FRESHEN_BUFFER_MS: i64 = 10 * 60 * 1000;

pub fn add(token: Option<String>, email: Option<String>, alias: Option<String>) -> Result<()> {
    let _lock = StoreLock::acquire()?;
    if let Some(token) = token {
        add_from_token(&token, email, alias)
    } else {
        add_from_live_login(alias)
    }
}

pub fn list() -> Result<()> {
    let roster = Roster::load()?;
    if roster.accounts.is_empty() {
        println!("No accounts yet — log into Claude Code and run `cx account add`.");
        return Ok(());
    }

    let active = active_account_number(&roster)?;
    for account in &roster.accounts {
        let marker = if Some(account.number) == active {
            "*"
        } else {
            " "
        };
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

    if let Some(live) = claude::live_identity()?
        && live.email == target.email
        && live.organization_uuid == target.organization_uuid
    {
        println!("Already on {}.", target.email);
        return Ok(());
    }

    switch_to(&mut roster, &target)?;
    println!("Switched to {}.", target.email);
    Ok(())
}

pub fn map(directory: &Path, identifier: &str) -> Result<()> {
    let roster = Roster::load()?;
    let account = roster.find(identifier)?;
    let key = mappings::set(directory, &account.email, &account.organization_uuid)?;
    println!("Mapped {key} to {}.", account.email);
    Ok(())
}

pub fn unmap(directory: &Path) -> Result<()> {
    if mappings::remove(directory)? {
        println!("Mapping removed.");
    } else {
        println!("No mapping for that directory.");
    }
    Ok(())
}

pub fn list_mappings() -> Result<()> {
    let all = mappings::all()?;
    if all.is_empty() {
        println!("No mappings yet — add one with `cx map ./dir --to <account>`.");
        return Ok(());
    }
    for (directory, mapping) in all {
        println!("{directory} → {}", mapping.email);
    }
    Ok(())
}

/// Switch the default Claude Code login to `target`. The caller holds the
/// store lock; this takes Claude Code's own credential and config locks, backs
/// up the outgoing login into its slot, then writes the target's credentials
/// and identity. Any needed token freshening happens before the locks — never
/// hold them across the network.
pub fn switch_to(roster: &mut Roster, target: &Account) -> Result<()> {
    let mut credentials = store::read_credentials(target)?;
    let config = store::read_config(target)?;
    freshen(target, &mut credentials)?;

    let _credentials_lock = CredentialsLock::acquire()?;
    let _config_lock = ConfigLock::acquire()?;

    back_up_outgoing_login(roster)?;
    claude::activate_credentials(&credentials)?;
    claude::splice_oauth_account(&config)?;

    roster.active_account_number = Some(target.number);
    roster.save()
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
                email: identity.email.clone(),
                uuid: identity.uuid,
                organization_uuid: identity.organization_uuid,
                organization_name: identity.organization_name,
                alias: None,
                added: crate::timestamp(),
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
        email: email.clone(),
        uuid: String::new(),
        organization_uuid: String::new(),
        organization_name: String::new(),
        alias: None,
        added: crate::timestamp(),
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

fn back_up_outgoing_login(roster: &Roster) -> Result<()> {
    let Some(identity) = claude::live_identity()? else {
        return Ok(());
    };
    let Some(outgoing) = roster.find_by_identity(&identity.email, &identity.organization_uuid)
    else {
        return Ok(());
    };
    let Some(credentials) = claude::live_credentials()? else {
        return Ok(());
    };
    if credentials.oauth.access_token.is_empty() {
        return Ok(());
    }

    store::write_credentials(outgoing.number, &credentials)?;
    store::write_config(
        outgoing.number,
        &fs::read_to_string(paths::global_config_path())?,
    )
}

fn organization_tag(account: &Account) -> &str {
    if account.organization_name.is_empty() {
        "personal"
    } else {
        &account.organization_name
    }
}
