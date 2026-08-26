//! Talks to Anthropic's OAuth endpoints: token refresh and usage lookup.

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::{SystemTime, UNIX_EPOCH};

const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const OAUTH_BETA_HEADER: &str = "oauth-2025-04-20";
const USER_AGENT: &str = concat!("ax/", env!("CARGO_PKG_VERSION"));

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct OauthCredentials {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription_type: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl OauthCredentials {
    pub fn expires_within(&self, buffer_ms: i64) -> bool {
        if let Some(expires_at) = self.expires_at {
            now_ms() + buffer_ms >= expires_at
        } else {
            false
        }
    }
}

pub struct Usage {
    pub five_hour: Option<Window>,
    pub seven_day: Option<Window>,
}

impl Usage {
    pub fn utilization(&self) -> Option<f64> {
        [&self.five_hour, &self.seven_day]
            .into_iter()
            .flatten()
            .map(|it| it.utilization)
            .max_by(f64::total_cmp)
    }
}

pub struct Window {
    pub utilization: f64,
}

pub fn refresh(credentials: &mut OauthCredentials) -> Result<()> {
    let refresh_token = credentials
        .refresh_token
        .as_ref()
        .ok_or_else(|| anyhow!("account has no refresh token"))?;
    let request = json!({
        "grant_type": "refresh_token",
        "refresh_token": refresh_token,
        "client_id": CLIENT_ID,
    });

    let mut response = ureq::post(TOKEN_URL)
        .header("Content-Type", "application/json")
        .header("User-Agent", USER_AGENT)
        .send(request.to_string())
        .context("token refresh failed")?;
    let body: serde_json::Value = response.body_mut().read_json()?;

    apply_refresh_response(credentials, &body)
}

fn apply_refresh_response(
    credentials: &mut OauthCredentials,
    body: &serde_json::Value,
) -> Result<()> {
    let access_token = body["access_token"]
        .as_str()
        .context("token response carried no access_token")?;
    let expires_in = body["expires_in"]
        .as_i64()
        .context("token response carried no expires_in")?;

    credentials.access_token = access_token.to_string();
    credentials.expires_at = Some(now_ms() + expires_in * 1000);
    if let Some(rotated) = body["refresh_token"].as_str().filter(|it| !it.is_empty()) {
        credentials.refresh_token = Some(rotated.to_string());
    }
    if let Some(scope) = body["scope"].as_str() {
        credentials.scopes = scope.split_whitespace().map(str::to_string).collect();
    }
    Ok(())
}

pub fn fetch_usage(access_token: &str) -> Result<Usage> {
    let mut response = ureq::get(USAGE_URL)
        .header("Authorization", &format!("Bearer {access_token}"))
        .header("anthropic-beta", OAUTH_BETA_HEADER)
        .header("User-Agent", USER_AGENT)
        .call()
        .context("usage lookup failed")?;
    let body: serde_json::Value = response.body_mut().read_json()?;

    Ok(Usage {
        five_hour: parse_window(&body["five_hour"]),
        seven_day: parse_window(&body["seven_day"]),
    })
}

fn parse_window(value: &serde_json::Value) -> Option<Window> {
    Some(Window {
        utilization: value["utilization"].as_f64()?,
    })
}

pub fn credentials_for_setup_token(token: &str) -> Result<OauthCredentials> {
    if !token.starts_with("sk-ant-oat") {
        bail!("that doesn't look like a setup token (expected an sk-ant-oat... value)");
    }
    Ok(OauthCredentials {
        access_token: token.to_string(),
        refresh_token: None,
        expires_at: None,
        scopes: vec!["user:inference".to_string()],
        subscription_type: None,
        extra: serde_json::Map::new(),
    })
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before the Unix epoch")
        .as_millis() as i64
}
