
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use futures::{StreamExt, stream::BoxStream};
use serde_json::json;
use tokio::{
    sync::mpsc::{self, UnboundedReceiver, UnboundedSender},
    task::JoinHandle,
};
use tunebox_core::{
    catalog::Track,
    player::{CurrentTrack, PlayerEvent, QueueTrack},
};

pub use crate::ytdlp::BotCheck;
use crate::{
    mpv::{Events, Mpv, MpvEvent},
    queue::Queue,
    ytdlp::Stream,
};

#[async_trait]
pub trait Source: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    fn is_track(&self, uri: &str) -> bool;
    fn key(&self, uri: &str) -> String;
    fn context(&self, uri: String) -> BoxStream<'static, Result<Vec<Track>>>;
    async fn track(&self, uri: String) -> Result<Option<Track>>;
    async fn resolve(&self, key: String) -> Result<Stream>;
    async fn autoplay(&self, seed: String) -> Result<Vec<Track>>;
    fn large_cover(&self, url: &str) -> String {
        url.to_owned()
    }
}

const RESTART_AFTER_MS: u32 = 3_000;
const POSITION_INTERVAL: Duration = Duration::from_secs(1);
const APPEND_BEFORE_END_MS: u32 = 20_000;
const ATTEMPTS: u8 = 3;
const MAX_SKIPS_IN_A_ROW: u32 = 5;
const SEEK_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub struct Player {
    commands: UnboundedSender<Command>,
}

#[derive(Debug)]
enum Command {
    Play { context: String, track: Option<String>, position_ms: u32, shuffle: Option<bool> },
    Toggle { has_track: bool },
    Pause,
    Next,
    Previous,
    Seek(u32),
    Volume(u16),
    Shuffle(bool),
    Repeat { context: bool, track: bool },
    ClearQueue,
    AddToQueue(String),
    SkipTo(String),
    Prefetch(String),
    Shutdown,
}

impl Player {
    pub async fn spawn(source: Arc<dyn Source>, volume: u16, events: UnboundedSender<PlayerEvent>) -> Result<Player> {
        let args = std::env::var("TUNEBOX_MPV_ARGS").unwrap_or_default();
        Self::spawn_with_args(source, volume, events, args.split_whitespace().map(str::to_owned).collect()).await
    }

    pub async fn spawn_with_args(
        source: Arc<dyn Source>,
        volume: u16,
        events: UnboundedSender<PlayerEvent>,
        mpv_args: Vec<String>,
    ) -> Result<Player> {
        let (mpv_tx, mpv_rx) = mpsc::unbounded_channel();
        let mpv = Mpv::spawn(&with_volume(&mpv_args, volume), mpv_tx.clone()).await?;
        let (commands, commands_rx) = mpsc::unbounded_channel();
        let (jobs, jobs_rx) = mpsc::unbounded_channel();
        let engine = Engine { mpv: Some(mpv), mpv_args, ..Engine::new(source, events, mpv_tx, jobs, volume) };
        let _ = engine.events.send(PlayerEvent::VolumeChanged { volume });
        tokio::spawn(engine.run(commands_rx, mpv_rx, jobs_rx));
        Ok(Player { commands })
    }

    pub fn play(&self, context_uri: String, track_uri: Option<String>) -> Result<()> {
        self.send(Command::Play { context: context_uri, track: track_uri, position_ms: 0, shuffle: None })
    }

    pub fn resume(&self, context: Option<String>, track: String, position_ms: u32) -> Result<()> {
        let context = context.unwrap_or_else(|| track.clone());
        self.send(Command::Play { context, track: Some(track), position_ms, shuffle: None })
    }

    pub fn play_shuffled(&self, context_uri: String) -> Result<()> {
        self.send(Command::Play { context: context_uri, track: None, position_ms: 0, shuffle: Some(true) })
    }

    pub fn toggle(&self, has_track: bool) -> Result<()> {
        self.send(Command::Toggle { has_track })
    }

    pub fn pause(&self) -> Result<()> {
        self.send(Command::Pause)
    }

    pub fn next(&self) -> Result<()> {
        self.send(Command::Next)
    }

    pub fn previous(&self) -> Result<()> {
        self.send(Command::Previous)
    }

    pub fn seek(&self, position_ms: u32) -> Result<()> {
        self.send(Command::Seek(position_ms))
    }

    pub fn set_volume(&self, volume: u16) -> Result<()> {
        self.send(Command::Volume(volume))
    }

    pub fn shuffle(&self, on: bool) -> Result<()> {
        self.send(Command::Shuffle(on))
    }

    pub fn repeat(&self, context: bool, track: bool) -> Result<()> {
        self.send(Command::Repeat { context, track })
    }

    pub fn clear_queue(&self) -> Result<()> {
        self.send(Command::ClearQueue)
    }

    pub fn add_to_queue(&self, uri: &str) -> Result<()> {
        self.send(Command::AddToQueue(uri.to_owned()))
    }

    pub fn prefetch(&self, uri: &str) -> Result<()> {
        self.send(Command::Prefetch(uri.to_owned()))
    }

    pub fn skip_to(&self, uri: &str) -> Result<()> {
        self.send(Command::SkipTo(uri.to_owned()))
    }

    pub fn shutdown(&self) {
        let _ = self.commands.send(Command::Shutdown);
    }

