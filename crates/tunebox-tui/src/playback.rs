
use std::time::Duration;

use tunebox_core::{
    connect::Playback,
    player::{CurrentTrack, PlayerEvent},
};

use crate::clock::Instant;
use crate::queue::Queue;

const ECHO_WINDOW: Duration = Duration::from_millis(1500);
const SEEK_TOLERANCE_MS: u32 = 1000;
const VOLUME_TIMEOUT: Duration = Duration::from_secs(1);
const REMOTE_GRACE: Duration = Duration::from_secs(2);
const REMOTE_SEEK_TOLERANCE_MS: u32 = 2000;
const REMOTE_VOLUME_TOLERANCE: u32 = 3;
pub const START_TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Repeat {
    #[default]
    Off,
    Context,
    Track,
}

impl Repeat {
    pub fn from_remote(mode: &str) -> Self {
        match mode {
            "track" => Repeat::Track,
            "context" => Repeat::Context,
            _ => Repeat::Off,
        }
    }
}

pub struct Cover {
    pub url: String,
    pub picture: crate::loader::Picture,
}

pub struct Pending {
    pub uri: Option<String>,
    from: Option<String>,
    pub skip: bool,
    pub since: Instant,
    loading: bool,
    previous: Option<Shown>,
}

struct Shown {
    track: CurrentTrack,
    cover: Option<Cover>,
    playing: bool,
    position_ms: u32,
    position_at: Instant,
}

pub struct NowPlaying {
    pub track: Option<CurrentTrack>,
    pub playing: bool,
    position_ms: u32,
    position_at: Instant,
    pub shuffle: bool,
    pub repeat: Repeat,
    pub volume: u16,
    pub cover: Option<Cover>,
    pub device: Option<String>,
    seeking: Option<(u32, Instant)>,
    pub pending: Option<Pending>,
}

impl NowPlaying {
    pub fn new() -> Self {
        Self {
            track: None,
            playing: false,
            position_ms: 0,
            position_at: Instant::now(),
            shuffle: false,
            repeat: Repeat::Off,
            volume: u16::MAX / 2,
            cover: None,
            device: None,
            seeking: None,
            pending: None,
        }
    }

    pub fn start(&mut self, uri: Option<String>) {
        let (from, previous) = match self.pending.take() {
            Some(pending) if !pending.loading => (pending.from, pending.previous),
            _ => (self.track.as_ref().map(|t| t.uri.clone()), None),
        };
        self.pending = Some(Pending { uri, from, skip: false, since: Instant::now(), loading: false, previous });
    }

    pub fn skip(&mut self) {
        self.start(None);
        if let Some(pending) = &mut self.pending {
            pending.skip = true;
        }
    }

    pub fn show_early(&mut self, track: CurrentTrack) {
        let Some(pending) = &mut self.pending else { return };
        if pending.uri.as_deref() != Some(track.uri.as_str()) || self.track.as_ref().is_some_and(|t| t.uri == track.uri)
        {
            return;
        }
        if pending.previous.is_none()
            && let Some(old) = self.track.take()
        {
            pending.previous = Some(Shown {
                track: old,
                cover: self.cover.take(),
                playing: self.playing,
                position_ms: self.position_ms,
                position_at: self.position_at,
            });
        }
        self.cover = None;
        self.track = Some(track);
        self.seeking = None;
        self.playing = false;
        self.set_position(0);
    }

    pub fn start_failed(&self) -> bool {
        self.pending.as_ref().is_some_and(|p| !p.loading && p.since.elapsed() >= START_TIMEOUT)
    }

    pub fn cancel_start(&mut self) {
        let Some(shown) = self.pending.take().and_then(|p| p.previous) else { return };
        self.track = Some(shown.track);
        self.cover = shown.cover;
        self.playing = shown.playing;
        self.position_ms = shown.position_ms;
        self.position_at = shown.position_at;
    }

