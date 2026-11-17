//! YouTube Music for tunebox: the catalog through InnerTube (the API music.youtube.com itself
//! uses), playback through mpv with stream URLs from yt-dlp. Signed out is a first-class mode:
//! browsing and playing work without an account; liking and the library need the cookies of
//! a browser that is signed in.

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

/// What `tracks` lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// The user's liked songs (`ytmusic:playlist:LM`).
    Liked,
    /// A playlist by URI.
    Playlist(String),
    /// An album by URI.
    Album(String),
}

/// The YouTube Music client. Cheap to clone; clones share the connection and the sign-in.
#[derive(Clone)]
pub struct YtMusic {
    inner: Arc<Inner>,
}

struct Inner {
    api: InnerTube,
    http: reqwest::Client,
    library: Library,
    /// Where the sign-in is kept, to read it from the browser again; `None` for one that isn't.
    paths: Option<Paths>,
    /// When the sign-in was last read from the browser again, so a failed try can be retried.
    last_refresh: Mutex<Option<Instant>>,
    /// Whether the last refresh was refused or couldn't be read: the message says so.
    refresh_failed: AtomicBool,
    /// The account the sign-in acted as, to tell another one's cookies apart.
    last_user: Mutex<Option<User>>,
}

/// A call that needs the sign-in, made once more when YouTube has stopped taking the cookies
/// and the browser gave fresh ones for the same account.
macro_rules! signed_in {
    ($self:expr, $call:expr) => {
        match $call.await {
            Err(err) if $self.refresh_sign_in(&err).await => $call.await,
            result => result,
        }
    };
}

/// How soon a failed refresh may be tried again: a browser's cookie store can be locked for a
/// moment, and a sign-in that comes back later shouldn't need a restart.
const REFRESH_COOLDOWN: Duration = Duration::from_secs(30);

/// Whether enough time has passed since the last try to read the browser again.
fn refresh_due(last: Option<Instant>, now: Instant) -> bool {
    last.is_none_or(|at| now.duration_since(at) >= REFRESH_COOLDOWN)
}

/// Whether YouTube has stopped taking the saved sign-in, rather than anything else going wrong.
fn refused(err: &anyhow::Error) -> bool {
    let message = err.to_string();
    matches!(innertube::status(err), Some(reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN))
        || message.contains("doesn't accept the saved sign-in")
        || message.contains("sign-in has expired")
}

impl YtMusic {
    /// Signs in with the cookies saved by `tunebox login ytmusic`, if any; signed out otherwise.
    pub fn new(paths: &Paths) -> Result<Self> {
        Self::with_api(InnerTube::new(paths.ytmusic_cookies())?, Some(paths.clone()))
    }

    /// Signed in with cookies that aren't kept, to try a sign-in before it replaces the one
    /// before.
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

    /// The signed-in account; `None` when signed out.
    pub async fn me(&self) -> Result<Option<User>> {
        if !self.signed_in() {
            return Ok(None);
        }
        match library::me(&self.inner.api).await {
            Ok(user) => {
                *self.inner.last_user.lock().unwrap() = Some(user.clone());
                Ok(Some(user))
            }
            Err(err) if self.refresh_sign_in(&err).await => library::me(&self.inner.api).await.map(Some),
            Err(err) => Err(self.sign_in_hint(err)),
        }
    }

    /// Where a refusal can't be mended by reading the browser again, says what to do: the
    /// sign-in was pasted, saved by 0.5.2 or older, or the browser's cookies no longer work.
    fn sign_in_hint(&self, err: anyhow::Error) -> anyhow::Error {
        if !refused(&err) {
            return err;
        }
        match self.inner.api.browser() {
            None => err.context(
                "This sign-in doesn't say which browser it came from, so it can't be read again: \
                 run `tunebox login ytmusic --force` once",
            ),
            Some(_) if self.inner.refresh_failed.load(Ordering::Relaxed) => err.context(
                "Couldn't refresh the YouTube Music sign-in from the browser: \
                 run `tunebox login ytmusic --force` once",
            ),
            Some(_) => err,
        }
    }

