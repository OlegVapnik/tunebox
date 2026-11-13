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

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
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
    /// Whether the sign-in was read from the browser again in this run, once at most.
    refreshed: AtomicBool,
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

/// Whether YouTube has stopped taking the saved sign-in, rather than anything else going wrong.
fn refused(err: &anyhow::Error) -> bool {
    matches!(innertube::status(err), Some(reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN))
        || err.to_string().contains("doesn't accept the saved sign-in")
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
                refreshed: AtomicBool::new(false),
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

    /// Where a refusal can't be mended by reading the browser again — the sign-in was pasted,
    /// or saved by 0.5.2 or older, and says nothing about where it came from — says what to do.
    fn sign_in_hint(&self, err: anyhow::Error) -> anyhow::Error {
        if refused(&err) && self.inner.api.browser().is_none() {
            err.context(
                "This sign-in doesn't say which browser it came from, so it can't be read again: \
                 run `tunebox login ytmusic --force` once",
            )
        } else {
            err
        }
    }

    /// Reads the sign-in from the browser it came from again, once per run, when YouTube has
    /// stopped taking the cookies that were copied: a browser that goes on using its sign-in
    /// gets new cookies for it. They are kept only when they are still the same account's, told
    /// by its channel or handle; otherwise the sign-in before stays and asks to be made again.
    /// Whether the call that failed is worth repeating.
    async fn refresh_sign_in(&self, err: &anyhow::Error) -> bool {
        let (Some(paths), Some(spec)) = (&self.inner.paths, self.inner.api.browser()) else { return false };
        if !refused(err) {
            return false;
        }
        // Once per run: a browser that is shut, or signed out there, isn't asked again and again.
        if self.inner.refreshed.swap(true, Ordering::Relaxed) {
            return false;
        }
        let Some(identity) = self.inner.api.identity() else { return false };
        let was = self.inner.last_user.lock().unwrap().clone();
        let mut browser = auth::Browser { name: spec.clone(), spec };
        match auth::SignIn::refresh(paths, &mut browser, was.as_ref(), identity).await {
            Ok(Some(found)) => {
                if found.save(paths).is_err() {
                    return false;
                }
                match paths.ytmusic_cookies().read() {
                    Ok(Some(bytes)) => {
                        self.inner.api.replace_sign_in(&String::from_utf8_lossy(&bytes));
                        tracing::info!("the YouTube Music sign-in was read from {} again", browser.name);
                        true
                    }
                    Ok(None) => false,
                    Err(err) => {
                        tracing::warn!("reading the refreshed YouTube Music sign-in: {err:#}");
                        false
                    }
                }
            }
            Ok(None) => false,
            Err(err) => {
                tracing::debug!("the YouTube Music sign-in wasn't read from the browser again: {err:#}");
                false
            }
        }
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
