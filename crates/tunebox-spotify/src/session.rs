use std::{fs, path::Path, time::Duration};

use anyhow::{Context, Result};
use librespot::{
    core::{authentication::Credentials, cache::Cache, config::SessionConfig, session::Session},
    oauth::OAuthClientBuilder,
};
use tunebox_core::paths::{Paths, write_private};

const OAUTH_REDIRECT: &str = "http://127.0.0.1:8975/login";
const AUDIO_CACHE_LIMIT: u64 = 1 << 30;
const STALE_TMP_AGE: Duration = Duration::from_secs(60 * 60);
const CORRUPTED: &str = "Stored streaming credentials corrupted – run `tunebox login`";

const OAUTH_SCOPES: &[&str] = &[
    "app-remote-control",
    "playlist-modify",
    "playlist-modify-private",
    "playlist-modify-public",
    "playlist-read",
    "playlist-read-collaborative",
    "playlist-read-private",
    "streaming",
    "ugc-image-upload",
    "user-follow-modify",
    "user-follow-read",
    "user-library-modify",
    "user-library-read",
    "user-modify",
    "user-modify-playback-state",
    "user-modify-private",
    "user-personalized",
    "user-read-birthdate",
    "user-read-currently-playing",
    "user-read-email",
    "user-read-play-history",
    "user-read-playback-position",
    "user-read-playback-state",
    "user-read-private",
    "user-read-recently-played",
    "user-top-read",
];

pub struct PendingSession {
    pub session: Session,
    pub credentials: Credentials,
    paths: Paths,
}

impl PendingSession {
    pub async fn connect(self) -> Result<Session> {
        self.session.connect(self.credentials, true).await.context("Connecting to Spotify failed")?;
        remember(&self.paths, &self.session)?;
        Ok(self.session)
    }
}

pub fn prepare(paths: &Paths) -> Result<PendingSession> {
    let cache = cache(paths, !paths.keychain)?;
    let credentials = stored_credentials(paths, &cache)?.context("Streaming not logged in – run `tunebox login`")?;
    Ok(PendingSession { session: Session::new(config(paths)?, Some(cache)), credentials, paths: paths.clone() })
}

pub fn is_logged_in(paths: &Paths) -> Result<bool> {
    Ok(stored_credentials(paths, &cache(paths, !paths.keychain)?)?.is_some())
}

pub fn is_premium(session: &Session) -> bool {
    session.get_user_attribute("product").is_some_and(|product| product == "premium")
}

pub fn remember(paths: &Paths, session: &Session) -> Result<()> {
    if !paths.keychain {
        return Ok(());
    }
    let secret = paths.streaming_credentials();
    let Some(bytes) = secret.read()? else { return Ok(()) };
    let mut stored: Credentials = serde_json::from_slice(&bytes).context(CORRUPTED)?;
    let auth_data = session.auth_data();
    if auth_data.is_empty() || auth_data == stored.auth_data {
        return Ok(());
    }
    stored.username = Some(session.username());
    stored.auth_data = auth_data;
    secret.write(&serde_json::to_vec(&stored)?)
}

fn stored_credentials(paths: &Paths, cache: &Cache) -> Result<Option<Credentials>> {
    if !paths.keychain {
        return Ok(cache.credentials());
    }
    match paths.streaming_credentials().read()? {
        Some(bytes) => Ok(Some(serde_json::from_slice(&bytes).context(CORRUPTED)?)),
        None => Ok(None),
    }
}

pub async fn login(paths: &Paths) -> Result<Session> {
    let config = config(paths)?;
    let token = OAuthClientBuilder::new(&config.client_id, OAUTH_REDIRECT, OAUTH_SCOPES.to_vec())
        .open_in_browser()
        .with_custom_message("tunebox: Streaming is connected. You can close this window.")
        .build()?
        .get_access_token_async()
        .await?;
    PendingSession {
        session: Session::new(config, Some(cache(paths, true)?)),
        credentials: Credentials::with_access_token(token.access_token),
        paths: paths.clone(),
    }
    .connect()
    .await
}

fn cache(paths: &Paths, credentials_file: bool) -> Result<Cache> {
    let session_dir = paths.session_dir();
    let audio_dir = paths.audio_cache();
    let credentials = credentials_file.then_some(&session_dir);
    Ok(Cache::new(credentials, Some(&session_dir), Some(&audio_dir), Some(AUDIO_CACHE_LIMIT))?)
}

fn config(paths: &Paths) -> Result<SessionConfig> {
    let mut config = SessionConfig::default();
    let path = paths.device_id();
    match fs::read_to_string(&path) {
        Ok(id) if !id.trim().is_empty() => config.device_id = id.trim().to_owned(),
        _ => write_private(&path, config.device_id.as_bytes())?,
    }
    let tmp = paths.audio_tmp();
    fs::create_dir_all(&tmp)?;
    remove_stale_tmp_files(&tmp);
    config.tmp_dir = tmp;
    Ok(config)
}

fn remove_stale_tmp_files(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let stale = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .is_ok_and(|modified| modified.elapsed().is_ok_and(|age| age > STALE_TMP_AGE));
        if stale {
            let _ = fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use super::*;

    #[test]
    fn removes_only_stale_tmp_files() {
        let dir = std::env::temp_dir().join(format!("tunebox-tmp-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let (old, fresh) = (dir.join("old"), dir.join("fresh"));
        fs::File::create(&old).unwrap().set_modified(SystemTime::now() - 2 * STALE_TMP_AGE).unwrap();
        fs::File::create(&fresh).unwrap();
        remove_stale_tmp_files(&dir);
        let left = (old.exists(), fresh.exists());
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(left, (false, true));
    }
}