    pub fn awaits_remote(&mut self, playback: &Playback, waiting: bool) -> bool {
        let Some(pending) = &self.pending else { return false };
        let shown = playback.track.as_ref().map(|t| t.uri.as_str());
        let through = shown != pending.from.as_deref() || (pending.uri.is_some() && shown == pending.uri.as_deref());
        let gave_up = if pending.uri.is_some() { pending.since.elapsed() >= START_TIMEOUT } else { !waiting };
        if through || gave_up {
            self.pending = None;
            return false;
        }
        true
    }

    pub fn position_ms(&self) -> u32 {
        let elapsed = if self.playing { self.position_at.elapsed().as_millis() as u32 } else { 0 };
        let duration = self.track.as_ref().map_or(u32::MAX, |t| t.duration_ms);
        (self.position_ms + elapsed).min(duration)
    }

    pub(crate) fn set_position(&mut self, position_ms: u32) {
        self.position_ms = position_ms;
        self.position_at = Instant::now();
    }

    pub fn set_playing(&mut self, playing: bool) {
        self.set_position(self.position_ms());
        self.playing = playing;
    }

    pub fn seek(&mut self, position_ms: u32) {
        self.set_position(position_ms);
        self.seeking = Some((position_ms, Instant::now()));
    }

    fn report_position(&mut self, position_ms: u32) {
        if self.pending.as_ref().is_some_and(|p| p.previous.is_some() && !p.loading) {
            return;
        }
        if let Some((target, at)) = self.seeking {
            if position_ms.abs_diff(target) > SEEK_TOLERANCE_MS && at.elapsed() < ECHO_WINDOW {
                return;
            }
            self.seeking = None;
        }
        self.set_position(position_ms);
    }

    pub fn restore(&mut self, last: &crate::state::LastTrack) {
        self.track = Some(CurrentTrack {
            uri: last.uri.clone(),
            name: last.name.clone(),
            artists: last.artists.clone(),
            artist_uris: last.artist_uris.clone(),
            album: last.album.clone(),
            album_uri: last.album_uri.clone(),
            duration_ms: last.duration_ms,
            cover_url: last.cover_url.clone(),
            cover_url_large: last.cover_url_large.clone(),
        });
        self.playing = false;
        self.set_position(last.position_ms);
    }

    pub fn apply_remote(&mut self, playback: &Playback, intent: &mut RemoteIntent) -> (Option<String>, bool) {
        let same_track = self.track.as_ref().map(|t| t.uri.as_str()) == playback.track.as_ref().map(|t| t.uri.as_str());
        let shown_ms = self.position_ms();
        let cover = self.mirror(playback);
        let unconfirmed = intent.reconcile(self, playback, same_track.then_some(shown_ms));
        (cover, unconfirmed)
    }

    fn mirror(&mut self, playback: &Playback) -> Option<String> {
        self.device = Some(playback.device.name.clone());
        self.playing = playback.playing;
        self.set_position(playback.progress_ms);
        self.shuffle = playback.shuffle;
        self.repeat = Repeat::from_remote(&playback.repeat);
        if let Some(percent) = playback.device.volume_percent {
            self.volume = (percent.min(100) * u16::MAX as u32 / 100) as u16;
        }
        let Some(remote) = &playback.track else {
            self.track = None;
            return None;
        };
        if self.track.as_ref().is_some_and(|t| t.uri == remote.uri && t.cover_url == remote.cover_url) {
            return None;
        }
        self.track = Some(CurrentTrack {
            uri: remote.uri.clone(),
            name: remote.name.clone(),
            artists: remote.artists.clone(),
            artist_uris: remote.artist_uris.clone(),
            album: remote.album.clone(),
            album_uri: remote.album_uri.clone(),
            duration_ms: remote.duration_ms,
            cover_url: remote.cover_url.clone(),
            cover_url_large: remote.cover_url_large.clone(),
        });
        let changed = self.cover.as_ref().map(|c| &c.url) != remote.cover_url.as_ref();
        changed.then(|| remote.cover_url.clone()).flatten()
    }

