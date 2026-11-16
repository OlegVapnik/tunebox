//! Spotify: the Web API for the library and catalog, librespot for playback and for what only
//! Spotify's own apps get, like Home, artist pages, DJ and Spotify-made playlists.

mod player;
mod queue;
mod remote;

use std::{
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock, PoisonError},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use futures::{
    StreamExt,
    future::{self, BoxFuture},
    stream::{self, BoxStream},
};
use librespot::core::{SpotifyUri, session::Session};
use tokio::sync::{
    OnceCell,
    mpsc::{self, UnboundedSender},
    watch,
};
use tunebox_core::{
    artist::{ArtistPage, TopTrack},
    catalog::{AlbumInfo, ArtistInfo, Page, PlaylistInfo, SearchResults, ShowInfo, Track, User},
    config::Config,
    connect::{Device, Playback},
    home::{Home, HomeItem, ItemKind, Shelf},
    lyrics::{Lyrics, LyricsQuery},
    paths::Paths,
    service::Service,
};
use tunebox_spotify::{
    artist, catalog, connect, home, library, lyrics, pathfinder::Pathfinder, search, session, webapi::WebApi,
};

use self::player::SessionState;
pub use self::queue::Listed;
use super::{Found, Launch, Player, Remote};
use crate::{
    app::AppEvent,
    playback::Repeat,
    queue::{Provider, Upcoming},
    views::TrackSource,
};

/// Track metadata requests running at the same time for one playlist.
const METADATA_BATCHES: usize = 3;
/// Library checks running at the same time for one list.
const SAVED_CONCURRENCY: usize = 2;
/// How long lyrics wait for the session, so a track restored at startup still gets Spotify's
/// synced lyrics instead of going straight to LRCLIB.
const LYRICS_SESSION_WAIT: Duration = Duration::from_secs(5);
const SEARCH_SESSION_WAIT: Duration = Duration::from_secs(5);
/// Longer, since the cached library shows meanwhile.
const LIBRARY_SESSION_WAIT: Duration = Duration::from_secs(15);

/// Signed in to the Web API, with or without Premium: without it, songs play through mpv and
/// YouTube, and the parts only Spotify's own apps get are left out.
pub struct Account {
    api: Arc<WebApi>,
}

impl Account {
    pub async fn open(paths: &Paths, config: &Config) -> Result<Self> {
        let api = Arc::new(WebApi::new(config.sp_dc(paths)?, paths.webapi_token())?);
        if !api.is_logged_in().await {
            bail!("Not logged in yet – run `tunebox login` first.");
        }
        Ok(Self { api })
    }

    /// Connects the player in the background: librespot where streaming credentials are saved,
    /// and mpv where the account only has the Web API, as a free one does.
    pub fn start(self, launch: Launch) -> Catalog {
        let hashes = launch.paths.pathfinder_hashes();
        if session::is_logged_in(&launch.paths).unwrap_or(false) {
            let state = Arc::new(watch::Sender::new(SessionState::Connecting));
            let volume = launch.volume.unwrap_or(tunebox_player::DEFAULT_VOLUME);
            player::spawn(launch.paths, launch.crossfade, volume, launch.retry_now, launch.tx.clone(), state.clone());
            return Catalog::new(self.api, Some(state), hashes, launch.tx, None);
        }
        let player = Arc::new(OnceLock::new());
        let volume = launch.volume.unwrap_or(u16::MAX / 2);
        let (paths, tx) = (launch.paths, launch.tx);
        let api = self.api.clone();
        let (task_tx, task_player) = (tx.clone(), player.clone());
        tokio::spawn(async move {
            let (events, mut received) = mpsc::unbounded_channel();
            let started = tunebox_spotify::Player::spawn(api, &paths, volume, events).await;
            let started = match started {
                Ok(started) => started,
                Err(err) => {
                    let _ = task_tx.send(AppEvent::PlayerFailed(Service::Spotify, format!("{err:#}")));
                    return;
                }
            };
            let _ = task_player.set(started.clone());
            if task_tx.send(AppEvent::PlayerReady(Box::new(Player::Spotify(Controller::Free(started))))).is_err() {
                return;
            }
            while let Some(event) = received.recv().await {
                if task_tx.send(AppEvent::Player(Service::Spotify, event)).is_err() {
                    break;
                }
            }
        });
        Catalog::new(self.api, None, hashes, tx, Some(player))
    }
}