    fn send(&self, command: Command) -> Result<()> {
        self.commands.send(command).map_err(|_| anyhow!("The player has stopped"))
    }
}

fn with_volume(args: &[String], volume: u16) -> Vec<String> {
    let mut args = args.to_vec();
    args.push(format!("--volume={}", mpv_volume(volume)));
    args
}

fn mpv_volume(volume: u16) -> f64 {
    (f64::from(volume) * 100.0 / f64::from(u16::MAX) * 10.0).round() / 10.0
}

enum Job {
    Resolved { key: String, result: Result<Stream> },
    Context { generation: u64, page: Result<Vec<Track>>, last: bool },
    Autoplay { generation: u64, tracks: Result<Vec<Track>> },
    Queued { uri: String, track: Result<Option<Track>> },
}

struct Starting {
    context: String,
    track: Option<String>,
    position_ms: u32,
    shuffle: bool,
    tracks: Vec<Track>,
}

struct Loaded {
    entry: u64,
    uri: String,
}

#[derive(Default)]
struct Autoplay {
    fetching: bool,
    waiting: bool,
}

struct Engine {
    source: Arc<dyn Source>,
    events: UnboundedSender<PlayerEvent>,
    mpv: Option<Mpv>,
    mpv_args: Vec<String>,
    mpv_events: Events,
    jobs: UnboundedSender<Job>,
    queue: Queue,
    adding: VecDeque<(String, Option<Option<Track>>)>,
    streams: HashMap<String, Stream>,
    resolving: HashSet<String>,
    unresolvable: HashSet<String>,
    blocked: bool,
    attempts: HashMap<String, u8>,
    skipped_in_a_row: u32,
    generation: u64,
    loader: Option<JoinHandle<()>>,
    context_loaded: bool,
    starting: Option<Starting>,
    current: Option<Loaded>,
    next: Option<Loaded>,
    dropped: Option<u64>,
    waiting: Option<u32>,
    started: bool,
    paused: bool,
    position_ms: u32,
    duration_ms: Option<u32>,
    seeking: Option<(u32, Instant)>,
    volume: u16,
    active: bool,
    autoplay: Autoplay,
    ended_at: Option<Instant>,
    reported: Option<(String, Option<String>, Vec<QueueTrack>)>,
}

impl Engine {
    fn new(
        source: Arc<dyn Source>,
        events: UnboundedSender<PlayerEvent>,
        mpv_events: Events,
        jobs: UnboundedSender<Job>,
        volume: u16,
    ) -> Self {
        Self {
            source,
            events,
            mpv: None,
            mpv_args: Vec::new(),
            mpv_events,
            jobs,
            queue: Queue::default(),
            adding: VecDeque::new(),
            streams: HashMap::new(),
            resolving: HashSet::new(),
            unresolvable: HashSet::new(),
            blocked: false,
            attempts: HashMap::new(),
            skipped_in_a_row: 0,
            generation: 0,
            loader: None,
            context_loaded: false,
            starting: None,
            current: None,
            next: None,
            dropped: None,
            waiting: None,
            started: false,
            paused: false,
            position_ms: 0,
            duration_ms: None,
            seeking: None,
            volume,
            active: false,
            autoplay: Autoplay::default(),
            ended_at: None,
            reported: None,
        }
    }

    async fn run(
        mut self,
        mut commands: UnboundedReceiver<Command>,
        mut mpv_events: UnboundedReceiver<(u64, MpvEvent)>,
        mut jobs: UnboundedReceiver<Job>,
    ) {
        let mut tick = tokio::time::interval(POSITION_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            let result = tokio::select! {
                command = commands.recv() => match command {
                    Some(Command::Shutdown) | None => break,
                    Some(command) => self.command(command).await,
                },
                Some((instance, event)) = mpv_events.recv() => self.mpv_event(instance, event).await,
                Some(job) = jobs.recv() => self.job(job).await,
                _ = tick.tick() => self.tick().await,
            };
            if let Err(err) = result {
                tracing::warn!("{} player: {err:#}", self.source.name());
            }
        }
        if let Some(loader) = self.loader.take() {
            loader.abort();
        }
        if let Some(mpv) = self.mpv.take() {
            mpv.quit().await;
        }
        if self.active {
            self.emit(PlayerEvent::Stopped);
            self.emit(PlayerEvent::Deactivated);
        }
    }

    fn emit(&self, event: PlayerEvent) {
        let _ = self.events.send(event);
    }

