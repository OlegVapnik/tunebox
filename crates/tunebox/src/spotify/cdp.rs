//! Signs in with a real Chromium browser and reads the token its web player obtains.

use std::{process::Stdio, time::Duration};

use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
    time::sleep,
};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tunebox_core::paths::Paths;

const SPOTIFY: &str = "https://open.spotify.com/";

fn candidates() -> Vec<String> {
    let mut list = Vec::new();
    if let Ok(binary) = std::env::var("CHROME_BIN") {
        if !binary.trim().is_empty() {
            list.push(binary.trim().to_owned());
        }
    }
    for name in ["google-chrome", "chromium", "chromium-browser", "brave-browser", "microsoft-edge"] {
        list.push(name.to_owned());
    }
    if let Some(home) = std::env::var_os("HOME") {
        let cached = std::path::Path::new(&home).join(".cache/ms-playwright/chromium-1243/chrome-linux64/chrome");
        if cached.exists() {
            list.push(cached.to_string_lossy().into_owned());
        }
    }
    list
}

async fn find_port(dir: &std::path::Path) -> Result<(u16, String)> {
    let file = dir.join("DevToolsActivePort");
    for _ in 0..100 {
        if let Ok(text) = tokio::fs::read_to_string(&file).await {
            let mut lines = text.lines();
            if let (Some(port), Some(path)) = (lines.next().and_then(|p| p.trim().parse::<u16>().ok()), lines.next()) {
                return Ok((port, path.trim().to_owned()));
            }
        }
        sleep(Duration::from_millis(100)).await;
    }
    bail!("The browser didn't open its debugging port")
}