/// tunebox's Spotify player: librespot for Premium, mpv for a free account.
pub enum Controller {
    Premium(player::Connect),
    Free(tunebox_spotify::Player),
}

impl Controller {
    pub fn play(&self, context_uri: String, track_uri: Option<String>) -> Result<()> {
        match self {
            Self::Premium(player) => player.play(context_uri, track_uri),
            Self::Free(player) => player.play(context_uri, track_uri),
        }
    }

    pub fn resume(&self, context: Option<String>, track: String, position_ms: u32) -> Result<()> {
        match self {
            Self::Premium(player) => player.resume(context, track, position_ms),
            Self::Free(player) => player.resume(context, track, position_ms),
        }
    }

    pub fn play_shuffled(&self, context_uri: String) -> Result<()> {
        match self {
            Self::Premium(player) => player.play_shuffled(context_uri),
            Self::Free(player) => player.play_shuffled(context_uri),
        }
    }

    pub fn toggle(&self, has_track: bool) -> Result<()> {
        match self {
            Self::Premium(player) => player.toggle(has_track),
            Self::Free(player) => player.toggle(has_track),
        }
    }

    pub fn pause(&self) -> Result<()> {
        match self {
            Self::Premium(player) => player.pause(),
            Self::Free(player) => player.pause(),
        }
    }

    pub fn next(&self) -> Result<()> {
        match self {
            Self::Premium(player) => player.next(),
            Self::Free(player) => player.next(),
        }
    }

    pub fn previous(&self) -> Result<()> {
        match self {
            Self::Premium(player) => player.previous(),
            Self::Free(player) => player.previous(),
        }
    }

    pub fn seek(&self, position_ms: u32) -> Result<()> {
        match self {
            Self::Premium(player) => player.seek(position_ms),
            Self::Free(player) => player.seek(position_ms),
        }
    }

    pub fn set_volume(&self, volume: u16) -> Result<()> {
        match self {
            Self::Premium(player) => player.set_volume(volume),
            Self::Free(player) => player.set_volume(volume),
        }
    }

    pub fn shuffle(&self, on: bool) -> Result<()> {
        match self {
            Self::Premium(player) => player.shuffle(on),
            Self::Free(player) => player.shuffle(on),
        }
    }

    pub fn repeat(&self, mode: Repeat) -> Result<()> {
        match self {
            Self::Premium(player) => player.repeat(mode),
            Self::Free(player) => player.repeat(mode != Repeat::Off, mode == Repeat::Track),
        }
    }

    pub fn clear_queue(&self) -> Result<()> {
        match self {
            Self::Premium(player) => player.clear_queue(),
            Self::Free(player) => player.clear_queue(),
        }
    }

    pub fn add_to_queue(&self, uri: &str) -> Result<()> {
        match self {
            Self::Premium(player) => player.add_to_queue(uri),
            Self::Free(player) => player.add_to_queue(uri),
        }
    }

    pub fn prefetch(&self, uri: &str) -> Result<()> {
        match self {
            Self::Premium(_) => Ok(()),
            Self::Free(player) => player.prefetch(uri),
        }
    }

    pub fn next_segment(&self) -> Result<()> {
        match self {
            Self::Premium(player) => player.next_segment(),
            Self::Free(_) => bail!("Spotify's DJ needs Premium"),
        }
    }

    pub fn ask_dj(&self, prompt: String, suggested: bool) -> Result<BoxFuture<'static, Result<()>>> {
        match self {
            Self::Premium(player) => player.ask_dj(prompt, suggested),
            Self::Free(_) => bail!("Spotify's DJ needs Premium"),
        }
    }

    pub fn set_crossfade(&self, crossfade: Duration) {
        if let Self::Premium(player) = self {
            player.set_crossfade(crossfade);
        }
    }

    pub fn device_id(&self) -> String {
        match self {
            Self::Premium(player) => player.device_id(),
            Self::Free(_) => String::new(),
        }
    }

    pub fn user(&self) -> String {
        match self {
            Self::Premium(player) => player.user(),
            Self::Free(_) => String::new(),
        }
    }

    pub fn reconnect(&self) {
        if let Self::Premium(player) = self {
            player.reconnect();
        }
    }

    pub fn shutdown(&self) {
        match self {
            Self::Premium(player) => player.shutdown(),
            Self::Free(player) => player.shutdown(),
        }
    }
}

