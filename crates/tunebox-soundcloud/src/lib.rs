//! SoundCloud for tunebox: its public API for browsing and search, the `oauth_token` cookie
//! for the library and likes, and mpv for playback.
//!
//! SoundCloud's API needs a `client_id`, which its web player embeds in its JavaScript. It is
//! fetched from there, kept in the cache and refreshed when it stops working, as yt-dlp does.

pub mod browser;
pub mod doctor;
mod player;
mod source;

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use futures::future::join_all;
use serde_json::Value;
use tokio::sync::Mutex;
use tunebox_core::{
    artist::{ArtistPage, TopTrack},
    catalog::{AlbumInfo, ArtistInfo, Page, PlaylistInfo, SearchResults, Track, User},
    home::{Home, HomeItem, ItemKind, Shelf},
    lyrics::{Lyrics, LyricsQuery},
    paths::Paths,
};

pub use self::{player::Player, source::SoundCloudSource};

const API: &str = "https://api-v2.soundcloud.com";
/// The page and the API request at most this many songs at a time.
const PAGE: u32 = 50;
/// How long a `client_id` is used before the site is asked for a new one.
const CLIENT_ID_AGE: Duration = Duration::from_secs(6 * 60 * 60);
/// Enough for a long listening history, and then some.
const MAX_LIKES: usize = 5_000;

/// What `tracks` lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// The tracks the user liked.
    Liked,
    /// A playlist, by its `soundcloud:playlist:…` URI.
    Playlist(String),
    /// A playlist SoundCloud calls an album.
    Album(String),
}

/// The SoundCloud client. Cheap to clone; clones share the client ID and the sign-in.
#[derive(Clone)]
pub struct SoundCloud {
    inner: Arc<Inner>,
}

struct Inner {
    http: reqwest::Client,
    token: Option<String>,
    client_id: Mutex<Option<String>>,
    client_id_file: PathBuf,
    liked: Mutex<Option<HashSet<String>>>,
    /// Whether SoundCloud has refused the sign-in, so requests go out without it.
    expired: AtomicBool,
}

impl SoundCloud {
    /// Reads the sign-in from `tunebox login soundcloud`, if there is one; browsing and
    /// playing work signed out.
    pub fn new(paths: &Paths) -> Result<Self> {
        let token = match paths.soundcloud_token().read()? {
            Some(bytes) => Some(String::from_utf8(bytes).context("Stored SoundCloud sign-in corrupted")?),
            None => None,
        };
        Self::with_token(paths, token)
    }

    fn with_token(paths: &Paths, token: Option<String>) -> Result<Self> {
        let http = tunebox_core::http::client(concat!("tunebox/", env!("CARGO_PKG_VERSION"))).build()?;
        Ok(Self {
            inner: Arc::new(Inner {
                http,
                token,
                client_id: Mutex::new(None),
                client_id_file: paths.cache.join("soundcloud").join("client_id"),
                liked: Mutex::new(None),
                expired: AtomicBool::new(false),
            }),
        })
    }

    pub fn signed_in(&self) -> bool {
        self.inner.token.is_some() && !self.expired()
    }

    /// Whether a saved sign-in is there but SoundCloud won't take it any more, which asks for
    /// `tunebox login soundcloud` again.
    pub fn sign_in_expired(&self) -> bool {
        self.inner.token.is_some() && self.expired()
    }

    fn expired(&self) -> bool {
        self.inner.expired.load(Ordering::Relaxed)
    }

    /// The sign-in to put on a request; nothing once SoundCloud has refused it.
    fn bearer(&self) -> Option<&str> {
        self.signed_in().then(|| self.inner.token.as_deref()).flatten()
    }

    /// Checks an `oauth_token` cookie and saves it when SoundCloud accepts it.
    pub async fn login_with_token(paths: &Paths, token: String) -> Result<User> {
        let token = token.trim().to_owned();
        if token.is_empty() {
            bail!("The oauth_token cookie is empty");
        }
        let client = Self::with_token(paths, Some(token.clone()))?;
        let me = client.me().await?.context("SoundCloud didn't accept this oauth_token")?;
        paths.soundcloud_token().write(token.as_bytes())?;
        Ok(me)
    }

    /// The account, for a signed-in client.
    pub async fn me(&self) -> Result<Option<User>> {
        if !self.signed_in() {
            return Ok(None);
        }
        let value = self.get("/me", &[]).await?;
        Ok(Some(user(&value)?))
    }