    async fn command(&mut self, command: Command) -> Result<()> {
        tracing::debug!("player: {command:?}");
        match command {
            Command::Play { context, track, position_ms, shuffle } => {
                self.play(context, track, position_ms, shuffle).await
            }
            Command::Toggle { has_track } => self.toggle(has_track).await,
            Command::Pause => self.set_paused(true).await,
            Command::Next => self.skip().await,
            Command::Previous => self.previous().await,
            Command::Seek(position_ms) => self.seek(position_ms).await,
            Command::Volume(volume) => {
                self.volume = volume;
                self.mpv().await?.set("volume", json!(mpv_volume(volume))).await?;
                self.emit(PlayerEvent::VolumeChanged { volume });
                Ok(())
            }
            Command::Shuffle(on) => {
                self.queue.set_shuffle(on);
                if let Some(starting) = self.starting.as_mut() {
                    starting.shuffle = on;
                }
                self.emit(PlayerEvent::ShuffleChanged { shuffle: on });
                self.queue_changed().await
            }
            Command::Repeat { context, track } => {
                self.queue.repeat_context = context;
                self.queue.repeat_track = track;
                self.emit(PlayerEvent::RepeatChanged { context, track });
                self.queue_changed().await
            }
            Command::ClearQueue => {
                self.queue.clear_queue();
                self.adding.clear();
                self.queue_changed().await
            }
            Command::AddToQueue(uri) => {
                self.adding.push_back((uri.clone(), None));
                let (source, jobs) = (self.source.clone(), self.jobs.clone());
                tokio::spawn(async move {
                    let track = source.track(uri.clone()).await;
                    let _ = jobs.send(Job::Queued { uri, track });
                });
                Ok(())
            }
            Command::SkipTo(uri) => {
                if self.queue.skip_to(&uri).is_none() {
                    bail!("{uri} isn't coming up");
                }
                self.begin(0).await
            }
            Command::Prefetch(uri) => {
                if self.source.is_track(&uri) {
                    let key = self.source.key(&uri);
                    if !self.streams.get(&key).is_some_and(Stream::fresh) && !self.resolving.contains(&key) {
                        self.resolve(&key);
                    }
                }
                Ok(())
            }
            Command::Shutdown => Ok(()),
        }
    }

    async fn mpv(&mut self) -> Result<&mut Mpv> {
        if self.mpv.is_none() {
            let mpv = Mpv::spawn(&with_volume(&self.mpv_args, self.volume), self.mpv_events.clone()).await?;
            self.mpv = Some(mpv);
        }
        Ok(self.mpv.as_mut().expect("just started"))
    }

    async fn play(
        &mut self,
        context: String,
        track: Option<String>,
        position_ms: u32,
        shuffle: Option<bool>,
    ) -> Result<()> {
        self.activate();
        self.generation += 1;
        self.context_loaded = false;
        self.autoplay = Autoplay::default();
        if let Some(loader) = self.loader.take() {
            loader.abort();
        }
        self.stop_audio().await?;
        if let Some(track) = &track {
            self.emit(PlayerEvent::Loading { uri: track.clone(), position_ms });
            if self.source.is_track(track) {
                let key = self.source.key(track);
                if !self.streams.get(&key).is_some_and(Stream::fresh) {
                    self.resolve(&key);
                }
            }
        } else if self.source.is_track(&context) {
            let key = self.source.key(&context);
            if !self.streams.get(&key).is_some_and(Stream::fresh) {
                self.resolve(&key);
            }
        }
        if let Some(shuffle) = shuffle.filter(|on| *on != self.queue.shuffle) {
            self.emit(PlayerEvent::ShuffleChanged { shuffle });
        }
        let shuffle = shuffle.unwrap_or(self.queue.shuffle);
        self.starting = Some(Starting { context: context.clone(), track, position_ms, shuffle, tracks: Vec::new() });
        self.loader = Some(load_context(self.source.clone(), self.jobs.clone(), self.generation, context));
        Ok(())
    }

    fn activate(&mut self) {
        self.active = true;
        self.emit(PlayerEvent::Activated);
        self.reported = None;
        self.blocked = false;
    }

    async fn stop_audio(&mut self) -> Result<()> {
        self.current = None;
        self.next = None;
        self.dropped = None;
        self.waiting = None;
        self.started = false;
        self.seeking = None;
        if let Some(mpv) = self.mpv.as_mut() {
            mpv.command(json!(["stop"])).await?;
        }
        Ok(())
    }

    async fn begin(&mut self, start_ms: u32) -> Result<()> {
        let Some(track) = self.queue.current().cloned() else { return self.stopped().await };
        self.stop_audio().await?;
        if self.paused {
            self.mpv().await?.set("pause", json!(false)).await?;
            self.paused = false;
        }
        self.position_ms = start_ms;
        self.emit(PlayerEvent::Loading { uri: track.uri.clone(), position_ms: start_ms });
        self.emit(PlayerEvent::TrackChanged { track: current_track(&track, self.source.as_ref()) });
        self.queue_changed().await?;
        self.load_current(start_ms).await?;
        self.want_autoplay();
        Ok(())
    }

    async fn load_current(&mut self, start_ms: u32) -> Result<()> {
        let Some(track) = self.queue.current().cloned() else { return Ok(()) };
        let key = self.source.key(&track.uri);
        let url = match self.streams.get(&key).filter(|s| s.fresh()) {
            Some(stream) => stream.url.clone(),
            None if self.unresolvable.contains(&key) => format!("ytdl://{key}"),
            None => {
                self.waiting = Some(start_ms);
                self.resolve(&key);
                return Ok(());
            }
        };
        self.waiting = None;
        let start = (start_ms > 0).then(|| f64::from(start_ms) / 1000.0);
        let entry = self.mpv().await?.load(&url, false, start, &media_title(&track)).await?;
        self.current = Some(Loaded { entry, uri: track.uri });
        self.next = None;
        self.dropped = None;
        self.started = false;
        self.seeking = None;
        Ok(())
    }

