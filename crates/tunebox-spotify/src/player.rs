
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::mpsc::UnboundedSender;
use tunebox_core::{paths::Paths, player::PlayerEvent};
use tunebox_mpv::Player as Engine;

use crate::{source::SpotifySource, webapi::WebApi};

#[derive(Clone)]
pub struct Player {
    inner: Engine,
}

impl Player {
    pub async fn spawn(
        api: Arc<WebApi>,
        paths: &Paths,
        volume: u16,
        events: UnboundedSender<PlayerEvent>,
    ) -> Result<Player> {
        let args = std::env::var("TUNEBOX_MPV_ARGS").unwrap_or_default();
        Self::spawn_with_args(api, paths, volume, events, args.split_whitespace().map(str::to_owned).collect()).await
    }

    pub async fn spawn_with_args(
        api: Arc<WebApi>,
        paths: &Paths,
        volume: u16,
        events: UnboundedSender<PlayerEvent>,
        mpv_args: Vec<String>,
    ) -> Result<Player> {
        let source = Arc::new(SpotifySource::new(api, paths));
        let inner = Engine::spawn_with_args(source, volume, events, mpv_args).await?;
        Ok(Self { inner })
    }

    pub fn play(&self, context_uri: String, track_uri: Option<String>) -> Result<()> {
        self.inner.play(context_uri, track_uri)
    }

    pub fn resume(&self, context: Option<String>, track: String, position_ms: u32) -> Result<()> {
        self.inner.resume(context, track, position_ms)
    }

    pub fn play_shuffled(&self, context_uri: String) -> Result<()> {
        self.inner.play_shuffled(context_uri)
    }

    pub fn toggle(&self, has_track: bool) -> Result<()> {
        self.inner.toggle(has_track)
    }

    pub fn pause(&self) -> Result<()> {
        self.inner.pause()
    }

    pub fn next(&self) -> Result<()> {
        self.inner.next()
    }

    pub fn previous(&self) -> Result<()> {
        self.inner.previous()
    }

    pub fn seek(&self, position_ms: u32) -> Result<()> {
        self.inner.seek(position_ms)
    }

    pub fn set_volume(&self, volume: u16) -> Result<()> {
        self.inner.set_volume(volume)
    }

    pub fn shuffle(&self, on: bool) -> Result<()> {
        self.inner.shuffle(on)
    }

    pub fn repeat(&self, context: bool, track: bool) -> Result<()> {
        self.inner.repeat(context, track)
    }

    pub fn clear_queue(&self) -> Result<()> {
        self.inner.clear_queue()
    }

    pub fn add_to_queue(&self, uri: &str) -> Result<()> {
        self.inner.add_to_queue(uri)
    }

    pub fn prefetch(&self, uri: &str) -> Result<()> {
        self.inner.prefetch(uri)
    }

    pub fn skip_to(&self, uri: &str) -> Result<()> {
        self.inner.skip_to(uri)
    }

    pub fn shutdown(&self) {
        self.inner.shutdown();
    }
}
