//! Checks what YouTube Music needs: the programs playback runs on, InnerTube, the sign-in, and
//! a stream URL actually resolving, as `tunebox doctor` sections.

use std::{
    path::Path,
    process::Stdio,
    time::Duration,
};

use anyhow::{Result, bail};
use tokio::process::Command;
use tunebox_core::doctor::{Check, Expect, Outcome, Section, outcome};

use crate::YtMusic;

/// Starboy, which has lyrics and a radio.
const TRACK: &str = "ytmusic:track:plnfIj7dkJE";
const TIMEOUT: Duration = Duration::from_secs(45);

/// Every check, in the order `tunebox doctor` shows them.
pub async fn all(client: &YtMusic) -> Vec<Section> {
    vec![programs().await, innertube(client).await, playback().await]
}

/// mpv plays, yt-dlp finds the streams, and yt-dlp needs deno for YouTube's challenges.
pub async fn programs() -> Section {
    let checks = vec![
        version("mpv", "mpv", &["--version"], Expect::Available).await,
        version("yt-dlp", "yt-dlp", &["--version"], Expect::Available).await,
        version("deno (for yt-dlp)", "deno", &["--version"], Expect::Available).await,
    ];
    Section { title: "PROGRAMS · playback", checks }
}

pub async fn innertube(client: &YtMusic) -> Section {
    let mut checks = vec![
        timed("Home", Expect::Available, async {
            let home = client.home(None).await?;
            Ok(format!("{} shelves", home.shelves.len()))
        })
        .await,
        timed("Search", Expect::Available, async {
            let results = client.search("daft punk").await?;
            Ok(format!(
                "{} songs · {} albums · {} artists · {} playlists",
                results.tracks.len(),
                results.albums.len(),
                results.artists.len(),
                results.playlists.len()
            ))
        })
        .await,
        timed("Song radio", Expect::Available, async { Ok(format!("{} songs", client.radio(TRACK).await?.len())) })
            .await,
        timed("Lyrics", Expect::Optional, async {
            let query = tunebox_core::lyrics::LyricsQuery {
                uri: TRACK,
                title: "Starboy",
                artist: "The Weeknd",
                album: "Starboy",
                duration_ms: 230_000,
            };
            match client.lyrics(&query).await? {
                Some(lyrics) => Ok(format!(
                    "{} lines · {} · {}",
                    lyrics.lines.len(),
                    if lyrics.synced { "time-synced" } else { "plain" },
                    lyrics.provider
                )),
                None => bail!("none found"),
            }
        })
        .await,
    ];
    let account = if client.signed_in() {
        timed("Signed in", Expect::Available, async {
            // The address tells a work and a private account of the same name apart.
            let user = match client.account().await? {
                Some(account) => account.label(),
                None => client.me().await?.map(|u| u.name).unwrap_or_default(),
            };
            let liked = client.liked_count().await?;
            Ok(format!("{user} · {liked} liked songs"))
        })
        .await
    } else {
        let detail = "signed out: browsing and playing work; `tunebox login ytmusic` adds your library";
        Check { name: "Signed in".into(), expect: Expect::Optional, outcome: Outcome::Ok(detail.into()) }
    };
    checks.push(account);
    Section { title: "INNERTUBE · music.youtube.com", checks }
}

/// A stream URL resolves, which needs yt-dlp, deno and YouTube to agree.
pub async fn playback() -> Section {
    playback_with(None).await
}

/// As `playback`, with the saved sign-in so YouTube's bot check doesn't refuse the stream.
pub async fn playback_with(cookies: Option<&Path>) -> Section {
    let check = timed("Stream URL (yt-dlp)", Expect::Available, async {
        let started = std::time::Instant::now();
        let video = TRACK.trim_start_matches("ytmusic:track:");
        let url = format!("https://music.youtube.com/watch?v={video}");
        let stream = tunebox_mpv::ytdlp::resolve_with(&url, None, "YouTube Music", cookies).await?.url;
        let itag = url::Url::parse(&stream)
            .ok()
            .and_then(|url| url.query_pairs().find(|(k, _)| k == "itag").map(|(_, v)| v.into_owned()));
        Ok(format!("itag {} in {:.1} s", itag.unwrap_or_else(|| "?".into()), started.elapsed().as_secs_f32()))
    })
    .await;
    Section { title: "STREAMS", checks: vec![check] }
}

async fn version(name: &str, program: &str, args: &[&str], expect: Expect) -> Check {
    let output = Command::new(program).args(args).stdin(Stdio::null()).kill_on_drop(true).output().await;
    let outcome = match output {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout);
            Outcome::Ok(text.lines().next().unwrap_or_default().trim().to_owned())
        }
        Ok(output) => {
            Outcome::Failed(String::from_utf8_lossy(&output.stderr).lines().next().unwrap_or("failed").to_owned())
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Outcome::Failed("not installed".into()),
        Err(err) => Outcome::Failed(err.to_string()),
    };
    Check { name: name.into(), expect, outcome }
}

/// Like [`tunebox_core::doctor::timed`], with time for yt-dlp, and how long the probe took.
async fn timed(name: &str, expect: Expect, probe: impl Future<Output = Result<String>>) -> Check {
    let started = std::time::Instant::now();
    let outcome = match outcome(TIMEOUT, probe).await {
        Outcome::Ok(detail) => Outcome::Ok(format!("{detail} ({} ms)", started.elapsed().as_millis())),
        failed => failed,
    };
    Check { name: name.into(), expect, outcome }
}