    fn resolve(&mut self, key: &str) {
        if !self.resolving.insert(key.to_owned()) {
            return;
        }
        let (jobs, source, key) = (self.jobs.clone(), self.source.clone(), key.to_owned());
        tokio::spawn(async move {
            let started = Instant::now();
            let result = source.resolve(key.clone()).await;
            tracing::debug!("resolved {key} in {:?}", started.elapsed());
            let _ = jobs.send(Job::Resolved { key, result });
        });
    }

    async fn sync_next(&mut self) -> Result<()> {
        if !self.started || self.current.is_none() {
            return Ok(());
        }
        let wanted = self.queue.peek().map(|t| (t.uri.clone(), media_title(t)));
        if self.next.as_ref().map(|n| &n.uri) == wanted.as_ref().map(|(uri, _)| uri) {
            return Ok(());
        }
        if let Some(next) = self.next.take() {
            self.dropped = Some(next.entry);
            self.mpv().await?.command(json!(["playlist-clear"])).await?;
        }
        let Some((uri, title)) = wanted else { return Ok(()) };
        let key = self.source.key(&uri);
        let Some(stream) = self.streams.get(&key).filter(|s| s.fresh()) else {
            if !self.unresolvable.contains(&key) {
                self.resolve(&key);
            }
            return Ok(());
        };
        let duration_ms = self.duration_ms.unwrap_or_else(|| self.queue.current().map_or(0, |t| t.duration_ms));
        if duration_ms > 0 && self.position_ms + APPEND_BEFORE_END_MS < duration_ms {
            return Ok(());
        }
        let url = stream.url.clone();
        let entry = self.mpv().await?.load(&url, true, None, &title).await?;
        self.next = Some(Loaded { entry, uri });
        Ok(())
    }

    async fn queue_changed(&mut self) -> Result<()> {
        let report =
            (self.queue.context_uri.clone(), self.queue.current().map(|t| t.uri.clone()), self.queue.upcoming());
        if self.reported.as_ref() != Some(&report) {
            let (context_uri, current, next) = report.clone();
            self.emit(PlayerEvent::QueueChanged { context_uri, current, next });
            self.reported = Some(report);
        }
        self.sync_next().await
    }

    fn want_autoplay(&mut self) {
        if !self.context_loaded || self.autoplay.fetching || !self.queue.wants_autoplay() {
            return;
        }
        let Some(seed) = self.queue.seed().map(|t| t.uri.clone()) else { return };
        self.autoplay.fetching = true;
        let (source, jobs, generation) = (self.source.clone(), self.jobs.clone(), self.generation);
        tokio::spawn(async move {
            let tracks = source.autoplay(seed).await;
            let _ = jobs.send(Job::Autoplay { generation, tracks });
        });
    }

    async fn toggle(&mut self, has_track: bool) -> Result<()> {
        if self.current.is_some() || self.waiting.is_some() {
            return self.set_paused(!self.paused).await;
        }
        if has_track && self.queue.current().is_some() {
            return self.begin(0).await;
        }
        Ok(())
    }

    async fn set_paused(&mut self, paused: bool) -> Result<()> {
        if self.current.is_none() && self.waiting.is_none() {
            return Ok(());
        }
        self.mpv().await?.set("pause", json!(paused)).await
    }

    async fn skip(&mut self) -> Result<()> {
        if self.queue.advance(false).is_some() {
            return self.begin(0).await;
        }
        self.end_of_queue().await
    }

    async fn previous(&mut self) -> Result<()> {
        if self.position_ms < RESTART_AFTER_MS && self.queue.back().is_some() {
            return self.begin(0).await;
        }
        self.seek(0).await
    }

    async fn seek(&mut self, position_ms: u32) -> Result<()> {
        if self.waiting.is_some() {
            self.waiting = Some(position_ms);
            self.position_ms = position_ms;
            self.emit(PlayerEvent::Seeked { position_ms });
            return Ok(());
        }
        if self.current.is_none() {
            return Ok(());
        }
        let seconds = f64::from(position_ms) / 1000.0;
        self.mpv().await?.command(json!(["seek", seconds, "absolute"])).await?;
        self.seeking = Some((position_ms, Instant::now()));
        Ok(())
    }

    async fn end_of_queue(&mut self) -> Result<()> {
        if self.autoplay.fetching {
            self.autoplay.waiting = true;
            self.stop_audio().await?;
            return Ok(());
        }
        self.stopped().await
    }

    async fn stopped(&mut self) -> Result<()> {
        self.stop_audio().await?;
        self.emit(PlayerEvent::Stopped);
        Ok(())
    }

    async fn tick(&mut self) -> Result<()> {
        if self.seeking.is_some_and(|(_, since)| since.elapsed() >= SEEK_TIMEOUT) {
            tracing::debug!("mpv didn't finish seeking");
            self.seeking = None;
        }
        if !self.started || self.paused || self.seeking.is_some() || self.current.is_none() {
            return Ok(());
        }
        self.emit(PlayerEvent::PositionChanged { position_ms: self.position_ms });
        if self.next.is_none() {
            self.sync_next().await?;
        }
        Ok(())
    }