/// Opens Spotify in Chromium, waits for the user to sign in, and returns the token the web
/// player received. No cookies are copied: the browser's own session makes the request.
pub async fn login(paths: &Paths, sp_dc: &str) -> Result<(String, u64)> {
    let binary = candidates()
        .into_iter()
        .find(|b| std::path::Path::new(b).exists() || which(b))
        .context("No Chromium browser found. Install Chrome/Chromium/Brave, or set CHROME_BIN.")?;
    let dir = paths.cache.join("spotify-cdp");
    // A port file left by a previous run points at a dead browser; start from an empty profile,
    // the sign-in cookie is injected below anyway.
    let _ = tokio::fs::remove_dir_all(&dir).await;
    tokio::fs::create_dir_all(&dir).await?;
    let mut child: Child = Command::new(&binary)
        .args([
            "--remote-debugging-port=0",
            "--no-first-run",
            "--no-default-browser-check",
            "--remote-allow-origins=*",
            &format!("--user-data-dir={}", dir.display()),
            SPOTIFY,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("Couldn't start {binary}"))?;
    // Keep the pipe drained so a chatty browser never blocks on it.
    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while lines.next_line().await.ok().flatten().is_some() {}
        });
    }
    let result = async {
        let (port, _) = find_port(&dir).await?;
        // Network/Page domains live on a page target, not on the browser-level socket. Ask the
        // browser which tab it opened and connect to that one.
        let targets: Vec<Value> = reqwest::get(format!("http://127.0.0.1:{port}/json"))
            .await
            .context("Can't list the browser's tabs")?
            .json()
            .await
            .context("Can't read the browser's tabs")?;
        let page = targets
            .iter()
            .find(|target| target["type"] == "page" && target["url"].as_str().is_some_and(|url| url.contains("spotify.com")))
            .context("The browser didn't open a Spotify tab")?;
        let page_ws = page["webSocketDebuggerUrl"].as_str().context("The tab has no debugging socket")?;
        tracing::debug!("CDP tab: {page_ws}");
        let (mut socket, _) = connect_async(page_ws).await.context("Can't connect to the tab's debugging socket")?;
        socket.send(Message::Text(json!({"id": 1, "method": "Network.enable"}).to_string().into())).await?;
        // Put the user's sign-in in the browser's own cookie jar, then reload: the web player
        // makes its token request from inside the page, with all the real browser headers.
        let cookie = json!({
            "id": 3,
            "method": "Network.setCookie",
            "params": {
                "name": "sp_dc", "value": sp_dc, "url": SPOTIFY, "domain": ".spotify.com",
                "path": "/", "secure": true, "httpOnly": true, "sameSite": "Lax"
            }
        });
        socket.send(Message::Text(cookie.to_string().into())).await?;
        socket.send(Message::Text(json!({"id": 4, "method": "Page.enable"}).to_string().into())).await?;
        // Fetch pauses the response before the page reads it, which is the only reliable way to
        // get the body: Network.loadingFinished never arrives for these streamed responses.
        let fetch = json!({
            "id": 7,
            "method": "Fetch.enable",
            "params": {"patterns": [{"urlPattern": "*api/token*", "requestStage": "Response"}]}
        });
        socket.send(Message::Text(fetch.to_string().into())).await?;
        // The page caches its session token in browser storage. Drop it so the reload makes a
        // fresh token request that we can watch.
        let clear = json!({
            "id": 6, "method": "Runtime.evaluate",
            "params": {"expression": "localStorage.clear();sessionStorage.clear();'ok'", "returnByValue": true}
        });
        socket.send(Message::Text(clear.to_string().into())).await?;
        socket.send(Message::Text(
            json!({"id": 5, "method": "Page.reload", "params": {"ignoreCache": true}}).to_string().into(),
        ))
        .await?;
        let mut pending = Vec::new();
        let seconds = std::env::var("TUNEBOX_CDP_TIMEOUT").ok().and_then(|s| s.parse().ok()).unwrap_or(300);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
        while tokio::time::Instant::now() < deadline {
            let Some(message) = socket.next().await else { break };
            let Message::Text(text) = message? else { continue };
            let value: Value = serde_json::from_str(&text)?;
            if value["method"] == "Network.responseReceived" {
                let response = &value["params"]["response"];
                if response["status"].as_u64() == Some(200) {
                    pending.push(response["requestId"].as_str().unwrap_or("").to_owned());
                }
            } else if value["method"] == "Fetch.requestPaused" {
                let id = value["params"]["requestId"].as_str().unwrap_or("").to_owned();
                let request = json!({"id": 2, "method": "Fetch.getResponseBody", "params": {"requestId": id}});
                socket.send(Message::Text(request.to_string().into())).await?;
                let resume = json!({"id": 8, "method": "Fetch.continueRequest", "params": {"requestId": id}});
                socket.send(Message::Text(resume.to_string().into())).await?;
            } else if value["method"] == "Network.loadingFinished" || value["method"] == "Network.loadingFailed" {
                let id = value["params"]["requestId"].as_str().unwrap_or("");
                if pending.iter().any(|p| p == id) {
                    let request = json!({"id": 2, "method": "Network.getResponseBody", "params": {"requestId": id}});
                    socket.send(Message::Text(request.to_string().into())).await?;
                }
            } else if value["id"] == 2 {
                let raw = value["result"]["body"].as_str().unwrap_or("");
                let decoded;
                let body = if value["result"]["base64Encoded"].as_bool().unwrap_or(false) {
                    decoded = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, raw)
                        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                        .unwrap_or_default();
                    decoded.as_str()
                } else {
                    raw
                };
                if let Some(token) = token_from(body) {
                    return Ok(token);
                }
            }
        }
        bail!("Timed out waiting for Spotify to sign in")
    }
    .await;
    let _ = child.kill().await;
    result
}

fn token_from(body: &str) -> Option<(String, u64)> {
    let value: Value = serde_json::from_str(body).ok()?;
    // A fresh browser profile also fetches a token, but it belongs to nobody. Only accept the
    // one made after sp_dc was injected, so we never save an anonymous session as the user's.
    if value["isAnonymous"].as_bool().unwrap_or(true) {
        return None;
    }
    let access = value["accessToken"].as_str()?;
    let expires = value["accessTokenExpirationTimestampMs"].as_u64()? / 1000;
    (!access.is_empty()).then(|| (access.to_owned(), expires))
}

fn which(name: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(name).is_file()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "opens a real browser window"]
    async fn captures_a_real_token() {
        let paths = tunebox_core::paths::Paths::resolve().unwrap();
        let config = tunebox_core::config::Config::load(&paths).unwrap();
        let sp_dc = config.sp_dc(&paths).unwrap();
        let (token, expires) = login(&paths, &sp_dc).await.unwrap();
        assert!(!token.is_empty());
        println!("token expires at {expires}");
    }

    #[test]
    fn reads_the_token_from_the_web_players_response() {
        let body = r#"{"accessToken":"tok","accessTokenExpirationTimestampMs":1794854821000,"isAnonymous":false}"#;
        assert_eq!(token_from(body), Some(("tok".into(), 1794854821)));
        let anonymous = r#"{"accessToken":"tok","accessTokenExpirationTimestampMs":1794854821000,"isAnonymous":true}"#;
        assert_eq!(token_from(anonymous), None);
        assert_eq!(token_from("{}"), None);
    }
}
