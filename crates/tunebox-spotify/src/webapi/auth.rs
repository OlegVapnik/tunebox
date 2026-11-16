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

/// Exchanges the `sp_dc` cookie for a short-lived Web API access token. Spotify's web player
/// used to do this through `get_access_token`; when that endpoint refuses the request, try to
/// read the session token from the rendered home page instead.
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
    if !status.is_success() {
        let _ = response.bytes().await;
        return ssr_token(http, sp_dc).await.with_context(|| format!("Spotify's token endpoint returned {status}"));
    }
    let body = response.text().await.unwrap_or_default();
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

/// Looks for an access token in the session JSON the web page may embed. Older builds rendered
/// `<script id="session" type="application/json">`; the current one fetches its token from
/// JavaScript instead, so a missing token is reported as such rather than guessed.
async fn ssr_token(http: &Client, sp_dc: &str) -> Result<Token> {
    let html = http
        .get("https://open.spotify.com/")
        .header("Cookie", format!("sp_dc={}", sp_dc.trim()))
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .send()
        .await
        .context("Can't reach Spotify's home page")?
        .error_for_status()
        .context("Spotify's home page refused the request")?
        .text()
        .await
        .context("Can't read Spotify's home page")?;
    for marker in ["id=\"session\"", "id=\"__NEXT_DATA__\"", "id=\"appServerConfig\""] {
        let Some(start) = html.find(marker) else { continue };
        let Some(open) = html[start..].find('>') else { continue };
        let body = &html[start + open + 1..];
        let Some(end) = body.find("</script>") else { continue };
        let Ok(json) = serde_json::from_str::<serde_json::Value>(body[..end].trim()) else { continue };
        if let Some(access_token) = find_string(&json, "accessToken") {
            let expires_at = find_u64(&json, "accessTokenExpirationTimestampMs").unwrap_or_else(|| unix_now() + 3600);
            return Ok(Token { access_token, expires_at, cookie_hash: cookie_hash(sp_dc) });
        }
    }
    bail!(
        "Spotify's home page doesn't embed an access token any more, and its token endpoint refuses this \
         request. Nothing can be signed in from sp_dc alone here. Use a client ID from a developer app owned \
         by a Premium account (`tunebox login spotify`), or sign in with a real browser."
    )
}

fn find_string(value: &serde_json::Value, key: &str) -> Option<String> {
    match value {
        serde_json::Value::Object(map) => map.iter().find_map(|(name, child)| {
            if name == key {
                child.as_str().map(str::to_owned).or_else(|| find_string(child, key))
            } else {
                find_string(child, key)
            }
        }),
        serde_json::Value::Array(items) => items.iter().find_map(|child| find_string(child, key)),
        _ => None,
    }
}

fn find_u64(value: &serde_json::Value, key: &str) -> Option<u64> {
    match value {
        serde_json::Value::Object(map) => map.iter().find_map(|(name, child)| {
            if name == key {
                child.as_u64().or_else(|| child.as_str().and_then(|s| s.parse().ok())).or_else(|| find_u64(child, key))
            } else {
                find_u64(child, key)
            }
        }),
        serde_json::Value::Array(items) => items.iter().find_map(|child| find_u64(child, key)),
        _ => None,
    }
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
    fn finds_a_token_in_an_embedded_session_object() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{"props":{"session":{"accessToken":"tok","accessTokenExpirationTimestampMs":1794854821000}}}"#,
        )
        .unwrap();
        assert_eq!(find_string(&json, "accessToken").as_deref(), Some("tok"));
        assert_eq!(find_u64(&json, "accessTokenExpirationTimestampMs"), Some(1794854821000));
        assert_eq!(find_string(&json, "notThere"), None);
    }

    #[test]
    fn an_empty_cookie_is_reported_before_a_request() {
        // The error is deterministic and doesn't touch the network.
        assert!(check_cookie("  ").is_err());
        assert!(check_cookie("real-cookie").is_ok());
    }
}
