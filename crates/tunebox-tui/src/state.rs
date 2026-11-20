
use std::{collections::BTreeMap, fs, io, path::Path};

use serde::{Deserialize, Serialize};

use crate::app::LibraryFilter;

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    #[serde(flatten)]
    pub page: Page,
    pub service: Option<String>,
    pub pages: BTreeMap<String, Page>,
    pub last_track: Option<LastTrack>,
    pub volume: Option<u16>,
    pub crossfade: Option<u64>,
    pub usage: Usage,
}

pub const STAR_AFTER_DAYS: u32 = 2;

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Usage {
    pub days: u32,
    pub last_day: u64,
    pub star_asked: bool,
}

impl Usage {
    pub fn start(&mut self, today: u64) {
        if today != self.last_day {
            self.days += 1;
            self.last_day = today;
        }
    }

    pub fn wants_star(&self) -> bool {
        !self.star_asked && self.days >= STAR_AFTER_DAYS
    }
}

pub fn today() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() / 86_400)
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Page {
    pub view: SavedView,
    pub selection: Selection,
    pub sidebar_focused: bool,
    pub sidebar_selected: usize,
    pub library_filter: LibraryFilter,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SavedView {
    #[default]
    Home,
    Library {
        filter: LibraryFilter,
    },
    Liked,
    Playlist {
        id: String,
        uri: String,
        name: String,
        owner_id: String,
        owner_name: String,
        #[serde(default)]
        collaborative: bool,
        image_url: Option<String>,
    },
    Album {
        id: String,
        uri: String,
        name: String,
        artists: Vec<String>,
        image_url: Option<String>,
    },
    Artist {
        uri: String,
        name: String,
    },
    Search {
        query: String,
    },
}

impl SavedView {
    pub fn uri(&self) -> Option<&str> {
        match self {
            SavedView::Playlist { uri, .. } | SavedView::Album { uri, .. } | SavedView::Artist { uri, .. } => Some(uri),
            SavedView::Home | SavedView::Library { .. } | SavedView::Liked | SavedView::Search { .. } => None,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Selection {
    pub index: usize,
    pub shelf: usize,
    pub item: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LastTrack {
    pub uri: String,
    pub name: String,
    pub artists: Vec<String>,
    #[serde(default)]
    pub artist_uris: Vec<String>,
    pub album: String,
    #[serde(default)]
    pub album_uri: Option<String>,
    pub duration_ms: u32,
    pub cover_url: Option<String>,
    pub cover_url_large: Option<String>,
    pub context: Option<String>,
    pub position_ms: u32,
    #[serde(default)]
    pub queued: Vec<String>,
}

pub fn load(path: &Path) -> State {
    fs::read(path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
}

pub fn save(path: &Path, state: &State) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let temp = path.with_extension("json.tmp");
    fs::write(&temp, serde_json::to_vec_pretty(state)?)?;
    fs::rename(temp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_star_is_asked_for_on_the_second_day_of_use_once() {
        let mut usage = Usage::default();
        for _ in 0..3 {
            usage.start(100);
        }
        assert_eq!(usage.days, 1, "several starts on one day count once");
        assert!(!usage.wants_star(), "not on the day tunebox is tried");
        usage.start(103);
        assert!(usage.wants_star());
        usage.star_asked = true;
        usage.start(111);
        assert!(!usage.wants_star());
    }

    #[test]
    fn a_file_from_before_the_star_counts_from_zero() {
        let state: State = serde_json::from_str(r#"{"view":{"kind":"home"},"volume":3}"#).unwrap();
        assert_eq!(state.usage, Usage::default());
    }

    #[test]
    fn a_file_from_before_youtube_music_holds_the_page_of_its_one_service() {
        let old = r#"{"view":{"kind":"artist","uri":"spotify:artist:1","name":"Daft Punk"},
            "selection":{"index":2,"shelf":0,"item":1},"sidebar_focused":true,"sidebar_selected":3,
            "library_filter":"Albums","last_track":null,"volume":30000}"#;
        let state: State = serde_json::from_str(old).unwrap();
        assert_eq!(state.service, None);
        assert!(state.pages.is_empty());
        assert_eq!(state.page.view, SavedView::Artist { uri: "spotify:artist:1".into(), name: "Daft Punk".into() });
        assert_eq!((state.page.selection.index, state.page.sidebar_selected), (2, 3));
        assert_eq!(state.page.library_filter, LibraryFilter::Albums);
        assert_eq!(state.volume, Some(30000));
    }

    #[test]
    fn a_state_from_before_queue_persistence_still_loads() {
        let old = r#"{"view":{"kind":"home"},"last_track":{"uri":"ytmusic:video:1","name":"One",
            "artists":[],"artist_uris":[],"album":"","album_uri":null,"duration_ms":1000,
            "cover_url":null,"cover_url_large":null,"context":null,"position_ms":42}}"#;
        let state: State = serde_json::from_str(old).unwrap();
        assert_eq!(state.last_track.unwrap().queued, Vec::<String>::new());
    }

    #[test]
    fn keeps_every_services_page() {
        let dir = std::env::temp_dir().join(format!("tunebox-state-{}", std::process::id()));
        let path = dir.join("state.json");
        let search = Page { view: SavedView::Search { query: "daft punk".into() }, ..Page::default() };
        let library = Page { view: SavedView::Library { filter: LibraryFilter::Artists }, ..Page::default() };
        let state = State {
            page: search.clone(),
            service: Some("ytmusic".into()),
            pages: BTreeMap::from([("spotify".to_owned(), library.clone())]),
            ..State::default()
        };
        save(&path, &state).unwrap();
        let loaded = load(&path);
        assert_eq!(loaded.service.as_deref(), Some("ytmusic"));
        assert_eq!(loaded.page, search);
        assert_eq!(loaded.pages.get("spotify"), Some(&library));
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains(r#""view""#) && text.contains("daft punk"), "{text}");
        fs::remove_dir_all(dir).unwrap();
    }
}
