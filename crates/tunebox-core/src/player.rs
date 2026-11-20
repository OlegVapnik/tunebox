
use crate::catalog::Track;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayerEvent {
    Loading {
        uri: String,
        position_ms: u32,
    },
    TrackChanged {
        track: CurrentTrack,
    },
    Playing {
        position_ms: u32,
    },
    Paused {
        position_ms: u32,
    },
    Stopped,
    Seeked {
        position_ms: u32,
    },
    PositionCorrection {
        position_ms: u32,
    },
    PositionChanged {
        position_ms: u32,
    },
    ShuffleChanged {
        shuffle: bool,
    },
    RepeatChanged {
        context: bool,
        track: bool,
    },
    VolumeChanged {
        volume: u16,
    },
    QueueChanged {
        context_uri: String,
        current: Option<String>,
        next: Vec<QueueTrack>,
    },
    Activated,
    Deactivated,
    Unavailable {
        uri: String,
        refused: bool,
    },
    Blocked {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueTrack {
    pub uri: String,
    pub provider: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrentTrack {
    pub uri: String,
    pub name: String,
    pub artists: Vec<String>,
    pub artist_uris: Vec<String>,
    pub album: String,
    pub album_uri: Option<String>,
    pub duration_ms: u32,
    pub cover_url: Option<String>,
    pub cover_url_large: Option<String>,
}

impl CurrentTrack {
    pub fn to_track(&self) -> Track {
        Track {
            uri: self.uri.clone(),
            name: self.name.clone(),
            artists: self.artists.clone(),
            artist_uris: self.artist_uris.clone(),
            album: self.album.clone(),
            album_uri: self.album_uri.clone().unwrap_or_default(),
            duration_ms: self.duration_ms,
            image_url: self.cover_url.clone(),
        }
    }
}
