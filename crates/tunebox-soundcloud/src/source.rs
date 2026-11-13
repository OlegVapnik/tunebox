//! SoundCloud as a playback source: URIs to songs, context pages and stream URLs.

use std::path::PathBuf;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use futures::{StreamExt, stream, stream::BoxStream};
use tunebox_core::{catalog::Track, paths::Paths};
use tunebox_mpv::{Source as PlaybackSource, Stream, ytdlp};

use crate::{SoundCloud, Source, uri_path};

const TRACK: &str = "soundcloud:track:";
const SONGS: &str = "soundcloud:songs:";

/// SoundCloud playback: its API for the catalog and streams, yt-dlp where it has none.
#[derive(Clone)]
pub struct SoundCloudSource {
    client: SoundCloud,
    cache_dir: Option<PathBuf>,
}

impl SoundCloudSource {
    pub fn new(client: SoundCloud, paths: &Paths) -> Self {
        Self { client, cache_dir: Some(paths.cache.join("soundcloud").join("yt-dlp")) }
    }

    fn pages(client: SoundCloud, source: Source) -> BoxStream<'static, Result<Vec<Track>>> {
        Box::pin(
            stream::once(async move {
                let pages = client.pages(source).await?;
                Ok::<_, anyhow::Error>(
                    pages.into_iter().map(|page| page.items).chain(std::iter::once(Vec::new())).collect::<Vec<_>>(),
                )
            })
            .flat_map(|pages: Result<Vec<Vec<Track>>>| match pages {
                Ok(pages) => stream::iter(pages.into_iter().map(Ok).collect::<Vec<_>>()).boxed(),
                Err(err) => stream::iter(vec![Err(err)]).boxed(),
            }),
        )
    }
}

#[async_trait]
impl PlaybackSource for SoundCloudSource {
    fn name(&self) -> &'static str {
        "SoundCloud"
    }

    fn is_track(&self, uri: &str) -> bool {
        uri.starts_with(TRACK)
    }

    fn key(&self, uri: &str) -> String {
        uri_path(uri).unwrap_or(uri).to_owned()
    }

    fn context(&self, context: String) -> BoxStream<'static, Result<Vec<Track>>> {
        if context.starts_with(TRACK) {
            let client = self.client.clone();
            return Box::pin(stream::once(async move {
                let uri = context.clone();
                match client.track_by_uri(&uri).await {
                    Ok(Some(track)) => Ok(vec![track]),
                    Ok(None) => Err(anyhow!("This song isn't on SoundCloud any more")),
                    Err(err) => Err(err),
                }
            }));
        }
        if let Some(paths) = context.strip_prefix(SONGS) {
            let client = self.client.clone();
            let uris: Vec<String> = paths.split(',').filter(|p| !p.is_empty()).map(|p| format!("{TRACK}{p}")).collect();
            return Box::pin(stream::once(async move {
                let tracks = client.tracks_by_uri(&uris).await?;
                if tracks.is_empty() { Err(anyhow!("These songs aren't on SoundCloud any more")) } else { Ok(tracks) }
            }));
        }
        if context == "soundcloud:playlist:likes" {
            return Self::pages(self.client.clone(), Source::Liked);
        }
        if context.starts_with("soundcloud:playlist:") {
            return Self::pages(self.client.clone(), Source::Playlist(context));
        }
        if context.starts_with("soundcloud:album:") {
            return Self::pages(self.client.clone(), Source::Album(context));
        }
        if context.starts_with("soundcloud:artist:") {
            let client = self.client.clone();
            return Box::pin(stream::once(async move {
                let artist = client.artist(&context).await?;
                Ok(artist.top_tracks.into_iter().map(|track| track.track).collect())
            }));
        }
        Box::pin(stream::once(async move { Err(anyhow!("SoundCloud can't play {context}")) }))
    }

    async fn track(&self, uri: String) -> Result<Option<Track>> {
        self.client.track_by_uri(&uri).await
    }

    async fn resolve(&self, key: String) -> Result<Stream> {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(60 * 60);
        match self.client.stream_url(&key).await {
            Ok(url) => Ok(Stream { url, until }),
            Err(err) => {
                tracing::warn!("SoundCloud's own stream for {key} failed ({err:#}); asking yt-dlp");
                let url = format!("https://soundcloud.com/{key}");
                ytdlp::resolve(&url, self.cache_dir.clone(), "SoundCloud").await
            }
        }
    }

    async fn autoplay(&self, seed: String) -> Result<Vec<Track>> {
        self.client.related(&seed).await
    }
}
