//! Codex's login and OpenAI's endpoints. A login is one file, `auth.json`,
//! under `CODEX_HOME` or `~/.codex`; it's kept as JSON and passed along whole,
//! so what Codex adds to it survives a switch.

use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use crate::clock;
use crate::fsutil;
use crate::oauth::{AskedTooOften, Usage, Window};

const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const USER_AGENT: &str = concat!("ax/", env!("CARGO_PKG_VERSION"));
const AUTH_CLAIMS: &str = "https://api.openai.com/auth";
const DAY_SECONDS: i64 = 86_400;

pub struct Identity {
    pub email: String,
    pub user_id: String,
    pub account_id: String,
    pub plan: String,
}

pub struct Home {
    pub directory: PathBuf,
}

impl Home {
    pub fn current() -> Home {
        let directory = env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(default_directory);
        Home { directory }
    }

    pub fn at(directory: &Path) -> Home {
        Home { directory: directory.to_path_buf() }
    }

    pub fn auth(&self) -> Result<Option<Value>> {
        let path = self.auth_path();
        if !path.exists() {
            return Ok(None);
        }

        let contents = fs::read_to_string(&path)?;
        if contents.trim().is_empty() {
            Ok(None)
        } else {
            let auth = serde_json::from_str(&contents)
                .with_context(|| format!("could not parse {}", path.display()))?;
            Ok(Some(auth))
        }
    }

    pub fn write_auth(&self, auth: &Value) -> Result<()> {
        fsutil::write_json_atomically(&self.auth_path(), auth)
    }

    /// None for no login, and for an API key login, which has no account.
    pub fn identity(&self) -> Result<Option<Identity>> {
        match self.auth()? {
            Some(auth) if auth["tokens"].is_object() => identity_of(&auth).map(Some),
            _ => Ok(None),
        }
    }

    fn auth_path(&self) -> PathBuf {
        self.directory.join("auth.json")
    }
}

pub fn default_directory() -> PathBuf {
    dirs::home_dir().expect("could not determine the home directory").join(".codex")
}

pub fn identity_of(auth: &Value) -> Result<Identity> {
    let token = auth["tokens"]["id_token"].as_str().context("the Codex login has no id_token")?;
    let claims = claims_of(token)?;
    let details = &claims[AUTH_CLAIMS];
    let account_id = details["chatgpt_account_id"]
        .as_str()
        .or_else(|| auth["tokens"]["account_id"].as_str())
        .filter(|it| !it.is_empty())
        .context("the Codex login names no ChatGPT account")?;

    Ok(Identity {
        email: claims["email"].as_str().context("the Codex login names no email")?.to_string(),
        user_id: details["chatgpt_user_id"].as_str().unwrap_or_default().to_string(),
        account_id: account_id.to_string(),
        plan: details["chatgpt_plan_type"].as_str().unwrap_or("account").to_string(),
    })
}

/// Seconds since the epoch at which the access token stops working.
pub fn expires_at(auth: &Value) -> Option<i64> {
    let token = auth["tokens"]["access_token"].as_str()?;
    claims_of(token).ok()?["exp"].as_i64()
}

pub fn refresh(auth: &mut Value) -> Result<()> {
    let refresh_token = auth["tokens"]["refresh_token"].as_str().context("account has no refresh token")?;
    let request = json!({
        "client_id": CLIENT_ID,
        "grant_type": "refresh_token",
        "refresh_token": refresh_token,
    });

    let mut response = ureq::post(TOKEN_URL)
        .header("Content-Type", "application/json")
        .header("User-Agent", USER_AGENT)
        .send(request.to_string())
        .context("token refresh failed")?;
    let body: Value = response.body_mut().read_json()?;

    apply_refresh_response(auth, &body)
}

fn apply_refresh_response(auth: &mut Value, body: &Value) -> Result<()> {
    let access_token = body["access_token"].as_str().context("token response carried no access_token")?;
    auth["tokens"]["access_token"] = json!(access_token);
    for rotated in ["id_token", "refresh_token"] {
        if let Some(value) = body[rotated].as_str().filter(|it| !it.is_empty()) {
            auth["tokens"][rotated] = json!(value);
        }
    }
    auth["last_refresh"] = json!(clock::timestamp());
    Ok(())
}

pub fn fetch_usage(auth: &Value) -> Result<Usage> {
    let access_token = auth["tokens"]["access_token"].as_str().context("the Codex login has no access token")?;
    let account_id = auth["tokens"]["account_id"].as_str().context("the Codex login names no ChatGPT account")?;
    let answered = ureq::get(USAGE_URL)
        .header("Authorization", &format!("Bearer {access_token}"))
        .header("ChatGPT-Account-Id", account_id)
        .header("User-Agent", USER_AGENT)
        .call();

    let mut response = match answered {
        Err(ureq::Error::StatusCode(429)) => return Err(AskedTooOften.into()),
        answered => answered.context("usage lookup failed")?,
    };
    let body: Value = response.body_mut().read_json()?;

    Ok(usage_in(&body))
}