    async fn me_required(&self) -> Result<(String, User)> {
        let me = self.me().await?.context("Not signed in to SoundCloud – run `tunebox login soundcloud`")?;
        Ok((me.id.clone(), me))
    }

    /// What to try after SoundCloud refused a request, or nothing when it is no use asking
    /// again: first a new client ID, then no sign-in at all.
    fn refused(&self, attempt: u32, status: u16) -> Option<u32> {
        if !matches!(status, 401 | 403) {
            return None;
        }
        match attempt {
            // A client ID that has gone stale is the usual reason for a refusal.
            0 => Some(1),
            // …and when a new one doesn't help, it is the sign-in SoundCloud won't take, so
            // the public API is asked without it: browsing and playing keep working.
            1 if self.inner.token.is_some() && !self.expired() => {
                self.inner.expired.store(true, Ordering::Relaxed);
                Some(2)
            }
            _ => None,
        }
    }

    pub async fn home(&self, _chip: Option<&str>) -> Result<Home> {
        let mut shelves = Vec::new();
        if let Ok(trending) = self.trending().await {
            if !trending.is_empty() {
                let context = songs_context(&trending);
                shelves.push(Shelf {
                    uri: "soundcloud:trending".into(),
                    title: "Trending now".into(),
                    strapline: "What SoundCloud plays most this week".into(),
                    shortcuts: false,
                    songs: true,
                    play_all: Some(context),
                    items: trending.iter().map(song_item).collect(),
                });
            }
        }
        if let Ok((playlists, _)) = self.search_playlists("popular", 24).await {
            if !playlists.is_empty() {
                shelves.push(Shelf {
                    uri: "soundcloud:search:popular".into(),
                    title: "Playlists to try".into(),
                    strapline: String::new(),
                    shortcuts: false,
                    songs: false,
                    play_all: None,
                    items: playlists
                        .iter()
                        .map(|playlist| HomeItem {
                            uri: playlist.uri.clone(),
                            kind: ItemKind::Playlist,
                            title: playlist.name.clone(),
                            subtitle: playlist.owner_name.clone(),
                            image_url: playlist.image_url.clone(),
                            owner_id: playlist.owner_id.clone(),
                            owner_name: playlist.owner_name.clone(),
                            context_uri: playlist.uri.clone(),
                        })
                        .collect(),
                });
            }
        }
        if let Ok((_, albums)) = self.search_playlists("album", 24).await {
            if !albums.is_empty() {
                shelves.push(Shelf {
                    uri: "soundcloud:search:album".into(),
                    title: "Albums on SoundCloud".into(),
                    strapline: String::new(),
                    shortcuts: false,
                    songs: false,
                    play_all: None,
                    items: albums
                        .iter()
                        .map(|album| HomeItem {
                            uri: album.uri.clone(),
                            kind: ItemKind::Album,
                            title: album.name.clone(),
                            subtitle: album.artists.join(", "),
                            image_url: album.image_url.clone(),
                            owner_id: String::new(),
                            owner_name: String::new(),
                            context_uri: album.uri.clone(),
                        })
                        .collect(),
                });
            }
        }
        Ok(Home { greeting: "SoundCloud".into(), chips: Vec::new(), shelves })
    }

    pub async fn search(&self, query: &str) -> Result<SearchResults> {
        let tracks = self.search_tracks(query, 20).await?;
        let (playlists, albums) = self.search_playlists(query, 20).await?;
        let artists = self.search_artists(query, 20).await?;
        Ok(SearchResults { tracks, albums, artists, playlists })
    }

    async fn search_tracks(&self, query: &str, limit: u32) -> Result<Vec<Track>> {
        let value = self.get("/search/tracks", &[("q", query), ("limit", &limit.to_string())]).await?;
        Ok(collection(&value).filter_map(track).collect())
    }

    async fn search_artists(&self, query: &str, limit: u32) -> Result<Vec<ArtistInfo>> {
        let value = self.get("/search/users", &[("q", query), ("limit", &limit.to_string())]).await?;
        Ok(collection(&value).filter_map(artist).collect())
    }

    async fn search_playlists(&self, query: &str, limit: u32) -> Result<(Vec<PlaylistInfo>, Vec<AlbumInfo>)> {
        let value = self.get("/search/playlists", &[("q", query), ("limit", &limit.to_string())]).await?;
        let mut playlists = Vec::new();
        let mut albums = Vec::new();
        for value in collection(&value) {
            if value["is_album"].as_bool().unwrap_or(false) {
                if let Some(album) = album(value) {
                    albums.push(album);
                }
            } else if let Some(playlist) = playlist(value) {
                playlists.push(playlist);
            }
        }
        Ok((playlists, albums))
    }

