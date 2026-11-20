
mod account;
pub mod auth;
mod cookies;
pub mod doctor;
mod home;
mod innertube;
mod library;
mod link;
mod lyrics;
mod pages;
mod parse;
mod player;
mod search;
mod source;
mod watch;

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::Result;
use futures::Stream;
use serde_json::Value;
use tunebox_core::{
    artist::ArtistPage,
    catalog::{AlbumInfo, ArtistInfo, Page, PlaylistInfo, SearchResults, Track, User},
    home::Home,
    lyrics::{Lyrics, LyricsQuery},
    paths::Paths,
};

pub use crate::{
    account::{Account, Identity},
    link::{Playable, playable},
    player::Player,
};
use crate::{innertube::InnerTube, library::Library};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Liked,
    Playlist(String),
    Album(String),
}

#[derive(Clone)]
pub struct YtMusic {
    inner: Arc<Inner>,
}

struct Inner {
    api: InnerTube,
    http: reqwest::Client,
    library: Library,
    paths: Option<Paths>,
    last_refresh: Mutex<Option<Instant>>,
    refresh_failed: AtomicBool,
    last_user: Mutex<Option<User>>,
}

macro_rules! signed_in {
    ($self:expr, $call:expr) => {
        match $call.await {
            Err(err) if $self.refresh_sign_in(&err).await => $call.await,
            result => result,
        }
    };
}

const REFRESH_COOLDOWN: Duration = Duration::from_secs(30);

fn refresh_due(last: Option<Instant>, now: Instant) -> bool {
    last.is_none_or(|at| now.duration_since(at) >= REFRESH_COOLDOWN)
}

fn refused(err: &anyhow::Error) -> bool {
    let message = err.to_string();
    matches!(innertube::status(err), Some(reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN))
        || message.contains("doesn't accept the saved sign-in")
        || message.contains("sign-in has expired")
}

impl YtMusic {
    pub fn new(paths: &Paths) -> Result<Self> {
        Self::with_api(InnerTube::new(paths.ytmusic_cookies())?, Some(paths.clone()))
    }

    pub(crate) fn unsaved(jar: cookies::Jar) -> Result<Self> {
        Self::with_api(InnerTube::unsaved(jar)?, None)
    }

    fn with_api(api: InnerTube, paths: Option<Paths>) -> Result<Self> {
        let http = tunebox_core::http::client(concat!("tunebox/", env!("CARGO_PKG_VERSION"))).build()?;
        Ok(Self {
            inner: Arc::new(Inner {
                api,
                http,
                library: Library::default(),
                paths,
                last_refresh: Mutex::new(None),
                refresh_failed: AtomicBool::new(false),
                last_user: Mutex::new(None),
            }),
        })
    }

    pub fn signed_in(&self) -> bool {
        self.inner.api.signed_in()
    }

    pub async fn me(&self) -> Result<Option<User>> {
        if !self.signed_in() {
            return Ok(None);
        }
        self.refresh_now().await;
        match library::me(&self.inner.api).await {
            Ok(user) => {
                *self.inner.last_user.lock().unwrap() = Some(user.clone());
                Ok(Some(user))
            }
            Err(err) if self.refresh_sign_in(&err).await => library::me(&self.inner.api).await.map(Some),
            Err(err) => Err(self.sign_in_hint(err)),
        }
    }

    fn sign_in_hint(&self, err: anyhow::Error) -> anyhow::Error {
        if !refused(&err) {
            return err;
        }
        match self.inner.api.browser() {
            None => err.context(
                "This sign-in doesn't record its browser, so it can't be read again: run \
                 `tunebox login ytmusic` and it will re-read the cookies from the browser that \
                 opens links",
            ),
            Some(_) if self.inner.refresh_failed.load(Ordering::Relaxed) => err.context(
                "Couldn't refresh the YouTube Music sign-in from the browser: \
                 run `tunebox login ytmusic --force` once",
            ),
            Some(_) => err,
        }
    }

    async fn refresh_sign_in(&self, err: &anyhow::Error) -> bool {
        self.refresh_sign_in_inner(Some(err), false).await
    }

    pub async fn refresh_now(&self) -> bool {
        self.refresh_sign_in_inner(None, true).await
    }

    async fn refresh_sign_in_inner(&self, err: Option<&anyhow::Error>, force: bool) -> bool {
        let Some(paths) = &self.inner.paths else { return false };
        let browser = match self.inner.api.browser() {
            Some(spec) => Some(auth::Browser { name: spec.clone(), spec }),
            None => auth::default_browser(),
        };
        let Some(mut browser) = browser else { return false };
        let _ = force;
        if let Some(err) = err {
            if !refused(err) {
                return false;
            }
        }
        let allowed = {
            let mut last = self.inner.last_refresh.lock().unwrap();
            if !refresh_due(*last, Instant::now()) {
                false
            } else {
                *last = Some(Instant::now());
                true
            }
        };
        if !allowed {
            return false;
        }
        let identity = self.inner.api.identity().unwrap_or_default();
        let was = self.inner.last_user.lock().unwrap().clone();
        let outcome = match auth::SignIn::refresh(paths, &mut browser, was.as_ref(), identity).await {
            Ok(Some(found)) => match found.save(paths) {
                Ok(()) => match paths.ytmusic_cookies().read() {
                    Ok(Some(bytes)) => {
                        self.inner.api.replace_sign_in(&String::from_utf8_lossy(&bytes));
                        tracing::info!("the YouTube Music sign-in was read from {} again", browser.name);
                        Some(true)
                    }
                    Ok(None) => None,
                    Err(err) => {
                        tracing::warn!("reading the refreshed YouTube Music sign-in: {err:#}");
                        None
                    }
                },
                Err(err) => {
                    tracing::warn!("keeping the YouTube Music sign-in from the browser: {err:#}");
                    None
                }
            },
            Ok(None) => None,
            Err(err) => {
                tracing::debug!("the YouTube Music sign-in wasn't read from the browser again: {err:#}");
                None
            }
        };
        self.inner.refresh_failed.store(outcome.is_none(), Ordering::Relaxed);
        outcome.unwrap_or(false)
    }

