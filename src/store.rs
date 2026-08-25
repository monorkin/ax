//! The account store under `<data_dir>/`: a roster in `accounts.json`, one
//! credential file per account under `credentials/`, and a verbatim copy of
//! each account's `~/.claude.json` under `configs/`.

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use crate::fsutil;
use crate::oauth::OauthCredentials;
use crate::paths;

#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Roster {
    pub active_account_number: Option<u32>,
    pub accounts: Vec<Account>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub number: u32,
    pub email: String,
    #[serde(default)]
    pub uuid: String,
    #[serde(default)]
    pub organization_uuid: String,
    #[serde(default)]
    pub organization_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    pub added: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct CredentialsFile {
    #[serde(rename = "claudeAiOauth")]
    pub oauth: OauthCredentials,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl Roster {
    pub fn load() -> Result<Roster> {
        let path = roster_path();
        if path.exists() {
            let contents = fs::read_to_string(&path)?;
            serde_json::from_str(&contents)
                .with_context(|| format!("could not parse {}", path.display()))
        } else {
            Ok(Roster::default())
        }
    }

    pub fn save(&self) -> Result<()> {
        fsutil::write_json_atomically(&roster_path(), self)
    }

    pub fn find(&self, identifier: &str) -> Result<&Account> {
        self.lookup(identifier)
            .ok_or_else(|| anyhow!("no account matches '{identifier}' — see `cx account list`"))
    }

    pub fn find_mut(&mut self, identifier: &str) -> Result<&mut Account> {
        let number = self.find(identifier)?.number;
        Ok(self
            .accounts
            .iter_mut()
            .find(|it| it.number == number)
            .unwrap())
    }

    pub fn find_by_identity(&self, email: &str, organization_uuid: &str) -> Option<&Account> {
        self.accounts
            .iter()
            .find(|it| it.email == email && it.organization_uuid == organization_uuid)
    }

    pub fn next_number(&self) -> u32 {
        self.accounts.iter().map(|it| it.number).max().unwrap_or(0) + 1
    }

    pub fn ensure_alias_is_free(&self, alias: &str, owner: u32) -> Result<()> {
        let taken = self
            .accounts
            .iter()
            .any(|it| it.number != owner && it.alias.as_deref() == Some(alias));
        if taken {
            bail!("alias '{alias}' is already taken");
        }
        Ok(())
    }

    fn lookup(&self, identifier: &str) -> Option<&Account> {
        if let Ok(number) = identifier.parse::<u32>() {
            self.accounts.iter().find(|it| it.number == number)
        } else {
            self.accounts
                .iter()
                .find(|it| it.alias.as_deref() == Some(identifier))
                .or_else(|| self.accounts.iter().find(|it| it.email == identifier))
        }
    }
}

pub fn validate_alias(alias: &str) -> Result<String> {
    let normalized = alias.trim().to_lowercase();
    if normalized.is_empty() {
        bail!("alias cannot be empty");
    }
    if normalized.chars().all(|it| it.is_ascii_digit()) {
        bail!("alias cannot be purely numeric — numbers identify account slots");
    }
    if normalized.starts_with('-') {
        bail!("alias cannot start with '-' — it would be read as a flag");
    }
    let valid = normalized
        .chars()
        .all(|it| it.is_ascii_alphanumeric() || matches!(it, '-' | '_' | '.'));
    if !valid {
        bail!("alias may only contain letters, digits, '-', '_', and '.'");
    }
    Ok(normalized)
}

pub fn read_credentials(account: &Account) -> Result<CredentialsFile> {
    let path = credentials_path(account.number);
    let contents = fs::read_to_string(&path).with_context(|| {
        format!(
            "no stored credentials for {} — re-add it with `cx account add`",
            account.email
        )
    })?;
    serde_json::from_str(&contents).with_context(|| format!("could not parse {}", path.display()))
}

pub fn write_credentials(number: u32, credentials: &CredentialsFile) -> Result<()> {
    write_protected(
        &credentials_path(number),
        &serde_json::to_string_pretty(credentials)?,
    )
}

pub fn remove_credentials(number: u32) -> Result<()> {
    let path = credentials_path(number);
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

pub fn read_config(account: &Account) -> Result<serde_json::Value> {
    fsutil::read_json(&config_path(account.number)).with_context(|| {
        format!(
            "no stored config for {} — re-add it with `cx account add`",
            account.email
        )
    })
}

pub fn write_config(number: u32, contents: &str) -> Result<()> {
    write_protected(&config_path(number), contents)
}

pub fn remove_config(number: u32) -> Result<()> {
    let path = config_path(number);
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn write_protected(path: &Path, contents: &str) -> Result<()> {
    let parent = path.parent().unwrap();
    fs::create_dir_all(parent)?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    fsutil::write_atomically(path, contents)
}

fn roster_path() -> PathBuf {
    paths::data_dir().join("accounts.json")
}

fn credentials_path(number: u32) -> PathBuf {
    paths::data_dir()
        .join("credentials")
        .join(format!("account-{number}.json"))
}

fn config_path(number: u32) -> PathBuf {
    paths::data_dir()
        .join("configs")
        .join(format!("account-{number}.json"))
}

pub struct StoreLock {
    _file: File,
}

impl StoreLock {
    pub fn acquire() -> Result<StoreLock> {
        fs::create_dir_all(paths::data_dir())?;
        let file = File::create(paths::data_dir().join(".lock"))?;

        let start = Instant::now();
        loop {
            let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if result == 0 {
                return Ok(StoreLock { _file: file });
            }
            if start.elapsed() > Duration::from_secs(10) {
                bail!("another cx instance holds the store lock — retry in a few seconds");
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
}