#[derive(Clone)]
pub struct Catalog {
    api: Arc<WebApi>,
    /// librespot's session, where the account has Premium; without it, songs play through mpv.
    session: Option<Arc<watch::Sender<SessionState>>>,
    /// The mpv player of a free account, filled once it is up.
    free_player: Option<Arc<OnceLock<tunebox_spotify::Player>>>,
    pathfinder: Arc<Mutex<Option<Arc<Pathfinder>>>>,
    pathfinder_hashes: PathBuf,
    /// The user's Spotify ID, to tell their own playlists from followed ones.
    user_id: Arc<OnceCell<String>>,
    /// For LRCLIB, where lyrics come from when Spotify has none.
    http: reqwest::Client,
    remote: UnboundedSender<connect::Command>,
}

impl Catalog {
    fn new(
        api: Arc<WebApi>,
        session: Option<Arc<watch::Sender<SessionState>>>,
        pathfinder_hashes: PathBuf,
        tx: UnboundedSender<AppEvent>,
        free_player: Option<Arc<OnceLock<tunebox_spotify::Player>>>,
    ) -> Self {
        let http =
            tunebox_core::http::client(concat!("tunebox/", env!("CARGO_PKG_VERSION"))).build().unwrap_or_default();
        let (remote, commands) = mpsc::unbounded_channel();
        tokio::spawn(remote::send_remote(api.clone(), tx, commands));
        Self {
            api,
            session,
            free_player,
            pathfinder: Arc::default(),
            pathfinder_hashes,
            user_id: Arc::default(),
            http,
            remote,
        }
    }

    /// The librespot session, once the player has connected.
    async fn session(&self) -> Result<Session> {
        let Some(session) = &self.session else {
            bail!("This needs Spotify Premium; on a free account, tunebox plays its songs through YouTube.");
        };
        let mut session = session.subscribe();
        let state = session.wait_for(|state| !matches!(state, SessionState::Connecting)).await?;
        match &*state {
            SessionState::Ready(session) => Ok(session.clone()),
            SessionState::Failed(err) => bail!("No connection to Spotify: {err}"),
            SessionState::Connecting => unreachable!("checked by wait_for"),
        }
    }

    /// Recreated after a reconnect, since the old session is shut down.
    async fn pathfinder(&self) -> Result<Arc<Pathfinder>> {
        let session = self.session().await?;
        let mut current = self.pathfinder.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(pathfinder) = current.as_ref().filter(|p| !p.session_lost()) {
            return Ok(pathfinder.clone());
        }
        let pathfinder = Arc::new(Pathfinder::new(session, self.pathfinder_hashes.clone()));
        *current = Some(pathfinder.clone());
        Ok(pathfinder)
    }

    async fn pathfinder_within(&self, wait: Duration) -> Result<Arc<Pathfinder>> {
        tokio::time::timeout(wait, self.pathfinder()).await.context("Spotify is still connecting")?
    }

    /// The user's ID; asks Spotify itself when `/me` has not answered yet, as happens for a
    /// playlist restored at startup.
    async fn user_id(&self) -> Option<String> {
        let id = self.user_id.get_or_try_init(|| async { catalog::me(&self.api).await.map(|user| user.id) }).await;
        id.ok().cloned()
    }

    pub async fn me(&self) -> Result<User> {
        let user = catalog::me(&self.api).await?;
        let _ = self.user_id.set(user.id.clone());
        Ok(user)
    }

    pub async fn home(&self, _chip: Option<&str>) -> Result<Home> {
        if self.session.is_none() {
            return self.free_home().await;
        }
        home::home(&*self.pathfinder().await?).await
    }

    pub async fn artist(&self, uri: &str) -> Result<ArtistPage> {
        if self.session.is_none() {
            return self.free_artist(uri).await;
        }
        artist::artist(&*self.pathfinder().await?, uri).await
    }

