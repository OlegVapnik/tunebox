
use tunebox_core::player::QueueTrack;

use crate::backend::{self, Listed};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Queue,
    Context,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upcoming {
    pub uri: String,
    pub uid: String,
    pub provider: Provider,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Queue {
    pub current: Option<String>,
    pub next: Vec<Upcoming>,
    pub context: String,
    pub context_name: Option<String>,
    pub listed: Option<Box<Listed>>,
}

impl Queue {
    pub fn from_player(context: String, current: Option<String>, next: &[QueueTrack]) -> Self {
        Self {
            current: current.filter(|uri| !uri.is_empty()),
            next: next.iter().filter_map(|t| upcoming(&t.uri, "", &t.provider)).collect(),
            context,
            ..Self::default()
        }
    }

    pub fn with_listing(&self, playing: &str, context: String, uris: Vec<String>) -> Self {
        let known = self.knows(playing);
        let queued = if known { self.after(Some(playing)) } else { &self.next[..] };
        let queued: Vec<&str> =
            queued.iter().filter(|t| t.provider == Provider::Queue).map(|t| t.uri.as_str()).collect();
        let lead = uris.iter().zip(&queued).take_while(|(uri, queued)| uri == *queued).count();
        let rest = if known || lead > 0 { Provider::Context } else { Provider::Unknown };
        let next = uris
            .into_iter()
            .enumerate()
            .map(|(index, uri)| Upcoming {
                uri,
                uid: String::new(),
                provider: if index < lead { Provider::Queue } else { rest },
            })
            .collect();
        let context_name = self.context_name.clone().filter(|_| self.context == context);
        Self { current: Some(playing.to_owned()), next, context, context_name, listed: None }
    }

    pub fn knows(&self, playing: &str) -> bool {
        self.current.as_deref() == Some(playing) || self.next.iter().any(|t| t.uri == playing)
    }

    pub fn after(&self, playing: Option<&str>) -> &[Upcoming] {
        let Some(playing) = playing.filter(|uri| self.current.as_deref() != Some(uri)) else { return &self.next };
        match self.next.iter().position(|t| t.uri == playing) {
            Some(index) => &self.next[index + 1..],
            None => &[],
        }
    }

    pub fn without(&self, entry: &Upcoming) -> Option<Listed> {
        self.listed.as_deref()?.without(entry)
    }
}

pub(crate) fn upcoming(uri: &str, uid: &str, provider: &str) -> Option<Upcoming> {
    let provider = match provider {
        "queue" => Provider::Queue,
        "context" | "autoplay" | "" => Provider::Context,
        _ => return None,
    };
    backend::is_playable(uri).then(|| Upcoming { uri: uri.to_owned(), uid: uid.to_owned(), provider })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uris(entries: &[Upcoming]) -> Vec<&str> {
        entries.iter().map(|t| t.uri.as_str()).collect()
    }

    #[test]
    fn tuneboxs_own_player_reports_without_ids() {
        let track = |uri: &str, provider: &str| QueueTrack { uri: uri.into(), provider: provider.into() };
        let next = [
            track("spotify:track:a", "queue"),
            track("spotify:delimiter", "context"),
            track("spotify:track:c", "context"),
        ];
        let queue = Queue::from_player("spotify:album:1".into(), Some("spotify:track:x".into()), &next);
        assert_eq!(queue.current.as_deref(), Some("spotify:track:x"));
        assert_eq!(uris(&queue.next), ["spotify:track:a", "spotify:track:c"]);
        assert_eq!(queue.next[0].provider, Provider::Queue);
        assert!(queue.next[0].uid.is_empty());
    }
}
