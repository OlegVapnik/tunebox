//! Checks what SoundCloud needs: the programs playback runs on, its public API, the sign-in,
//! and a stream URL actually resolving, as `tunebox doctor` sections.

use std::{
    process::Stdio,
    time::{Duration, Instant},
};

use anyhow::{Result, bail};
use tunebox_core::doctor::{Check, Expect, Outcome, Section, outcome};
use tokio::process::Command;

use crate::{SoundCloud, uri_path};

const TIMEOUT: Duration = Duration::from_secs(45);

/// Every check, in the order `tunebox doctor` shows them.
pub async fn all(client: &SoundCloud) -> Vec<Section> {
    vec![programs().await, soundcloud(client).await, playback(client).await]
}

/// mpv plays, yt-dlp finds SoundCloud streams when its own HLS path can't.
pub async fn programs() -> Section {
    let checks = vec![
        version("mpv", "mpv", &["--version"], Expect::Available).await,
        version("yt-dlp", "yt-dlp", &["--version"], Expect::Available).await,
    ];
    Section { title: "PROGRAMS · playback", checks }
}

/// SoundCloud's public API reaches it; signing in only adds the library and likes.
pub async fn soundcloud(client: &SoundCloud) -> Section {
    let mut checks = vec![
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
        timed("Home", Expect::Available, async {
            let home = client.home(None).await?;
            if home.shelves.is_empty() {
                bail!("SoundCloud returned no shelves")
            }
            Ok(format!("{} shelves", home.shelves.len()))
        })
        .await,
    ];
    let signed_in = if client.signed_in() {
        timed("Signed in", Expect::Available, async {
            let me = client.me().await?.ok_or_else(|| anyhow::anyhow!("SoundCloud didn't accept the sign-in"))?;
            let liked = client.liked_count().await?;
            Ok(format!("{} · {liked} liked songs", me.name))
        })
        .await
    } else {
        let detail = "signed out: browsing and playing work; `tunebox login soundcloud` adds your library";
        Check { name: "Signed in".into(), expect: Expect::Optional, outcome: Outcome::Ok(detail.into()) }
    };
    checks.push(signed_in);
    Section { title: "SOUNDCLOUD · api-v2", checks }
}

/// A stream URL resolves, for a track found by Search, which needs yt-dlp and SoundCloud to agree.
pub async fn playback(client: &SoundCloud) -> Section {
    let check = timed("Stream URL", Expect::Available, async {
        let started = Instant::now();
        let track = client
            .search("daft punk")
            .await?
            .tracks
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("SoundCloud returned no songs for the probe"))?;
        let path = uri_path(&track.uri)
            .ok_or_else(|| anyhow::anyhow!("SoundCloud track without a path: {}", track.uri))?
            .to_owned();
        client.stream_url(&path).await?;
        Ok(format!("in {:.1} s", started.elapsed().as_secs_f32()))
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

/// Like [`tunebox_core::doctor::timed`], with time for SoundCloud, and how long the probe took.
async fn timed(name: &str, expect: Expect, probe: impl Future<Output = Result<String>>) -> Check {
    let started = Instant::now();
    let outcome = match outcome(TIMEOUT, probe).await {
        Outcome::Ok(detail) => Outcome::Ok(format!("{detail} ({} ms)", started.elapsed().as_millis())),
        failed => failed,
    };
    Check { name: name.into(), expect, outcome }
}
