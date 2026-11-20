
#![cfg_attr(not(all(feature = "spotify", feature = "ytmusic")), allow(unused_variables))]

#[cfg(test)]
pub mod fake;
#[cfg(feature = "soundcloud")]
pub mod soundcloud;
#[cfg(feature = "spotify")]
pub mod spotify;
#[cfg(feature = "ytmusic")]
pub mod ytmusic;

use std::{sync::Arc, time::Duration};

use anyhow::{Result, bail};
use futures::{future::BoxFuture, stream::BoxStream};
use tokio::sync::{Notify, mpsc::UnboundedSender};
pub use tunebox_core::service::Service;
use tunebox_core::{
    artist::ArtistPage,
    catalog::{AlbumInfo, ArtistInfo, Page, PlaylistInfo, SearchResults, ShowInfo, Track, User},
    config::Config,
    connect::{Device, Playback},
    home::Home,
    lyrics::{Lyrics, LyricsQuery},
    paths::Paths,
};

use crate::{app::AppEvent, playback::Repeat, queue::Upcoming, views::TrackSource};

pub const COMPILED: &[Service] = &[
    #[cfg(feature = "spotify")]
    Service::Spotify,
    #[cfg(feature = "ytmusic")]
    Service::YouTubeMusic,
    #[cfg(feature = "soundcloud")]
    Service::SoundCloud,
];

pub const VARIANT: Option<&str> = match (cfg!(feature = "spotify"), cfg!(feature = "ytmusic")) {
    (true, true) => Some("full"),
    (true, false) => Some("spotify"),
    (false, true) => Some("ytmusic"),
    (false, false) => None,
};

macro_rules! dispatch {
    ($value:expr, $enum:ident, $inner:ident => $body:expr) => {
        match *$value {
            #[cfg(feature = "spotify")]
            $enum::Spotify(ref $inner) => $body,
            #[cfg(feature = "ytmusic")]
            $enum::YouTubeMusic(ref $inner) => $body,
            #[cfg(feature = "soundcloud")]
            $enum::SoundCloud(ref $inner) => $body,
            #[cfg(test)]
            $enum::Fake(ref $inner) => $body,
        }
    };
}


pub fn is_playable(uri: &str) -> bool {
    match Service::of(uri) {
        Some(Service::Spotify) => uri.starts_with("spotify:track:") || uri.starts_with("spotify:episode:"),
        Some(Service::YouTubeMusic) => uri.starts_with("ytmusic:track:"),
        Some(Service::SoundCloud) => uri.starts_with("soundcloud:track:"),
        None => false,
    }
}

pub fn is_track(uri: &str) -> bool {
    Service::of(uri).is_some_and(|service| uri[service.key().len()..].starts_with(":track:"))
}

pub fn is_episode(uri: &str) -> bool {
    uri.starts_with("spotify:episode:")
}

pub fn liked_context(service: Service, user: Option<&str>) -> Option<String> {
    match service {
        Service::Spotify => Some(format!("spotify:user:{}:collection", user?)),
        Service::YouTubeMusic => Some("ytmusic:playlist:LM".into()),
        Service::SoundCloud => Some("soundcloud:playlist:likes".into()),
    }
}

pub fn is_liked_context(uri: &str) -> bool {
    match Service::of(uri) {
        Some(Service::Spotify) => uri.ends_with(":collection"),
        Some(Service::YouTubeMusic) => uri == "ytmusic:playlist:LM",
        Some(Service::SoundCloud) => uri == "soundcloud:playlist:likes",
        None => false,
    }
}

const SPOTIFY_DJ: &str = "spotify:playlist:37i9dQZF1EYkqdzj48dyYq";

pub fn dj_context(service: Service) -> Option<&'static str> {
    (service == Service::Spotify).then_some(SPOTIFY_DJ)
}

pub fn is_dj(uri: &str) -> bool {
    uri == SPOTIFY_DJ
}

