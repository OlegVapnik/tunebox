
use std::{
    sync::Mutex,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use reqwest::{StatusCode, header::HeaderMap};
use serde_json::{Value, json};
use sha1::{Digest, Sha1};
use tunebox_core::paths::Secret;

use crate::{
    account::Identity,
    cookies::{self, Jar},
};

pub(crate) const ORIGIN: &str = "https://music.youtube.com";
const HOST: &str = "music.youtube.com";
const API: &str = "https://music.youtube.com/youtubei/v1/";
const BROWSER: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36";
const ANDROID_VERSION: &str = "7.21.50";
const ANDROID_AGENT: &str = "com.google.android.apps.youtube.music/7.21.50 (Linux; U; Android 14) gzip";
const MAX_ATTEMPTS: u32 = 3;
const RETRY_DELAY: Duration = Duration::from_millis(400);
const SAVE_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClientKind {
    Web,
    Android,
}

pub(crate) struct InnerTube {
    http: reqwest::Client,
    version: String,
    gl: Option<String>,
    visitor: Mutex<Option<String>>,
    auth: Mutex<Option<Auth>>,
}

struct Auth {
    jar: Jar,
    identity: Identity,
    store: Option<Stored>,
    browser: Option<String>,
    dirty: bool,
    saved: Instant,
}

struct Stored {
    secret: Secret,
    bytes: Vec<u8>,
}

impl InnerTube {
    pub fn new(store: Secret) -> Result<Self> {
        let auth = store.read()?.and_then(|bytes| {
            let text = String::from_utf8_lossy(&bytes);
            let (jar, identity) = (Jar::parse(&text), Identity::parse(&text));
            let browser = crate::auth::browser_of(&text);
            signed_in(&jar).then(|| Auth::new(jar, identity, browser, Some(Stored { secret: store, bytes })))
        });
        Self::with_auth(auth)
    }

    pub fn unsaved(jar: Jar) -> Result<Self> {
        Self::with_auth(signed_in(&jar).then(|| Auth::new(jar, Identity::default(), None, None)))
    }

    fn with_auth(auth: Option<Auth>) -> Result<Self> {
        let http = tunebox_core::http::client(BROWSER).build()?;
        Ok(Self { http, version: web_version(), gl: country(), visitor: Mutex::new(None), auth: Mutex::new(auth) })
    }

    pub fn signed_in(&self) -> bool {
        self.auth.lock().unwrap().is_some()
    }

    pub fn identity(&self) -> Option<Identity> {
        self.auth.lock().unwrap().as_ref().map(|auth| auth.identity.clone())
    }

    pub fn set_identity(&self, identity: Identity) {
        if let Some(auth) = self.auth.lock().unwrap().as_mut() {
            auth.dirty |= auth.identity != identity;
            auth.identity = identity;
        }
        *self.visitor.lock().unwrap() = None;
    }

    pub fn replace_sign_in(&self, text: &str) {
        if let Some(auth) = self.auth.lock().unwrap().as_mut() {
            auth.jar = Jar::parse(text);
            auth.identity = Identity::parse(text);
            let browser = crate::auth::browser_of(text).or_else(|| auth.browser.take());
            auth.browser = browser;
            auth.dirty = true;
            if let Some(stored) = auth.store.as_mut() {
                stored.bytes = text.as_bytes().to_vec();
            }
        }
        *self.visitor.lock().unwrap() = None;
    }

    pub async fn call(&self, endpoint: &str, body: Value) -> Result<Value> {
        self.call_as(ClientKind::Web, endpoint, body).await
    }

    pub async fn browse(&self, browse_id: &str) -> Result<Value> {
        self.call("browse", json!({ "browseId": browse_id })).await
    }

    pub async fn continuation(&self, endpoint: &str, token: &str) -> Result<Value> {
        self.call(endpoint, json!({ "continuation": token })).await
    }

    pub async fn call_as(&self, kind: ClientKind, endpoint: &str, body: Value) -> Result<Value> {
        self.call_with(kind, None, endpoint, body).await
    }

    pub async fn call_as_identity(&self, identity: &Identity, endpoint: &str, body: Value) -> Result<Value> {
        self.call_with(ClientKind::Web, Some(identity), endpoint, body).await
    }

    async fn call_with(
        &self,
        kind: ClientKind,
        identity: Option<&Identity>,
        endpoint: &str,
        body: Value,
    ) -> Result<Value> {
        let mut attempt = 1;
        loop {
            match self.request(kind, identity, endpoint, &body).await {
                Ok(value) => return Ok(value),
                Err(Failure::Retry(err)) if attempt < MAX_ATTEMPTS => {
                    tracing::debug!("{endpoint}: {err:#}, trying again");
                    attempt += 1;
                    tokio::time::sleep(RETRY_DELAY * attempt).await;
                }
                Err(Failure::Retry(err) | Failure::Fatal(err)) => return Err(err),
            }
        }
    }

    async fn request(
        &self,
        kind: ClientKind,
        identity: Option<&Identity>,
        endpoint: &str,
        body: &Value,
    ) -> Result<Value, Failure> {
        let identity = identity.cloned().or_else(|| self.identity());
        let mut payload = json!({ "context": self.context(kind, identity.as_ref()) });
        if let (Some(target), Some(extra)) = (payload.as_object_mut(), body.as_object()) {
            target.extend(extra.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
        let url = format!("{API}{endpoint}?prettyPrint=false");
        let mut request = self.http.post(url).json(&payload).header("Referer", format!("{ORIGIN}/"));
        for (name, value) in self.headers(kind, identity.as_ref()) {
            request = request.header(name, value);
        }
        let response = request.send().await.map_err(|err| {
            let err = anyhow::Error::from(err).context(format!("YouTube Music didn't answer ({endpoint})"));
            Failure::Retry(err)
        })?;
        let status = response.status();
        self.keep_cookies(response.headers());
        let bytes = response.bytes().await.map_err(|err| Failure::Retry(err.into()))?;
        let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        if !status.is_success() {
            let message = value.pointer("/error/message").and_then(Value::as_str).unwrap_or_default().to_owned();
            let err = anyhow::Error::new(ApiError { status, message, endpoint: endpoint.to_owned() });
            return Err(match status {
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN if self.signed_in() => Failure::Fatal(
                    err.context("The YouTube Music sign-in has expired – run `tunebox login ytmusic` again"),
                ),
                s if s.is_server_error() || s == StatusCode::TOO_MANY_REQUESTS => Failure::Retry(err),
                _ => Failure::Fatal(err),
            });
        }
        if value.is_null() {
            return Err(Failure::Retry(anyhow::anyhow!("YouTube Music sent no JSON ({endpoint})")));
        }
        let own = kind == ClientKind::Web && identity == self.identity();
        if let Some(visitor) = value.pointer("/responseContext/visitorData").and_then(Value::as_str).filter(|_| own) {
            let mut current = self.visitor.lock().unwrap();
            if current.is_none() {
                *current = Some(visitor.to_owned());
            }
        }
        Ok(value)
    }

    fn context(&self, kind: ClientKind, identity: Option<&Identity>) -> Value {
        let mut client = match kind {
            ClientKind::Web => json!({ "clientName": "WEB_REMIX", "clientVersion": self.version }),
            ClientKind::Android => json!({
                "clientName": "ANDROID_MUSIC",
                "clientVersion": ANDROID_VERSION,
                "androidSdkVersion": 34,
                "osName": "Android",
                "osVersion": "14",
            }),
        };
        client["hl"] = "en".into();
        if let Some(gl) = &self.gl {
            client["gl"] = gl.as_str().into();
        }
        if let Some(visitor) = self.visitor.lock().unwrap().as_deref() {
            client["visitorData"] = visitor.into();
        }
        let mut user = json!({});
        if let Some(page_id) = identity.and_then(|identity| identity.page_id.as_deref()) {
            user["onBehalfOfUser"] = page_id.into();
        }
        json!({ "client": client, "user": user })
    }

    fn headers(&self, kind: ClientKind, identity: Option<&Identity>) -> Vec<(&'static str, String)> {
        let mut headers = vec![("Origin", ORIGIN.to_owned()), ("X-Origin", ORIGIN.to_owned())];
        match kind {
            ClientKind::Web => {
                headers.push(("X-YouTube-Client-Name", "67".into()));
                headers.push(("X-YouTube-Client-Version", self.version.clone()));
            }
            ClientKind::Android => {
                headers.push(("User-Agent", ANDROID_AGENT.into()));
                headers.push(("X-YouTube-Client-Name", "21".into()));
                headers.push(("X-YouTube-Client-Version", ANDROID_VERSION.into()));
            }
        }
        if let Some(visitor) = self.visitor.lock().unwrap().clone() {
            headers.push(("X-Goog-Visitor-Id", visitor));
        }
        match self.auth.lock().unwrap().as_ref() {
            Some(auth) => {
                headers.push(("Cookie", auth.jar.header(HOST)));
                if let Some(sapisid) = sapisid(&auth.jar) {
                    headers.push(("Authorization", sapisid_hash(sapisid, unix_time(), ORIGIN)));
                }
                let identity = identity.unwrap_or(&auth.identity);
                headers.push(("X-Goog-AuthUser", identity.index.to_string()));
                if let Some(page_id) = &identity.page_id {
                    headers.push(("X-Goog-PageId", page_id.clone()));
                }
            }
            None => headers.push(("Cookie", "SOCS=CAI".into())),
        }
        headers
    }

    fn keep_cookies(&self, headers: &HeaderMap) {
        let mut auth = self.auth.lock().unwrap();
        let Some(auth) = auth.as_mut() else { return };
        for value in headers.get_all(reqwest::header::SET_COOKIE) {
            if let Ok(value) = value.to_str() {
                auth.dirty |= auth.jar.update(HOST, value);
            }
        }
        if auth.dirty && auth.saved.elapsed() >= SAVE_INTERVAL {
            auth.save();
        }
    }

    pub fn flush(&self) {
        if let Some(auth) = self.auth.lock().unwrap().as_mut().filter(|auth| auth.dirty) {
            auth.save();
        }
    }

    #[cfg(test)]
    pub fn jar(&self) -> Option<Jar> {
        self.auth.lock().unwrap().as_ref().map(|auth| auth.jar.clone())
    }

    pub fn sign_in_text(&self) -> Option<String> {
        self.auth.lock().unwrap().as_ref().map(Auth::text)
    }

    pub fn browser(&self) -> Option<String> {
        self.auth.lock().unwrap().as_ref().and_then(|auth| auth.browser.clone())
    }

    pub fn set_browser(&self, browser: Option<String>) {
        if let Some(auth) = self.auth.lock().unwrap().as_mut() {
            auth.dirty |= auth.browser != browser;
            auth.browser = browser;
        }
    }
}

impl Auth {
    fn new(jar: Jar, identity: Identity, browser: Option<String>, store: Option<Stored>) -> Self {
        Self { jar, identity, store, browser, dirty: false, saved: Instant::now() }
    }

    fn text(&self) -> String {
        let mut text = self.jar.to_text() + &self.identity.notes();
        if let Some(browser) = &self.browser {
            text.push_str(crate::auth::BROWSER_NOTE);
            text.push_str(browser);
            text.push('\n');
        }
        text
    }

    fn save(&mut self) {
        self.dirty = false;
        self.saved = Instant::now();
        let text = self.text().into_bytes();
        let Some(store) = &mut self.store else { return };
        match store.secret.read() {
            Ok(Some(bytes)) if bytes == store.bytes => {}
            Ok(_) => {
                tracing::info!("The YouTube Music sign-in changed elsewhere; leaving it as it is");
                self.store = None;
                return;
            }
            Err(err) => {
                tracing::warn!("reading YouTube Music cookies: {err:#}");
                return;
            }
        }
        match store.secret.write(&text) {
            Ok(()) => store.bytes = text,
            Err(err) => tracing::warn!("saving YouTube Music cookies: {err:#}"),
        }
    }
}

impl Drop for InnerTube {
    fn drop(&mut self) {
        self.flush();
    }
}

#[derive(Debug)]
pub(crate) struct ApiError {
    pub status: StatusCode,
    message: String,
    endpoint: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "YouTube Music: {} {} ({})", self.status.as_u16(), self.message, self.endpoint)
    }
}

impl std::error::Error for ApiError {}

pub(crate) fn status(err: &anyhow::Error) -> Option<StatusCode> {
    err.chain().find_map(|cause| cause.downcast_ref::<ApiError>()).map(|err| err.status)
}

enum Failure {
    Retry(anyhow::Error),
    Fatal(anyhow::Error),
}

pub(crate) fn sapisid(jar: &Jar) -> Option<&str> {
    jar.get("SAPISID").or_else(|| jar.get("__Secure-3PAPISID"))
}

pub(crate) fn sapisid_hash(sapisid: &str, time: u64, origin: &str) -> String {
    let digest = Sha1::digest(format!("{time} {sapisid} {origin}").as_bytes());
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("SAPISIDHASH {time}_{hex}")
}

fn unix_time() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn web_version() -> String {
    let (year, month, day) = cookies::civil_from_days(unix_time() / 86_400 - 1);
    format!("1.{year}{month:02}{day:02}.01.00")
}

fn country() -> Option<String> {
    ["LC_ALL", "LC_MESSAGES", "LANG"].iter().filter_map(|name| std::env::var(name).ok()).find_map(|locale| {
        let region = locale.split(['.', '@']).next()?.split_once('_')?.1;
        (region.len() == 2 && region.chars().all(|c| c.is_ascii_alphabetic())).then(|| region.to_ascii_uppercase())
    })
}

fn signed_in(jar: &Jar) -> bool {
    jar.get("LOGIN_INFO").is_some() && sapisid(jar).is_some()
}

pub(crate) fn check_jar(jar: &Jar) -> Result<()> {
    if jar.is_empty() {
        bail!("No cookies found");
    }
    jar.get("LOGIN_INFO").context("Not signed in to YouTube: the LOGIN_INFO cookie is missing")?;
    sapisid(jar).context("The SAPISID cookie is missing")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signs_like_the_browser() {
        assert_eq!(
            sapisid_hash("abc/DEF-123", 1_700_000_000, ORIGIN),
            "SAPISIDHASH 1700000000_2f568e5165ecba49f0ad8ef012883d1978d235fb"
        );
    }

    #[test]
    fn signed_in_requests_carry_cookies_and_signature() {
        let dir = std::env::temp_dir().join(format!("tunebox-ytm-innertube-{}", std::process::id()));
        let file = dir.join("cookies.txt");
        let text = "# Netscape HTTP Cookie File\n\
            .youtube.com\tTRUE\t/\tTRUE\t1893456000\tSAPISID\tsynthetic/sapisid\n\
            #HttpOnly_.youtube.com\tTRUE\t/\tTRUE\t1893456000\tLOGIN_INFO\tsynthetic-login\n\
            .google.com\tTRUE\t/\tTRUE\t1893456000\tSID\tgoogle-only\n";
        tunebox_core::paths::write_private(&file, text.as_bytes()).unwrap();
        let api = InnerTube::new(Secret::File(file)).unwrap();
        assert!(api.signed_in());
        let headers: std::collections::HashMap<_, _> = api.headers(ClientKind::Web, None).into_iter().collect();
        assert_eq!(headers["Cookie"], "SAPISID=synthetic/sapisid; LOGIN_INFO=synthetic-login");
        assert!(headers["Authorization"].starts_with("SAPISIDHASH "), "{}", headers["Authorization"]);
        assert_eq!(headers["X-Goog-AuthUser"], "0", "a sign-in without an account acts as the first");
        assert!(!headers.contains_key("X-Goog-PageId"));
        assert_eq!(headers["Origin"], ORIGIN);
        std::fs::remove_dir_all(dir).unwrap();

        let signed_out = InnerTube::new(Secret::File(std::env::temp_dir().join("tunebox-no-such-file"))).unwrap();
        assert!(!signed_out.signed_in());
        let headers: std::collections::HashMap<_, _> = signed_out.headers(ClientKind::Web, None).into_iter().collect();
        assert_eq!(headers["Cookie"], "SOCS=CAI");
        assert!(!headers.contains_key("Authorization"));
        assert!(!headers.contains_key("X-Goog-AuthUser"));
    }

    #[test]
    fn requests_act_as_the_account_picked() {
        let dir = std::env::temp_dir().join(format!("tunebox-ytm-identity-{}", std::process::id()));
        let file = dir.join("cookies.txt");
        let brand = Identity { index: 1, page_id: Some("100000000000000000099".into()) };
        let text = Jar::from_header("SAPISID=a; LOGIN_INFO=b").to_text() + &brand.notes();
        tunebox_core::paths::write_private(&file, text.as_bytes()).unwrap();
        let api = InnerTube::new(Secret::File(file)).unwrap();
        assert_eq!(api.identity(), Some(brand.clone()));
        let headers: std::collections::HashMap<_, _> = api.headers(ClientKind::Web, None).into_iter().collect();
        assert_eq!(headers["X-Goog-AuthUser"], "1");
        assert_eq!(headers["X-Goog-PageId"], "100000000000000000099");
        let context = api.context(ClientKind::Web, api.identity().as_ref());
        assert_eq!(context["user"]["onBehalfOfUser"], "100000000000000000099");
        let other = Identity { index: 3, page_id: None };
        let headers: std::collections::HashMap<_, _> = api.headers(ClientKind::Web, Some(&other)).into_iter().collect();
        assert_eq!(headers["X-Goog-AuthUser"], "3");
        assert!(!headers.contains_key("X-Goog-PageId"));
        assert_eq!(api.context(ClientKind::Web, Some(&other))["user"], json!({}));
        assert_eq!(api.identity(), Some(brand));
        std::fs::remove_dir_all(dir).unwrap();

        let api = InnerTube::unsaved(Jar::from_header("SAPISID=a; LOGIN_INFO=b")).unwrap();
        assert_eq!(api.identity(), Some(Identity::default()));
        api.set_identity(Identity { index: 2, page_id: None });
        let headers: std::collections::HashMap<_, _> = api.headers(ClientKind::Web, None).into_iter().collect();
        assert_eq!(headers["X-Goog-AuthUser"], "2");
        let kept = api.sign_in_text().unwrap();
        assert_eq!(Identity::parse(&kept), Identity { index: 2, page_id: None });
        assert_eq!(Jar::parse(&kept).get("SAPISID"), Some("a"));
    }

    #[test]
    fn refreshed_cookies_never_undo_a_sign_out_or_another_sign_in() {
        let dir = std::env::temp_dir().join(format!("tunebox-ytm-keep-{}", std::process::id()));
        let file = dir.join("cookies.txt");
        let sign_in = |sapisid: &str| {
            let jar = Jar::from_header(&format!("SAPISID={sapisid}; LOGIN_INFO=x"));
            tunebox_core::paths::write_private(&file, jar.to_text().as_bytes()).unwrap();
        };
        let refresh = |api: InnerTube, value: &str| {
            let mut headers = HeaderMap::new();
            let cookie = format!("SIDCC={value}; Domain=.youtube.com; Path=/; Secure");
            headers.insert(reqwest::header::SET_COOKIE, cookie.parse().unwrap());
            api.keep_cookies(&headers);
            api.flush();
            api
        };
        let saved = || Jar::parse(&std::fs::read_to_string(&file).unwrap_or_default());

        sign_in("first");
        let api = refresh(InnerTube::new(Secret::File(file.clone())).unwrap(), "1");
        assert_eq!(saved().get("SIDCC"), Some("1"), "what YouTube refreshes is kept");
        let api = refresh(api, "2");
        assert_eq!(saved().get("SIDCC"), Some("2"), "also after the client wrote it itself");

        std::fs::remove_file(&file).unwrap();
        drop(refresh(api, "3"));
        assert!(!file.exists(), "signed out stays signed out");

        sign_in("first");
        let api = InnerTube::new(Secret::File(file.clone())).unwrap();
        sign_in("second");
        let api = refresh(api, "4");
        assert_eq!((saved().get("SAPISID"), saved().get("SIDCC")), (Some("second"), None));
        assert!(api.signed_in(), "the client goes on with its own cookies");
        drop(refresh(api, "5"));
        assert_eq!(saved().get("SAPISID"), Some("second"));

        let api = refresh(InnerTube::unsaved(Jar::from_header("SAPISID=tried; LOGIN_INFO=x")).unwrap(), "6");
        assert_eq!(api.jar().unwrap().get("SIDCC"), Some("6"));
        assert_eq!(saved().get("SAPISID"), Some("second"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn version_looks_like_the_sites() {
        let version = web_version();
        assert!(version.starts_with("1.20") && version.ends_with(".01.00") && version.len() == 16, "{version}");
    }

    #[test]
    fn a_sign_in_needs_login_info_and_sapisid() {
        let jar = Jar::from_header("SAPISID=a; LOGIN_INFO=b");
        assert!(check_jar(&jar).is_ok());
        assert!(check_jar(&Jar::from_header("SAPISID=a")).is_err());
        assert!(check_jar(&Jar::from_header("LOGIN_INFO=b")).is_err());
        assert_eq!(sapisid(&Jar::from_header("__Secure-3PAPISID=c; LOGIN_INFO=b")), Some("c"));
    }
}
