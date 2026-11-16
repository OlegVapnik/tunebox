use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use reqwest::Client;
use serde::{Deserialize, Serialize};

/// Spotify's web player hands out a Web API token for the account behind an `sp_dc` cookie.
/// The token belongs to Spotify's own app, so it isn't subject to the "premium owner" rule
/// that blocks tokens from a user-created developer app.
const WEB_TOKEN_URL: &str = "https://open.spotify.com/get_access_token?reason=transport&productType=web-player";
/// Spotify's web player identifies as a browser; a bare reqwest client gets turned away.
const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Token {
    pub access_token: String,
    /// Unix seconds.
    pub expires_at: u64,
    /// Hash of the cookie this token came from. A changed cookie (or an old OAuth token,
    /// which has no hash) invalidates the cache.
    #[serde(default)]
    pub cookie_hash: u64,
}

impl Token {
    pub fn is_fresh(&self) -> bool {
        unix_now() + 60 < self.expires_at
    }
}

#[derive(Deserialize)]
struct WebToken {
    #[serde(rename = "accessToken")]
    access_token: String,
    #[serde(rename = "accessTokenExpirationTimestampMs")]
    expires_at_ms: u64,
}

/// Exchanges the `sp_dc` cookie for a short-lived Web API access token.
pub(super) async fn access_token(http: &Client, sp_dc: &str) -> Result<Token> {
    check_cookie(sp_dc)?;
    let response = http
        .get(WEB_TOKEN_URL)
        .header("Cookie", format!("sp_dc={}", sp_dc.trim()))
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .send()
        .await
        .context("Can't reach Spotify's web player")?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("Spotify didn't accept the sp_dc cookie ({status}): update it with `tunebox login spotify`");
    }
    let token: WebToken =
        serde_json::from_str(&body).context("Spotify's web token response was unexpected – is sp_dc still valid?")?;
    if token.access_token.is_empty() {
        bail!("Spotify returned an empty token: update sp_dc with `tunebox login spotify`");
    }
    Ok(Token {
        access_token: token.access_token,
        expires_at: token.expires_at_ms / 1000,
        cookie_hash: cookie_hash(sp_dc),
    })
}

/// Stable hash used only to notice a changed cookie; not a security measure.
pub(super) fn cookie_hash(sp_dc: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    sp_dc.trim().hash(&mut hasher);
    hasher.finish()
}

fn check_cookie(sp_dc: &str) -> Result<()> {
    if sp_dc.trim().is_empty() {
        bail!("No Spotify sign-in: add `sp_dc` to the config or run `tunebox login spotify`");
    }
    Ok(())
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_web_players_token_response() {
        let json = r#"{"accessToken":"tok","accessTokenExpirationTimestampMs":1794854821000,"isAnonymous":false}"#;
        let raw: WebToken = serde_json::from_str(json).unwrap();
        let token = Token {
            access_token: raw.access_token,
            expires_at: raw.expires_at_ms / 1000,
            cookie_hash: 0,
        };
        assert_eq!(token.access_token, "tok");
        assert_eq!(token.expires_at, 1794854821);
    }

    #[test]
    fn an_empty_cookie_is_reported_before_a_request() {
        // The error is deterministic and doesn't touch the network.
        assert!(check_cookie("  ").is_err());
        assert!(check_cookie("real-cookie").is_ok());
    }
}
