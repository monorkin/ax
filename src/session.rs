//! `ax run`: launch Claude Code as a specific account in this terminal only.
//!
//! Each account gets its own profile directory under `<data_dir>/sessions/`,
//! and claude is launched with `CLAUDE_CONFIG_DIR` pointing at it — the
//! default login in `~/.claude` is never touched. Day-to-day setup
//! (settings, CLAUDE.md, skills, MCP servers) is shared from the default
//! profile; credentials and chat history stay per-profile.

use anyhow::{Context, Result};
use serde_json::json;
use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::fs::symlink;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::account;
use crate::claude;
use crate::fsutil;
use crate::mappings;
use crate::paths;
use crate::store::{self, Account, CredentialsFile, Roster, StoreLock};

const SHARED_ITEMS: [&str; 6] = [
    "settings.json",
    "keybindings.json",
    "CLAUDE.md",
    "skills",
    "commands",
    "agents",
];

const AUTH_ENV_VARS: [&str; 5] = [
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR",
    "CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR",
];

pub fn run(identifier: Option<&str>, claude_args: &[String]) -> Result<()> {
    let roster = Roster::load()?;
    let account = match identifier {
        Some(identifier) => Some(roster.find(identifier)?.clone()),
        None => mapped_account(&roster)?,
    };

    match account {
        Some(account) => run_as(&account, claude_args),
        None => exec_claude_directly(claude_args),
    }
}

fn mapped_account(roster: &Roster) -> Result<Option<Account>> {
    let current_directory = env::current_dir()?;
    if let Some(mapping) = mappings::resolve(&current_directory)? {
        let account = roster
            .find_by_identity(&mapping.email, &mapping.organization_uuid)
            .with_context(|| {
                format!(
                    "this directory is mapped to {}, which is no longer stored",
                    mapping.email
                )
            })?;
        Ok(Some(account.clone()))
    } else {
        Ok(None)
    }
}

fn run_as(account: &Account, claude_args: &[String]) -> Result<()> {
    if is_current_default_login(account)? {
        return exec_claude_directly(claude_args);
    }

    let profile = profile_directory(account);
    {
        let _lock = StoreLock::acquire()?;
        sync_newer_profile_credentials_back(account, &profile)?;
        seed_credentials(account, &profile)?;
        seed_identity(account, &profile)?;
        mirror_mcp_servers(&profile)?;
        share_default_setup(&profile)?;
    }

    let mut command = Command::new("claude");
    command.args(claude_args).env("CLAUDE_CONFIG_DIR", &profile);
    for variable in AUTH_ENV_VARS {
        command.env_remove(variable);
    }
    Err(command.exec()).context("could not launch claude — is it on your PATH?")
}

fn is_current_default_login(account: &Account) -> Result<bool> {
    if paths::claude_config_home_is_chosen() {
        return Ok(false);
    }
    match claude::live_identity()? {
        Some(live) => {
            Ok(live.email == account.email && live.organization_uuid == account.organization_uuid)
        }
        None => Ok(false),
    }
}

fn exec_claude_directly(claude_args: &[String]) -> Result<()> {
    Err(Command::new("claude").args(claude_args).exec())
        .context("could not launch claude — is it on your PATH?")
}

fn profile_directory(account: &Account) -> PathBuf {
    paths::data_dir()
        .join("sessions")
        .join(format!("{}-{}", account.number, slug(&account.email)))
}

fn slug(email: &str) -> String {
    email
        .chars()
        .map(|it| {
            if it.is_ascii_alphanumeric() || matches!(it, '.' | '_' | '-') {
                it
            } else {
                '_'
            }
        })
        .collect()
}

/// Once claude runs inside the profile it rotates the token family in place,
/// leaving the store's copy stale. Copying the newer generation back keeps
/// `ax switch` working with the freshest credentials.
fn sync_newer_profile_credentials_back(account: &Account, profile: &Path) -> Result<()> {
    let path = profile.join(".credentials.json");
    if !path.exists() {
        return Ok(());
    }

    let contents = fs::read_to_string(&path)?;
    if let Ok(profile_credentials) = serde_json::from_str::<CredentialsFile>(&contents) {
        let stored = store::read_credentials(account)?;
        if profile_credentials.oauth.expires_at > stored.oauth.expires_at {
            store::write_credentials(account.number, &profile_credentials)?;
        }
    }
    Ok(())
}

fn seed_credentials(account: &Account, profile: &Path) -> Result<()> {
    let path = profile.join(".credentials.json");
    if path.exists() {
        return Ok(());
    }

    fs::create_dir_all(profile)?;
    fs::set_permissions(profile, fs::Permissions::from_mode(0o700))?;
    let mut credentials = store::read_credentials(account)?;
    account::freshen(account, &mut credentials)?;
    fsutil::write_atomically(&path, &serde_json::to_string_pretty(&credentials)?)
}

fn seed_identity(account: &Account, profile: &Path) -> Result<()> {
    let stored_config = store::read_config(account)?;
    let oauth_account = stored_config
        .get("oauthAccount")
        .context("stored config carries no oauthAccount")?;

    let path = profile.join(".claude.json");
    let mut config = if path.exists() {
        fsutil::read_json(&path)?
    } else {
        json!({})
    };
    config["oauthAccount"] = oauth_account.clone();
    config["hasCompletedOnboarding"] = json!(true);
    if config.get("theme").is_none() {
        config["theme"] = stored_config
            .get("theme")
            .cloned()
            .unwrap_or_else(|| json!("dark"));
    }
    fsutil::write_atomically(&path, &serde_json::to_string_pretty(&config)?)
}

/// User-scope MCP servers are mirrored one-way from the default profile on
/// every launch — manage them there; in-session edits don't persist.
fn mirror_mcp_servers(profile: &Path) -> Result<()> {
    let default_config_path = paths::default_global_config_path();
    if !default_config_path.exists() {
        return Ok(());
    }

    let servers = fsutil::read_json(&default_config_path)?
        .get("mcpServers")
        .cloned()
        .filter(|it| it.as_object().is_some_and(|servers| !servers.is_empty()));

    let path = profile.join(".claude.json");
    let mut config = fsutil::read_json(&path)?;
    match servers {
        Some(servers) => config["mcpServers"] = servers,
        None => {
            config.as_object_mut().unwrap().remove("mcpServers");
        }
    }
    fsutil::write_atomically(&path, &serde_json::to_string_pretty(&config)?)
}

fn share_default_setup(profile: &Path) -> Result<()> {
    for item in SHARED_ITEMS {
        let source = paths::default_claude_config_home().join(item);
        let destination = profile.join(item);
        if source.exists() && !destination.exists() && !destination.is_symlink() {
            symlink(&source, &destination)?;
        }
    }
    Ok(())
}