    async fn trending(&self) -> Result<Vec<Track>> {
        let value = self
            .get("/charts", &[("kind", "trending"), ("genre", "soundcloud:genres:all-music"), ("limit", "30")])
            .await?;
        Ok(collection(&value).filter_map(|item| item.get("track").and_then(track).or_else(|| track(item))).collect())
    }

    /// The user's own playlists. Signed out, none.
    pub async fn playlists(&self) -> Result<Vec<PlaylistInfo>> {
        if !self.signed_in() {
            return Ok(Vec::new());
        }
        let (id, _) = self.me_required().await?;
        let mut url = format!("{API}/users/{id}/playlists?limit={PAGE}&linked_partitioning=1");
        let mut playlists = Vec::new();
        for _ in 0..20 {
            let value = self.get_url(&url).await?;
            playlists.extend(collection(&value).filter_map(playlist));
            match next(&value) {
                Some(found) => url = found,
                None => break,
            }
        }
        Ok(playlists)
    }

    pub async fn liked_count(&self) -> Result<u32> {
        if !self.signed_in() {
            return Ok(0);
        }
        let (id, _) = self.me_required().await?;
        let value = self.get(&format!("/users/{id}"), &[]).await?;
        Ok(value["likes_count"].as_u64().unwrap_or(0).min(u64::from(u32::MAX)) as u32)
    }

    pub async fn followed_artists(&self) -> Result<Vec<ArtistInfo>> {
        if !self.signed_in() {
            return Ok(Vec::new());
        }
        let (id, _) = self.me_required().await?;
        let value = self.get(&format!("/users/{id}/followings"), &[("limit", "50")]).await?;
        Ok(collection(&value).filter_map(artist).collect())
    }

    pub async fn artist(&self, uri: &str) -> Result<ArtistPage> {
        let value = self.resolve(uri).await?;
        let path = path_of(&value).ok_or_else(|| anyhow!("SoundCloud artist without a page"))?;
        let id = value["id"].as_u64().ok_or_else(|| anyhow!("SoundCloud artist without an ID"))?;
        let tracks = self.get(&format!("/users/{id}/tracks"), &[("limit", "24")]).await?;
        let top_tracks: Vec<TopTrack> =
            collection(&tracks).filter_map(track).map(|track| TopTrack { track, playcount: None }).collect();
        let playlists = self.get(&format!("/users/{id}/playlists"), &[("limit", "24")]).await?;
        let items: Vec<HomeItem> = collection(&playlists)
            .filter_map(playlist)
            .map(|playlist| HomeItem {
                uri: playlist.uri.clone(),
                kind: ItemKind::Playlist,
                title: playlist.name.clone(),
                subtitle: playlist.owner_name.clone(),
                image_url: playlist.image_url.clone(),
                owner_id: String::new(),
                owner_name: String::new(),
                context_uri: playlist.uri.clone(),
            })
            .collect();
        let shelves = if items.is_empty() {
            Vec::new()
        } else {
            vec![Shelf {
                uri: format!("soundcloud:artist:{path}:playlists"),
                title: "Playlists".into(),
                strapline: String::new(),
                shortcuts: false,
                songs: false,
                play_all: None,
                items,
            }]
        };
        Ok(ArtistPage {
            uri: format!("soundcloud:artist:{path}"),
            name: value["username"].as_str().unwrap_or_default().to_owned(),
            image_url: value["avatar_url"].as_str().map(artwork),
            monthly_listeners: None,
            followers: value["followers_count"].as_u64(),
            saved: false,
            top_tracks,
            shelves,
        })
    }

    /// What a page is: `track`, `playlist`, `user`; and whether a playlist SoundCloud calls
    /// an album.
    pub async fn kind(&self, path: &str) -> Result<(String, bool)> {
        let value = self.resolve(&format!("https://soundcloud.com/{path}")).await?;
        Ok((value["kind"].as_str().unwrap_or_default().to_owned(), value["is_album"].as_bool().unwrap_or(false)))
    }

    pub async fn playlist_info(&self, uri: &str) -> Result<PlaylistInfo> {
        playlist(&self.resolve(uri).await?).ok_or_else(|| anyhow!("SoundCloud playlist not found"))
    }

    pub async fn album_info(&self, uri: &str) -> Result<AlbumInfo> {
        album(&self.resolve(uri).await?).ok_or_else(|| anyhow!("SoundCloud album not found"))
    }

