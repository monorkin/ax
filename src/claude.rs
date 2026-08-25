//! Reads and writes the live Claude Code files: `~/.claude/.credentials.json`
//! and the `oauthAccount` identity inside `~/.claude.json`. Callers hold the
//! locks; this module only moves bytes.

use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::fs;

use crate::fsutil;
use crate::paths;
use crate::store::CredentialsFile;

/// Credential keys that belong to the machine, not the account: MCP server
/// logins and plugin secrets. On activation the live values win — presence
/// and absence — so a slot's older snapshot never clobbers a live MCP login,
/// and a login removed from the machine isn't resurrected. Everything else
/// (`claudeAiOauth`, `trustedDeviceToken`, unrecognized keys) travels with
/// the account.
const MACHINE_SHARED_KEYS: [&str; 5] = [
    "mcpOAuth",
    "mcpOAuthClientConfig",
    "mcpXaaIdp",
    "mcpXaaIdpConfig",
    "pluginSecrets",
];

pub struct Identity {
    pub email: String,
    pub uuid: String,
    pub organization_uuid: String,
    pub organization_name: String,
}

pub fn live_identity() -> Result<Option<Identity>> {
    let path = paths::global_config_path();
    if !path.exists() {
        return Ok(None);
    }

    let config = fsutil::read_json(&path)?;
    let account = &config["oauthAccount"];
    match account["emailAddress"].as_str().filter(|it| !it.is_empty()) {
        Some(email) => Ok(Some(Identity {
            email: email.to_string(),
            uuid: string_at(account, "accountUuid"),
            organization_uuid: string_at(account, "organizationUuid"),
            organization_name: string_at(account, "organizationName"),
        })),
        None => Ok(None),
    }
}

pub fn live_credentials() -> Result<Option<CredentialsFile>> {
    let path = paths::credentials_path();
    if !path.exists() {
        return Ok(None);
    }

    let contents = fs::read_to_string(&path)?;
    if contents.trim().is_empty() {
        return Ok(None);
    }
    let credentials = serde_json::from_str(&contents)
        .with_context(|| format!("could not parse {}", path.display()))?;
    Ok(Some(credentials))
}

pub fn activate_credentials(stored: &CredentialsFile) -> Result<()> {
    let mut activated = stored.clone();
    if let Some(live) = live_credentials()? {
        carry_over_machine_shared_keys(&mut activated, &live);
    }
    write_credentials(&activated)
}

pub fn write_credentials(credentials: &CredentialsFile) -> Result<()> {
    fsutil::write_atomically(
        &paths::credentials_path(),
        &serde_json::to_string_pretty(credentials)?,
    )
}

pub fn splice_oauth_account(stored_config: &Value) -> Result<()> {
    let oauth_account = stored_config
        .get("oauthAccount")
        .context("stored config carries no oauthAccount")?;

    let path = paths::global_config_path();
    if path.exists() {
        let mut config =
            fsutil::read_json(&path).context("refusing to rewrite an unreadable ~/.claude.json")?;
        if !config.is_object() {
            bail!("{} is not a JSON object", path.display());
        }
        config["oauthAccount"] = oauth_account.clone();
        fsutil::write_atomically(&path, &serde_json::to_string_pretty(&config)?)
    } else {
        fsutil::write_atomically(&path, &serde_json::to_string_pretty(stored_config)?)
    }
}

fn carry_over_machine_shared_keys(activated: &mut CredentialsFile, live: &CredentialsFile) {
    for key in MACHINE_SHARED_KEYS {
        if let Some(value) = live.extra.get(key) {
            activated.extra.insert(key.to_string(), value.clone());
        } else {
            activated.extra.remove(key);
        }
    }
}

fn string_at(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or_default().to_string()
}