    async fn mpv_event(&mut self, instance: u64, event: MpvEvent) -> Result<()> {
        if self.mpv.as_ref().is_none_or(|mpv| mpv.instance != instance) {
            tracing::debug!("mpv {instance}, gone: {event:?}");
            return Ok(());
        }
        if !matches!(event, MpvEvent::Property { id: crate::mpv::TIME_POS, .. }) {
            tracing::debug!("mpv: {event:?}");
        }
        match event {
            MpvEvent::Property { id: crate::mpv::TIME_POS, data } => {
                if let Some(seconds) = data.as_f64().filter(|_| self.started && self.seeking.is_none()) {
                    self.position_ms = (seconds * 1000.0) as u32;
                }
            }
            MpvEvent::Property { id: crate::mpv::PAUSE, data } => {
                let paused = data.as_bool().unwrap_or(false);
                if paused != self.paused {
                    self.paused = paused;
                    if self.current.is_some() || self.waiting.is_some() {
                        let position_ms = self.position_ms;
                        self.emit(if paused {
                            PlayerEvent::Paused { position_ms }
                        } else {
                            PlayerEvent::Playing { position_ms }
                        });
                    }
                }
            }
            MpvEvent::Property { id: crate::mpv::DURATION, data } => {
                self.duration_ms = data.as_f64().map(|seconds| (seconds * 1000.0) as u32);
            }
            MpvEvent::Property { .. } => {}
            MpvEvent::StartFile { entry } => {
                if self.next.as_ref().is_some_and(|next| next.entry == entry) {
                    self.moved_on().await?;
                } else if self.dropped == Some(entry) {
                    tracing::debug!("mpv went on to a song taken out of its playlist");
                    self.dropped = None;
                    self.ended().await?;
                }
            }
            MpvEvent::PlaybackRestart => {
                if self.current.is_none() {
                    return Ok(());
                }
                if !self.started {
                    self.started = true;
                    if let Some(ended) = self.ended_at.take() {
                        tracing::debug!("gap between songs: {:?}", ended.elapsed());
                    }
                    if let Some(uri) = self.current.as_ref().map(|c| c.uri.clone()) {
                        self.attempts.remove(&uri);
                    }
                    self.skipped_in_a_row = 0;
                    let position_ms = self.position_ms;
                    self.emit(if self.paused {
                        PlayerEvent::Paused { position_ms }
                    } else {
                        PlayerEvent::Playing { position_ms }
                    });
                    self.sync_next().await?;
                } else if let Some((position_ms, _)) = self.seeking.take() {
                    self.position_ms = position_ms;
                    self.emit(PlayerEvent::Seeked { position_ms });
                    self.sync_next().await?;
                }
            }
            MpvEvent::EndFile { entry, reason, error } => {
                if self.current.as_ref().is_none_or(|current| current.entry != entry) {
                    return Ok(());
                }
                match reason.as_str() {
                    "eof" if self.next.is_some() => self.ended_at = Some(Instant::now()),
                    "eof" => self.ended().await?,
                    "error" => self.failed(error.unwrap_or_default()).await?,
                    _ => {}
                }
            }
            MpvEvent::Exited => {
                tracing::warn!("mpv exited");
                self.mpv = None;
                if self.current.take().is_some() || self.waiting.take().is_some() {
                    self.next = None;
                    self.dropped = None;
                    self.started = false;
                    self.seeking = None;
                    self.emit(PlayerEvent::Stopped);
                }
            }
        }
        Ok(())
    }

    async fn moved_on(&mut self) -> Result<()> {
        let next = self.next.take().expect("checked by the caller");
        let track = self.queue.advance(true).cloned();
        if track.as_ref().map(|t| &t.uri) != Some(&next.uri) {
            tracing::warn!("mpv went on to {}, which isn't next any more", next.uri);
            return self.begin(0).await;
        }
        let track = track.expect("checked above");
        self.current = Some(next);
        self.dropped = None;
        self.started = false;
        self.seeking = None;
        self.position_ms = 0;
        self.emit(PlayerEvent::TrackChanged { track: current_track(&track, self.source.as_ref()) });
        self.mpv().await?.command(json!(["playlist-clear"])).await?;
        self.queue_changed().await?;
        self.want_autoplay();
        Ok(())
    }

    async fn ended(&mut self) -> Result<()> {
        if self.queue.advance(true).is_some() {
            return self.begin(0).await;
        }
        self.end_of_queue().await
    }

    async fn failed(&mut self, error: String) -> Result<()> {
        let Some(track) = self.queue.current().cloned() else { return Ok(()) };
        let key = self.source.key(&track.uri);
        let attempts = self.attempts.entry(track.uri.clone()).or_default();
        *attempts += 1;
        let attempts = *attempts;
        tracing::warn!("can't play {} ({error}), try {attempts} of {ATTEMPTS}", track.uri);
        self.streams.remove(&key);
        if attempts >= ATTEMPTS || self.unresolvable.contains(&key) {
            self.unresolvable.remove(&key);
            self.attempts.remove(&track.uri);
            self.skipped_in_a_row += 1;
            if self.skipped_in_a_row >= MAX_SKIPS_IN_A_ROW {
                tracing::warn!("{} songs in a row couldn't play; stopping", self.skipped_in_a_row);
                self.skipped_in_a_row = 0;
                return self.stopped().await;
            }
            tracing::info!("skipping {} – {}: not available", track.uri, track.name);
            return self.skip().await;
        }
        if attempts > 1 {
            self.unresolvable.insert(key);
        }
        let position_ms = self.position_ms;
        self.current = None;
        self.load_current(position_ms).await
    }