pub fn web_link(uri: &str) -> String {
    let mut parts = uri.split(':');
    let (Some(service), Some(kind), Some(id)) = (parts.next().and_then(Service::from_key), parts.next(), parts.next())
    else {
        return uri.to_owned();
    };
    match (service, kind) {
        (Service::Spotify, _) => format!("https://open.spotify.com/{kind}/{id}"),
        (Service::YouTubeMusic, "track") => format!("https://music.youtube.com/watch?v={id}"),
        (Service::YouTubeMusic, "playlist") => format!("https://music.youtube.com/playlist?list={id}"),
        (Service::YouTubeMusic, "album") => format!("https://music.youtube.com/browse/{id}"),
        (Service::YouTubeMusic, "artist") => format!("https://music.youtube.com/channel/{id}"),
        (Service::YouTubeMusic, _) => uri.to_owned(),
        (Service::SoundCloud, "track" | "playlist" | "album" | "artist") => {
            format!("https://soundcloud.com/{id}")
        }
        (Service::SoundCloud, _) => uri.to_owned(),
    }
}


pub fn supports_dj(service: Service) -> bool {
    service == Service::Spotify
}

pub fn supports_devices(service: Service) -> bool {
    service == Service::Spotify
}

pub fn supports_crossfade(service: Service) -> bool {
    service == Service::Spotify
}


pub enum Account {
    #[cfg(feature = "spotify")]
    Spotify(spotify::Account),
    #[cfg(feature = "ytmusic")]
    YouTubeMusic(ytmusic::Account),
    #[cfg(feature = "soundcloud")]
    SoundCloud(soundcloud::Account),
}

pub struct Launch {
    pub paths: Paths,
    pub crossfade: Duration,
    pub volume: Option<u16>,
    pub retry_now: Arc<Notify>,
    pub tx: UnboundedSender<AppEvent>,
}

impl Account {
    pub async fn open(service: Service, paths: &Paths, config: &Config) -> Result<Self> {
        match service {
            #[cfg(feature = "spotify")]
            Service::Spotify => Ok(Account::Spotify(spotify::Account::open(paths, config).await?)),
            #[cfg(feature = "ytmusic")]
            Service::YouTubeMusic => Ok(Account::YouTubeMusic(ytmusic::Account::open(paths)?)),
            #[cfg(feature = "soundcloud")]
            Service::SoundCloud => Ok(Account::SoundCloud(soundcloud::Account::open(paths)?)),
            #[allow(unreachable_patterns)]
            other => bail!("{} isn't built into this tunebox", other.name()),
        }
    }

    pub fn start(self, launch: Launch) -> Catalog {
        match self {
            #[cfg(feature = "spotify")]
            Account::Spotify(account) => Catalog::Spotify(account.start(launch)),
            #[cfg(feature = "ytmusic")]
            Account::YouTubeMusic(account) => Catalog::YouTubeMusic(account.start(launch)),
            #[cfg(feature = "soundcloud")]
            Account::SoundCloud(account) => Catalog::SoundCloud(account.start(launch)),
        }
    }
}


pub enum Player {
    #[cfg(feature = "spotify")]
    Spotify(spotify::Controller),
    #[cfg(feature = "ytmusic")]
    YouTubeMusic(ytmusic::Controller),
    #[cfg(feature = "soundcloud")]
    SoundCloud(soundcloud::Controller),
    #[cfg(test)]
    Fake(fake::Fake),
}

impl Player {
    pub fn service(&self) -> Service {
        match *self {
            #[cfg(feature = "spotify")]
            Player::Spotify(_) => Service::Spotify,
            #[cfg(feature = "ytmusic")]
            Player::YouTubeMusic(_) => Service::YouTubeMusic,
            #[cfg(feature = "soundcloud")]
            Player::SoundCloud(_) => Service::SoundCloud,
            #[cfg(test)]
            Player::Fake(ref fake) => fake.service(),
        }
    }

    pub fn play(&self, context_uri: String, track_uri: Option<String>) -> Result<()> {
        dispatch!(self, Player, player => player.play(context_uri, track_uri))
    }

    pub fn resume(&self, context: Option<String>, track: String, position_ms: u32) -> Result<()> {
        dispatch!(self, Player, player => player.resume(context, track, position_ms))
    }

    pub fn play_shuffled(&self, context_uri: String) -> Result<()> {
        dispatch!(self, Player, player => player.play_shuffled(context_uri))
    }

    pub fn toggle(&self, has_track: bool) -> Result<()> {
        dispatch!(self, Player, player => player.toggle(has_track))
    }

    pub fn pause(&self) -> Result<()> {
        dispatch!(self, Player, player => player.pause())
    }

    pub fn next(&self) -> Result<()> {
        dispatch!(self, Player, player => player.next())
    }

