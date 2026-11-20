
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use tokio::sync::mpsc;
use tunebox_core::{doctor::Section, paths::Paths, player::PlayerEvent};
use tunebox_os::media::Command;
use tunebox_soundcloud::{Player, SoundCloud, uri_path};

use crate::{
    headless::{Clock, describe, draw_progress},
    media::Media,
    prompt, style,
};

const VOLUME: u16 = u16::MAX / 2;

pub async fn login(paths: &Paths, force: bool) -> Result<bool> {
    let client = SoundCloud::new(paths)?;
    if client.signed_in() && !force {
        match client.me().await {
            Ok(Some(user)) => {
                println!("  {} already signed in as {}", style::green("✓"), user.name);
                println!("  {}\n", style::dim("`tunebox login soundcloud --force` signs in with another account"));
                return Ok(true);
            }
            Ok(None) => {}
            Err(err) => println!("  {} {}\n", style::red("✗"), style::red(&format!("{err:#}"))),
        }
    }
    println!("  Browsing and playing work without an account; signing in adds your likes and playlists.");
    println!("  Looking for the SoundCloud sign-in in your browsers…");
    match browser_sign_in(paths).await {
        Ok(true) => return Ok(true),
        Ok(false) => {}
        Err(err) => println!("  {} {err:#}", style::red("✗")),
    }
    if !prompt::interactive() {
        bail!(
            "No browser is signed in to SoundCloud. Sign in at soundcloud.com, then run `tunebox login soundcloud` again."
        );
    }
    println!("\n  If the browser holds it, the copy is here: developer tools, then Application,");
    println!("  Cookies, soundcloud.com, and the value of {}", style::bold("oauth_token"));
    println!("  {}", style::dim("It is long; the terminal won't show it as you paste.\n"));
    let Some(token) = prompt::secret("oauth_token")? else { return Ok(false) };
    let user = SoundCloud::login_with_token(paths, token).await?;
    println!("  {} signed in as {}\n", style::green("✓"), user.name);
    Ok(true)
}

async fn browser_sign_in(paths: &Paths) -> Result<bool> {
    let Some((browser, token)) = tunebox_soundcloud::browser::oauth_token(paths).await? else {
        println!("  {} no browser is signed in to SoundCloud", style::dim("·"));
        return Ok(false);
    };
    match SoundCloud::login_with_token(paths, token).await {
        Ok(user) => {
            println!("  {} signed in as {} (from {})\n", style::green("✓"), user.name, browser);
            Ok(true)
        }
        Err(err) => {
            println!("  {} the sign-in from {} didn't work: {err:#}", style::red("✗"), browser);
            Ok(false)
        }
    }
}

pub fn logout(paths: &Paths) -> Result<()> {
    let signed_in = SoundCloud::new(paths)?.signed_in();
    paths.soundcloud_token().delete()?;
    let text = if signed_in { "Signed out of SoundCloud" } else { "Not signed in to SoundCloud" };
    println!("\n  {} {text}\n", style::green("✓"));
    Ok(())
}

pub async fn doctor(paths: &Paths, mut report: impl FnMut(Section)) -> Result<()> {
    let client = SoundCloud::new(paths)?;
    report(tunebox_soundcloud::doctor::programs().await);
    report(tunebox_soundcloud::doctor::soundcloud(&client).await);
    report(tunebox_soundcloud::doctor::playback(&client).await);
    Ok(())
}

pub fn signed_in(paths: &Paths) -> Result<bool> {
    Ok(SoundCloud::new(paths)?.signed_in())
}

pub async fn play(paths: &Paths, input: &str, shuffle: bool) -> Result<()> {
    let client = SoundCloud::new(paths)?;
    let (context, track) = playable(&client, input).await?;
    let (events_tx, mut events) = mpsc::unbounded_channel();
    let player = Player::spawn(client, paths, VOLUME, events_tx).await?;
    if shuffle {
        player.play_shuffled(context)?;
    } else {
        player.play(context, track)?;
    }

    let (mut media, mut media_events) = Media::new();
    println!();
    let mut clock = Clock::default();
    let mut duration = Duration::ZERO;
    let (mut paused, mut played) = (false, false);
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    let result = loop {
        tokio::select! {
            Some(event) = media_events.recv() => media.on_event(event, |command| control(&player, command, paused)),
            Some(event) = events.recv() => {
                media.on_player(&event);
                match event {
                    PlayerEvent::TrackChanged { track } => {
                        played = true;
                        duration = Duration::from_millis(track.duration_ms.into());
                        clock = Clock::default();
                        println!("\r\x1b[2K  ▶  {}", describe(&track));
                    }
                    PlayerEvent::Playing { position_ms } => {
                        paused = false;
                        clock.resume(position_ms);
                    }
                    PlayerEvent::Paused { position_ms } => {
                        paused = true;
                        clock.pause(position_ms);
                    }
                    PlayerEvent::Seeked { position_ms } | PlayerEvent::PositionChanged { position_ms } if paused => {
                        clock.pause(position_ms);
                    }
                    PlayerEvent::Seeked { position_ms } | PlayerEvent::PositionChanged { position_ms } => {
                        clock.resume(position_ms);
                    }
                    PlayerEvent::Blocked { reason } => println!("\r\x1b[2K  ✕  {reason}"),
                    PlayerEvent::Stopped if played => break Ok(()),
                    PlayerEvent::Stopped => break Err(anyhow!("SoundCloud can't play {input}")),
                    _ => {}
                }
            }
            _ = tick.tick(), if !duration.is_zero() => draw_progress(clock.position().min(duration), duration),
            _ = tokio::signal::ctrl_c() => break Ok(()),
        }
    };
    player.shutdown();
    println!("\n");
    result
}

async fn playable(client: &SoundCloud, input: &str) -> Result<(String, Option<String>)> {
    let path = uri_path(input).with_context(|| format!("Not a SoundCloud link: {input}"))?;
    let (kind, is_album) = client.kind(path).await?;
    Ok(match kind.as_str() {
        "track" => (format!("soundcloud:track:{path}"), None),
        "playlist" if is_album => (format!("soundcloud:album:{path}"), None),
        "playlist" => (format!("soundcloud:playlist:{path}"), None),
        "user" => (format!("soundcloud:artist:{path}"), None),
        other => bail!("SoundCloud can't play a {other}"),
    })
}

fn control(player: &Player, command: Command, paused: bool) -> Result<()> {
    match command {
        Command::Play if paused => player.toggle(true),
        Command::Pause if !paused => player.pause(),
        Command::Play | Command::Pause => Ok(()),
        Command::Toggle => player.toggle(true),
        Command::Next => player.next(),
        Command::Previous => player.previous(),
        Command::Seek(position) => player.seek(position.as_millis().try_into().unwrap_or(u32::MAX)),
        Command::Volume(volume) => player.set_volume(volume),
    }
}
