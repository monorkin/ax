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

/// How much of an account's allowance is used, per limit, in percent.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Usage {
    /// The session limit, over five hours.
    pub five_hour: Option<Window>,
    /// The weekly limit.
    pub seven_day: Option<Window>,
    /// A weekly limit that applies to one model only — "Fable" today —
    /// named as the endpoint names it, because which model it is has
    /// changed before and will again.
    #[serde(default)]
    pub scoped: Option<Scoped>,
}

/// A limit that covers one model rather than everything.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Scoped {
    pub name: String,
    pub window: Window,
}

impl Usage {
    /// What switching goes by: the fuller of the session and the week. The
    /// Fable limit isn't among them — past it Claude Code moves to usage
    /// credits or another model, not to a stop.
    pub fn utilization(&self) -> Option<f64> {
        [&self.five_hour, &self.seven_day]
            .into_iter()
            .flatten()
            .map(|it| it.utilization)
            .max_by(f64::total_cmp)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Window {
    pub utilization: f64,
    /// When it resets, as the API gives it: an RFC 3339 timestamp.
    #[serde(default)]
    pub resets_at: Option<String>,
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

/// The endpoint's answer when it has been asked too often. Nothing is wrong
/// with the account or the token; the answer is to wait longer.
#[derive(Debug)]
pub struct AskedTooOften;

impl std::fmt::Display for AskedTooOften {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "the usage endpoint is being asked too often")
    }
}

impl std::error::Error for AskedTooOften {}

pub fn fetch_usage(access_token: &str) -> Result<Usage> {
    let answered = ureq::get(USAGE_URL)
        .header("Authorization", &format!("Bearer {access_token}"))
        .header("anthropic-beta", OAUTH_BETA_HEADER)
        .header("User-Agent", USER_AGENT)
        .call();

    let mut response = match answered {
        Err(ureq::Error::StatusCode(429)) => return Err(AskedTooOften.into()),
        answered => answered.context("usage lookup failed")?,
    };
    let body: serde_json::Value = response.body_mut().read_json()?;

    Ok(usage_in(&body))
}

/// `limits` is what the endpoint says about itself: each limit with the
/// kind it is, what it covers and how full it is, and it is where a limit on
/// one model is named. The windows beside it — `five_hour`, `seven_day` —
/// are the older shape, and answer for an account that has no `limits`.
fn usage_in(body: &serde_json::Value) -> Usage {
    match body["limits"].as_array() {
        Some(limits) => Usage {
            five_hour: limit_window(limits, "session"),
            seven_day: limit_window(limits, "weekly_all"),
            scoped: scoped_limit(limits),
        },
        None => Usage {
            five_hour: parse_window(&body["five_hour"]),
            seven_day: parse_window(&body["seven_day"]),
            scoped: parse_window(&body["seven_day_overage_included"])
                .map(|window| Scoped { name: "Fable".to_string(), window }),
        },
    }
}

fn limit_of<'a>(limits: &'a [serde_json::Value], kind: &str) -> Option<&'a serde_json::Value> {
    limits.iter().find(|it| it["kind"].as_str() == Some(kind))
}

fn limit_window(limits: &[serde_json::Value], kind: &str) -> Option<Window> {
    let limit = limit_of(limits, kind)?;
    Some(Window {
        utilization: limit["percent"].as_f64()?,
        resets_at: limit["resets_at"].as_str().map(String::from),
    })
}

fn scoped_limit(limits: &[serde_json::Value]) -> Option<Scoped> {
    let limit = limit_of(limits, "weekly_scoped")?;
    let name = limit["scope"]["model"]["display_name"].as_str().unwrap_or("scoped").to_string();
    Some(Scoped { name, window: limit_window(limits, "weekly_scoped")? })
}

fn parse_window(value: &serde_json::Value) -> Option<Window> {
    Some(Window {
        utilization: value["utilization"].as_f64()?,
        resets_at: value["resets_at"].as_str().map(String::from),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape the endpoint answers with, cut down to what is read here.
    #[test]
    fn every_limit_comes_from_what_the_endpoint_says_it_has() {
        let body = json!({
            "five_hour": { "utilization": 19.0, "resets_at": "2026-09-23T09:50:00Z" },
            "seven_day": { "utilization": 41.0, "resets_at": "2026-09-26T21:00:00Z" },
            "seven_day_opus": null,
            "limits": [
                { "kind": "session", "group": "session", "percent": 19, "resets_at": "2026-09-23T09:50:00Z", "scope": null },
                { "kind": "weekly_all", "group": "weekly", "percent": 41, "resets_at": "2026-09-26T21:00:00Z", "scope": null },
                {
                    "kind": "weekly_scoped", "group": "weekly", "percent": 20, "resets_at": "2026-09-26T21:00:00Z",
                    "scope": { "model": { "id": null, "display_name": "Fable" }, "surface": null }
                }
            ]
        });

        let usage = usage_in(&body);
        assert_eq!(usage.five_hour.unwrap().utilization, 19.0);
        assert_eq!(usage.seven_day.unwrap().utilization, 41.0);
        let scoped = usage.scoped.expect("the limit on one model is read from limits, where it is named");
        assert_eq!(scoped.name, "Fable");
        assert_eq!(scoped.window.utilization, 20.0);
        assert_eq!(scoped.window.resets_at.as_deref(), Some("2026-09-26T21:00:00Z"));
    }

    #[test]
    fn an_account_without_a_limits_list_is_read_the_way_it_was_before() {
        let body = json!({
            "five_hour": { "utilization": 19.0, "resets_at": "2026-09-23T09:50:00Z" },
            "seven_day": { "utilization": 41.0 },
            "seven_day_overage_included": { "utilization": 3.0 },
        });

        let usage = usage_in(&body);
        assert_eq!(usage.five_hour.unwrap().utilization, 19.0);
        assert_eq!(usage.seven_day.unwrap().resets_at, None);
        let scoped = usage.scoped.unwrap();
        assert_eq!((scoped.name.as_str(), scoped.window.utilization), ("Fable", 3.0));

        assert!(usage_in(&json!({ "limits": [] })).five_hour.is_none(), "a list with nothing in it reports nothing");
    }
}