    pub fn previous(&self) -> Result<()> {
        dispatch!(self, Player, player => player.previous())
    }

    pub fn seek(&self, position_ms: u32) -> Result<()> {
        dispatch!(self, Player, player => player.seek(position_ms))
    }

    pub fn set_volume(&self, volume: u16) -> Result<()> {
        dispatch!(self, Player, player => player.set_volume(volume))
    }

    pub fn shuffle(&self, on: bool) -> Result<()> {
        dispatch!(self, Player, player => player.shuffle(on))
    }

    pub fn repeat(&self, mode: Repeat) -> Result<()> {
        dispatch!(self, Player, player => player.repeat(mode))
    }

    pub fn clear_queue(&self) -> Result<()> {
        dispatch!(self, Player, player => player.clear_queue())
    }

    pub fn add_to_queue(&self, uri: &str) -> Result<()> {
        dispatch!(self, Player, player => player.add_to_queue(uri))
    }

    pub fn prefetch_track(&self, uri: &str) -> Result<()> {
        match self {
            #[cfg(feature = "spotify")]
            Player::Spotify(player) => player.prefetch(uri),
            #[cfg(feature = "ytmusic")]
            Player::YouTubeMusic(player) => player.prefetch(uri),
            #[cfg(feature = "soundcloud")]
            Player::SoundCloud(player) => player.prefetch(uri),
            #[cfg(test)]
            Player::Fake(player) => player.prefetch(uri),
        }
    }

    pub fn shutdown(&self) {
        dispatch!(self, Player, player => player.shutdown())
    }


    pub fn skip_to(&self, uri: &str) -> Result<bool> {
        match self {
            #[cfg(feature = "ytmusic")]
            Player::YouTubeMusic(player) => player.skip_to(uri),
            #[cfg(feature = "soundcloud")]
            Player::SoundCloud(player) => player.skip_to(uri),
            #[allow(unreachable_patterns)]
            _ => Ok(false),
        }
    }

    pub fn next_segment(&self) -> Result<()> {
        match self {
            #[cfg(feature = "spotify")]
            Player::Spotify(player) => player.next_segment(),
            #[allow(unreachable_patterns)]
            _ => bail!("{} has no DJ", self.service().name()),
        }
    }

    pub fn ask_dj(&self, prompt: String, suggested: bool) -> Result<BoxFuture<'static, Result<()>>> {
        match self {
            #[cfg(feature = "spotify")]
            Player::Spotify(player) => player.ask_dj(prompt, suggested),
            #[allow(unreachable_patterns)]
            _ => bail!("{} has no DJ", self.service().name()),
        }
    }

    pub fn set_crossfade(&self, crossfade: Duration) {
        match self {
            #[cfg(feature = "spotify")]
            Player::Spotify(player) => player.set_crossfade(crossfade),
            #[allow(unreachable_patterns)]
            _ => {}
        }
    }

    pub fn device_id(&self) -> Option<String> {
        match self {
            #[cfg(feature = "spotify")]
            Player::Spotify(player) => Some(player.device_id()),
            #[allow(unreachable_patterns)]
            _ => None,
        }
    }

    pub fn user(&self) -> Option<String> {
        match self {
            #[cfg(feature = "spotify")]
            Player::Spotify(player) => Some(player.user()),
            #[allow(unreachable_patterns)]
            _ => None,
        }
    }

    pub fn reconnect(&self) {
        match self {
            #[cfg(feature = "spotify")]
            Player::Spotify(player) => player.reconnect(),
            #[allow(unreachable_patterns)]
            _ => {}
        }
    }

    pub fn supports_devices(&self) -> bool {
        #[cfg(test)]
        if matches!(self, Player::Fake(_)) {
            return false;
        }
        supports_devices(self.service())
    }
}