    /// Home without Spotify's own app: the playlists and albums the Web API has.
    async fn free_home(&self) -> Result<Home> {
        let mut shelves = Vec::new();
        if let Ok(playlists) = catalog::playlists(&self.api).await
            && !playlists.is_empty()
        {
            shelves.push(Shelf {
                uri: "spotify:playlists".into(),
                title: "Your playlists".into(),
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
        if let Ok(albums) = catalog::saved_albums(&self.api).await
            && !albums.is_empty()
        {
            shelves.push(Shelf {
                uri: "spotify:albums".into(),
                title: "Saved albums".into(),
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
        if shelves.is_empty() {
            bail!("Spotify returned nothing for Home; try Search instead");
        }
        Ok(Home { greeting: String::new(), chips: Vec::new(), shelves })
    }

    /// An artist page without Spotify's own app: their top songs and albums from the Web API.
    async fn free_artist(&self, uri: &str) -> Result<ArtistPage> {
        let id = uri.strip_prefix("spotify:artist:").unwrap_or(uri);
        let found = catalog::artist(&self.api, id).await?;
        let top_tracks = catalog::artist_top_tracks(&self.api, id)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|track| TopTrack { track, playcount: None })
            .collect();
        let albums = catalog::artist_albums(&self.api, id).await.unwrap_or_default();
        let shelves = if albums.is_empty() {
            Vec::new()
        } else {
            vec![Shelf {
                uri: format!("spotify:artist:{id}:albums"),
                title: "Albums".into(),
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
                        subtitle: album.year.clone(),
                        image_url: album.image_url.clone(),
                        owner_id: String::new(),
                        owner_name: String::new(),
                        context_uri: album.uri.clone(),
                    })
                    .collect(),
            }]
        };
        Ok(ArtistPage {
            uri: found.uri,
            name: found.name,
            image_url: found.image_url,
            monthly_listeners: None,
            followers: None,
            saved: false,
            top_tracks,
            shelves,
        })
    }

    /// Playlists come from Spotify's own search, which finds the user's mixes.
    pub async fn search(&self, query: &str) -> Result<SearchResults> {
        if self.session.is_none() {
            return catalog::search(&self.api, query).await;
        }
        let internal = async { search::playlists(&*self.pathfinder_within(SEARCH_SESSION_WAIT).await?, query).await };
        let (results, internal) = tokio::join!(catalog::search(&self.api, query), internal);
        let mut results = results?;
        match internal {
            Ok(found) => results.playlists = library::with_web_details(found, std::mem::take(&mut results.playlists)).0,
            Err(err) => tracing::warn!("Spotify's own search failed, showing the Web API's playlists: {err:#}"),
        }
        Ok(results)
    }

    /// Pages after the first are fetched a few at a time and arrive in order.
    pub fn tracks(&self, source: TrackSource) -> BoxStream<'static, Result<Page<Track>>> {
        let this = self.clone();
        stream::once(async move {
            if this.session.is_some()
                && let TrackSource::Playlist(playlist) = &source
                && !playlist.collaborative
                && this.user_id().await.as_deref() != Some(playlist.owner_id.as_str())
            {
                return this.internal_playlist(playlist.id.clone()).await;
            }
            let api = this.api.clone();
            let fetch = move |offset| {
                let (api, source) = (api.clone(), source.clone());
                async move {
                    match &source {
                        TrackSource::Liked => catalog::liked_tracks(&api, offset).await,
                        TrackSource::Playlist(p) => catalog::playlist_tracks(&api, &p.id, offset).await,
                        TrackSource::Album(a) => catalog::album_tracks(&api, a, offset).await,
                    }
                }
            };
            let first = fetch(0).await;
            let offsets: Vec<u32> = match &first {
                Ok(Page { next_offset: Some(next), total, .. }) => {
                    (*next..*total).step_by(catalog::PAGE as usize).collect()
                }
                _ => Vec::new(),
            };
            let rest = stream::iter(offsets).map(fetch).buffered(catalog::PAGE_CONCURRENCY);
            stream::iter([first]).chain(rest).boxed()
        })
        .flatten()
        .boxed()
    }

    /// Followed and Spotify-made playlists only resolve through librespot. Their tracks arrive
    /// in batches, so the first rows show while the rest load.
    async fn internal_playlist(self, id: String) -> BoxStream<'static, Result<Page<Track>>> {
        let listed = async {
            let session = self.session().await?;
            let uris = catalog::playlist_track_uris(&session, &id).await?;
            anyhow::Ok((session, uris))
        };
        let (session, uris) = match listed.await {
            Ok(listed) => listed,
            Err(err) => return stream::iter([Err(err)]).boxed(),
        };
        if uris.is_empty() {
            return stream::iter([Ok(Page { items: Vec::new(), total: 0, next_offset: None })]).boxed();
        }
        let count = uris.len();
        let batches: Vec<_> = uris.chunks(catalog::METADATA_BATCH).map(<[_]>::to_vec).collect();
        stream::iter(batches)
            .map(move |batch| {
                let session = session.clone();
                async move { (batch.len(), catalog::tracks_metadata(&session, &batch).await) }
            })
            .buffered(METADATA_BATCHES)
            .scan((0, 0), move |(requested, delivered), (len, items)| {
                *requested += len;
                *delivered += items.len();
                let last = *requested == count;
                // Tracks without details are left out, so the final count is what actually arrived.
                let total = if last { *delivered } else { count } as u32;
                let next_offset = (!last).then_some(*requested as u32);
                future::ready(Some(Ok(Page { items, total, next_offset })))
            })
            .boxed()
    }

    /// Spotify's own library, which includes mixes, with song counts from the Web API.
    pub async fn playlists(&self) -> Result<Vec<PlaylistInfo>> {
        if self.session.is_none() {
            return catalog::playlists(&self.api).await;
        }
        let internal = async { library::playlists(&*self.pathfinder_within(LIBRARY_SESSION_WAIT).await?).await };
        match tokio::join!(internal, catalog::playlists(&self.api)) {
            (Ok(found), Ok(web)) => {
                let (mut playlists, rest) = library::with_web_details(found, web);
                playlists.extend(rest);
                Ok(playlists)
            }
            (Ok(found), Err(err)) => {
                tracing::warn!("Web API playlists failed, showing them without song counts: {err:#}");
                Ok(found)
            }
            (Err(err), web) => {
                tracing::warn!("Spotify's own library failed, showing the Web API's playlists: {err:#}");
                web
            }
        }
    }

    pub async fn liked_count(&self) -> Result<u32> {
        catalog::liked_count(&self.api).await
    }

    pub async fn saved_albums(&self) -> Result<Vec<AlbumInfo>> {
        catalog::saved_albums(&self.api).await
    }

    pub async fn followed_artists(&self) -> Result<Vec<ArtistInfo>> {
        catalog::followed_artists(&self.api).await
    }

    pub async fn saved_shows(&self) -> Result<Vec<ShowInfo>> {
        catalog::saved_shows(&self.api).await
    }

    /// Spotify's own lyrics, else LRCLIB's.
    pub async fn lyrics(&self, query: LyricsQuery<'_>) -> Result<Option<Lyrics>> {
        let session = tokio::time::timeout(LYRICS_SESSION_WAIT, self.session()).await.ok().and_then(Result::ok);
        lyrics::fetch(session.as_ref(), &self.http, query).await
    }

    /// Each batch reports as soon as it is known, so the first rows of a long list get their
    /// hearts without waiting for the rest.
    pub fn check_saved(&self, uris: Vec<String>) -> BoxStream<'static, Result<Vec<(String, bool)>>> {
        let api = self.api.clone();
        let batches: Vec<Vec<String>> = uris.chunks(catalog::LIBRARY_BATCH).map(<[_]>::to_vec).collect();
        stream::iter(batches)
            .map(move |batch| {
                let api = api.clone();
                async move {
                    let flags = catalog::contains(&api, &batch).await?;
                    Ok(batch.into_iter().zip(flags).collect())
                }
            })
            .buffered(SAVED_CONCURRENCY)
            .boxed()
    }

