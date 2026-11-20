use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::{Method, StatusCode};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::Mutex;
use tunebox_core::paths::Secret;

use super::auth::{self, Token};

const API_BASE: &str = "https://api.spotify.com/v1";
const MAX_ATTEMPTS: u32 = 3;
const RETRY_DELAY: Duration = Duration::from_millis(300);
const MAX_RETRY_AFTER: u64 = 10;

pub struct WebApi {
    http: reqwest::Client,
    sp_dc: String,
    store: Secret,
    token: Mutex<Option<Token>>,
}

#[derive(Debug)]
pub struct ApiResponse {
    pub status: StatusCode,
    pub body: Value,
}

impl ApiResponse {
    pub fn error_text(&self) -> String {
        let message = self.body.pointer("/error/message").and_then(Value::as_str).unwrap_or_default();
        format!("{} {message}", self.status.as_u16()).trim_end().to_owned()
    }
}

impl WebApi {
    pub fn new(sp_dc: impl Into<String>, store: Secret) -> Result<Self> {
        let sp_dc = sp_dc.into();
        let http = tunebox_core::http::client(concat!("tunebox/", env!("CARGO_PKG_VERSION"))).build()?;
        let cookie_hash = auth::cookie_hash(&sp_dc);
        let token = match store.read()? {
            Some(bytes) => {
                let token: Token =
                    serde_json::from_slice(&bytes).context("Stored token corrupted – run `tunebox login`")?;
                (token.cookie_hash == cookie_hash).then_some(token)
            }
            None => None,
        };
        Ok(Self { http, sp_dc, store, token: Mutex::new(token) })
    }

    pub async fn is_logged_in(&self) -> bool {
        self.token.lock().await.is_some()
    }

    pub async fn login(&self) -> Result<()> {
        let token = auth::access_token(&self.http, &self.sp_dc).await?;
        self.persist(&token)?;
        *self.token.lock().await = Some(token);
        Ok(())
    }

    pub async fn save_browser_token(&self, access_token: String, expires_at: u64) -> Result<()> {
        let token = Token { access_token, expires_at, cookie_hash: auth::cookie_hash(&self.sp_dc) };
        self.persist(&token)?;
        *self.token.lock().await = Some(token);
        Ok(())
    }

    pub async fn get(&self, path: &str) -> Result<ApiResponse> {
        self.request(Method::GET, path, None).await
    }

    pub async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let response = self.get(path).await?;
        if !response.status.is_success() {
            bail!("Spotify: {}", response.error_text());
        }
        serde_json::from_value(response.body).with_context(|| format!("Unexpected response from {path}"))
    }

    pub async fn call(&self, method: Method, path: &str, body: Option<&Value>) -> Result<()> {
        let response = self.request(method, path, body).await?;
        if !response.status.is_success() {
            bail!("Spotify: {}", response.error_text());
        }
        Ok(())
    }

    pub async fn request(&self, method: Method, path: &str, body: Option<&Value>) -> Result<ApiResponse> {
        let token = self.access_token().await?;
        let url = format!("{API_BASE}{path}");
        let retries = method == Method::GET;
        let mut attempt = 1;
        loop {
            let mut request = self.http.request(method.clone(), &url).bearer_auth(&token);
            request = match body {
                Some(body) => request.json(body),
                None if method != Method::GET => request.body(Vec::new()),
                None => request,
            };
            let delay = match request.send().await {
                Ok(response) => {
                    let status = response.status();
                    let delay = match status {
                        StatusCode::TOO_MANY_REQUESTS => {
                            tracing::warn!("Web API rate limit on {path}");
                            retry_after(&response)
                        }
                        StatusCode::BAD_GATEWAY | StatusCode::SERVICE_UNAVAILABLE if attempt == 1 => Some(RETRY_DELAY),
                        _ => None,
                    };
                    match delay {
                        Some(delay) if retries && attempt < MAX_ATTEMPTS => delay,
                        _ => {
                            let bytes = response.bytes().await?;
                            let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
                            if !status.is_success() {
                                tracing::warn!("Spotify {path}: {status} {body}");
                            }
                            return Ok(ApiResponse { status, body });
                        }
                    }
                }
                Err(err) if retries && attempt == 1 && (err.is_timeout() || err.is_connect()) => RETRY_DELAY,
                Err(err) => return Err(err.into()),
            };
            tokio::time::sleep(delay).await;
            attempt += 1;
        }
    }

    async fn access_token(&self) -> Result<String> {
        let mut guard = self.token.lock().await;
        if let Some(token) = guard.as_ref()
            && token.is_fresh()
        {
            return Ok(token.access_token.clone());
        }
        let refreshed = match auth::access_token(&self.http, &self.sp_dc).await {
            Ok(token) => token,
            Err(err) => {
                *guard = None;
                return Err(err).context("Spotify sign-in expired – run `tunebox login spotify`");
            }
        };
        self.persist(&refreshed)?;
        let access_token = refreshed.access_token.clone();
        *guard = Some(refreshed);
        Ok(access_token)
    }

    fn persist(&self, token: &Token) -> Result<()> {
        self.store.write(&serde_json::to_vec_pretty(token)?)
    }
}

fn retry_after(response: &reqwest::Response) -> Option<Duration> {
    let seconds = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(1);
    (seconds <= MAX_RETRY_AFTER).then(|| Duration::from_secs(seconds))
}