    /// Every song of a context, in order.
    pub async fn tracks(&self, source: Source) -> Result<Vec<Track>> {
        match source {
            Source::Liked => self.liked_tracks().await,
            Source::Playlist(uri) | Source::Album(uri) => {
                let value = self.resolve(&uri).await?;
                let mut tracks = self.playlist_tracks(&value).await?;
                // A track's list is its radio, which playback asks for separately.
                if tracks.is_empty() && value["kind"].as_str() == Some("track") {
                    tracks.extend(track(&value));
                }
                Ok(tracks)
            }
        }
    }

    /// A context's songs in pages, for the interface.
    pub async fn pages(&self, source: Source) -> Result<Vec<Page<Track>>> {
        let tracks = self.tracks(source).await?;
        let total = tracks.len() as u32;
        let pages: Vec<Page<Track>> = tracks
            .chunks(PAGE as usize)
            .enumerate()
            .map(|(index, chunk)| Page {
                items: chunk.to_vec(),
                total,
                next_offset: (((index + 1) as u32 * PAGE) < total).then_some((index as u32 + 1) * PAGE),
            })
            .collect();
        Ok(pages)
    }

    pub async fn track_by_uri(&self, uri: &str) -> Result<Option<Track>> {
        let path = uri_path(uri).unwrap_or(uri);
        let value = self.resolve(&format!("https://soundcloud.com/{path}")).await?;
        Ok(track(&value))
    }

    pub async fn tracks_by_uri(&self, uris: &[String]) -> Result<Vec<Track>> {
        let found = join_all(uris.iter().map(|uri| async move { self.track_by_uri(uri).await })).await;
        let mut tracks = Vec::new();
        for (uri, found) in uris.iter().zip(found) {
            match found {
                Ok(Some(track)) => tracks.push(track),
                Ok(None) => tracing::warn!("{uri} isn't on SoundCloud any more"),
                Err(err) => tracing::warn!("{uri}: {err:#}"),
            }
        }
        Ok(tracks)
    }

    /// Which of `uris` the user liked. Only tracks can be liked; signed out, nothing is.
    pub async fn check_saved(&self, uris: &[String]) -> Result<Vec<(String, bool)>> {
        if !self.signed_in() {
            return Ok(uris.iter().map(|uri| (uri.clone(), false)).collect());
        }
        let liked = self.liked_keys().await?;
        Ok(uris.iter().map(|uri| (uri.clone(), uri_path(uri).is_some_and(|path| liked.contains(path)))).collect())
    }

    pub async fn set_saved(&self, uri: &str, saved: bool) -> Result<()> {
        let path = uri_path(uri).context("SoundCloud URIs such as soundcloud:track:artist/song")?;
        let value = self.resolve(&format!("https://soundcloud.com/{path}")).await?;
        if value["kind"].as_str() != Some("track") {
            bail!("SoundCloud can only like tracks");
        }
        let id = value["id"].as_u64().ok_or_else(|| anyhow!("SoundCloud track without an ID"))?;
        let method = if saved { reqwest::Method::POST } else { reqwest::Method::DELETE };
        self.request(method, &format!("/likes/tracks/{id}"), &[], None).await?;
        *self.inner.liked.lock().await = None;
        Ok(())
    }

    /// SoundCloud can't add to someone else's playlist through the API tunebox uses.
    pub async fn add_to_playlist(&self, _playlist: &str, _uris: &[String]) -> Result<()> {
        bail!("SoundCloud doesn't let tunebox change its playlists")
    }

    pub async fn lyrics(&self, query: &LyricsQuery<'_>) -> Result<Option<Lyrics>> {
        tunebox_core::lyrics::lrclib(&self.inner.http, query).await
    }