    pub async fn set_saved(&self, uri: &str, saved: bool) -> Result<()> {
        let uris = [uri.to_owned()];
        if saved { catalog::save(&self.api, &uris).await } else { catalog::unsave(&self.api, &uris).await }
    }

    pub async fn add_to_playlist(&self, playlist_id: &str, uris: &[String]) -> Result<()> {
        catalog::add_to_playlist(&self.api, playlist_id, uris).await
    }

    /// Premium queues through the Web API, so it reaches whichever device plays; a free
    /// account queues in tunebox's own player.
    pub async fn add_to_queue(&self, uri: &str) -> Result<()> {
        if let Some(player) = self.free_player.as_ref().and_then(|player| player.get()) {
            return player.add_to_queue(uri);
        }
        catalog::queue(&self.api, uri).await
    }

    /// From librespot's metadata, a batch at a time. Without a session, nothing reaches
    /// Spotify; URIs that aren't Spotify's are reported at once, without details.
    pub fn queue_tracks(&self, uris: Vec<String>) -> BoxStream<'static, Found> {
        if self.session.is_none() {
            let api = self.api.clone();
            let batches: Vec<Vec<String>> = uris.chunks(catalog::METADATA_BATCH).map(<[_]>::to_vec).collect();
            return stream::iter(batches)
                .then(move |asked| {
                    let api = api.clone();
                    async move {
                        match catalog::tracks_by_ids(&api, &asked).await {
                            Ok(tracks) => Found { asked, tracks, reached: true },
                            Err(err) => {
                                tracing::warn!("songs of the queue: {err:#}");
                                Found { asked, tracks: Vec::new(), reached: false }
                            }
                        }
                    }
                })
                .boxed();
        }
        let this = self.clone();
        stream::once(async move {
            let Ok(session) = this.session().await else {
                return stream::iter([Found { asked: uris, tracks: Vec::new(), reached: false }]).boxed();
            };
            let (parsed, unknown): (Vec<_>, Vec<_>) =
                uris.into_iter().map(|uri| (SpotifyUri::from_uri(&uri).ok(), uri)).partition(|(id, _)| id.is_some());
            let unknown = (!unknown.is_empty()).then(|| Found {
                asked: unknown.into_iter().map(|(_, uri)| uri).collect(),
                tracks: Vec::new(),
                reached: true,
            });
            let batches: Vec<Vec<_>> = parsed.chunks(catalog::METADATA_BATCH).map(<[_]>::to_vec).collect();
            let found = stream::iter(batches).then(move |batch| {
                let session = session.clone();
                async move {
                    let ids: Vec<SpotifyUri> = batch.iter().filter_map(|(id, _)| id.clone()).collect();
                    let tracks = catalog::tracks_metadata(&session, &ids).await;
                    Found { asked: batch.into_iter().map(|(_, uri)| uri).collect(), tracks, reached: true }
                }
            });
            stream::iter(unknown).chain(found).boxed()
        })
        .flatten()
        .boxed()
    }

    /// The Web API's list of what plays next; tunebox's own player says it itself.
    pub async fn upcoming(&self) -> Result<Vec<Track>> {
        if self.session.is_none() {
            bail!("Spotify's own queue needs Premium; tunebox's player says what plays next itself.");
        }
        catalog::upcoming(&self.api).await
    }

    pub async fn dj_suggestions(&self, track_uri: &str) -> Result<Vec<String>> {
        Ok(self.session().await?.spclient().get_dj_suggestions(track_uri).await?)
    }

    pub async fn playback(&self) -> Result<Option<Playback>> {
        connect::playback(&self.api).await
    }

    pub async fn devices(&self) -> Result<Vec<Device>> {
        connect::devices(&self.api).await
    }

    pub async fn transfer(&self, device_id: &str) -> Result<()> {
        connect::transfer(&self.api, device_id).await
    }

    pub fn remote(&self, command: Remote) {
        let _ = self.remote.send(remote::command(command));
    }

    /// Through Spotify Connect, as tunebox's own device.
    pub async fn skip_to(&self, device: &str, entry: &Upcoming) -> Result<()> {
        let session = self.session().await?;
        connect::skip_to(&session, device, &entry.uri, &entry.uid, provider_name(entry.provider)).await
    }

    /// Through Spotify Connect, as tunebox's own device.
    pub async fn set_queue(&self, device: &str, listed: &Listed) -> Result<()> {
        let session = self.session().await?;
        connect::set_queue(&session, device, &listed.next, &listed.previous, &listed.revision).await
    }
}

/// Spotify's name for where an entry comes from, as commands about the entry name it.
fn provider_name(provider: Provider) -> &'static str {
    match provider {
        Provider::Queue => "queue",
        Provider::Context | Provider::Unknown => "context",
    }
}
