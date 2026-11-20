
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Home {
    pub greeting: String,
    #[serde(default)]
    pub chips: Vec<Chip>,
    pub shelves: Vec<Shelf>,
}

impl Home {
    pub fn chip(&self) -> Option<&Chip> {
        self.chips.iter().find(|chip| chip.selected)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chip {
    pub title: String,
    pub id: String,
    pub selected: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Shelf {
    pub uri: String,
    pub title: String,
    #[serde(default)]
    pub strapline: String,
    pub shortcuts: bool,
    #[serde(default)]
    pub songs: bool,
    #[serde(default)]
    pub play_all: Option<String>,
    pub items: Vec<HomeItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ItemKind {
    Playlist,
    Album,
    Artist,
    Audiobook,
    Podcast,
    Episode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HomeItem {
    pub uri: String,
    pub kind: ItemKind,
    pub title: String,
    pub subtitle: String,
    pub image_url: Option<String>,
    pub owner_id: String,
    pub owner_name: String,
    pub context_uri: String,
}

impl HomeItem {
    pub fn is_song(&self) -> bool {
        self.kind == ItemKind::Playlist && self.context_uri != self.uri
    }
}