#[derive(Debug, Clone)]
pub enum Remote {
    Play,
    Pause,
    Next,
    Previous,
    Seek(u32),
    Volume(u8),
    Shuffle(bool),
    Repeat(Repeat),
    PlayContext {
        context: String,
        track: Option<String>,
        shuffle: bool,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Listed {
    #[cfg(feature = "spotify")]
    Spotify(spotify::Listed),
}

impl Listed {
    pub fn without(&self, entry: &Upcoming) -> Option<Listed> {
        match *self {
            #[cfg(feature = "spotify")]
            Listed::Spotify(ref listed) => listed.without(entry).map(Listed::Spotify),
        }
    }
}


pub struct Found {
    pub asked: Vec<String>,
    pub tracks: Vec<Track>,
    pub reached: bool,
}

#[derive(Clone)]
pub enum Catalog {
    #[cfg(feature = "spotify")]
    Spotify(spotify::Catalog),
    #[cfg(feature = "ytmusic")]
    YouTubeMusic(ytmusic::Catalog),
    #[cfg(feature = "soundcloud")]
    SoundCloud(soundcloud::Catalog),
    #[cfg(test)]
    Fake(fake::Catalog),
}

impl Catalog {
    pub fn service(&self) -> Service {
        match *self {
            #[cfg(feature = "spotify")]
            Catalog::Spotify(_) => Service::Spotify,
            #[cfg(feature = "ytmusic")]
            Catalog::YouTubeMusic(_) => Service::YouTubeMusic,
            #[cfg(feature = "soundcloud")]
            Catalog::SoundCloud(_) => Service::SoundCloud,
            #[cfg(test)]
            Catalog::Fake(ref fake) => fake.service(),
        }
    }

    pub fn signed_in(&self) -> bool {
        match self {
            #[cfg(feature = "ytmusic")]
            Catalog::YouTubeMusic(catalog) => catalog.signed_in(),
            #[cfg(feature = "soundcloud")]
            Catalog::SoundCloud(catalog) => catalog.signed_in(),
            #[allow(unreachable_patterns)]
            _ => true,
        }
    }

    pub async fn me(&self) -> Result<User> {
        dispatch!(self, Catalog, catalog => catalog.me().await)
    }

    pub async fn home(&self, chip: Option<&str>) -> Result<Home> {
        dispatch!(self, Catalog, catalog => catalog.home(chip).await)
    }

    pub async fn artist(&self, uri: &str) -> Result<ArtistPage> {
        dispatch!(self, Catalog, catalog => catalog.artist(uri).await)
    }

    pub async fn search(&self, query: &str) -> Result<SearchResults> {
        dispatch!(self, Catalog, catalog => catalog.search(query).await)
    }

    pub fn tracks(&self, source: TrackSource) -> BoxStream<'static, Result<Page<Track>>> {
        dispatch!(self, Catalog, catalog => catalog.tracks(source))
    }

    pub async fn playlists(&self) -> Result<Vec<PlaylistInfo>> {
        dispatch!(self, Catalog, catalog => catalog.playlists().await)
    }

    pub async fn liked_count(&self) -> Result<u32> {
        dispatch!(self, Catalog, catalog => catalog.liked_count().await)
    }

    pub async fn saved_albums(&self) -> Result<Vec<AlbumInfo>> {
        dispatch!(self, Catalog, catalog => catalog.saved_albums().await)
    }

    pub async fn followed_artists(&self) -> Result<Vec<ArtistInfo>> {
        dispatch!(self, Catalog, catalog => catalog.followed_artists().await)
    }

    pub async fn saved_shows(&self) -> Result<Vec<ShowInfo>> {
        dispatch!(self, Catalog, catalog => catalog.saved_shows().await)
    }

    pub async fn lyrics(&self, query: LyricsQuery<'_>) -> Result<Option<Lyrics>> {
        dispatch!(self, Catalog, catalog => catalog.lyrics(query).await)
    }

    pub fn check_saved(&self, uris: Vec<String>) -> BoxStream<'static, Result<Vec<(String, bool)>>> {
        dispatch!(self, Catalog, catalog => catalog.check_saved(uris))
    }

    pub async fn set_saved(&self, uri: &str, saved: bool) -> Result<()> {
        dispatch!(self, Catalog, catalog => catalog.set_saved(uri, saved).await)
    }

    pub async fn add_to_playlist(&self, playlist_id: &str, uris: &[String]) -> Result<()> {
        dispatch!(self, Catalog, catalog => catalog.add_to_playlist(playlist_id, uris).await)
    }

    pub async fn add_to_queue(&self, uri: &str) -> Result<()> {
        dispatch!(self, Catalog, catalog => catalog.add_to_queue(uri).await)
    }