    /// A playable URL for a track page, trying the progressive stream first and HLS after.
    pub async fn stream_url(&self, path: &str) -> Result<String> {
        let value = self.resolve(&format!("https://soundcloud.com/{path}")).await?;
        if !value["streamable"].as_bool().unwrap_or(true) {
            bail!("This track isn't streamable on SoundCloud");
        }
        let transcodings = value
            .pointer("/media/transcodings")
            .and_then(Value::as_array)
            .context("SoundCloud has no stream for this track")?;
        let authorization = value["track_authorization"].as_str().unwrap_or_default().to_owned();
        let client_id = self.client_id(false).await?;
        // Progressive plays anywhere; HLS needs mpv's manifest support. Within each, best
        // quality first. SoundCloud's abr_sq stream is sometimes refused, so each is tried
        // in turn rather than only the preferred one.
        let mut chosen: Vec<&Value> =
            transcodings.iter().filter(|item| item["snipped"].as_bool() != Some(true)).collect();
        chosen.sort_by(|a, b| {
            let key = |item: &Value| {
                let protocol = item.pointer("/format/protocol").and_then(Value::as_str).unwrap_or_default();
                let quality = item["quality"].as_str().unwrap_or_default();
                (protocol != "progressive", quality != "sq")
            };
            key(a).cmp(&key(b))
        });
        let mut last_err = None;
        for item in chosen {
            let Some(url) = item["url"].as_str() else { continue };
            let url = format!("{url}?client_id={client_id}&track_authorization={authorization}");
            match self.get_url(&url).await {
                Ok(value) => {
                    return value["url"].as_str().map(str::to_owned).context("SoundCloud didn't give a stream URL");
                }
                Err(err) => {
                    tracing::debug!("{url}: {err:#}");
                    last_err = Some(err);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow!("SoundCloud has no stream for this track")))
    }

    /// Songs SoundCloud plays after this one.
    pub async fn related(&self, path: &str) -> Result<Vec<Track>> {
        let value = self.resolve(&format!("https://soundcloud.com/{path}")).await?;
        let id = value["id"].as_u64().ok_or_else(|| anyhow!("SoundCloud track without an ID"))?;
        let value = self.get(&format!("/tracks/{id}/related"), &[("limit", "20")]).await?;
        Ok(collection(&value).filter_map(track).collect())
    }

    async fn playlist_tracks(&self, playlist: &Value) -> Result<Vec<Track>> {
        let raw = playlist["tracks"].as_array().cloned().unwrap_or_default();
        let mut slots: Vec<Option<Track>> = Vec::with_capacity(raw.len());
        let mut wanted = Vec::new();
        for item in &raw {
            match track(item) {
                Some(found) => slots.push(Some(found)),
                None => {
                    slots.push(None);
                    if let Some(id) = item["id"].as_u64() {
                        wanted.push(id);
                    }
                }
            }
        }
        let mut by_id: HashMap<u64, Track> = HashMap::new();
        for chunk in wanted.chunks(PAGE as usize) {
            let ids = chunk.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
            let value = self.get("/tracks", &[("ids", &ids)]).await?;
            for value in collection(&value) {
                if let (Some(id), Some(found)) = (value["id"].as_u64(), track(value)) {
                    by_id.insert(id, found);
                }
            }
        }
        Ok(raw
            .iter()
            .zip(slots)
            .filter_map(|(item, slot)| slot.or_else(|| item["id"].as_u64().and_then(|id| by_id.remove(&id))))
            .collect())
    }

    async fn liked_tracks(&self) -> Result<Vec<Track>> {
        let (id, _) = self.me_required().await?;
        let mut url = format!("{API}/users/{id}/likes?limit={PAGE}&linked_partitioning=1");
        let mut tracks = Vec::new();
        while tracks.len() < MAX_LIKES {
            let value = self.get_url(&url).await?;
            for item in collection(&value) {
                let found = item.get("track").and_then(track).or_else(|| track(item));
                if let Some(found) = found {
                    tracks.push(found);
                }
            }
            match next(&value) {
                Some(found) => url = found,
                None => break,
            }
        }
        Ok(tracks)
    }

    async fn liked_keys(&self) -> Result<HashSet<String>> {
        if let Some(liked) = self.inner.liked.lock().await.clone() {
            return Ok(liked);
        }
        let liked: HashSet<String> =
            self.liked_tracks().await?.iter().filter_map(|track| uri_path(&track.uri).map(str::to_owned)).collect();
        *self.inner.liked.lock().await = Some(liked.clone());
        Ok(liked)
    }

    /// The public API, with the client ID SoundCloud's site uses.
    async fn get(&self, path: &str, query: &[(&str, &str)]) -> Result<Value> {
        self.request(reqwest::Method::GET, path, query, None).await
    }

    async fn get_url(&self, url: &str) -> Result<Value> {
        let mut attempt = 0;
        loop {
            let client_id = self.client_id(attempt > 0).await?;
            let sep = if url.contains('?') { '&' } else { '?' };
            let mut request = self.inner.http.get(format!("{url}{sep}client_id={client_id}"));
            if let Some(token) = self.bearer() {
                request = request.header(reqwest::header::AUTHORIZATION, format!("OAuth {token}"));
            }
            let response = request.send().await.context("Can't reach SoundCloud")?;
            let status = response.status();
            if status.is_success() {
                return response.json().await.context("SoundCloud sent something unexpected");
            }
            let text = response.text().await.unwrap_or_default();
            attempt = self
                .refused(attempt, status.as_u16())
                .ok_or_else(|| anyhow!("SoundCloud: {status} {}", first_line(&text)))?;
        }
    }

    async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<Value>,
    ) -> Result<Value> {
        let mut attempt = 0;
        loop {
            let client_id = self.client_id(attempt > 0).await?;
            let mut request = self
                .inner
                .http
                .request(method.clone(), format!("{API}{path}"))
                .query(&[("client_id", client_id.as_str())])
                .query(query);
            if let Some(token) = self.bearer() {
                request = request.header(reqwest::header::AUTHORIZATION, format!("OAuth {token}"));
            }
            if let Some(body) = &body {
                request = request.json(body);
            }
            let response = request.send().await.context("Can't reach SoundCloud")?;
            let status = response.status();
            if status.is_success() {
                let text = response.text().await.context("SoundCloud sent nothing")?;
                if text.trim().is_empty() {
                    return Ok(Value::Null);
                }
                return serde_json::from_str(&text).context("SoundCloud sent something unexpected");
            }
            let text = response.text().await.unwrap_or_default();
            attempt = self
                .refused(attempt, status.as_u16())
                .ok_or_else(|| anyhow!("SoundCloud: {status} {}", first_line(&text)))?;
        }
    }

