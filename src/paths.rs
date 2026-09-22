use std::env;
use std::path::PathBuf;

use crate::settings;

pub fn claude_config_home() -> PathBuf {
    chosen_claude_config_home().unwrap_or_else(default_claude_config_home)
}

/// Whether the current login is somewhere other than `~/.claude`: chosen by
/// the program built on ax, or by `CLAUDE_CONFIG_DIR` in the shell.
pub fn claude_config_home_is_chosen() -> bool {
    chosen_claude_config_home().is_some()
}

fn chosen_claude_config_home() -> Option<PathBuf> {
    if let Some(dir) = &settings::settings().claude_config_dir {
        Some(dir.clone())
    } else {
        env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from)
    }
}

pub fn global_config_path() -> PathBuf {
    let legacy = claude_config_home().join(".config.json");
    if legacy.exists() {
        legacy
    } else if let Some(dir) = chosen_claude_config_home() {
        dir.join(".claude.json")
    } else {
        home().join(".claude.json")
    }
}

pub fn credentials_path() -> PathBuf {
    claude_config_home().join(".credentials.json")
}

pub fn default_claude_config_home() -> PathBuf {
    home().join(".claude")
}

pub fn default_global_config_path() -> PathBuf {
    let legacy = default_claude_config_home().join(".config.json");
    if legacy.exists() {
        legacy
    } else {
        home().join(".claude.json")
    }
}

/// A program built on ax that keeps accounts of its own says where in
/// `settings`.
pub fn data_dir() -> PathBuf {
    if let Some(dir) = &settings::settings().data_dir {
        dir.clone()
    } else if let Some(dir) = env::var_os("XDG_DATA_HOME").filter(|it| !it.is_empty()) {
        PathBuf::from(dir).join("ax")
    } else {
        home().join(".local/share/ax")
    }
}

/// What someone types to reach these commands, for messages that tell them
/// what to run next.
pub fn invoked_as() -> String {
    settings::settings().invoked_as.clone().unwrap_or_else(|| "ax".to_string())
}

fn home() -> PathBuf {
    dirs::home_dir().expect("could not determine the home directory")
}
