//! Playback through mpv. The queue, gapless loading and mpv control live in `tunebox-mpv`;
//! this is the YouTube Music source it plays.

use std::sync::Arc;

use anyhow::Result;
use tokio::sync::mpsc::UnboundedSender;
use tunebox_core::{paths::Paths, player::PlayerEvent};
use tunebox_mpv::Player as Engine;

use crate::{YtMusic, source::YtMusicSource};

/// The YouTube Music player.
#[derive(Clone)]
pub struct Player {
    inner: Engine,
}

impl Player {
    /// Starts mpv at `volume` (0 to 65535). Fails when mpv isn't installed. `TUNEBOX_MPV_ARGS`
    /// adds arguments for mpv, separated by spaces, such as `--ao=null` for tests.
    pub async fn spawn(
        client: YtMusic,
        paths: &Paths,
        volume: u16,
        events: UnboundedSender<PlayerEvent>,
    ) -> Result<Player> {
        let args = std::env::var("TUNEBOX_MPV_ARGS").unwrap_or_default();
        Self::spawn_with_args(client, paths, volume, events, args.split_whitespace().map(str::to_owned).collect()).await
    }

    /// [`Player::spawn`] with more arguments for mpv, such as `--ao=null` for tests.
    pub async fn spawn_with_args(
        client: YtMusic,
        paths: &Paths,
        volume: u16,
        events: UnboundedSender<PlayerEvent>,
        mpv_args: Vec<String>,
    ) -> Result<Player> {
        let source = Arc::new(YtMusicSource::new(client, paths));
        let inner = Engine::spawn_with_args(source, volume, events, mpv_args).await?;
        Ok(Self { inner })
    }

    /// Plays a playlist, album or artist (its songs), or a song and its radio, from
    /// `track_uri` if given.
    pub fn play(&self, context_uri: String, track_uri: Option<String>) -> Result<()> {
        self.inner.play(context_uri, track_uri)
    }

    /// Continues a song from the last session at `position_ms`, in its context if known.
    pub fn resume(&self, context: Option<String>, track: String, position_ms: u32) -> Result<()> {
        self.inner.resume(context, track, position_ms)
    }

    pub fn play_shuffled(&self, context_uri: String) -> Result<()> {
        self.inner.play_shuffled(context_uri)
    }

    /// Plays or pauses; with nothing loaded, there is nothing to do.
    pub fn toggle(&self, has_track: bool) -> Result<()> {
        self.inner.toggle(has_track)
    }

    pub fn pause(&self) -> Result<()> {
        self.inner.pause()
    }

    pub fn next(&self) -> Result<()> {
        self.inner.next()
    }

    /// Goes to the song before, or restarts this one once it has played for 3 seconds.
    pub fn previous(&self) -> Result<()> {
        self.inner.previous()
    }

    pub fn seek(&self, position_ms: u32) -> Result<()> {
        self.inner.seek(position_ms)
    }

    /// From 0 to 65535, as for librespot.
    pub fn set_volume(&self, volume: u16) -> Result<()> {
        self.inner.set_volume(volume)
    }

    pub fn shuffle(&self, on: bool) -> Result<()> {
        self.inner.shuffle(on)
    }

    pub fn repeat(&self, context: bool, track: bool) -> Result<()> {
        self.inner.repeat(context, track)
    }

    /// Empties the queue; the rest of the context stays.
    pub fn clear_queue(&self) -> Result<()> {
        self.inner.clear_queue()
    }

    /// Adds a song after the ones queued before.
    pub fn add_to_queue(&self, uri: &str) -> Result<()> {
        self.inner.add_to_queue(uri)
    }

    pub fn prefetch(&self, uri: &str) -> Result<()> {
        self.inner.prefetch(uri)
    }

    /// Jumps to an upcoming song.
    pub fn skip_to(&self, uri: &str) -> Result<()> {
        self.inner.skip_to(uri)
    }

    /// Stops playback and mpv.
    pub fn shutdown(&self) {
        self.inner.shutdown();
    }
}