    async fn resolve(&self, uri: &str) -> Result<Value> {
        let page = if let Some(path) = uri_path(uri) {
            format!("https://soundcloud.com/{path}")
        } else if uri.starts_with("https://") {
            uri.to_owned()
        } else {
            bail!("Not a SoundCloud link: {uri}");
        };
        self.get("/resolve", &[("url", &page)]).await
    }

    /// A `client_id` from the cache, the environment, or the site's JavaScript.
    async fn client_id(&self, refresh: bool) -> Result<String> {
        if !refresh {
            if let Some(id) = self.inner.client_id.lock().await.clone() {
                return Ok(id);
            }
            if let Ok(id) = std::env::var("TUNEBOX_SOUNDCLOUD_CLIENT_ID") {
                if id.len() == 32 {
                    *self.inner.client_id.lock().await = Some(id.clone());
                    return Ok(id);
                }
            }
            if let Some(id) = self.cached_client_id() {
                *self.inner.client_id.lock().await = Some(id.clone());
                return Ok(id);
            }
        }
        let id = self.discover_client_id().await?;
        *self.inner.client_id.lock().await = Some(id.clone());
        self.cache_client_id(&id);
        Ok(id)
    }

    fn cached_client_id(&self) -> Option<String> {
        let modified = std::fs::metadata(&self.inner.client_id_file).and_then(|meta| meta.modified()).ok()?;
        if modified.elapsed().ok()? > CLIENT_ID_AGE {
            return None;
        }
        let id = std::fs::read_to_string(&self.inner.client_id_file).ok()?.trim().to_owned();
        (id.len() == 32).then_some(id)
    }

    fn cache_client_id(&self, id: &str) {
        if let Some(dir) = self.inner.client_id_file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&self.inner.client_id_file, id);
    }

    async fn discover_client_id(&self) -> Result<String> {
        let page = self
            .inner
            .http
            .get("https://soundcloud.com/")
            .header(reqwest::header::USER_AGENT, "Mozilla/5.0")
            .send()
            .await
            .context("Can't open soundcloud.com to find its client ID")?
            .text()
            .await
            .unwrap_or_default();
        let scripts = script_urls(&page);
        if scripts.is_empty() {
            bail!("SoundCloud's page has no scripts to look for a client ID in");
        }
        for batch in scripts.rchunks(6) {
            let bodies = join_all(batch.iter().map(|url| async move {
                self.inner
                    .http
                    .get(url)
                    .header(reqwest::header::USER_AGENT, "Mozilla/5.0")
                    .send()
                    .await
                    .ok()?
                    .text()
                    .await
                    .ok()
            }))
            .await;
            for body in bodies.into_iter().flatten() {
                if let Some(id) = client_id_in(&body) {
                    return Ok(id);
                }
            }
        }
        bail!("SoundCloud didn't say its client ID; set TUNEBOX_SOUNDCLOUD_CLIENT_ID")
    }
}

impl std::fmt::Debug for SoundCloud {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SoundCloud").field("signed_in", &self.signed_in()).finish()
    }
}

