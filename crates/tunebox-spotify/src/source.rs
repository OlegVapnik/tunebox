
use std::{path::PathBuf, sync::Arc};

use anyhow::{Result, bail};
use async_trait::async_trait;
use futures::{StreamExt, future::BoxFuture, stream, stream::BoxStream};
use tunebox_core::{
    catalog::{Page, Track},
    paths::Paths,
};
use tunebox_mpv::{Source as PlaybackSource, Stream, ytdlp};

use crate::{catalog, webapi::WebApi};

const AUTOPLAY: u32 = 20;
const MAX_PAGES: usize = 200;

#[derive(Clone)]
pub struct SpotifySource {
    api: Arc<WebApi>,
    cache_dir: Option<PathBuf>,
}

impl SpotifySource {
    pub fn new(api: Arc<WebApi>, paths: &Paths) -> Self {
        Self { api, cache_dir: Some(paths.cache.join("spotify").join("yt-dlp")) }
    }

    async fn pages(&self, context: &str) -> Result<Vec<Vec<Track>>> {
        let pages = if let Some(id) = id_of(context, "track") {
            vec![vec![catalog::track(&self.api, id).await?]]
        } else if context.ends_with(":collection") {
            let api = self.api.clone();
            paged(move |offset| {
                let api = api.clone();
                Box::pin(async move { catalog::liked_tracks(&api, offset).await })
            })
            .await?
        } else if let Some(id) = id_of(context, "playlist") {
            let (api, id) = (self.api.clone(), id.to_owned());
            paged(move |offset| {
                let (api, id) = (api.clone(), id.clone());
                Box::pin(async move { catalog::playlist_tracks(&api, &id, offset).await })
            })
            .await?
        } else if let Some(id) = id_of(context, "album") {
            let album = catalog::album(&self.api, id).await?;
            let api = self.api.clone();
            paged(move |offset| {
                let (api, album) = (api.clone(), album.clone());
                Box::pin(async move { catalog::album_tracks(&api, &album, offset).await })
            })
            .await?
        } else if let Some(id) = id_of(context, "artist") {
            vec![catalog::artist_top_tracks(&self.api, id).await?]
        } else {
            bail!("Spotify can't play {context}");
        };
        let mut pages: Vec<Vec<Track>> = pages.into_iter().filter(|page| !page.is_empty()).collect();
        pages.push(Vec::new());
        Ok(pages)
    }
}

async fn paged(mut fetch: impl FnMut(u32) -> BoxFuture<'static, Result<Page<Track>>>) -> Result<Vec<Vec<Track>>> {
    let mut pages = Vec::new();
    let mut offset = 0;
    for _ in 0..MAX_PAGES {
        let found = fetch(offset).await?;
        let next = found.next_offset;
        pages.push(found.items);
        match next {
            Some(next) => offset = next,
            None => break,
        }
    }
    Ok(pages)
}

fn id_of<'a>(uri: &'a str, kind: &str) -> Option<&'a str> {
    uri.strip_prefix("spotify:")?.strip_prefix(kind)?.strip_prefix(':')
}

#[async_trait]
impl PlaybackSource for SpotifySource {
    fn name(&self) -> &'static str {
        "Spotify"
    }

    fn is_track(&self, uri: &str) -> bool {
        uri.starts_with("spotify:track:")
    }

    fn key(&self, uri: &str) -> String {
        id_of(uri, "track").unwrap_or(uri).to_owned()
    }

    fn context(&self, context: String) -> BoxStream<'static, Result<Vec<Track>>> {
        let this = self.clone();
        Box::pin(stream::once(async move { this.pages(&context).await }).flat_map(|pages: Result<Vec<Vec<Track>>>| {
            match pages {
                Ok(pages) => stream::iter(pages.into_iter().map(Ok).collect::<Vec<_>>()).boxed(),
                Err(err) => stream::iter(vec![Err(err)]).boxed(),
            }
        }))
    }

    async fn track(&self, uri: String) -> Result<Option<Track>> {
        let Some(id) = id_of(&uri, "track") else { return Ok(None) };
        Ok(Some(catalog::track(&self.api, id).await?))
    }

    async fn resolve(&self, key: String) -> Result<Stream> {
        let track = catalog::track(&self.api, &key).await?;
        let artist = track.artists.join(", ");
        if artist.is_empty() {
            bail!("Spotify doesn't say who made this song");
        }
        let query = format!("{artist} - {}", track.name);
        ytdlp::resolve(&format!("ytsearch1:{query}"), self.cache_dir.clone(), "Spotify").await
    }

    async fn autoplay(&self, seed: String) -> Result<Vec<Track>> {
        let Some(id) = id_of(&seed, "track") else { return Ok(Vec::new()) };
        match catalog::recommendations(&self.api, id, AUTOPLAY).await {
            Ok(tracks) => Ok(tracks),
            Err(err) => {
                tracing::debug!("Spotify has no recommendations for {id}: {err:#}");
                Ok(Vec::new())
            }
        }
    }
}

impl std::fmt::Debug for SpotifySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpotifySource").finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_ids_from_uris() {
        assert_eq!(id_of("spotify:track:abc", "track"), Some("abc"));
        assert_eq!(id_of("spotify:album:abc", "album"), Some("abc"));
        assert_eq!(id_of("spotify:artist:abc", "artist"), Some("abc"));
        assert_eq!(id_of("spotify:playlist:abc", "playlist"), Some("abc"));
        assert_eq!(id_of("spotify:track:", "track"), Some(""));
        assert_eq!(id_of("spotify:album:abc", "track"), None);
        assert_eq!(id_of("ytmusic:track:abc", "track"), None);
    }
}
