
#[derive(Debug, Clone)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub active: bool,
    pub volume_percent: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct Playback {
    pub device: Device,
    pub playing: bool,
    pub progress_ms: u32,
    pub shuffle: bool,
    pub repeat: String,
    pub track: Option<PlayingTrack>,
    pub context: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PlayingTrack {
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