fn collection(value: &Value) -> impl Iterator<Item = &Value> {
    value["collection"].as_array().into_iter().flatten()
}

fn next(value: &Value) -> Option<String> {
    value["next_href"].as_str().filter(|next| !next.is_empty()).map(str::to_owned)
}

fn first_line(text: &str) -> String {
    text.lines().find(|line| !line.trim().is_empty()).unwrap_or_default().trim().to_owned()
}

/// The path of a SoundCloud URI or page: `artist/song`, `artist/sets/playlist`, `artist`.
pub fn uri_path(uri: &str) -> Option<&str> {
    let path = if let Some(rest) = uri.strip_prefix("soundcloud:") {
        match rest.split_once(':') {
            Some((kind, path)) if matches!(kind, "track" | "playlist" | "album" | "artist" | "songs") => path,
            _ => return None,
        }
    } else if uri.contains("://") {
        let site = uri.strip_prefix("https://").or_else(|| uri.strip_prefix("http://"))?;
        site.strip_prefix("soundcloud.com/").or_else(|| site.strip_prefix("www.soundcloud.com/"))?
    } else if uri.contains(':') {
        return None;
    } else {
        uri
    };
    let path = path.split(['?', '#']).next().unwrap_or(path).trim_matches('/');
    (!path.is_empty()).then_some(path)
}

fn path_of(value: &Value) -> Option<String> {
    let url = value["permalink_url"].as_str()?;
    uri_path(url).map(str::to_owned)
}

fn track(value: &Value) -> Option<Track> {
    if value["kind"].as_str().is_some_and(|kind| kind != "track") {
        return None;
    }
    let path = path_of(value)?;
    let user = &value["user"];
    let artist = user["username"].as_str().unwrap_or_default();
    let artist_path = user["permalink_url"].as_str().and_then(uri_path).map(str::to_owned);
    let album = value.pointer("/publisher_metadata/album_title").and_then(Value::as_str).unwrap_or_default();
    Some(Track {
        uri: format!("soundcloud:track:{path}"),
        name: value["title"].as_str().unwrap_or_default().to_owned(),
        artists: (!artist.is_empty()).then(|| artist.to_owned()).into_iter().collect(),
        artist_uris: artist_path.into_iter().map(|path| format!("soundcloud:artist:{path}")).collect(),
        album: album.to_owned(),
        album_uri: String::new(),
        duration_ms: value["full_duration"]
            .as_u64()
            .or_else(|| value["duration"].as_u64())
            .unwrap_or(0)
            .min(u64::from(u32::MAX)) as u32,
        image_url: value["artwork_url"].as_str().map(artwork),
    })
}

fn playlist(value: &Value) -> Option<PlaylistInfo> {
    if value["kind"].as_str().is_some_and(|kind| kind != "playlist") {
        return None;
    }
    let path = path_of(value)?;
    let user = &value["user"];
    Some(PlaylistInfo {
        id: value["id"].as_u64().map(|id| id.to_string()).unwrap_or_default(),
        uri: format!("soundcloud:playlist:{path}"),
        name: value["title"].as_str().unwrap_or_default().to_owned(),
        owner_id: user["id"].as_u64().map(|id| id.to_string()).unwrap_or_default(),
        owner_name: user["username"].as_str().unwrap_or_default().to_owned(),
        collaborative: false,
        total: value["track_count"].as_u64().map(|count| count.min(u64::from(u32::MAX)) as u32),
        image_url: value["artwork_url"].as_str().map(artwork),
    })
}

fn album(value: &Value) -> Option<AlbumInfo> {
    let path = path_of(value)?;
    let user = &value["user"];
    Some(AlbumInfo {
        id: value["id"].as_u64().map(|id| id.to_string()).unwrap_or_default(),
        uri: format!("soundcloud:album:{path}"),
        name: value["title"].as_str().unwrap_or_default().to_owned(),
        artists: vec![user["username"].as_str().unwrap_or_default().to_owned()],
        year: value["release_date"]
            .as_str()
            .or_else(|| value["created_at"].as_str())
            .and_then(|date| date.get(..4))
            .unwrap_or_default()
            .to_owned(),
        total: value["track_count"].as_u64().unwrap_or(0).min(u64::from(u32::MAX)) as u32,
        image_url: value["artwork_url"].as_str().map(artwork),
    })
}

