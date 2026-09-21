use std::env;
use std::path::PathBuf;

pub fn claude_config_home() -> PathBuf {
    if let Some(dir) = env::var_os("CLAUDE_CONFIG_DIR") {
        PathBuf::from(dir)
    } else {
        default_claude_config_home()
    }
}

pub fn global_config_path() -> PathBuf {
    let legacy = claude_config_home().join(".config.json");
    if legacy.exists() {
        legacy
    } else if let Some(dir) = env::var_os("CLAUDE_CONFIG_DIR") {
        PathBuf::from(dir).join(".claude.json")
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

/// `AX_DATA_DIR` puts the account store somewhere of the caller's choosing,
/// for a program built on ax that keeps accounts of its own.
pub fn data_dir() -> PathBuf {
    if let Some(dir) = env::var_os("AX_DATA_DIR").filter(|it| !it.is_empty()) {
        PathBuf::from(dir)
    } else if let Some(dir) = env::var_os("XDG_DATA_HOME").filter(|it| !it.is_empty()) {
        PathBuf::from(dir).join("ax")
    } else {
        home().join(".local/share/ax")
    }
}

fn home() -> PathBuf {
    dirs::home_dir().expect("could not determine the home directory")
}
