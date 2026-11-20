
use std::{sync::Arc, time::Duration};

use crate::os;

#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration: Duration,
    pub artwork: Option<Arc<[u8]>>,
    pub link: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct State {
    pub track: Option<Track>,
    pub playing: bool,
    pub position: Duration,
    pub volume: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Play,
    Pause,
    Toggle,
    Next,
    Previous,
    Seek(Duration),
    Volume(u16),
}

pub struct Controls(os::media::Controls);

impl Controls {
    pub fn new(on_command: impl Fn(Command) + Send + Sync + 'static) -> Option<Self> {
        os::media::Controls::new(Arc::new(on_command)).map(Self)
    }

    pub fn update(&self, state: State) {
        self.0.update(state);
    }
}