    pub fn apply(&mut self, event: PlayerEvent) -> Option<String> {
        match event {
            PlayerEvent::Loading { uri, position_ms } => {
                if self.pending.is_none() {
                    self.start(None);
                }
                if let Some(pending) = &mut self.pending {
                    pending.uri = Some(uri.clone());
                    pending.loading = true;
                }
                self.set_playing(false);
                if self.track.as_ref().is_some_and(|t| t.uri == uri) {
                    self.report_position(position_ms);
                }
            }
            PlayerEvent::TrackChanged { track } => {
                let cover_url = track.cover_url.clone();
                self.track = Some(track);
                self.pending = None;
                self.seeking = None;
                self.set_position(0);
                if self.cover.as_ref().map(|c| &c.url) != cover_url.as_ref() {
                    return cover_url;
                }
            }
            PlayerEvent::Playing { position_ms } => {
                self.pending = None;
                self.set_playing(false);
                self.report_position(position_ms);
                self.playing = true;
            }
            PlayerEvent::Paused { position_ms } => {
                self.pending = None;
                self.set_playing(false);
                self.report_position(position_ms);
            }
            PlayerEvent::Stopped => {
                self.pending = None;
                self.playing = false;
            }
            PlayerEvent::Seeked { position_ms } => {
                if self.pending.as_ref().is_some_and(|p| p.skip && !p.loading) {
                    self.pending = None;
                }
                self.report_position(position_ms);
            }
            PlayerEvent::PositionCorrection { position_ms } | PlayerEvent::PositionChanged { position_ms } => {
                self.report_position(position_ms)
            }
            PlayerEvent::ShuffleChanged { shuffle } => self.shuffle = shuffle,
            PlayerEvent::RepeatChanged { context, track } => {
                self.repeat = match (context, track) {
                    (_, true) => Repeat::Track,
                    (true, false) => Repeat::Context,
                    (false, false) => Repeat::Off,
                };
            }
            PlayerEvent::VolumeChanged { volume } => self.volume = volume,
            _ => {}
        }
        None
    }
}

#[derive(Default)]
pub struct VolumeSync {
    target: Option<u16>,
    sent: Option<(u16, Instant)>,
}

impl VolumeSync {
    pub fn request(&mut self, volume: u16) -> Option<u16> {
        self.target = Some(volume);
        self.next()
    }

    pub fn defer(&mut self, volume: u16) {
        self.target = Some(volume);
    }

    pub fn next(&mut self) -> Option<u16> {
        if self.sent.is_some() {
            return None;
        }
        let target = self.target?;
        self.sent = Some((target, Instant::now()));
        Some(target)
    }

    pub fn reported(&mut self, volume: u16) -> (bool, Option<u16>) {
        if self.sent.is_some_and(|(sent, at)| sent == volume || at.elapsed() >= VOLUME_TIMEOUT) {
            self.sent = None;
        }
        match self.target {
            None => (true, None),
            Some(target) if target == volume && self.sent.is_none() => {
                self.target = None;
                (true, None)
            }
            Some(_) => (false, self.next()),
        }
    }

    pub fn expire(&mut self) -> Option<u16> {
        let (sent, at) = self.sent?;
        if at.elapsed() < VOLUME_TIMEOUT {
            return None;
        }
        self.sent = None;
        if self.target == Some(sent) {
            self.target = None;
        }
        self.next()
    }
}

#[derive(Default)]
pub struct RemoteIntent {
    pub playing: Option<bool>,
    pub shuffle: Option<bool>,
    pub repeat: Option<Repeat>,
    pub volume: Option<u16>,
    pub seek: Option<(u32, Instant)>,
    in_flight: usize,
    answered_at: Option<Instant>,
}

impl RemoteIntent {
    pub fn sent(&mut self) {
        self.in_flight += 1;
    }

    pub fn answered(&mut self, commands: usize, ok: bool) {
        self.in_flight = self.in_flight.saturating_sub(commands);
        self.answered_at = Some(Instant::now());
        if !ok {
            self.forget();
        }
    }

    pub fn is_waiting(&self) -> bool {
        self.in_flight > 0 || self.answered_at.is_some_and(|at| at.elapsed() < REMOTE_GRACE)
    }

    fn forget(&mut self) {
        *self = Self { in_flight: self.in_flight, answered_at: self.answered_at, ..Self::default() };
    }