    /// Reads the sign-in from the browser it came from again, with a short cooldown, when
    /// YouTube has stopped taking the cookies that were copied: a browser that goes on using its
    /// sign-in gets new cookies for it. They are kept only when they are still the same account's,
    /// told by its channel or handle; otherwise the sign-in before stays. A failed try is retried
    /// later, so a locked cookie store or a browser that was shut for a moment doesn't need a
    /// restart. Whether the call that failed is worth repeating.
    async fn refresh_sign_in(&self, err: &anyhow::Error) -> bool {
        let (Some(paths), Some(spec)) = (&self.inner.paths, self.inner.api.browser()) else { return false };
        if !refused(err) {
            return false;
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
        let Some(identity) = self.inner.api.identity() else {
            self.inner.refresh_failed.store(true, Ordering::Relaxed);
            return false;
        };
        let was = self.inner.last_user.lock().unwrap().clone();
        let mut browser = auth::Browser { name: spec.clone(), spec };
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

    /// Which of the browser's accounts the sign-in acts as, with its address; `None` when
    /// signed out, or when YouTube no longer lists it.
    pub async fn account(&self) -> Result<Option<Account>> {
        signed_in!(self, account::current(&self.inner.api))
    }

    /// The Home feed's shelves, as music.youtube.com shows them. Home items have no kind for
    /// songs, so a song is its radio: a playlist (`ytmusic:playlist:RDAMVM<id>`) to open, whose
    /// `context_uri` is the song itself (`ytmusic:track:<id>`) to play. With a chip's ID, the
    /// Home of that chip.
    pub async fn home(&self, chip: Option<&str>) -> Result<Home> {
        signed_in!(self, home::home(&self.inner.api, chip))
    }

    /// Songs, albums, artists and playlists for `query`.
    pub async fn search(&self, query: &str) -> Result<SearchResults> {
        signed_in!(self, search::search(&self.inner.api, query))
    }

    /// Completions for a query being typed.
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

    /// The songs of a playlist, an album or the liked songs, a page at a time as YouTube sends
    /// them (100 per page for playlists). Every page but the last has `next_offset` set to the
    /// number of songs so far; `total` is the best count known by then.
    pub fn tracks(&self, source: Source) -> impl Stream<Item = Result<Page<Track>>> + Send + 'static {
        pages::tracks(self.clone(), source)
    }

    /// The playlists in the library, without Liked Music.
    pub async fn library_playlists(&self) -> Result<Vec<PlaylistInfo>> {
        signed_in!(self, library::playlists(&self.inner.api, &self.inner.library))
    }

    pub async fn saved_albums(&self) -> Result<Vec<AlbumInfo>> {
        signed_in!(self, library::albums(&self.inner.api, &self.inner.library))
    }

    /// The artists the user subscribed to.
    pub async fn followed_artists(&self) -> Result<Vec<ArtistInfo>> {
        signed_in!(self, library::artists(&self.inner.api, &self.inner.library))
    }

    pub async fn liked_count(&self) -> Result<u32> {
        signed_in!(self, library::liked_count(&self.inner.api))
    }

    /// Songs by URI, with their details, in the given order; unknown ones are left out.
    pub async fn tracks_by_uri(&self, uris: &[String]) -> Result<Vec<Track>> {
        signed_in!(self, watch::tracks_by_uri(&self.inner.api, uris))
    }

    /// A song's radio: the song, then what YouTube Music would play after it.
    pub async fn radio(&self, track_uri: &str) -> Result<Vec<Track>> {
        signed_in!(self, watch::radio(&self.inner.api, track_uri))
    }

    /// Time-synced lyrics where YouTube Music has them, else its plain ones, else LRCLIB's.
    pub async fn lyrics(&self, query: &LyricsQuery<'_>) -> Result<Option<Lyrics>> {
        lyrics::lyrics(&self.inner.api, &self.inner.http, query).await
    }

    /// Whether each URI is in the library: liked songs, saved albums and playlists, subscribed
    /// artists. Signed out, nothing is.
    pub async fn check_saved(&self, uris: &[String]) -> Result<Vec<(String, bool)>> {
        signed_in!(self, library::check_saved(&self.inner.api, &self.inner.library, uris))
    }

    /// Likes or unlikes a song, saves or removes an album or playlist, subscribes to or
    /// unsubscribes from an artist.
    pub async fn set_saved(&self, uri: &str, saved: bool) -> Result<()> {
        signed_in!(self, library::set_saved(&self.inner.api, &self.inner.library, uri, saved))
    }

    pub async fn add_to_playlist(&self, playlist_uri: &str, uris: &[String]) -> Result<()> {
        signed_in!(self, library::add_to_playlist(&self.inner.api, playlist_uri, uris))
    }

    /// A raw InnerTube call, for diagnostics and the `ytm` example.
    #[doc(hidden)]
    pub async fn raw(&self, endpoint: &str, body: Value) -> Result<Value> {
        self.inner.api.call(endpoint, body).await
    }

    /// [`YtMusic::raw`] as the Android app, which gets time-synced lyrics.
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