    async fn job(&mut self, job: Job) -> Result<()> {
        match job {
            Job::Resolved { key, result } => {
                self.resolving.remove(&key);
                match result {
                    Ok(stream) => {
                        self.blocked = false;
                        self.streams.insert(key.clone(), stream);
                    }
                    Err(err) => {
                        tracing::warn!("can't resolve {key}: {err:#}");
                        self.unresolvable.insert(key.clone());
                        if err.downcast_ref::<BotCheck>().is_some() && !self.blocked {
                            self.blocked = true;
                            self.emit(PlayerEvent::Blocked { reason: err.to_string() });
                        }
                    }
                }
                let current = self.queue.current().is_some_and(|t| self.source.key(&t.uri) == key);
                if let Some(start_ms) = self.waiting.filter(|_| current) {
                    return self.load_current(start_ms).await;
                }
                self.sync_next().await
            }
            Job::Context { generation, page, last } if generation == self.generation => {
                self.context_page(page, last).await
            }
            Job::Autoplay { generation, tracks } if generation == self.generation => {
                self.autoplay.fetching = false;
                match tracks {
                    Ok(tracks) => self.queue.add_autoplay(tracks),
                    Err(err) => tracing::warn!("radio: {err:#}"),
                }
                if std::mem::take(&mut self.autoplay.waiting) {
                    return self.ended().await;
                }
                self.queue_changed().await
            }
            Job::Queued { uri, track } => {
                let track = match track {
                    Ok(Some(track)) => Some(track),
                    Ok(None) => {
                        tracing::warn!("{uri} can't be queued: not found");
                        None
                    }
                    Err(err) => {
                        tracing::warn!("{uri} can't be queued: {err:#}");
                        None
                    }
                };
                let Some(slot) = self.adding.iter_mut().find(|(added, slot)| *added == uri && slot.is_none()) else {
                    return Ok(());
                };
                slot.1 = Some(track);
                while let Some((_, Some(_))) = self.adding.front() {
                    if let Some((_, Some(Some(track)))) = self.adding.pop_front() {
                        self.queue.add(track);
                    }
                }
                self.queue_changed().await
            }
            Job::Context { .. } | Job::Autoplay { .. } => Ok(()),
        }
    }

    async fn context_page(&mut self, page: Result<Vec<Track>>, last: bool) -> Result<()> {
        let tracks = match page {
            Ok(tracks) => tracks,
            Err(err) => {
                self.context_loaded = true;
                if let Some(starting) = self.starting.take() {
                    self.emit(PlayerEvent::Stopped);
                    bail!("can't play {}: {err:#}", starting.context);
                }
                tracing::warn!("rest of the context: {err:#}");
                return Ok(());
            }
        };
        self.context_loaded = last;
        let Some(starting) = self.starting.as_mut() else {
            self.queue.extend(tracks);
            self.want_autoplay();
            return self.queue_changed().await;
        };
        starting.tracks.extend(tracks);
        let found = starting.track.as_ref().is_none_or(|uri| starting.tracks.iter().any(|t| &t.uri == uri));
        if !found && !last {
            return Ok(());
        }
        let starting = self.starting.take().expect("checked above");
        if let Some(track) =
            starting.track.clone().filter(|t| !found && self.source.is_track(t) && *t != starting.context)
        {
            tracing::info!("{track} isn't in {}; playing it with its radio", starting.context);
            return self.play(track.clone(), Some(track), starting.position_ms, Some(starting.shuffle)).await;
        }
        if !found {
            tracing::warn!(
                "{} isn't in {}; playing it from the start",
                starting.track.as_deref().unwrap_or_default(),
                starting.context
            );
        }
        let start = starting.track.as_deref().filter(|_| found);
        if !self.queue.load(starting.context.clone(), starting.tracks, start, starting.shuffle) {
            self.emit(PlayerEvent::Stopped);
            bail!("{} has nothing to play", starting.context);
        }
        self.begin(if found { starting.position_ms } else { 0 }).await
    }
}

fn media_title(track: &Track) -> String {
    if track.artists.is_empty() {
        track.name.clone()
    } else {
        format!("{} – {}", track.name, track.artists.join(", "))
    }
}

fn load_context(
    source: Arc<dyn Source>,
    jobs: UnboundedSender<Job>,
    generation: u64,
    context: String,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut pages = source.context(context);
        while let Some(page) = pages.next().await {
            match page {
                Ok(tracks) => {
                    let last = tracks.is_empty();
                    let _ = jobs.send(Job::Context { generation, page: Ok(tracks), last });
                    if last {
                        return;
                    }
                }
                Err(err) => {
                    let _ = jobs.send(Job::Context { generation, page: Err(err), last: true });
                    return;
                }
            }
        }
        let _ = jobs.send(Job::Context { generation, page: Ok(Vec::new()), last: true });
    })
}

fn current_track(track: &Track, source: &dyn Source) -> CurrentTrack {
    CurrentTrack {
        uri: track.uri.clone(),
        name: track.name.clone(),
        artists: track.artists.clone(),
        artist_uris: track.artist_uris.clone(),
        album: track.album.clone(),
        album_uri: Some(track.album_uri.clone()).filter(|uri| !uri.is_empty()),
        duration_ms: track.duration_ms,
        cover_url: track.image_url.clone(),
        cover_url_large: track.image_url.as_deref().map(|url| source.large_cover(url)),
    }
}

#[cfg(test)]
mod tests {
    use futures::stream;