    fn reconcile(&mut self, now: &mut NowPlaying, playback: &Playback, shown_ms: Option<u32>) -> bool {
        if !self.is_waiting() {
            self.forget();
            return false;
        }
        let mut keep_position = false;
        if let Some(playing) = self.playing {
            if playback.playing == playing {
                self.playing = None;
            } else {
                now.playing = playing;
                keep_position = true;
            }
        }
        if let Some((target, at)) = self.seek {
            let moved = if now.playing { at.elapsed().as_millis() as u32 } else { 0 };
            if shown_ms.is_none() || playback.progress_ms.abs_diff(target + moved) <= REMOTE_SEEK_TOLERANCE_MS {
                self.seek = None;
            } else {
                keep_position = true;
            }
        }
        if let Some(position_ms) = shown_ms.filter(|_| keep_position) {
            now.set_position(position_ms);
        }
        if let Some(shuffle) = self.shuffle {
            if playback.shuffle == shuffle {
                self.shuffle = None;
            } else {
                now.shuffle = shuffle;
            }
        }
        if let Some(repeat) = self.repeat {
            if Repeat::from_remote(&playback.repeat) == repeat {
                self.repeat = None;
            } else {
                now.repeat = repeat;
            }
        }
        if let Some(volume) = self.volume {
            let wanted = volume as u32 * 100 / u16::MAX as u32;
            match playback.device.volume_percent {
                Some(percent) if percent.abs_diff(wanted) > REMOTE_VOLUME_TOLERANCE => now.volume = volume,
                _ => self.volume = None,
            }
        }
        self.playing.is_some()
            || self.seek.is_some()
            || self.shuffle.is_some()
            || self.repeat.is_some()
            || self.volume.is_some()
    }
}

#[derive(Debug, Clone)]
pub struct ConnectPush {
    pub active_device: String,
    pub track: String,
    pub context: String,
    pub playing: bool,
    pub queue: Queue,
}

impl ConnectPush {
    pub fn worth_polling(&self, own_device: Option<&str>, remote_active: bool, context: Option<&str>) -> bool {
        let here = own_device == Some(self.active_device.as_str());
        let elsewhere = !here && !self.active_device.is_empty();
        let new_context = here && !self.context.is_empty() && context != Some(self.context.as_str());
        elsewhere || new_context || remote_active
    }

