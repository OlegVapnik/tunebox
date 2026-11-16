//! `tunebox play` and `tunebox connect`: Spotify without the interface.

use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use librespot::{
    connect::{LoadContextOptions, LoadRequest, LoadRequestOptions, Options, PlayingTrack, Spirc},
    core::SpotifyUri,
    metadata::{Metadata, Track},
};
use tunebox_core::{paths::Paths, player::PlayerEvent};
use tunebox_os::media::Command;
use tunebox_player::{DEFAULT_VOLUME, Engine, events};
use tunebox_spotify::session;

use crate::{
    headless::{Clock, describe, draw_progress},
    media::Media,
    style,
};

const DEVICE_NAME: &str = "tunebox";

/// Plays standalone: tunebox activates itself as the playing device and loads the context.
/// Without Premium, the songs come from YouTube and play through mpv.
pub async fn play(paths: &Paths, input: &str, shuffle: bool) -> Result<()> {
    if !session::is_logged_in(paths)? {
        return play_free(paths, input, shuffle).await;
    }
    let uri = parse_uri(input)?;
    let pending = session::prepare(paths)?;
    let session = pending.session.clone();
    let (engine, mut lost_outputs) = Engine::new(&session, Duration::ZERO)?;
    let mut player_events = engine.player.get_player_event_channel();
    let (spirc, task) = engine.connect_device(pending, DEVICE_NAME.into(), DEFAULT_VOLUME).await?;
    session::remember(paths, &session)?;
    tokio::pin!(task);

    let mut options = LoadRequestOptions {
        start_playing: true,
        context_options: Some(LoadContextOptions::Options(Options { shuffle, ..Options::default() })),
        ..LoadRequestOptions::default()
    };
    // Like Spotify itself: a single track plays within its album, so next/previous work.
    let context = match &uri {
        SpotifyUri::Track { .. } => {
            let track = Track::get(&session, &uri).await?;
            options.playing_track = Some(PlayingTrack::Uri(uri.to_uri()));
            track.album.id.to_uri()
        }
        _ => uri.to_uri(),
    };
    spirc.activate()?;
    spirc.load(LoadRequest::from_context_uri(context, options))?;

    let (mut media, mut media_events) = Media::new();
    println!();
    let mut clock = Clock::default();
    let mut duration = Duration::ZERO;
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    loop {
        tokio::select! {
            () = &mut task => break,
            Some(event) = media_events.recv() => media.on_event(event, |command| control(&spirc, command)),
            Some(name) = lost_outputs.recv() => {
                spirc.pause()?;
                println!("\r\x1b[2K  {}", style::dim(&format!("❚❚ paused: {name} disconnected")));
            }
            Some(event) = player_events.recv() => {
                let Some(event) = events::neutral(event) else { continue };
                let event = events::refusals(event, Some(&session));
                media.on_player(&event);
                match event {
                    PlayerEvent::Unavailable { refused, .. } => println!("\r\x1b[2K  {}", unavailable(refused)),
                    PlayerEvent::TrackChanged { track } => {
                        duration = Duration::from_millis(track.duration_ms.into());
                        clock = Clock::default();
                        println!("\r\x1b[2K  ▶  {}", describe(&track));
                    }
                    PlayerEvent::Playing { position_ms } => clock.resume(position_ms),
                    PlayerEvent::Paused { position_ms } => clock.pause(position_ms),
                    _ => {}
                }
            }
            _ = tick.tick(), if !duration.is_zero() => draw_progress(clock.position().min(duration), duration),
            _ = tokio::signal::ctrl_c() => {
                spirc.shutdown()?;
                (&mut task).await;
                break;
            }
        }
    }
    println!("\n");
    Ok(())
}

