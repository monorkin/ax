//! `ax run`: launch Claude Code or Codex as an account or a pool in this
//! terminal only.
//!
//! Each account gets its own profile directory under `<data_dir>/sessions/`,
//! and each pool one per tool under `<data_dir>/pools/`, and the tool is
//! launched with `CLAUDE_CONFIG_DIR` or `CODEX_HOME` pointing at it — the
//! default login is never touched. Day-to-day setup (settings, CLAUDE.md,
//! skills, MCP servers) is shared from the default profile; credentials and
//! chat history stay per-profile.

use anyhow::{Context, Result, bail};
use serde_json::json;
use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::fs::symlink;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::account;
use crate::auto_switch::{self, Decision, Scope};
use crate::codex;
use crate::fsutil;
use crate::login::{self, Login};
use crate::mappings;
use crate::paths;
use crate::store::{self, Account, CredentialsFile, Provider, Roster, StoreLock};

const SHARED_ITEMS: [&str; 6] = [
    "settings.json",
    "keybindings.json",
    "CLAUDE.md",
    "skills",
    "commands",
    "agents",
];

const SHARED_CODEX_ITEMS: [&str; 7] = [
    "config.toml",
    "AGENTS.md",
    "hooks.json",
    "skills",
    "rules",
    "prompts",
    "plugins",
];

const CODEX_AUTH_ENV_VARS: [&str; 3] = ["OPENAI_API_KEY", "CODEX_API_KEY", "CODEX_ACCESS_TOKEN"];

/// The threshold a launch rotates a pool's login at when nobody has said.
const LAUNCH_THRESHOLD: f64 = 90.0;

const AUTH_ENV_VARS: [&str; 5] = [
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR",
    "CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR",
];

pub fn run(identifier: Option<&str>, claude_args: &[String]) -> Result<()> {
    launch(identifier, None, None, claude_args)
}

pub fn launch(identifier: Option<&str>, pool: Option<&str>, tool: Option<Provider>, args: &[String]) -> Result<()> {
    let wanted = tool.unwrap_or_default();
    if let Some(pool) = pool {
        if identifier.is_some() {
            bail!("--account and --pool can't be used together");
        }
        return run_in_pool(pool, wanted, args);
    }

    let roster = Roster::load()?;
    let account = match identifier {
        Some(identifier) => {
            let account = roster.find(identifier)?.clone();
            if tool.is_some_and(|it| it != account.provider) {
                bail!("{} is not a {} account", account.email, wanted.program());
            }
            Some(account)
        }
        None => mapped_account(&roster, wanted)?,
    };

    match account {
        Some(account) => run_as(&account, &roster, args),
        None => exec_directly(wanted, args),
    }
}

fn run_in_pool(pool: &str, provider: Provider, args: &[String]) -> Result<()> {
    let roster = Roster::load()?;
    if roster.members_of(pool, provider)?.is_empty() {
        bail!("pool {pool} has no {} accounts", provider.program());
    }
    let login = Login::of_pool(pool, provider);
    let folder = login.directory.clone().expect("a pool's login has a folder");
    make_private(&folder)?;

    let live_member = login
        .live_account(&roster)?
        .filter(|it| roster.pool(pool).is_ok_and(|members| members.contains(&it.number)));
    match live_member {
        Some(member) => {
            login::warn_if_live_elsewhere(&member, &roster, &login)?;
            rotate_if_spent(pool, provider);
        }
        None => {
            let member = auto_switch::emptiest_member(pool, provider)?
                .with_context(|| format!("every {} account in pool {pool} is live in another login", provider.program()))?;
            if provider == Provider::Claude && !folder.join(".claude.json").exists() {
                fsutil::write_atomically(&folder.join(".claude.json"), "{}")?;
            }
            let _lock = StoreLock::acquire()?;
            login.switch_to(&mut Roster::load()?, &member)?;
        }
    }

    let _lock = StoreLock::acquire()?;
    let live = login.live_account(&Roster::load()?)?.context("the pool's login names no stored account")?;
    match provider {
        Provider::Claude => prepare_claude_profile(&live, &folder)?,
        Provider::Codex => share_default_codex_setup(&folder)?,
    }
    drop(_lock);
    exec_with_profile(provider, &folder, args)
}

/// A launch that can't check usage still launches: the login it has may work.
fn rotate_if_spent(pool: &str, provider: Provider) {
    let rotated = match auto_switch::decide_in(LAUNCH_THRESHOLD, &Scope::of_pool(pool, provider)) {
        Ok(Decision::Switch(switch)) => switch.carry_out().map(|it| eprintln!("{it}")),
        Ok(Decision::Stay(_)) => Ok(()),
        Err(error) => Err(error),
    };
    if let Err(error) = rotated {
        eprintln!("warning: could not check pool {pool}: {error:#}");
    }
}