    pub fn is_ahead_of(&self, playback: &Playback) -> bool {
        let other_track = !self.track.is_empty() && playback.track.as_ref().is_some_and(|t| t.uri != self.track);
        self.active_device == playback.device.id && (self.playing != playback.playing || other_track)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const URI: &str = "spotify:track:4uLU6hMCjMI75M1A2tKUQC";

    fn track(uri: &str) -> CurrentTrack {
        CurrentTrack {
            uri: uri.into(),
            name: "Song".into(),
            artists: vec!["Artist".into()],
            artist_uris: Vec::new(),
            album: "Album".into(),
            album_uri: None,
            duration_ms: 200_000,
            cover_url: None,
            cover_url_large: None,
        }
    }

    fn paused_track() -> NowPlaying {
        let mut now = NowPlaying::new();
        now.track = Some(track(URI));
        now
    }

    fn seeked(position_ms: u32) -> PlayerEvent {
        PlayerEvent::Seeked { position_ms }
    }

    fn position_changed(position_ms: u32) -> PlayerEvent {
        PlayerEvent::PositionChanged { position_ms }
    }

    fn poll(uri: &str, playing: bool, progress_ms: u32) -> Playback {
        use tunebox_core::connect::{Device, PlayingTrack};
        Playback {
            device: Device {
                id: "phone".into(),
                name: "Phone".into(),
                kind: "Smartphone".into(),
                active: true,
                volume_percent: Some(50),
            },
            playing,
            progress_ms,
            shuffle: false,
            repeat: "off".into(),
            track: Some(PlayingTrack {
                uri: uri.into(),
                name: "Song".into(),
                artists: Vec::new(),
                artist_uris: Vec::new(),
                album: String::new(),
                album_uri: None,
                duration_ms: 200_000,
                cover_url: None,
                cover_url_large: None,
            }),
            context: None,
        }
    }

    fn remote_track(progress_ms: u32) -> (NowPlaying, RemoteIntent) {
        let (mut now, mut intent) = (NowPlaying::new(), RemoteIntent::default());
        now.apply_remote(&poll(URI, false, progress_ms), &mut intent);
        (now, intent)
    }

    #[test]
    fn seek_moves_at_once_and_ignores_reports_from_before() {
        let mut now = paused_track();
        now.seek(60_000);
        assert_eq!(now.position_ms(), 60_000);
        now.apply(position_changed(10_000));
        assert_eq!(now.position_ms(), 60_000);
        now.apply(seeked(60_200));
        assert_eq!(now.position_ms(), 60_200);
        now.apply(position_changed(10_000));
        assert_eq!(now.position_ms(), 10_000);
    }

    #[test]
    fn quick_seeks_wait_for_the_last_one() {
        let mut now = paused_track();
        now.seek(5_000);
        now.seek(10_000);
        now.apply(seeked(5_000));
        assert_eq!(now.position_ms(), 10_000);
        now.apply(seeked(10_000));
        assert_eq!(now.position_ms(), 10_000);
    }

    #[test]
    fn a_lost_seek_stops_holding_the_position() {
        let mut now = paused_track();
        now.seek(60_000);
        now.seeking = Some((60_000, Instant::now() - ECHO_WINDOW));
        now.apply(position_changed(10_000));
        assert_eq!(now.position_ms(), 10_000);
    }

    #[test]
    fn pausing_after_a_seek_keeps_the_new_position() {
        let mut now = paused_track();
        now.playing = true;
        now.seek(90_000);
        now.apply(PlayerEvent::Paused { position_ms: 30_000 });
        assert!(!now.playing);
        assert!((90_000..90_500).contains(&now.position_ms()));
    }

    #[test]
    fn volume_changes_wait_for_each_other_and_collapse() {
        let mut sync = VolumeSync::default();
        assert_eq!(sync.request(100), Some(100));
        assert_eq!(sync.request(200), None);
        assert_eq!(sync.request(300), None);
        assert_eq!(sync.reported(100), (false, Some(300)));
        assert_eq!(sync.reported(300), (true, None));
        assert_eq!(sync.reported(5_000), (true, None));
    }

    #[test]
    fn stale_volume_reports_stay_hidden() {
        let mut sync = VolumeSync::default();
        assert_eq!(sync.request(1_000), Some(1_000));
        assert_eq!(sync.reported(400), (false, None));
        assert_eq!(sync.reported(1_000), (true, None));
    }

    #[test]
    fn a_volume_without_report_gives_way_to_the_next() {
        let mut sync = VolumeSync::default();
        sync.request(100);
        sync.request(200);
        assert_eq!(sync.expire(), None);
        sync.sent = Some((100, Instant::now() - VOLUME_TIMEOUT));
        assert_eq!(sync.expire(), Some(200));
        sync.sent = Some((200, Instant::now() - VOLUME_TIMEOUT));
        assert_eq!(sync.expire(), None);
        assert_eq!(sync.reported(700), (true, None));
    }

    #[test]
    fn a_deferred_volume_goes_out_on_activation() {
        let mut sync = VolumeSync::default();
        sync.defer(900);
        assert_eq!(sync.next(), Some(900));
        assert_eq!(sync.reported(500), (false, None));
        assert_eq!(sync.reported(900), (true, None));
    }

    #[test]
    fn remote_play_survives_polls_that_lag_behind() {
        let (mut now, mut intent) = remote_track(10_000);
        now.set_playing(true);
        intent.playing = Some(true);
        intent.sent();
        assert!(now.apply_remote(&poll(URI, false, 12_000), &mut intent).1);
        assert!(now.playing);
        assert!((10_000..10_500).contains(&now.position_ms()));
        intent.answered(1, true);
        assert!(!now.apply_remote(&poll(URI, true, 10_100), &mut intent).1);
        assert!(now.playing);
        assert!(intent.playing.is_none());
    }

    #[test]
    fn polls_win_once_spotify_had_time_to_catch_up() {
        let (mut now, mut intent) = remote_track(10_000);
        now.shuffle = true;
        intent.shuffle = Some(true);
        intent.sent();
        intent.answered(1, true);
        intent.answered_at = Some(Instant::now() - REMOTE_GRACE);
        assert!(!now.apply_remote(&poll(URI, false, 10_000), &mut intent).1);
        assert!(!now.shuffle);
    }

    #[test]
    fn polls_win_after_a_failed_command() {
        let (mut now, mut intent) = remote_track(10_000);
        now.repeat = Repeat::Track;
        intent.repeat = Some(Repeat::Track);
        intent.sent();
        intent.answered(1, false);
        now.apply_remote(&poll(URI, false, 10_000), &mut intent);
        assert_eq!(now.repeat, Repeat::Off);
    }

    #[test]
    fn remote_seek_holds_until_a_poll_shows_it() {
        let (mut now, mut intent) = remote_track(10_000);
        now.set_position(60_000);
        intent.seek = Some((60_000, Instant::now()));
        intent.sent();
        assert!(now.apply_remote(&poll(URI, false, 10_000), &mut intent).1);
        assert_eq!(now.position_ms(), 60_000);
        assert!(!now.apply_remote(&poll(URI, false, 60_300), &mut intent).1);
        assert_eq!(now.position_ms(), 60_300);
    }

    #[test]
    fn a_new_remote_track_brings_its_own_position() {
        let (mut now, mut intent) = remote_track(10_000);
        now.set_position(60_000);
        intent.seek = Some((60_000, Instant::now()));
        intent.sent();
        now.apply_remote(&poll("spotify:track:0000000000000000000000", false, 1_000), &mut intent);
        assert_eq!(now.position_ms(), 1_000);
        assert!(intent.seek.is_none());
    }

    #[test]
    fn remote_volume_accepts_the_device_rounding_it() {
        let (mut now, mut intent) = remote_track(0);
        now.volume = u16::MAX;
        intent.volume = Some(u16::MAX);
        intent.sent();
        now.apply_remote(&poll(URI, false, 0), &mut intent);
        assert_eq!(now.volume, u16::MAX);
        let mut rounded = poll(URI, false, 0);
        rounded.device.volume_percent = Some(98);
        assert!(!now.apply_remote(&rounded, &mut intent).1);
        assert_eq!(now.volume, (98 * u16::MAX as u32 / 100) as u16);
    }

    const OTHER: &str = "spotify:track:0000000000000000000000";

    fn playing(position_ms: u32) -> PlayerEvent {
        PlayerEvent::Playing { position_ms }
    }

    fn loading(uri: &str) -> PlayerEvent {
        PlayerEvent::Loading { uri: uri.into(), position_ms: 0 }
    }

    #[test]
    fn a_picked_track_shows_at_once_and_comes_back_if_it_fails() {
        let mut now = paused_track();
        now.playing = true;
        now.position_ms = 42_000;
        now.start(Some(OTHER.into()));
        now.show_early(track(OTHER));
        assert_eq!(now.track.as_ref().unwrap().uri, OTHER);
        assert!(!now.playing);
        assert_eq!(now.position_ms(), 0);
        assert!(!now.start_failed());
        now.pending.as_mut().unwrap().since = Instant::now() - START_TIMEOUT;
        assert!(now.start_failed());
        now.cancel_start();
        assert_eq!(now.track.as_ref().unwrap().uri, URI);
        assert!(now.playing);
        assert!(now.position_ms() >= 42_000);
        assert!(now.pending.is_none());
    }

    #[test]
    fn the_old_track_doesnt_move_one_shown_early() {
        let mut now = paused_track();
        now.start(Some(OTHER.into()));
        now.show_early(track(OTHER));
        now.apply(position_changed(50_000));
        assert_eq!(now.position_ms(), 0);
    }

    #[test]
    fn a_loading_track_waits_as_long_as_it_takes() {
        let mut now = paused_track();
        now.playing = true;
        now.start(Some(OTHER.into()));
        now.show_early(track(OTHER));
        now.apply(loading(OTHER));
        now.pending.as_mut().unwrap().since = Instant::now() - START_TIMEOUT;
        assert!(!now.start_failed());
        now.apply(playing(0));
        assert!(now.pending.is_none());
        assert!(now.playing);
    }

    #[test]
    fn loading_announced_by_the_player_spins_too() {
        let mut now = paused_track();
        now.playing = true;
        now.apply(loading(OTHER));
        assert_eq!(now.pending.as_ref().and_then(|p| p.uri.as_deref()), Some(OTHER));
        assert!(!now.playing);
    }

    #[test]
    fn previous_that_restarts_the_track_ends_the_skip() {
        let mut now = paused_track();
        now.skip();
        now.apply(seeked(0));
        assert!(now.pending.is_none());
    }

    #[test]
    fn a_remote_start_ignores_polls_until_the_device_switches() {
        let (mut now, _) = remote_track(10_000);
        now.start(Some(OTHER.into()));
        now.show_early(track(OTHER));
        assert!(now.awaits_remote(&poll(URI, true, 11_000), true));
        assert!(now.awaits_remote(&poll(URI, true, 12_000), false));
        assert!(!now.awaits_remote(&poll(OTHER, true, 300), false));
        assert!(now.pending.is_none());
        let mut full = poll(OTHER, true, 300);
        full.track.as_mut().unwrap().cover_url = Some("https://i.scdn.co/image/cover".into());
        let (cover, _) = now.apply_remote(&full, &mut RemoteIntent::default());
        assert_eq!(cover.as_deref(), Some("https://i.scdn.co/image/cover"));
    }

    #[test]
    fn quick_picks_still_wait_for_the_track_from_before() {
        let (mut now, _) = remote_track(10_000);
        now.start(Some(OTHER.into()));
        now.show_early(track(OTHER));
        let third = "spotify:track:1111111111111111111111";
        now.start(Some(third.into()));
        now.show_early(track(third));
        assert!(now.awaits_remote(&poll(URI, true, 11_000), true));
        now.cancel_start();
        assert_eq!(now.track.as_ref().unwrap().uri, URI);
    }

    #[test]
    fn a_remote_skip_waits_only_while_spotify_may_lag() {
        let (mut now, _) = remote_track(10_000);
        now.skip();
        assert!(now.awaits_remote(&poll(URI, true, 11_000), true));
        assert!(!now.awaits_remote(&poll(URI, true, 12_000), false));
        assert!(now.pending.is_none());
    }

    fn push(active_device: &str, track: &str, playing: bool) -> ConnectPush {
        ConnectPush {
            active_device: active_device.into(),
            track: track.into(),
            context: "spotify:playlist:abc".into(),
            playing,
            queue: Queue::default(),
        }
    }

    #[test]
    fn pushes_about_other_devices_are_worth_a_poll() {
        let own = Some("tunebox");
        let context = Some("spotify:playlist:abc");
        assert!(push("phone", URI, true).worth_polling(own, false, context));
        assert!(!push("tunebox", URI, true).worth_polling(own, false, context));
        assert!(push("tunebox", URI, true).worth_polling(own, false, Some("spotify:album:xyz")));
        assert!(!push("", "", false).worth_polling(own, false, context));
        assert!(push("", "", false).worth_polling(own, true, context));
    }

    #[test]
    fn a_poll_older_than_a_push_is_noticed() {
        let mut remote = poll(URI, true, 1_000);
        remote.device.id = "phone".into();
        assert!(!push("phone", URI, true).is_ahead_of(&remote));
        assert!(push("phone", URI, false).is_ahead_of(&remote));
        assert!(push("phone", OTHER, true).is_ahead_of(&remote));
        assert!(!push("speaker", OTHER, false).is_ahead_of(&remote));
        assert!(!push("phone", "", true).is_ahead_of(&remote));
    }

    #[test]
    fn pausing_freezes_the_clock_where_it_stands() {
        let mut now = paused_track();
        now.playing = true;
        now.position_ms = 1_000;
        now.position_at = Instant::now() - Duration::from_secs(2);
        now.set_playing(false);
        let frozen = now.position_ms();
        assert!((3_000..3_500).contains(&frozen));
        assert_eq!(now.position_ms(), frozen);
    }
}
