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
pub async fn login(paths: &Paths) -> Result<(String, u64)> {
    let binary = candidates()
        .into_iter()
        .find(|b| std::path::Path::new(b).exists() || which(b))
        .context("No Chromium browser found. Install Chrome/Chromium/Brave, or set CHROME_BIN.")?;
    let dir = paths.cache.join("spotify-cdp");
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
        let (port, path) = find_port(&dir).await?;
        let (mut socket, _) = connect_async(format!("ws://127.0.0.1:{port}{path}"))
            .await
            .context("Can't connect to the browser's debugging socket")?;
        socket.send(Message::Text(json!({"id": 1, "method": "Network.enable"}).to_string().into())).await?;
        let mut pending = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(300);
        while tokio::time::Instant::now() < deadline {
            let Some(message) = socket.next().await else { break };
            let Message::Text(text) = message? else { continue };
            let value: Value = serde_json::from_str(&text)?;
            if value["method"] == "Network.responseReceived" {
                let response = &value["params"]["response"];
                let url = response["url"].as_str().unwrap_or("");
                if url.contains("/api/token") {
                    pending.push(response["requestId"].as_str().unwrap_or("").to_owned());
                }
            } else if value["method"] == "Network.loadingFinished" {
                let id = value["params"]["requestId"].as_str().unwrap_or("");
                if pending.iter().any(|p| p == id) {
                    let request = json!({"id": 2, "method": "Network.getResponseBody", "params": {"requestId": id}});
                    socket.send(Message::Text(request.to_string().into())).await?;
                }
            } else if value["id"] == 2 {
                let body = value["result"]["body"].as_str().unwrap_or("");
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

    #[test]
    fn reads_the_token_from_the_web_players_response() {
        let body = r#"{"accessToken":"tok","accessTokenExpirationTimestampMs":1794854821000,"isAnonymous":false}"#;
        assert_eq!(token_from(body), Some(("tok".into(), 1794854821)));
        assert_eq!(token_from("{}"), None);
    }
}
