
use std::path::PathBuf;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use futures::{StreamExt, stream, stream::BoxStream};
use tunebox_core::{catalog::Track, paths::Paths};
use tunebox_mpv::{Source as PlaybackSource, Stream, ytdlp};

use crate::{
    Source, YtMusic, pages,
    parse::{self, ALBUM, ARTIST, PLAYLIST, SONGS, TRACK},
};

#[derive(Clone)]
pub struct YtMusicSource {
    client: YtMusic,
    cache_dir: Option<PathBuf>,
    cookies: Option<PathBuf>,
}

impl YtMusicSource {
    pub fn new(client: YtMusic, paths: &Paths) -> Self {
        Self {
            client,
            cache_dir: Some(paths.cache.join("ytmusic").join("yt-dlp")),
            cookies: Some(paths.ytmusic_cookies().file().to_path_buf()),
        }
    }
}

#[async_trait]
impl PlaybackSource for YtMusicSource {
    fn name(&self) -> &'static str {
        "YouTube Music"
    }

    fn is_track(&self, uri: &str) -> bool {
        uri.starts_with(TRACK)
    }

    fn key(&self, uri: &str) -> String {
        uri.strip_prefix(TRACK).unwrap_or(uri).to_owned()
    }

    fn context(&self, context: String) -> BoxStream<'static, Result<Vec<Track>>> {
        let client = self.client.clone();
        let pages_for = move |source: Source| -> BoxStream<'static, Result<Vec<Track>>> {
            pages::tracks(client.clone(), source)
                .map(|page| page.map(|page| page.items))
                .chain(stream::once(async { Ok(Vec::new()) }))
                .boxed()
        };
        if context.starts_with(TRACK) {
            let client = self.client.clone();
            return Box::pin(stream::once(async move {
                let track = client.tracks_by_uri(std::slice::from_ref(&context)).await;
                let track =
                    track.and_then(|t| if t.is_empty() { Err(anyhow!("This song isn't available")) } else { Ok(t) });
                track
            }));
        }
        if let Some(videos) = context.strip_prefix(SONGS) {
            let uris: Vec<_> = videos.split(',').filter(|v| !v.is_empty()).map(|v| format!("{TRACK}{v}")).collect();
            let client = self.client.clone();
            return Box::pin(stream::once(async move {
                let tracks = client.tracks_by_uri(&uris).await;
                tracks.and_then(|t| if t.is_empty() { Err(anyhow!("These songs aren't available")) } else { Ok(t) })
            }));
        }
        if context.starts_with(ALBUM) {
            return pages_for(Source::Album(context));
        }
        if context.starts_with(PLAYLIST) {
            return pages_for(Source::Playlist(context));
        }
        if context.starts_with(ARTIST) {
            let client = self.client.clone();
            return Box::pin(
                stream::once(async move {
                    match pages::artist_songs(client.api(), &context).await {
                        Ok((_, Some(playlist))) => {
                            let source = Source::Playlist(format!("{PLAYLIST}{playlist}"));
                            Box::pin(
                                pages::tracks(client.clone(), source)
                                    .map(|page| page.map(|page| page.items))
                                    .chain(stream::once(async { Ok(Vec::new()) }))
                                    .boxed(),
                            ) as BoxStream<'static, Result<Vec<Track>>>
                        }
                        Ok((artist, None)) => Box::pin(stream::once(async move {
                            Ok(artist.top_tracks.into_iter().map(|track| track.track).collect())
                        })),
                        Err(err) => Box::pin(stream::once(async move { Err(err) })),
                    }
                })
                .flatten(),
            );
        }
        Box::pin(stream::once(async move { Err(anyhow!("YouTube Music can't play {context}")) }))
    }

    async fn track(&self, uri: String) -> Result<Option<Track>> {
        Ok(self.client.tracks_by_uri(std::slice::from_ref(&uri)).await?.into_iter().next())
    }

    async fn resolve(&self, key: String) -> Result<Stream> {
        let url = format!("https://music.youtube.com/watch?v={key}");
        ytdlp::resolve_with(&url, self.cache_dir.clone(), "YouTube Music", self.cookies.as_deref()).await
    }

    async fn autoplay(&self, seed: String) -> Result<Vec<Track>> {
        self.client.radio(&seed).await
    }

    fn large_cover(&self, url: &str) -> String {
        parse::large(url)
    }
}
