
use std::{sync::Arc, time::Duration};

use anyhow::Result;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tunebox_core::{
    player::{CurrentTrack, PlayerEvent},
    service,
};
use tunebox_os::media::{Command, Controls, State, Track};

pub enum Event {
    Command(Command),
    Artwork { url: String, bytes: Arc<[u8]> },
}

pub struct Media {
    controls: Option<Controls>,
    state: State,
    artwork_url: Option<String>,
    tx: UnboundedSender<Event>,
    http: reqwest::Client,
}

impl Media {
    pub fn new() -> (Self, UnboundedReceiver<Event>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let controls = Controls::new({
            let tx = tx.clone();
            move |command| {
                let _ = tx.send(Event::Command(command));
            }
        });
        let media = Self { controls, state: State::default(), artwork_url: None, tx, http: reqwest::Client::new() };
        (media, rx)
    }

    pub fn on_player(&mut self, event: &PlayerEvent) {
        if self.controls.is_none() {
            return;
        }
        match event {
            PlayerEvent::TrackChanged { track: current } => {
                self.state.track = Some(track(current));
                self.state.position = Duration::ZERO;
                self.load_artwork(current);
            }
            PlayerEvent::Playing { position_ms } => {
                self.state.playing = true;
                self.state.position = Duration::from_millis((*position_ms).into());
            }
            PlayerEvent::Paused { position_ms } => {
                self.state.playing = false;
                self.state.position = Duration::from_millis((*position_ms).into());
            }
            PlayerEvent::Seeked { position_ms } | PlayerEvent::PositionCorrection { position_ms } => {
                self.state.position = Duration::from_millis((*position_ms).into());
            }
            PlayerEvent::VolumeChanged { volume } => self.state.volume = *volume,
            PlayerEvent::Stopped => self.state = State { volume: self.state.volume, ..State::default() },
            _ => return,
        }
        if let Some(controls) = &self.controls {
            controls.update(self.state.clone());
        }
    }

    pub fn on_event(&mut self, event: Event, control: impl FnOnce(Command) -> Result<()>) {
        match event {
            Event::Command(command) => {
                if let Err(err) = control(command) {
                    tracing::warn!("media control {command:?}: {err:#}");
                }
            }
            Event::Artwork { url, bytes } => {
                if let (Some(controls), Some(track)) = (&self.controls, &mut self.state.track)
                    && self.artwork_url.as_deref() == Some(url.as_str())
                {
                    track.artwork = Some(bytes);
                    controls.update(self.state.clone());
                }
            }
        }
    }

    fn load_artwork(&mut self, current: &CurrentTrack) {
        self.artwork_url = current.cover_url_large.clone();
        let Some(url) = self.artwork_url.clone() else { return };
        let (http, tx) = (self.http.clone(), self.tx.clone());
        tokio::spawn(async move {
            let result = async { http.get(&url).send().await?.error_for_status()?.bytes().await }.await;
            match result {
                Ok(bytes) => {
                    let _ = tx.send(Event::Artwork { url, bytes: Arc::from(bytes.as_ref()) });
                }
                Err(err) => tracing::warn!("artwork {url}: {err}"),
            }
        });
    }
}

fn track(current: &CurrentTrack) -> Track {
    Track {
        title: current.name.clone(),
        artist: current.artists.join(", "),
        album: current.album.clone(),
        duration: Duration::from_millis(current.duration_ms.into()),
        artwork: None,
        link: service::web_link(&current.uri),
    }
}