fn artist(value: &Value) -> Option<ArtistInfo> {
    let path = path_of(value)?;
    Some(ArtistInfo {
        uri: format!("soundcloud:artist:{path}"),
        name: value["username"].as_str().unwrap_or_default().to_owned(),
        image_url: value["avatar_url"].as_str().map(artwork),
    })
}

fn user(value: &Value) -> Result<User> {
    let id = value["id"]
        .as_u64()
        .map(|id| id.to_string())
        .or_else(|| value["id"].as_str().map(str::to_owned))
        .context("SoundCloud's account has no ID")?;
    Ok(User { id, name: value["username"].as_str().unwrap_or_default().to_owned() })
}

fn song_item(track: &Track) -> HomeItem {
    HomeItem {
        uri: track.uri.clone(),
        kind: ItemKind::Playlist,
        title: track.name.clone(),
        subtitle: track.artists.join(", "),
        image_url: track.image_url.clone(),
        owner_id: String::new(),
        owner_name: String::new(),
        // A song plays with its radio after it, as in SoundCloud's own app.
        context_uri: track.uri.clone(),
    }
}

fn songs_context(tracks: &[Track]) -> String {
    let paths: Vec<_> = tracks.iter().filter_map(|track| uri_path(&track.uri)).collect();
    format!("soundcloud:songs:{}", paths.join(","))
}

/// Asks for a bigger cover than the one lists show.
fn artwork(url: &str) -> String {
    url.replace("-large.", "-t500x500.").replace("-badge.", "-t500x500.")
}

/// The page's scripts, from its HTML.
fn script_urls(page: &str) -> Vec<String> {
    let mut urls = Vec::new();
    let mut rest = page;
    while let Some(start) = rest.find("<script") {
        rest = &rest[start..];
        if let Some(src) = rest.find("src=\"") {
            let after = &rest[src + 5..];
            if let Some(end) = after.find('"') {
                let url = &after[..end];
                if url.starts_with("https://") && url.contains("sndcdn.com") && url.ends_with(".js") {
                    urls.push(url.to_owned());
                }
            }
        }
        rest = &rest[7..];
    }
    urls
}

/// The `client_id` in a JavaScript asset, as in `client_id:"0123…"`.
fn client_id_in(script: &str) -> Option<String> {
    let mut rest = script;
    while let Some(start) = rest.find("client_id") {
        rest = &rest[start + "client_id".len()..];
        let Some(colon) = rest.find([':', '=']) else { continue };
        let after = rest[colon + 1..].trim_start();
        let after = after.trim_start_matches(['"', '\'']).trim_start();
        let id: String = after.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
        if id.len() == 32 {
            return Some(id);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_client_id_in_assets() {
        assert_eq!(
            client_id_in(r#"{"client_id":"c0lcDb60e4gNCrZY24mdmz9uWo1Ih29e","x":1}"#),
            Some("c0lcDb60e4gNCrZY24mdmz9uWo1Ih29e".into())
        );
        assert_eq!(client_id_in("client_id: 'short'"), None);
    }

    #[test]
    fn reads_the_paths_of_uris_and_pages() {
        assert_eq!(uri_path("soundcloud:track:artist/song"), Some("artist/song"));
        assert_eq!(uri_path("soundcloud:playlist:artist/sets/mix"), Some("artist/sets/mix"));
        assert_eq!(uri_path("https://soundcloud.com/artist/song?si=1"), Some("artist/song"));
        assert_eq!(uri_path("soundcloud:track:"), None);
        assert_eq!(uri_path("spotify:track:1"), None);
    }

    #[test]
    fn makes_covers_large() {
        assert_eq!(
            artwork("https://i1.sndcdn.com/artworks-x-large.jpg"),
            "https://i1.sndcdn.com/artworks-x-t500x500.jpg"
        );
    }

    #[test]
    fn reads_track_pages() {
        let value = serde_json::json!({
            "kind": "track",
            "title": "Song",
            "permalink_url": "https://soundcloud.com/artist/song",
            "full_duration": 123_000,
            "artwork_url": "https://i1.sndcdn.com/a-large.jpg",
            "user": {"username": "Artist", "permalink_url": "https://soundcloud.com/artist"}
        });
        let track = track(&value).unwrap();
        assert_eq!(track.uri, "soundcloud:track:artist/song");
        assert_eq!(track.artists, ["Artist"]);
        assert_eq!(track.artist_uris, ["soundcloud:artist:artist"]);
        assert_eq!(track.duration_ms, 123_000);
        assert_eq!(track.image_url.as_deref(), Some("https://i1.sndcdn.com/a-t500x500.jpg"));
    }
}