fn mapped_account(roster: &Roster, provider: Provider) -> Result<Option<Account>> {
    let current_directory = env::current_dir()?;
    if let Some(mapping) = mappings::resolve(&current_directory, provider)? {
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

fn run_as(account: &Account, roster: &Roster, args: &[String]) -> Result<()> {
    if is_current_default_login(account, roster)? {
        return exec_directly(account.provider, args);
    }
    login::warn_if_live_elsewhere(account, roster, &profile_login(account))?;

    let profile = profile_directory(account);
    {
        let _lock = StoreLock::acquire()?;
        login::take_back_newest(account, roster)?;
        match account.provider {
            Provider::Claude => {
                seed_credentials(account, &profile)?;
                prepare_claude_profile(account, &profile)?;
            }
            Provider::Codex => {
                seed_codex_auth(account, &profile)?;
                share_default_codex_setup(&profile)?;
            }
        }
    }
    exec_with_profile(account.provider, &profile, args)
}

fn profile_login(account: &Account) -> Login {
    Login { provider: account.provider, directory: Some(profile_directory(account)) }
}

fn prepare_claude_profile(account: &Account, profile: &Path) -> Result<()> {
    seed_identity(account, profile)?;
    mirror_mcp_servers(profile)?;
    share_default_setup(profile)
}

fn exec_with_profile(provider: Provider, profile: &Path, args: &[String]) -> Result<()> {
    let mut command = Command::new(provider.program());
    command.args(args);
    match provider {
        Provider::Claude => {
            command.env("CLAUDE_CONFIG_DIR", profile);
            AUTH_ENV_VARS.iter().for_each(|it| { command.env_remove(it); });
        }
        Provider::Codex => {
            command.env("CODEX_HOME", profile);
            CODEX_AUTH_ENV_VARS.iter().for_each(|it| { command.env_remove(it); });
        }
    }
    Err(command.exec()).with_context(|| format!("could not launch {} — is it on your PATH?", provider.program()))
}

fn is_current_default_login(account: &Account, roster: &Roster) -> Result<bool> {
    let chosen = match account.provider {
        Provider::Claude => paths::claude_config_home_is_chosen(),
        Provider::Codex => env::var_os("CODEX_HOME").is_some(),
    };
    if chosen {
        Ok(false)
    } else {
        let live = Login::default_of(account.provider).live_account(roster)?;
        Ok(live.is_some_and(|it| it.number == account.number))
    }
}

fn exec_directly(provider: Provider, args: &[String]) -> Result<()> {
    Err(Command::new(provider.program()).args(args).exec())
        .with_context(|| format!("could not launch {} — is it on your PATH?", provider.program()))
}

pub(crate) fn profile_directory(account: &Account) -> PathBuf {
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

fn make_private(profile: &Path) -> Result<()> {
    fs::create_dir_all(profile)?;
    fs::set_permissions(profile, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// The newest generation of tokens was just taken back into the store, so a
/// profile that's behind it — a pool used the account since — is brought up
/// to date. Its machine-wide logins stay.
fn seed_credentials(account: &Account, profile: &Path) -> Result<()> {
    make_private(profile)?;
    let path = profile.join(".credentials.json");
    if path.exists() {
        let mut existing: CredentialsFile = serde_json::from_str(&fs::read_to_string(&path)?)
            .with_context(|| format!("could not parse {}", path.display()))?;
        let stored = store::read_credentials(account)?;
        if stored.oauth.expires_at > existing.oauth.expires_at {
            existing.oauth = stored.oauth;
            fsutil::write_atomically(&path, &serde_json::to_string_pretty(&existing)?)?;
        }
        return Ok(());
    }

    let mut credentials = store::read_credentials(account)?;
    account::freshen(account, &mut credentials)?;
    fsutil::write_atomically(&path, &serde_json::to_string_pretty(&credentials)?)
}

fn seed_codex_auth(account: &Account, profile: &Path) -> Result<()> {
    make_private(profile)?;
    let home = codex::Home::at(profile);
    let mut stored = store::read_codex_auth(account)?;
    let behind = match home.auth()? {
        Some(existing) => codex::expires_at(&stored) > codex::expires_at(&existing),
        None => true,
    };
    if behind {
        login::freshen_codex(account, &mut stored)?;
        home.write_auth(&stored)?;
    }
    Ok(())
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
    share_items(&SHARED_ITEMS, &paths::default_claude_config_home(), profile)
}

fn share_default_codex_setup(profile: &Path) -> Result<()> {
    share_items(&SHARED_CODEX_ITEMS, &codex::default_directory(), profile)
}

fn share_items(items: &[&str], home: &Path, profile: &Path) -> Result<()> {
    for item in items {
        let source = home.join(item);
        let destination = profile.join(item);
        if source.exists() && !destination.exists() && !destination.is_symlink() {
            symlink(&source, &destination)?;
        }
    }
    Ok(())
}