    use super::*;

    #[test]
    fn maps_librespots_volume_to_mpvs() {
        assert_eq!(mpv_volume(0), 0.0);
        assert_eq!(mpv_volume(u16::MAX), 100.0);
        assert_eq!(mpv_volume(u16::MAX / 2), 50.0);
    }

    #[test]
    fn the_player_bar_gets_a_large_cover_too() {
        let track = Track {
            uri: "test:track:x".into(),
            name: "Song".into(),
            artists: vec!["A".into()],
            artist_uris: Vec::new(),
            album: String::new(),
            album_uri: String::new(),
            duration_ms: 1,
            image_url: Some("https://example.com/a=w300-h300".into()),
        };
        let current = current_track(&track, &Fake::default());
        assert_eq!(current.album_uri, None);
        assert_eq!(current.cover_url_large.as_deref(), Some("https://example.com/a=w800-h300"));
    }

    #[derive(Clone, Default)]
    struct Fake {
        context: Arc<Vec<Track>>,
    }

    #[async_trait]
    impl Source for Fake {
        fn name(&self) -> &'static str {
            "Test"
        }

        fn is_track(&self, uri: &str) -> bool {
            uri.starts_with("test:track:")
        }

        fn key(&self, uri: &str) -> String {
            uri.strip_prefix("test:track:").unwrap_or(uri).to_owned()
        }