/// Plays through mpv, the songs found on YouTube: no librespot and no Premium.
async fn play_free(paths: &Paths, input: &str, shuffle: bool) -> Result<()> {
    let config = tunebox_core::config::Config::load(paths)?;
    let api =
        std::sync::Arc::new(tunebox_spotify::webapi::WebApi::new(config.sp_dc(paths)?, paths.webapi_token())?);
    if !api.is_logged_in().await {
        bail!("Not signed in to Spotify – run `tunebox login spotify` first.");
    }
    let context = parse_uri(input)?.to_uri();
    let (events_tx, mut received) = tokio::sync::mpsc::unbounded_channel();
    let player = tunebox_spotify::Player::spawn(api, paths, DEFAULT_VOLUME, events_tx).await?;
    if shuffle {
        player.play_shuffled(context)?;
    } else {
        player.play(context, None)?;
    }

    let (mut media, mut media_events) = Media::new();
    println!();
    let mut clock = Clock::default();
    let mut duration = Duration::ZERO;
    let (mut paused, mut played) = (false, false);
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    let result = loop {
        tokio::select! {
            Some(event) = media_events.recv() => media.on_event(event, |command| control_free(&player, command, paused)),
            Some(event) = received.recv() => {
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
                    PlayerEvent::Stopped => break Err(anyhow!("Spotify can't play {input}")),
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

/// Remote mode: waits until a Spotify app picks tunebox as its playback device.
pub async fn connect(paths: &Paths, name: String) -> Result<()> {
    let pending = session::prepare(paths)?;
    let session = pending.session.clone();
    let (engine, mut lost_outputs) = Engine::new(&pending.session, Duration::ZERO)?;
    let mut player_events = engine.player.get_player_event_channel();
    let (spirc, task) = engine.connect_device(pending, name.clone(), DEFAULT_VOLUME).await?;
    session::remember(paths, &session)?;
    tokio::pin!(task);

    println!(
        "\n  {} {} is online. Pick it as a device in the Spotify app.  {}\n",
        style::green("◉"),
        style::bold(&name),
        style::dim("Ctrl+C quits")
    );
    let (mut media, mut media_events) = Media::new();
    let mut paused = false;
    loop {
        tokio::select! {
            () = &mut task => break,
            Some(event) = media_events.recv() => media.on_event(event, |command| control(&spirc, command)),
            Some(name) = lost_outputs.recv() => {
                spirc.pause()?;
                paused = true;
                println!("  {}", style::dim(&format!("❚❚ paused: {name} disconnected")));
            }
            Some(event) = player_events.recv() => {
                let Some(event) = events::neutral(event) else { continue };
                let event = events::refusals(event, Some(&session));
                media.on_player(&event);
                match event {
                    PlayerEvent::Unavailable { refused, .. } => println!("  {}", unavailable(refused)),
                    PlayerEvent::TrackChanged { track } => println!("  ▶  {}", describe(&track)),
                    PlayerEvent::Paused { .. } if !paused => {
                        paused = true;
                        println!("  {}", style::dim("❚❚ paused"));
                    }
                    PlayerEvent::Playing { .. } if paused => {
                        paused = false;
                        println!("  {}", style::dim("▶  resumed"));
                    }
                    _ => {}
                }
            }
            _ = tokio::signal::ctrl_c() => {
                spirc.shutdown()?;
                (&mut task).await;
                break;
            }
        }
    }
    Ok(())
}

/// Hands a request from the system's media controls to Spirc.
fn control(spirc: &Spirc, command: Command) -> Result<()> {
    match command {
        Command::Play => spirc.play()?,
        Command::Pause => spirc.pause()?,
        Command::Toggle => spirc.play_pause()?,
        Command::Next => spirc.next()?,
        Command::Previous => spirc.prev()?,
        Command::Seek(position) => spirc.set_position_ms(position.as_millis().try_into().unwrap_or(u32::MAX))?,
        Command::Volume(volume) => spirc.set_volume(volume)?,
    }
    Ok(())
}

/// Hands a request from the system's media controls to the mpv player.
fn control_free(player: &tunebox_spotify::Player, command: Command, paused: bool) -> Result<()> {
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

/// Accepts `spotify:…` URIs as well as `https://open.spotify.com/…` links.
fn parse_uri(input: &str) -> Result<SpotifyUri> {
    let uri = match input.strip_prefix("https://open.spotify.com/") {
        Some(path) => {
            let mut parts = path.split(['/', '?']).filter(|p| !p.is_empty() && !p.starts_with("intl-"));
            match (parts.next(), parts.next()) {
                (Some(kind), Some(id)) => format!("spotify:{kind}:{id}"),
                _ => bail!("Unknown Spotify link: {input}"),
            }
        }
        None => input.to_owned(),
    };
    SpotifyUri::from_uri(&uri).with_context(|| format!("“{input}” is not a valid Spotify URI"))
}

/// What `tunebox play` and `connect` print for a track that couldn't load.
fn unavailable(refused: bool) -> String {
    if refused {
        style::red(
            "✗  Spotify refused the key to this song, so it was skipped: github.com/OlegVapnik/tunebox/blob/main/website/src/pages/troubleshooting",
        )
    } else {
        style::dim("✗  this song couldn't load, so it was skipped")
    }
}