/// Which window is which goes by its length, not its name: a plan with only a
/// weekly limit reports it as the primary one.
fn usage_in(body: &Value) -> Usage {
    let mut usage = Usage { five_hour: None, seven_day: None, scoped: None };
    for window in ["primary_window", "secondary_window"] {
        let window = &body["rate_limit"][window];
        if let Some(utilization) = window["used_percent"].as_f64() {
            let resets_at = window["reset_at"].as_u64().map(clock::format_epoch_seconds);
            let parsed = Some(Window { utilization, resets_at });
            if window["limit_window_seconds"].as_i64().unwrap_or(0) >= DAY_SECONDS {
                usage.seven_day = parsed;
            } else {
                usage.five_hour = parsed;
            }
        }
    }
    usage
}

fn claims_of(token: &str) -> Result<Value> {
    let payload = token.split('.').nth(1).ok_or_else(|| anyhow!("the Codex login holds a malformed token"))?;
    serde_json::from_slice(&decode_base64_url(payload)?).context("the Codex login holds a malformed token")
}

fn decode_base64_url(text: &str) -> Result<Vec<u8>> {
    let mut decoded = Vec::with_capacity(text.len() * 3 / 4);
    let mut buffer = 0u32;
    let mut bits = 0;
    for byte in text.bytes().filter(|it| *it != b'=') {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return Err(anyhow!("the Codex login holds a malformed token")),
        };
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            decoded.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Ok(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A header-less token over `{"email":"anna@example.com","exp":1790094000,...}`.
    const ID_TOKEN: &str = "x.eyJlbWFpbCI6ImFubmFAZXhhbXBsZS5jb20iLCJleHAiOjE3OTAwOTQwMDAsImh0dHBzOi8vYXBpLm9wZW5haS5jb20vYXV0aCI6eyJjaGF0Z3B0X2FjY291bnRfaWQiOiJhY2MtMSIsImNoYXRncHRfdXNlcl9pZCI6InVzZXItMSIsImNoYXRncHRfcGxhbl90eXBlIjoidGVhbSJ9fQ.y";

    #[test]
    fn who_a_login_belongs_to_comes_from_its_tokens() {
        let auth = json!({ "tokens": { "id_token": ID_TOKEN, "access_token": ID_TOKEN, "account_id": "acc-1" } });
        let identity = identity_of(&auth).unwrap();
        assert_eq!(identity.email, "anna@example.com");
        assert_eq!(identity.account_id, "acc-1");
        assert_eq!(identity.user_id, "user-1");
        assert_eq!(identity.plan, "team");
        assert_eq!(expires_at(&auth), Some(1_790_094_000));

        let malformed = json!({ "tokens": { "id_token": "x.%%%.y" } });
        assert!(identity_of(&malformed).is_err());
    }

    #[test]
    fn windows_are_told_apart_by_their_length() {
        let usage = usage_in(&json!({ "rate_limit": {
            "primary_window": { "used_percent": 12, "limit_window_seconds": 18000, "reset_at": 1790094000 },
            "secondary_window": { "used_percent": 60, "limit_window_seconds": 604800, "reset_at": 1790094060 },
        }}));
        assert_eq!(usage.five_hour.unwrap().utilization, 12.0);
        let week = usage.seven_day.unwrap();
        assert_eq!(week.utilization, 60.0);
        assert_eq!(week.resets_at.as_deref(), Some("2026-09-22T16:21:00Z"));

        let weekly_only = usage_in(&json!({ "rate_limit": {
            "primary_window": { "used_percent": 40, "limit_window_seconds": 604800 },
            "secondary_window": null,
        }}));
        assert!(weekly_only.five_hour.is_none());
        assert_eq!(weekly_only.seven_day.unwrap().utilization, 40.0);

        assert_eq!(usage_in(&json!({})).utilization(), None);
    }

    #[test]
    fn a_refresh_keeps_what_it_did_not_rotate() {
        let mut auth = json!({ "auth_mode": "chatgpt", "tokens": { "access_token": "a1", "refresh_token": "r1", "id_token": "i1", "account_id": "acc-1" } });
        apply_refresh_response(&mut auth, &json!({ "access_token": "a2", "refresh_token": "r2" })).unwrap();
        assert_eq!(auth["tokens"]["access_token"], "a2");
        assert_eq!(auth["tokens"]["refresh_token"], "r2");
        assert_eq!(auth["tokens"]["id_token"], "i1");
        assert_eq!(auth["tokens"]["account_id"], "acc-1");
        assert_eq!(auth["auth_mode"], "chatgpt");

        assert!(apply_refresh_response(&mut auth, &json!({})).is_err());
    }
}