    pub async fn account(&self) -> Result<Option<Account>> {
        signed_in!(self, account::current(&self.inner.api))
    }

    pub async fn home(&self, chip: Option<&str>) -> Result<Home> {
        signed_in!(self, home::home(&self.inner.api, chip))
    }

    pub async fn search(&self, query: &str) -> Result<SearchResults> {
        signed_in!(self, search::search(&self.inner.api, query))
    }

    pub async fn suggestions(&self, query: &str) -> Result<Vec<String>> {
        search::suggestions(&self.inner.api, query).await
    }

    pub async fn artist(&self, uri: &str) -> Result<ArtistPage> {
        signed_in!(self, pages::artist(&self.inner.api, uri))
    }

    pub async fn playlist_info(&self, uri: &str) -> Result<PlaylistInfo> {
        signed_in!(self, pages::playlist_info(&self.inner.api, uri))
    }

    pub async fn album_info(&self, uri: &str) -> Result<AlbumInfo> {
        let (album, playlist) = signed_in!(self, pages::album_info(&self.inner.api, uri))?;
        self.inner.library.remember_album(&album.uri, playlist);
        Ok(album)
    }

    pub fn tracks(&self, source: Source) -> impl Stream<Item = Result<Page<Track>>> + Send + 'static {
        pages::tracks(self.clone(), source)
    }

    pub async fn library_playlists(&self) -> Result<Vec<PlaylistInfo>> {
        signed_in!(self, library::playlists(&self.inner.api, &self.inner.library))
    }

    pub async fn saved_albums(&self) -> Result<Vec<AlbumInfo>> {
        signed_in!(self, library::albums(&self.inner.api, &self.inner.library))
    }

    pub async fn followed_artists(&self) -> Result<Vec<ArtistInfo>> {
        signed_in!(self, library::artists(&self.inner.api, &self.inner.library))
    }

    pub async fn liked_count(&self) -> Result<u32> {
        signed_in!(self, library::liked_count(&self.inner.api))
    }

    pub async fn tracks_by_uri(&self, uris: &[String]) -> Result<Vec<Track>> {
        signed_in!(self, watch::tracks_by_uri(&self.inner.api, uris))
    }

    pub async fn radio(&self, track_uri: &str) -> Result<Vec<Track>> {
        signed_in!(self, watch::radio(&self.inner.api, track_uri))
    }

    pub async fn lyrics(&self, query: &LyricsQuery<'_>) -> Result<Option<Lyrics>> {
        lyrics::lyrics(&self.inner.api, &self.inner.http, query).await
    }

    pub async fn check_saved(&self, uris: &[String]) -> Result<Vec<(String, bool)>> {
        signed_in!(self, library::check_saved(&self.inner.api, &self.inner.library, uris))
    }

    pub async fn set_saved(&self, uri: &str, saved: bool) -> Result<()> {
        signed_in!(self, library::set_saved(&self.inner.api, &self.inner.library, uri, saved))
    }

    pub async fn add_to_playlist(&self, playlist_uri: &str, uris: &[String]) -> Result<()> {
        signed_in!(self, library::add_to_playlist(&self.inner.api, playlist_uri, uris))
    }

    #[doc(hidden)]
    pub async fn raw(&self, endpoint: &str, body: Value) -> Result<Value> {
        self.inner.api.call(endpoint, body).await
    }

    #[doc(hidden)]
    pub async fn raw_android(&self, endpoint: &str, body: Value) -> Result<Value> {
        self.inner.api.call_as(innertube::ClientKind::Android, endpoint, body).await
    }

    pub(crate) fn api(&self) -> &InnerTube {
        &self.inner.api
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_is_told_apart_from_other_errors() {
        assert!(refused(&anyhow::anyhow!(
            "The YouTube Music sign-in has expired – run `tunebox login ytmusic` again"
        )));
        assert!(refused(&anyhow::anyhow!("YouTube doesn't accept the saved sign-in any more")));
        assert!(!refused(&anyhow::anyhow!("YouTube Music didn't answer (browse)")));
    }

    #[test]
    fn a_failed_refresh_is_retried_after_a_cooldown() {
        let now = Instant::now();
        assert!(refresh_due(None, now));
        assert!(!refresh_due(Some(now), now));
        assert!(!refresh_due(Some(now - REFRESH_COOLDOWN + Duration::from_secs(1)), now));
        assert!(refresh_due(Some(now - REFRESH_COOLDOWN), now));
    }
}