        fn context(&self, _uri: String) -> BoxStream<'static, Result<Vec<Track>>> {
            let tracks = self.context.clone();
            stream::once(async move { Ok((*tracks).clone()) }).boxed()
        }

        async fn track(&self, uri: String) -> Result<Option<Track>> {
            Ok(self.context.iter().find(|track| track.uri == uri).cloned())
        }

        async fn resolve(&self, _key: String) -> Result<Stream> {
            bail!("this test resolves no streams")
        }

        async fn autoplay(&self, _seed: String) -> Result<Vec<Track>> {
            Ok(Vec::new())
        }

        fn large_cover(&self, url: &str) -> String {
            url.replace("w300", "w800")
        }
    }

    impl Fake {
        fn with_context(tracks: Vec<Track>) -> Self {
            Self { context: Arc::new(tracks) }
        }
    }

    #[tokio::test]
    async fn the_bot_check_is_said_once_per_start() {
        let (mut engine, (mut events, _)) = idle_engine("bot-check");
        let mut blocked = async |engine: &mut Engine, key: &str| {
            engine.job(Job::Resolved { key: key.into(), result: Err(BotCheck.into()) }).await.unwrap();
            let mut said = false;
            while let Ok(event) = events.try_recv() {
                said |= matches!(event, PlayerEvent::Blocked { .. });
            }
            said
        };
        assert!(blocked(&mut engine, "a").await);
        assert!(!blocked(&mut engine, "b").await, "not again for the next song");
        engine.activate();
        assert!(blocked(&mut engine, "c").await, "again after another start");
    }

    type Reports = (UnboundedReceiver<PlayerEvent>, UnboundedReceiver<(u64, MpvEvent)>);

    fn idle_engine(name: &str) -> (Engine, Reports) {
        let _ = name;
        let (events, events_rx) = mpsc::unbounded_channel();
        let (mpv_events, mpv_rx) = mpsc::unbounded_channel();
        let (jobs, _) = mpsc::unbounded_channel();
        let engine = Engine::new(Arc::new(Fake::default()), events, mpv_events, jobs, u16::MAX / 2);
        (engine, (events_rx, mpv_rx))
    }

    async fn engine(name: &str) -> Option<(Engine, Reports)> {
        let (mut engine, events) = idle_engine(name);
        engine.mpv_args = vec!["--ao=null".into()];
        match engine.mpv().await {
            Ok(_) => Some((engine, events)),
            Err(err) if format!("{err:#}").contains("not installed") => {
                assert!(std::env::var_os("TUNEBOX_REQUIRE_MPV").is_none(), "{err:#}");
                eprintln!("skipped: {err:#}");
                None
            }
            Err(err) => panic!("{err:#}"),
        }
    }

    fn tone(engine: &mut Engine, id: &str, hz: u32, seconds: u32) -> Track {
        let url = format!("av://lavfi:sine=frequency={hz}:duration={seconds}");
        engine.streams.insert(id.into(), Stream { url, until: Instant::now() + Duration::from_secs(3600) });
        Track {
            uri: format!("test:track:{id}"),
            name: id.into(),
            artists: Vec::new(),
            artist_uris: Vec::new(),
            album: String::new(),
            album_uri: String::new(),
            duration_ms: seconds * 1000,
            image_url: None,
        }
    }

    async fn pump(
        engine: &mut Engine,
        mpv_events: &mut UnboundedReceiver<(u64, MpvEvent)>,
        done: impl Fn(&Engine) -> bool,
    ) -> bool {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !done(engine) {
            match tokio::time::timeout_at(deadline, mpv_events.recv()).await {
                Ok(Some((instance, event))) => engine.mpv_event(instance, event).await.unwrap(),
                _ => return false,
            }
        }
        true
    }

    #[tokio::test]
    async fn a_song_its_context_lists_as_another_plays_with_its_radio() {
        let (mut engine, _) = idle_engine("other-video");
        let opener = tone(&mut engine, "opener", 440, 10);
        let song = tone(&mut engine, "song", 440, 10);
        engine.source = Arc::new(Fake::with_context(vec![opener.clone()]));
        engine.play("test:album:a".into(), Some(song.uri.clone()), 0, None).await.unwrap();
        engine.loader.take().unwrap().abort();
        engine.context_page(Ok(vec![opener]), true).await.unwrap();
        engine.loader.take().unwrap().abort();
        let starting = engine.starting.as_ref().expect("the song starts anew");
        assert_eq!(starting.context, song.uri, "on its own, so its radio follows it");
        assert_eq!(starting.track.as_deref(), Some(song.uri.as_str()));
    }

    #[tokio::test]
    async fn each_start_reports_the_queue_again() {
        let (mut engine, (mut events, _)) = idle_engine("report");
        let track = tone(&mut engine, "a", 440, 10);
        engine.queue.load("test:album:a".into(), vec![track], None, false);
        let mut reports = || {
            std::iter::from_fn(|| events.try_recv().ok())
                .filter(|e| matches!(e, PlayerEvent::QueueChanged { .. }))
                .count()
        };
        engine.queue_changed().await.unwrap();
        engine.queue_changed().await.unwrap();
        assert_eq!(reports(), 1, "a queue that didn't change isn't reported again");
        engine.activate();
        engine.queue_changed().await.unwrap();
        assert_eq!(reports(), 1);
    }

    #[tokio::test]
    async fn an_mpv_that_failed_to_start_leaves_the_next_one_alone() {
        let Some((mut engine, (_, mut mpv_events))) = engine("restart").await else { return };
        engine.mpv = None;
        let failed = Mpv::spawn(&["--no-such-option".into()], engine.mpv_events.clone()).await;
        assert!(failed.is_err());
        let instance = engine.mpv().await.unwrap().instance;
        while let Ok(Some((from, event))) = tokio::time::timeout(Duration::from_millis(500), mpv_events.recv()).await {
            engine.mpv_event(from, event).await.unwrap();
        }
        let mpv = engine.mpv.as_mut().expect("the mpv that started is kept");
        assert_eq!(mpv.instance, instance);
        assert!(mpv.get("mpv-version").await.is_ok());
    }

    #[tokio::test]
    async fn a_seek_mpv_turns_down_holds_nothing_back() {
        let Some((mut engine, (mut events, _))) = engine("seek").await else { return };
        let dir = std::env::temp_dir().join(format!("tunebox-mpv-seek-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let pipe = dir.join("song");
        let path = std::ffi::CString::new(pipe.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        let track = tone(&mut engine, "pipe", 440, 10);
        engine.streams.get_mut("pipe").unwrap().url = pipe.display().to_string();
        engine.queue.load("c".into(), vec![track], None, false);
        engine.begin(0).await.unwrap();
        assert!(engine.seek(5_000).await.is_err());
        assert!(engine.seeking.is_none(), "a seek turned down holds back no position");

        engine.started = true;
        engine.seeking = Some((5_000, Instant::now() - SEEK_TIMEOUT));
        engine.tick().await.unwrap();
        assert!(engine.seeking.is_none());
        let reported = std::iter::from_fn(|| events.try_recv().ok());
        assert!(reported.into_iter().any(|e| matches!(e, PlayerEvent::PositionChanged { .. })));
        drop(engine);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_song_mpv_went_on_to_before_the_queue_changed_doesnt_play_unseen() {
        let Some((mut engine, (mut events, mut mpv_events))) = engine("dropped").await else { return };
        let first = tone(&mut engine, "first", 440, 1);
        let second = tone(&mut engine, "second", 550, 30);
        let queued = tone(&mut engine, "queued", 660, 30);
        engine.queue.load("c".into(), vec![first, second.clone()], None, false);
        engine.begin(0).await.unwrap();
        assert!(pump(&mut engine, &mut mpv_events, |e| e.next.is_some()).await);
        let ahead = engine.next.as_ref().unwrap().entry;
        let mut held = Vec::new();
        while held.last().is_none_or(|(_, event)| *event != MpvEvent::StartFile { entry: ahead }) {
            let event = tokio::time::timeout(Duration::from_secs(10), mpv_events.recv()).await;
            held.push(event.unwrap().unwrap());
        }
        engine.queue.add(queued.clone());
        engine.queue_changed().await.unwrap();
        for (instance, event) in held {
            engine.mpv_event(instance, event).await.unwrap();
        }
        assert_eq!(engine.queue.current().map(|t| &t.uri), Some(&queued.uri));
        assert_eq!(engine.queue.peek().map(|t| &t.uri), Some(&second.uri));
        let entry = engine.current.as_ref().map(|c| c.entry);
        let playlist = engine.mpv.as_mut().unwrap().get("playlist").await.unwrap();
        let listed: Vec<_> =
            playlist.as_array().unwrap().iter().map(|e| (e["id"].as_u64(), e["filename"].clone())).collect();
        let url = engine.streams["queued"].url.clone();
        assert_eq!(listed, [(entry, json!(url))], "mpv has only the queued song");
        let shown = std::iter::from_fn(|| events.try_recv().ok()).filter_map(|event| match event {
            PlayerEvent::TrackChanged { track } => Some(track.uri),
            _ => None,
        });
        assert_eq!(shown.last(), Some(queued.uri));
    }
}