    pub fn queue_tracks(&self, uris: Vec<String>) -> BoxStream<'static, Found> {
        dispatch!(self, Catalog, catalog => catalog.queue_tracks(uris))
    }

    pub async fn upcoming(&self) -> Result<Vec<Track>> {
        dispatch!(self, Catalog, catalog => catalog.upcoming().await)
    }


    pub async fn dj_suggestions(&self, track_uri: &str) -> Result<Vec<String>> {
        match self {
            #[cfg(feature = "spotify")]
            Catalog::Spotify(catalog) => catalog.dj_suggestions(track_uri).await,
            #[allow(unreachable_patterns)]
            _ => bail!("{} has no DJ", self.service().name()),
        }
    }

    pub async fn playback(&self) -> Result<Option<Playback>> {
        match self {
            #[cfg(feature = "spotify")]
            Catalog::Spotify(catalog) => catalog.playback().await,
            #[allow(unreachable_patterns)]
            _ => Ok(None),
        }
    }

    pub async fn devices(&self) -> Result<Vec<Device>> {
        match self {
            #[cfg(feature = "spotify")]
            Catalog::Spotify(catalog) => catalog.devices().await,
            #[allow(unreachable_patterns)]
            _ => bail!("{} doesn't play on other devices", self.service().name()),
        }
    }

    pub async fn transfer(&self, device_id: &str) -> Result<()> {
        match self {
            #[cfg(feature = "spotify")]
            Catalog::Spotify(catalog) => catalog.transfer(device_id).await,
            #[allow(unreachable_patterns)]
            _ => bail!("{} doesn't play on other devices", self.service().name()),
        }
    }

    pub fn remote(&self, command: Remote) {
        match self {
            #[cfg(feature = "spotify")]
            Catalog::Spotify(catalog) => catalog.remote(command),
            #[allow(unreachable_patterns)]
            _ => {}
        }
    }

    pub async fn skip_to(&self, device: &str, entry: &Upcoming) -> Result<()> {
        match self {
            #[cfg(feature = "spotify")]
            Catalog::Spotify(catalog) => catalog.skip_to(device, entry).await,
            #[allow(unreachable_patterns)]
            _ => bail!("{} doesn't play on other devices", self.service().name()),
        }
    }

    pub async fn set_queue(&self, device: &str, listed: &Listed) -> Result<()> {
        match (self, listed) {
            #[cfg(feature = "spotify")]
            (Catalog::Spotify(catalog), Listed::Spotify(listed)) => catalog.set_queue(device, listed).await,
            #[allow(unreachable_patterns)]
            _ => bail!("{} can't change what plays next there", self.service().name()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tells_songs_and_episodes_from_markers() {
        assert!(is_playable("spotify:track:4uLU6hMCjMI75M1A2tKUQC"));
        assert!(is_playable("spotify:episode:1"));
        assert!(!is_playable("spotify:delimiter"));
        assert!(is_playable("ytmusic:track:dQw4w9WgXcQ"));
        assert!(!is_playable("ytmusic:album:MPREb_1"));
        assert!(is_track("spotify:track:1") && is_track("ytmusic:track:1"));
        assert!(!is_track("spotify:episode:1") && !is_track("ytmusic:playlist:LM") && !is_track("track:1"));
    }

    #[test]
    fn builds_web_links() {
        assert_eq!(web_link("spotify:track:abc"), "https://open.spotify.com/track/abc");
        assert_eq!(web_link("spotify:album:abc"), "https://open.spotify.com/album/abc");
        assert_eq!(web_link("ytmusic:track:dQw4w9WgXcQ"), "https://music.youtube.com/watch?v=dQw4w9WgXcQ");
        assert_eq!(web_link("ytmusic:playlist:PL1"), "https://music.youtube.com/playlist?list=PL1");
        assert_eq!(web_link("ytmusic:album:MPREb_1"), "https://music.youtube.com/browse/MPREb_1");
        assert_eq!(web_link("ytmusic:artist:UC1"), "https://music.youtube.com/channel/UC1");
        assert_eq!(web_link("local:file"), "local:file");
    }

    #[test]
    fn liked_songs_are_a_context_of_their_own() {
        let spotify = liked_context(Service::Spotify, Some("sam")).unwrap();
        assert_eq!(spotify, "spotify:user:sam:collection");
        assert!(is_liked_context(&spotify));
        assert_eq!(liked_context(Service::Spotify, None), None);
        assert!(is_liked_context(&liked_context(Service::YouTubeMusic, None).unwrap()));
        assert!(!is_liked_context("spotify:playlist:37i9dQZF1EYkqdzj48dyYq"));
    }
}
