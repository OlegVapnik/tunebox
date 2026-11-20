mod browsing;
mod library;
mod menu;
mod persist;
mod queue;
mod settings;

use std::{
    collections::{HashMap, HashSet},
    io::Write,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    buffer::Cell,
    layout::{Position, Rect, Size},
};
use tunebox_core::{
    catalog::{AlbumInfo, ArtistInfo, Page, PlaylistInfo, ShowInfo, Track, User},
    connect::{Device, Playback},
    home::{Home, HomeItem, ItemKind},
    lyrics::Lyrics,
    paths::Paths,
    player::{CurrentTrack, PlayerEvent},
    update::{PackageManager, Release},
};
use tunebox_os::settings::Settings;

use crate::clock::Instant;
use crate::{
    backend::{self, Player, Remote, Service},
    colors::Colors,
    commands::Command,
    fx::{Effect, Effects},
    graphics::{INLINE_CELL, ImageProtocol, InlineLayer},
    loader::{DataEvent, Loader, Picture},
    media::MediaSync,
    notify::Notifier,
    overlay::{DjChoice, MenuAction, Overlay, PaletteAction, PickItem, Picker, PickerOutcome, dj_items},
    playback::{ConnectPush, Cover, NowPlaying, RemoteIntent, Repeat, VolumeSync},
    preferences::{AccentChoice, Preferences, Setting},
    queue::Queue,
    snapshot::Placement,
    theme::{self, Accent, Rgb},
    views::{
        ArtistSelection, ArtistView, HomeView, LibraryView, ListCursor, QueueRow, SearchEntry, SearchView, TrackSource,
        TrackView, View,
    },
};

pub const COVER_SIZE: Size = Size { width: 10, height: 5 };
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(250);
const STATUS_TTL: Duration = Duration::from_secs(8);
const STAR_DELAY: Duration = Duration::from_secs(5);
const SEEK_STEP_MS: i64 = 5_000;
const RESTART_AFTER_MS: u32 = 3_000;
const HISTORY_LIMIT: usize = 50;
const QUICK_NAV: Duration = Duration::from_millis(300);
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
const IMAGE_TTL: Duration = Duration::from_secs(120);
const TERMINAL_IMAGE_BUDGET: usize = 48 << 20;
const IMAGE_RETRY: Duration = Duration::from_secs(30);
const EVICTION_INTERVAL: Duration = Duration::from_secs(30);
const TRIM_DELAY: Duration = Duration::from_secs(1);
const ZEN_COVER_SETTLE: Duration = Duration::from_millis(150);
const ZEN_COVER_RETRY: Duration = Duration::from_secs(10);
const SCROLL_STEP: isize = 3;
const KITTY_PLACEHOLDER: char = '\u{10EEEE}';

pub fn not_animated(cell: &Cell) -> bool {
    cell.symbol().starts_with(KITTY_PLACEHOLDER) || cell.symbol() == INLINE_CELL
}
const POLL_INTERVAL: Duration = Duration::from_secs(3);
const QUICK_POLL: Duration = Duration::from_millis(500);
const PUSHED_POLL_INTERVAL: Duration = Duration::from_secs(15);
const PUSH_DELAY: Duration = Duration::from_millis(150);
const PUSH_LEAD: Duration = Duration::from_secs(2);
const TRACK_END_SLACK: Duration = Duration::from_millis(300);
const CONFIRM_DELAY: Duration = Duration::from_millis(300);
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(5);
const QUEUE_GRACE: Duration = Duration::from_secs(2);
const STATE_SAVE_INTERVAL: Duration = Duration::from_secs(20);
const SAVE_DEBOUNCE: Duration = Duration::from_millis(350);
const FRAME: Duration = Duration::from_millis(120);
const PAGE_FRESH: Duration = Duration::from_secs(60);
const HOME_FRESH: Duration = Duration::from_secs(5 * 60);
const CACHE_SAVE_DELAY: Duration = Duration::from_secs(2);
const LIBRARY_PARTS: u8 = 4;
const PRESS_FLASH: Duration = Duration::from_millis(150);
const LOW_POWER_FRAME: Duration = Duration::from_secs(1);
const SETTINGS_INTERVAL: Duration = Duration::from_secs(3);
const LONG_SLEEP: Duration = Duration::from_secs(60);

pub enum Start<'a> {
    Restore(Box<crate::state::State>),
    Demo(&'a crate::demo::Demo),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Update {
    Available(Release),
    Managed {
        release: Release,
        manager: PackageManager,
    },
    Installing {
        release: Release,
        percent: Option<u8>,
    },
    Installed {
        release: Release,
        binary: PathBuf,
    },
}

fn managed_update(release: &Release, manager: PackageManager) -> String {
    let variant = backend::VARIANT.unwrap_or_default();
    format!("tunebox {} is out · {}", release.version(), manager.how_to_update(variant))
}

fn system_settings() -> Settings {
    if cfg!(test) { Settings::default() } else { tunebox_os::settings::current() }
}

#[allow(clippy::large_enum_variant)]
pub enum AppEvent {
    Data(DataEvent),
    Player(Service, PlayerEvent),
    Pushed(Service, ConnectPush),
    PlayerReady(Box<Player>),
    PlayerFailed(Service, String),
    PlayerLost(Service),
    Power(tunebox_os::power::Event),
    Media(tunebox_os::media::Command),
    OutputLost(Service, String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Nav,
    Content,
    SearchInput,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavItem {
    Home,
    Search,
    Library(LibraryEntry),
}

impl NavItem {
    pub const FIXED: usize = 0;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LibraryEntry {
    Liked,
    Playlist(usize),
    Album(usize),
    Artist(usize),
    Show(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum LibraryFilter {
    #[default]
    All,
    Playlists,
    Albums,
    Artists,
    Podcasts,
}

impl LibraryFilter {
    pub fn label(self) -> &'static str {
        match self {
            LibraryFilter::All => "All",
            LibraryFilter::Playlists => "Playlists",
            LibraryFilter::Albums => "Albums",
            LibraryFilter::Artists => "Artists",
            LibraryFilter::Podcasts => "Podcasts",
        }
    }

    fn shows(self, kind: LibraryFilter) -> bool {
        self == LibraryFilter::All || self == kind
    }
}

pub enum ImageSlot {
    Loading(Arc<AtomicU64>),
    Ready(Picture),
    Failed(Instant),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    NavArea,
    Nav(usize),
    ContentArea,
    Row(usize),
    Card {
        shelf: usize,
        item: usize,
    },
    CardPlay {
        shelf: usize,
        item: usize,
    },
    RowPlay(usize),
    ShelfScroll {
        shelf: usize,
        forward: bool,
    },
    ShelfPlay(usize),
    PinnedCopy,
    PinnedClose,
    HomeChip(usize),
    Link(Link),
    Chip(LibraryFilter),
    PageChip(LibraryFilter),
    Hint(HintAction),
    Header(HeaderButton),
    LikePlaying,
    Service(Service),
    SearchInput,
    Command(Command),
    Seek {
        x: u16,
        width: u16,
    },
    Volume {
        x: u16,
        width: u16,
    },
    Cover,
    SettingStep {
        index: usize,
        forward: bool,
    },
    OverlayArea,
    OverlayItem(usize),
}

impl Hit {
    pub fn is_interactive(&self) -> bool {
        !matches!(self, Hit::NavArea | Hit::ContentArea | Hit::OverlayArea | Hit::Seek { .. } | Hit::Volume { .. })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Link {
    TrackArtist {
        row: usize,
        artist: usize,
    },
    TrackAlbum(usize),
    PlayingArtist(usize),
    PlayingAlbum,
    PlayingContext,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintAction {
    Run(Command),
    Activate,
    Back,
    ToggleFocus,
    Key(KeyCode),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderButton {
    Play,
    Shuffle,
    Save,
    More,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Fx {
    #[default]
    Content,
    Overlay,
    Screen,
}

pub struct LargeCover {
    pub url: String,
    pub size: Size,
    pub picture: Picture,
}

pub struct LyricsState {
    pub uri: String,
    pub result: Option<Result<Option<Lyrics>, String>>,
}

pub struct Status {
    pub text: String,
    pub error: bool,
    since: Instant,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Pinned {
    pub text: String,
    pub command: Option<String>,
    pub copied: bool,
    pub cause: Cause,
    pub service: Option<Service>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    SignIn,
    Blocked,
}

struct Transfer {
    device_id: String,
    name: String,
    since: Instant,
}

struct Target {
    uri: String,
    name: String,
    is_track: bool,
}

pub struct App {
    loader: Loader,
    state_path: std::path::PathBuf,
    last_save: Instant,
    resume: Option<crate::state::LastTrack>,
    last_context: Option<String>,
    restored_volume: Option<u16>,
    volume_sync: VolumeSync,
    players: HashMap<Service, Player>,
    active: Option<Service>,
    player_errors: HashMap<Service, String>,
    parked: HashMap<Service, browsing::Browsing>,
    saved_pages: HashMap<Service, crate::state::Page>,
    device_id: Option<String>,
    local_active: bool,
    remote_active: bool,
    active_device: Option<String>,
    remote_intent: RemoteIntent,
    last_command: Instant,
    devices_cache: Option<Vec<Device>>,
    devices_wanted: bool,
    transfer: Option<Transfer>,
    pub prefs: Preferences,
    next_poll: Instant,
    polling: bool,
    poll_started: Instant,
    last_push: Option<(ConnectPush, Instant)>,
    paths: Paths,
    pub now: NowPlaying,
    track_changed_at: Instant,
    queue: Queue,
    queue_tracks: HashMap<String, Option<Track>>,
    queue_tracks_missing: HashSet<String>,
    queue_version: u64,
    upcoming_asked: Option<(String, u64)>,
    upcoming_pending: bool,
    upcoming_due: Option<Instant>,
    queue_reshuffled: bool,
    media: Option<MediaSync>,
    notifier: Option<Notifier>,
    focused: bool,
    slept_at: Option<std::time::SystemTime>,
    system_settings: Settings,
    pub settings: Settings,
    settings_checked: Instant,
    colors: Option<Colors>,
    pub accent: Accent,
    cover_accent: Option<Rgb>,
    pub me: Option<User>,
    pub playlists: Vec<PlaylistInfo>,
    pub playlists_loading: bool,
    pub albums: Vec<AlbumInfo>,
    pub artists: Vec<ArtistInfo>,
    pub shows: Vec<ShowInfo>,
    pub liked_total: Option<u32>,
    pub library_loading: bool,
    library_pending: u8,
    pub library_filter: LibraryFilter,
    pub signed_out: bool,
    pub problem: Option<String>,
    shown_before: bool,
    home_cache: Option<Home>,
    home_fetched_at: Option<Instant>,
    pub(crate) usage: crate::state::Usage,
    star_due: Option<Instant>,
    #[cfg(test)]
    pub opened: Vec<String>,
    pub pinned: Option<Pinned>,
    data_dirty: bool,
    last_cache_save: Instant,
    cached_tracks: Option<crate::datacache::Tracks>,
    cached_tracks_version: Option<(u64, Instant)>,
    pub images: HashMap<String, ImageSlot>,
    image_used: HashMap<String, Instant>,
    last_eviction: Instant,
    trim_due: Option<Instant>,
    draws: u64,
    frame_started: Instant,
    pub terminal_out: Vec<String>,
    prefetch: Vec<(String, Size, bool)>,
    track_prefetch: Option<(String, Instant)>,
    pub saved: HashMap<String, bool>,
    pending_saves: HashMap<String, (bool, String, Instant)>,
    saving: HashSet<String>,
    unsaved: HashMap<String, (usize, library::LibraryItem)>,
    library_reloads: HashMap<(Service, library::LibraryPart), Instant>,
    pub focus: Focus,
    pub nav: ListCursor,
    pub view: View,
    history: Vec<View>,
    last_nav: Option<Instant>,
    next_view_id: u64,
    pub status: Option<Status>,
    pub overlay: Option<Overlay>,
    pub overlay_fx_pending: bool,
    pub zen: bool,
    pub zen_cover: Option<LargeCover>,
    pub zen_cover_want: Option<Size>,
    zen_cover_want_since: Option<(Size, Instant)>,
    zen_cover_pending: Option<(String, Size)>,
    zen_cover_failed: Option<(String, Size, Instant)>,
    pub lyrics: Option<LyricsState>,
    pub lyrics_scroll: f32,
    pub hits: Vec<(Rect, Hit)>,
    pub mouse: Option<Position>,
    hovered: Option<Hit>,
    dragging: Option<Hit>,
    pub seek_preview: Option<u32>,
    pub placements: Vec<Placement>,
    pub inline: InlineLayer,
    pub snapshot_requested: bool,
    last_click: Option<(Instant, Hit)>,
    pressed: Option<(Hit, Rect, Instant)>,
    pub effects: Effects<Fx>,
    pub last_draw: Instant,
    pub content_area: Rect,
    pub screen: Rect,
    pub page_height: usize,
    started: Instant,
    last_frame: u64,
    pub quit: bool,
    pub update: Option<Update>,
    pub restart: Option<PathBuf>,
    demo: bool,
}

impl App {
    pub fn new(
        loader: Loader,
        prefs: Preferences,
        start: Start,
        paths: Paths,
        media: Option<tunebox_os::media::Controls>,
        notifier: Option<Notifier>,
        mut colors: Option<Colors>,
    ) -> Self {
        let system_settings = system_settings();
        if let Some(palette) = colors.as_mut().and_then(Colors::poll) {
            theme::set_palette(palette);
        }
        let mut accent = Accent::new();
        if let Some(color) = prefs.accent.fixed() {
            accent.jump(color);
        }
        let mut app = Self {
            loader,
            state_path: paths.state_file(),
            last_save: Instant::now(),
            resume: None,
            last_context: None,
            restored_volume: None,
            volume_sync: VolumeSync::default(),
            players: HashMap::new(),
            active: None,
            player_errors: HashMap::new(),
            parked: HashMap::new(),
            saved_pages: HashMap::new(),
            device_id: None,
            local_active: false,
            remote_active: false,
            active_device: None,
            remote_intent: RemoteIntent::default(),
            last_command: Instant::now(),
            devices_cache: None,
            devices_wanted: false,
            transfer: None,
            settings: prefs.apply(system_settings),
            prefs,
            next_poll: Instant::now(),
            polling: false,
            poll_started: Instant::now(),
            last_push: None,
            paths,
            now: NowPlaying::new(),
            track_changed_at: Instant::now(),
            queue: Queue::default(),
            queue_tracks: HashMap::new(),
            queue_tracks_missing: HashSet::new(),
            queue_version: 0,
            upcoming_asked: None,
            upcoming_pending: false,
            upcoming_due: None,
            queue_reshuffled: false,
            media: media.map(MediaSync::new),
            notifier,
            focused: true,
            slept_at: None,
            system_settings,
            settings_checked: Instant::now(),
            colors,
            accent,
            cover_accent: None,
            me: None,
            playlists: Vec::new(),
            playlists_loading: true,
            albums: Vec::new(),
            artists: Vec::new(),
            shows: Vec::new(),
            liked_total: None,
            library_loading: true,
            library_pending: LIBRARY_PARTS,
            library_filter: LibraryFilter::All,
            signed_out: false,
            problem: None,
            shown_before: false,
            home_cache: None,
            home_fetched_at: None,
            usage: crate::state::Usage::default(),
            pinned: None,
            star_due: None,
            #[cfg(test)]
            opened: Vec::new(),
            data_dirty: false,
            last_cache_save: Instant::now(),
            cached_tracks: None,
            cached_tracks_version: None,
            images: HashMap::new(),
            image_used: HashMap::new(),
            last_eviction: Instant::now(),
            trim_due: None,
            draws: 0,
            frame_started: Instant::now(),
            terminal_out: Vec::new(),
            prefetch: Vec::new(),
            track_prefetch: None,
            saved: HashMap::new(),
            pending_saves: HashMap::new(),
            saving: HashSet::new(),
            unsaved: HashMap::new(),
            library_reloads: HashMap::new(),
            focus: Focus::Content,
            nav: ListCursor::default(),
            view: View::Home(HomeView::new(0, None)),
            history: Vec::new(),
            last_nav: None,
            next_view_id: 1,
            status: None,
            overlay: None,
            overlay_fx_pending: false,
            zen: false,
            zen_cover: None,
            zen_cover_want: None,
            zen_cover_want_since: None,
            zen_cover_pending: None,
            zen_cover_failed: None,
            lyrics: None,
            lyrics_scroll: 0.0,
            hits: Vec::new(),
            placements: Vec::new(),
            inline: InlineLayer::default(),
            snapshot_requested: false,
            mouse: None,
            hovered: None,
            dragging: None,
            seek_preview: None,
            last_click: None,
            pressed: None,
            effects: Effects::default(),
            last_draw: Instant::now(),
            content_area: Rect::default(),
            screen: Rect::default(),
            page_height: 10,
            started: Instant::now(),
            last_frame: 0,
            quit: false,
            update: None,
            restart: None,
            demo: matches!(start, Start::Demo(_)),
        };
        match start {
            Start::Restore(state) => app.restore(*state),
            Start::Demo(demo) => {
                app.open_browsing();
                app.start_demo(demo);
            }
        }
        app
    }

    fn start_demo(&mut self, demo: &crate::demo::Demo) {
        let playing = &demo.playing;
        let track = &playing.track;
        let last = crate::state::LastTrack {
            uri: track.uri.clone(),
            name: track.name.clone(),
            artists: track.artists.clone(),
            artist_uris: track.artist_uris.clone(),
            album: track.album.clone(),
            album_uri: Some(track.album_uri.clone()),
            duration_ms: track.duration_ms,
            cover_url: track.image_url.clone(),
            cover_url_large: playing.cover_url_large.clone(),
            context: playing.context.clone(),
            position_ms: playing.position_ms,
            queued: Vec::new(),
        };
        self.now.restore(&last);
        self.now.set_playing(true);
        self.now.volume = (u32::from(u16::MAX) * 7 / 10) as u16;
        self.last_context = last.context.clone();
        if let Some(url) = &last.cover_url {
            self.load_cover(url.clone());
        }
        self.after_track_change(None);
        let (queued, rest) = demo.upcoming();
        self.start_demo_queue(queued, rest);
    }

    #[cfg(test)]
    pub fn is_idle(&self) -> bool {
        self.loader.is_idle()
    }

    pub fn can_resume(&self) -> bool {
        self.resume.is_some()
    }

    pub fn is_connected(&self) -> bool {
        self.player().is_some()
    }

    fn player(&self) -> Option<&Player> {
        self.players.get(&self.active?)
    }

    pub fn service(&self) -> Option<Service> {
        self.active.or(self.loader.service())
    }

    pub fn player_error(&self) -> Option<&str> {
        self.player_errors.get(&self.service()?).map(String::as_str)
    }

    pub fn service_name(&self) -> &'static str {
        self.service().map_or("your music service", Service::name)
    }

    pub fn supports_dj(&self) -> bool {
        self.service().is_some_and(backend::supports_dj)
    }

    pub fn supports_devices(&self) -> bool {
        self.service().is_some_and(backend::supports_devices)
    }

    pub fn supports_crossfade(&self) -> bool {
        self.service().is_some_and(backend::supports_crossfade)
    }

    pub fn offers(&self, command: Command) -> bool {
        match command {
            Command::Dj => self.supports_dj(),
            Command::Devices => self.supports_devices(),
            Command::Crossfade => self.supports_crossfade(),
            Command::SwitchService => self.next_service().is_some(),
            Command::Update => backend::VARIANT.is_some(),
            _ => true,
        }
    }

    pub fn frame(&self) -> u64 {
        let frame = if self.settings.low_power { LOW_POWER_FRAME } else { FRAME };
        (self.started.elapsed().as_millis() / frame.as_millis()) as u64
    }

    pub fn calm(&self) -> bool {
        self.settings.reduce_motion || self.settings.low_power
    }

    pub fn is_saved(&self, uri: &str) -> bool {
        self.saved.get(uri).copied().unwrap_or(false)
    }

    pub fn pressed(&self) -> Option<(Hit, Rect)> {
        self.pressed.filter(|(_, _, at)| at.elapsed() < PRESS_FLASH).map(|(hit, area, _)| (hit, area))
    }

    pub fn shutdown(&mut self) {
        self.save_state();
        if self.data_dirty {
            self.save_library(false);
        }
        self.save_parked();
        self.save_open_tracks(false);
        for player in self.players.values() {
            player.shutdown();
        }
    }


    pub fn check_for_update(&mut self, asked: bool) {
        if self.offers(Command::Update) {
            self.loader.check_update(self.paths.clone(), asked);
        }
    }

    fn on_update_available(&mut self, release: Release) {
        if matches!(self.update, Some(Update::Installing { .. } | Update::Installed { .. })) {
            return;
        }
        let update = match PackageManager::of_running() {
            Some(manager) => Update::Managed { release, manager },
            None => Update::Available(release),
        };
        if self.update.as_ref() != Some(&update) {
            self.announce(&update);
        }
        self.update = Some(update);
    }

    fn announce(&mut self, update: &Update) {
        match update {
            Update::Available(release) => self.notice(format!("tunebox {} is out · U updates", release.version())),
            Update::Managed { release, manager } => self.notice(managed_update(release, *manager)),
            Update::Installing { .. } | Update::Installed { .. } => {}
        }
    }

    fn run_update(&mut self) {
        match self.update.clone() {
            None if self.demo => self.error("This is the demo: nothing updates here".into()),
            None => {
                self.notice("Looking for updates…".into());
                self.check_for_update(true);
            }
            Some(update @ Update::Managed { .. }) => self.announce(&update),
            Some(Update::Available(release)) => {
                self.notice(format!("Updating to tunebox {}…", release.version()));
                self.loader.install_update(release.clone());
                self.update = Some(Update::Installing { release, percent: None });
            }
            Some(Update::Installing { .. }) => {}
            Some(Update::Installed { binary, .. }) => {
                self.restart = Some(binary);
                self.quit = true;
            }
        }
    }

    fn update_label(&self) -> String {
        match &self.update {
            None => Command::Update.label().to_owned(),
            Some(Update::Available(release)) => format!("Update to tunebox {}", release.version()),
            Some(Update::Managed { release, manager }) => managed_update(release, *manager),
            Some(Update::Installing { .. }) => "Updating tunebox…".to_owned(),
            Some(Update::Installed { release, .. }) => format!("Restart into tunebox {}", release.version()),
        }
    }


    pub fn on_input(&mut self, event: Event) -> bool {
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                self.on_key(key);
                true
            }
            Event::Mouse(mouse) => self.on_mouse(mouse),
            Event::Resize(..) => true,
            Event::FocusGained | Event::FocusLost => {
                self.focused = matches!(event, Event::FocusGained);
                false
            }
            _ => false,
        }
    }

    pub fn on_event(&mut self, event: AppEvent) -> bool {
        match event {
            AppEvent::Player(service, event) => return self.on_player(service, event),
            AppEvent::Pushed(service, push) if self.is_active(service) => return self.on_push(push),
            AppEvent::Pushed(..) => return false,
            AppEvent::PlayerReady(player) => self.on_player_ready(*player),
            AppEvent::PlayerFailed(service, err) => self.on_player_failed(service, err),
            AppEvent::PlayerLost(service) => self.on_player_lost(service),
            AppEvent::Power(event) => self.on_power(event),
            AppEvent::Media(command) => self.on_media(command),
            AppEvent::OutputLost(service, name) => self.on_output_lost(service, name),
            AppEvent::Data(data) => return self.on_data(data),
        }
        true
    }

    fn is_active(&self, service: Service) -> bool {
        self.active.is_none_or(|active| active == service)
    }

    fn on_player_ready(&mut self, player: Player) {
        let service = player.service();
        self.player_errors.remove(&service);
        player.set_crossfade(Duration::from_secs(self.prefs.crossfade));
        self.players.insert(service, player);
        if self.active.is_none() || (self.now.track.is_none() && self.loader.service() == Some(service)) {
            self.take_over(service);
        } else if self.is_active(service) {
            self.device_id = self.players.get(&service).and_then(Player::device_id);
        }
        if self.loader.service() == Some(service) {
            self.reload_failed_view();
        }
    }

    fn on_player_lost(&mut self, service: Service) {
        self.players.remove(&service);
        if !self.is_active(service) {
            return;
        }
        if !self.remote_active
            && self.resume.is_none()
            && let Some(last) = self.snapshot().last_track
        {
            self.now.restore(&last);
            self.resume = Some(last);
        }
        self.notice(format!("Connection to {} lost, reconnecting…", service.name()));
    }

    fn on_power(&mut self, event: tunebox_os::power::Event) {
        use tunebox_os::power::Event;
        match event {
            Event::Sleep => {
                self.slept_at = Some(std::time::SystemTime::now());
                if !self.remote_active && self.now.playing {
                    self.control(|c| c.pause());
                }
                self.save_state();
            }
            Event::Wake => {
                let slept = self.slept_at.take().and_then(|at| at.elapsed().ok());
                if slept.is_none_or(|slept| slept >= LONG_SLEEP) {
                    for player in self.players.values() {
                        player.reconnect();
                    }
                }
                self.next_poll = Instant::now();
            }
        }
    }

    fn on_output_lost(&mut self, service: Service, name: String) {
        if !self.is_active(service) {
            if let Some(player) = self.players.get(&service) {
                let _ = player.pause();
            }
            return;
        }
        self.control(Player::pause);
        self.now.set_playing(false);
        self.notice(format!("Paused: {name} disconnected"));
    }

    fn on_media(&mut self, command: tunebox_os::media::Command) {
        use tunebox_os::media::Command as Media;
        match command {
            Media::Toggle => self.run(Command::PlayPause),
            Media::Play if !self.now.playing => self.run(Command::PlayPause),
            Media::Pause if !self.remote_active => {
                self.control(|c| c.pause());
            }
            Media::Pause if self.now.playing => self.run(Command::PlayPause),
            Media::Play | Media::Pause => {}
            Media::Next => self.run(Command::Next),
            Media::Previous => self.run(Command::Previous),
            Media::Seek(position) => self.seek_to(position.as_millis().try_into().unwrap_or(u32::MAX)),
            Media::Volume(volume) => self.set_volume(volume),
        }
    }

    pub fn announce_track(&mut self) {
        let Some(notifier) = self.notifier.as_mut().filter(|_| self.prefs.notifications) else { return };
        let track = self.now.track.as_ref().filter(|_| !self.remote_active);
        let artwork = track.and_then(|track| self.media.as_ref()?.artwork(track));
        if let Some(sequence) = notifier.update(track, self.now.playing, self.focused, artwork) {
            let mut stdout = std::io::stdout();
            let _ = stdout.write_all(sequence.as_bytes()).and_then(|()| stdout.flush());
        }
    }

    pub fn sync_media(&mut self) {
        let Some(media) = &mut self.media else { return };
        let track = self.now.track.as_ref().filter(|_| !self.remote_active);
        if let Some(url) = media.missing_artwork(track) {
            self.loader.artwork(url);
        }
        media.sync(&self.now, !self.remote_active);
    }

    fn on_player_failed(&mut self, service: Service, err: String) {
        tracing::warn!("{} player: {err}", service.name());
        self.player_errors.insert(service, err);
    }

    fn reload_failed_view(&mut self) {
        match &mut self.view {
            View::Home(v) if v.error.is_some() => {
                v.error = None;
                v.loading = true;
                self.loader.home(v.id, v.filter.as_ref().map(|c| c.id.clone()));
            }
            View::Artist(v) if v.error.is_some() => {
                v.error = None;
                self.loader.artist(v.id, v.uri.clone());
            }
            View::Tracks(v) if v.error.is_some() => {
                self.next_view_id += 1;
                v.load(self.next_view_id, |id, source| self.loader.tracks(id, source));
            }
            View::Search(v) if v.error.is_some() => {
                v.error = None;
                v.loading = true;
                self.loader.search(v.id, v.query.trim().to_owned());
            }
            _ => {}
        }
    }

    fn on_player(&mut self, service: Service, event: PlayerEvent) -> bool {
        if !self.is_active(service) {
            if !matches!(event, PlayerEvent::TrackChanged { .. } | PlayerEvent::Playing { .. }) {
                return false;
            }
            self.take_over(service);
        }
        if let PlayerEvent::QueueChanged { context_uri, current, next } = event {
            if !self.remote_active {
                self.set_queue(Queue::from_player(context_uri, current, &next));
            }
            return matches!(self.view, View::Queue(_));
        }
        if matches!(event, PlayerEvent::ShuffleChanged { shuffle } if shuffle != self.now.shuffle)
            && !self.remote_active
        {
            self.queue_reshuffled();
        }
        match &event {
            PlayerEvent::Activated => {
                self.local_active = true;
                if let Some(volume) = self.volume_sync.next() {
                    self.control(|c| c.set_volume(volume));
                }
            }
            PlayerEvent::Deactivated => {
                self.local_active = false;
                self.next_poll = Instant::now();
            }
            PlayerEvent::Unavailable { refused: true, .. } => {
                self.error(
                    "Spotify refused the key to a song, so it was skipped. See github.com/OlegVapnik/tunebox/blob/main/website/src/pages/troubleshooting".into(),
                );
            }
            PlayerEvent::Unavailable { refused: false, .. } => {
                self.notice("A song couldn't load, so it was skipped".into())
            }
            PlayerEvent::Blocked { reason } => {
                self.pinned =
                    Some(Pinned { text: reason.clone(), command: None, copied: false, cause: Cause::Blocked, service: self.service() });
            }
            PlayerEvent::Playing { .. } => self.unpin(Cause::Blocked),
            _ => {}
        }
        if matches!(event, PlayerEvent::TrackChanged { .. } | PlayerEvent::Playing { .. }) {
            self.resume = None;
            if self.transfer.as_ref().is_some_and(|t| Some(&t.device_id) == self.device_id.as_ref()) {
                self.transfer = None;
            }
        }
        if self.remote_active {
            match event {
                PlayerEvent::TrackChanged { .. } | PlayerEvent::Playing { .. } => {
                    self.remote_active = false;
                    self.now.device = None;
                }
                _ => return false,
            }
        }
        if let PlayerEvent::VolumeChanged { volume } = event {
            let (show, send) = self.volume_sync.reported(volume);
            if let Some(next) = send {
                self.control(|c| c.set_volume(next));
            }
            if !show {
                return false;
            }
            self.restored_volume = Some(volume);
        }
        let before = self.now.track.as_ref().map(|t| t.uri.clone());
        if let Some(url) = self.now.apply(event) {
            self.load_cover(url);
        }
        self.after_track_change(before);
        true
    }

    fn on_push(&mut self, push: ConnectPush) -> bool {
        if push.worth_polling(self.device_id.as_deref(), self.remote_active, self.last_context.as_deref()) {
            self.next_poll = self.next_poll.min(Instant::now() + PUSH_DELAY);
        }
        self.active_device = Some(push.active_device.clone()).filter(|id| !id.is_empty());
        self.set_queue(push.queue.clone());
        self.last_push = Some((push, Instant::now()));
        matches!(self.view, View::Queue(_))
    }

    fn on_playback(&mut self, playback: Option<Playback>) -> bool {
        if let (Some(transfer), Some(p)) = (&self.transfer, &playback)
            && transfer.device_id == p.device.id
        {
            self.transfer = None;
        }
        if let Some(p) = &playback
            && Some(p.device.id.as_str()) != self.device_id.as_deref()
        {
            let stale = if self.remote_active {
                self.now.awaits_remote(p, self.remote_intent.is_waiting())
            } else {
                self.now.pending.is_some()
            };
            if stale {
                self.next_poll = self.next_poll.min(Instant::now() + QUICK_POLL);
                return false;
            }
        }
        self.active_device = playback.as_ref().map(|p| p.device.id.clone());
        match playback {
            Some(p) if Some(p.device.id.as_str()) == self.device_id.as_deref() => {
                let changed = self.remote_active || (p.context.is_some() && p.context != self.last_context);
                if p.context.is_some() {
                    self.last_context = p.context.clone();
                }
                if self.remote_active {
                    self.remote_active = false;
                    self.now.device = None;
                }
                changed
            }
            Some(p) => {
                self.remote_active = true;
                self.resume = None;
                if p.context.is_some() {
                    self.last_context = p.context.clone();
                }
                let before = self.now.track.as_ref().map(|t| t.uri.clone());
                let dragged_volume = matches!(self.dragging, Some(Hit::Volume { .. })).then_some(self.now.volume);
                let (cover, unconfirmed) = self.now.apply_remote(&p, &mut self.remote_intent);
                if let Some(url) = cover {
                    self.load_cover(url);
                }
                let behind =
                    self.last_push.as_ref().is_some_and(|(push, at)| at.elapsed() < PUSH_LEAD && push.is_ahead_of(&p));
                if unconfirmed || behind {
                    self.next_poll = self.next_poll.min(Instant::now() + QUICK_POLL);
                }
                if self.now.playing
                    && let Some(track) = &self.now.track
                {
                    let left = track.duration_ms.saturating_sub(self.now.position_ms());
                    if left > 0 {
                        let end = Instant::now() + Duration::from_millis(left.into()) + TRACK_END_SLACK;
                        self.next_poll = self.next_poll.min(end);
                    }
                }
                if let Some(volume) = dragged_volume {
                    self.now.volume = volume;
                }
                self.after_track_change(before);
                true
            }
            None if self.remote_active => {
                self.remote_active = false;
                self.now.device = None;
                self.now.pending = None;
                self.now.set_playing(false);
                true
            }
            None => false,
        }
    }

    fn after_track_change(&mut self, before: Option<String>) {
        if let (Some(track), View::Tracks(v)) = (&mut self.now.track, &self.view)
            && track.album_uri.is_none()
            && let Some(listed) = v.tracks.iter().find(|t| t.uri == track.uri)
        {
            track.album_uri = Some(listed.album_uri.clone()).filter(|uri| !uri.is_empty());
            if track.artist_uris.is_empty() {
                track.artist_uris = listed.artist_uris.clone();
            }
        }
        if before.as_deref() != self.now.track.as_ref().map(|t| t.uri.as_str()) {
            self.track_changed_at = Instant::now();
            self.check_queue();
        }
        let Some(track) = &self.now.track else { return };
        if before.as_deref() == Some(track.uri.as_str()) {
            return;
        }
        self.lyrics = Some(LyricsState { uri: track.uri.clone(), result: None });
        self.lyrics_scroll = 0.0;
        self.loader.lyrics(track);
        if !self.saved.contains_key(&track.uri) {
            self.loader.check_saved(vec![track.uri.clone()]);
        }
    }

    fn on_data(&mut self, event: DataEvent) -> bool {
        if matches!(
            event,
            DataEvent::Image { .. }
                | DataEvent::Cover { .. }
                | DataEvent::LargeCover { .. }
                | DataEvent::Home { .. }
                | DataEvent::Artist { .. }
                | DataEvent::Tracks { .. }
        ) {
            self.trim_due = Some(Instant::now() + TRIM_DELAY);
        }
        match event {
            DataEvent::For(service, event) => {
                let mut redraw = false;
                self.with_service(service, |app| redraw = app.on_data(*event));
                return redraw;
            }
            DataEvent::Me(result) => match result {
                Ok(user) => {
                    if self.me.as_ref().is_some_and(|me| me.id != user.id) {
                        self.forget_library();
                    }
                    self.me = Some(user);
                    self.data_dirty = true;
                    if let Some(service) = self.loader.service()
                        && self.pinned.as_ref().and_then(|p| p.command.as_deref())
                            == Some(&*format!("tunebox login {}", service.key()))
                    {
                        self.unpin(Cause::SignIn);
                    }
                }
                Err(err) => self.error(err),
            },
            DataEvent::Playlists(result) => {
                self.playlists_loading = false;
                match result {
                    Ok(playlists) => self.set_playlists(playlists),
                    Err(err) => self.error(err),
                }
            }
            DataEvent::LikedCount(result) => {
                self.library_part_done();
                match result {
                    Ok(total) => {
                        self.liked_total = Some(total);
                        self.data_dirty = true;
                    }
                    Err(err) => tracing::warn!("liked songs: {err}"),
                }
            }
            DataEvent::Albums(result) => {
                self.library_part_done();
                match result {
                    Ok(list) => self.set_albums(list),
                    Err(err) => tracing::warn!("albums: {err}"),
                }
            }
            DataEvent::Artists(result) => {
                self.library_part_done();
                match result {
                    Ok(list) => self.set_artists(list),
                    Err(err) => tracing::warn!("artists: {err}"),
                }
            }
            DataEvent::Shows(result) => {
                self.library_part_done();
                match result {
                    Ok(list) => self.set_shows(list),
                    Err(err) => tracing::warn!("shows: {err}"),
                }
            }
            DataEvent::Artist { view, result } => {
                let Some(View::Artist(v)) = self.view_mut(view) else { return false };
                let page = match result {
                    Ok(page) => page,
                    Err(err) => {
                        v.error = Some(err);
                        return true;
                    }
                };
                let uris = page.top_tracks.iter().map(|t| t.track.uri.clone()).collect();
                let (uri, followed) = (page.uri.clone(), page.saved);
                v.set_page(page);
                self.saved.insert(uri, followed);
                self.loader.check_saved(uris);
            }
            DataEvent::Home { view, chip, result } => {
                if let Some(home) = result.as_ref().ok().filter(|_| chip.is_none()) {
                    self.home_cache = Some(home.clone());
                    self.home_fetched_at = Some(Instant::now());
                    self.data_dirty = true;
                }
                let Some(View::Home(v)) = self.view_mut(view) else { return false };
                if !v.wants(chip.as_deref()) {
                    return false;
                }
                match result {
                    Ok(home) => v.set_home(home),
                    Err(err) => match v.chip_failed() {
                        Some(failed) => self.error(format!("Couldn't load {failed}: {err}")),
                        None => {
                            v.loading = false;
                            v.error = Some(err);
                        }
                    },
                }
            }
            DataEvent::Image { .. } | DataEvent::Cover { .. } | DataEvent::LargeCover { .. } if !self.prefs.covers => {
                return false;
            }
            DataEvent::Image { key, result: Ok(picture) } if picture.kind() != self.loader.image_kind() => {
                if matches!(self.images.get(&key), Some(ImageSlot::Loading(_))) {
                    self.images.remove(&key);
                    self.image_used.remove(&key);
                }
            }
            DataEvent::Image { key, result } => {
                let slot = match result {
                    Ok(picture) => ImageSlot::Ready(picture),
                    Err(err) => {
                        tracing::warn!("image {key}: {err}");
                        ImageSlot::Failed(Instant::now())
                    }
                };
                self.images.insert(key, slot);
            }
            DataEvent::ImageSkipped { key } => {
                if matches!(self.images.get(&key), Some(ImageSlot::Loading(_))) {
                    self.images.remove(&key);
                    self.image_used.remove(&key);
                }
            }
            DataEvent::Tracks { view, page } => self.on_tracks(view, page),
            DataEvent::Search { view, query, result } => {
                let Some(View::Search(v)) = self.view_mut(view) else { return false };
                if v.query.trim() != query {
                    return false;
                }
                v.loading = false;
                let uris = match result {
                    Ok(results) => {
                        v.error = None;
                        let uris = results.tracks.iter().map(|t| t.uri.clone()).collect();
                        v.set_results(query, results);
                        uris
                    }
                    Err(err) => {
                        v.error = Some(err);
                        Vec::new()
                    }
                };
                self.loader.check_saved(uris);
            }
            DataEvent::Cover { url, result } => {
                let current = self.now.track.as_ref().and_then(|t| t.cover_url.as_deref());
                let stale = result.as_ref().is_ok_and(|(picture, _)| picture.kind() != self.loader.image_kind());
                if current != Some(url.as_str()) || stale {
                    return false;
                }
                match result {
                    Ok((picture, accent)) => {
                        if let Some(old) = self.now.cover.replace(Cover { url, picture }) {
                            self.terminal_out.extend(old.picture.terminal_delete());
                        }
                        self.cover_accent = accent;
                        if self.prefs.accent == AccentChoice::Cover {
                            self.show_accent();
                        }
                    }
                    Err(err) => tracing::warn!("cover {url}: {err}"),
                }
            }
            DataEvent::Artwork { url, result } => {
                let Some(media) = &mut self.media else { return false };
                match result {
                    Ok(bytes) => media.set_artwork(url, bytes),
                    Err(err) => {
                        tracing::warn!("artwork {url}: {err}");
                        media.artwork_failed(url);
                    }
                }
            }
            DataEvent::LargeCover { url, size, result } => {
                if self.zen_cover_pending.as_ref().is_some_and(|(u, s)| *u == url && *s == size) {
                    self.zen_cover_pending = None;
                }
                let current = self.now.track.as_ref().and_then(|t| t.cover_url_large.as_deref());
                let wanted = self.zen && self.zen_cover_want == Some(size) && current == Some(url.as_str());
                let wanted = wanted && result.as_ref().is_ok_and(|picture| picture.kind() == self.loader.image_kind());
                match result {
                    Ok(picture) if wanted => {
                        if let Some(old) = self.zen_cover.replace(LargeCover { url, size, picture }) {
                            self.terminal_out.extend(old.picture.terminal_delete());
                        }
                    }
                    Ok(_) => {}
                    Err(err) => {
                        tracing::warn!("large cover {url}: {err}");
                        self.zen_cover_failed = Some((url, size, Instant::now()));
                    }
                }
            }
            DataEvent::Lyrics { uri, result } => {
                if let Some(state) = &mut self.lyrics
                    && state.uri == uri
                {
                    state.result = Some(result);
                }
            }
            DataEvent::Saved(flags) => {
                let fresh: Vec<_> = flags.into_iter().filter(|(uri, _)| !self.save_in_progress(uri)).collect();
                let changed = fresh.iter().any(|(uri, saved)| self.saved.get(uri) != Some(saved));
                self.saved.extend(fresh);
                return changed;
            }
            DataEvent::SaveDone { uri, notice } => {
                self.saving.remove(&uri);
                if !self.pending_saves.contains_key(&uri) {
                    self.unsaved.remove(&uri);
                    self.schedule_reload(&uri);
                    self.notice(notice);
                }
            }
            DataEvent::SaveFailed { uri, saved, error } => {
                self.saving.remove(&uri);
                if !self.pending_saves.contains_key(&uri) {
                    self.saved.insert(uri.clone(), saved);
                    self.show_library_change_of(&uri, saved);
                    self.error(error);
                }
            }
            DataEvent::AddFailed { playlist_id, count, error } => {
                if let Some(playlist) = self.playlists.iter_mut().find(|p| p.id == playlist_id) {
                    playlist.total = playlist.total.map(|total| total.saturating_sub(count));
                }
                self.error(error);
            }
            DataEvent::Playback(result) => {
                self.polling = false;
                if self.poll_started < self.last_command {
                    return false;
                }
                match result {
                    Ok(playback) => return self.on_playback(playback),
                    Err(err) => {
                        tracing::warn!("playback state: {err}");
                        return false;
                    }
                }
            }
            DataEvent::RemoteDone { commands, result } => {
                self.remote_intent.answered(commands, result.is_ok());
                match result {
                    Ok(()) => self.next_poll = self.next_poll.min(Instant::now() + CONFIRM_DELAY),
                    Err(err) => {
                        self.cancel_start();
                        self.next_poll = Instant::now();
                        self.error(format!("Remote control: {err}"));
                    }
                }
            }
            DataEvent::Devices(result) => {
                let wanted = std::mem::take(&mut self.devices_wanted);
                match result {
                    Ok(devices) => {
                        self.devices_cache = Some(devices.clone());
                        if matches!(self.overlay, Some(Overlay::Devices { .. })) {
                            self.refresh_devices(devices);
                        } else if wanted {
                            self.open_devices(devices, false);
                        }
                    }
                    Err(err) => {
                        if let Some(Overlay::Devices { refreshing, .. }) = &mut self.overlay {
                            *refreshing = false;
                        }
                        self.error(format!("Devices: {err}"));
                    }
                }
            }
            DataEvent::Transferred { name, result } => match result {
                Ok(()) => {
                    self.notice(format!("Playing on {name}"));
                    self.next_poll = self.next_poll.min(Instant::now() + CONFIRM_DELAY);
                }
                Err(err) => {
                    self.transfer = None;
                    self.error(format!("Transfer: {err}"));
                }
            },
            DataEvent::QueueTracks { asked, tracks, reached } => {
                self.on_queue_tracks(asked, tracks, reached);
                return matches!(self.view, View::Queue(_));
            }
            DataEvent::Upcoming { playing, result } => self.on_upcoming(playing, result),
            DataEvent::SkippedTo(result) => self.on_skipped_to(result),
            DataEvent::DjSuggestions(result) => {
                let Some(Overlay::Dj { picker, suggestions, loading }) = &mut self.overlay else { return true };
                *loading = false;
                match result {
                    Ok(fresh) => {
                        let selected = picker.selected().copied();
                        *suggestions = fresh;
                        picker.set_items(dj_items(&picker.query, suggestions), |&choice| Some(choice) == selected);
                    }
                    Err(err) => tracing::warn!("DJ suggestions: {err}"),
                }
            }
            DataEvent::DjAsked(Ok(())) => {}
            DataEvent::DjAsked(Err(err)) => {
                self.cancel_start();
                self.error(format!("DJ: {err}"));
            }
            DataEvent::UpdateAvailable(release) => self.on_update_available(release),
            DataEvent::UpdateProgress(percent) => {
                if let Some(Update::Installing { percent: shown, .. }) = &mut self.update {
                    *shown = Some(percent);
                }
            }
            DataEvent::UpdateInstalled(binary) => {
                if let Some(Update::Installing { release, .. }) = self.update.take() {
                    self.notice(format!("tunebox {} is installed · U restarts into it", release.version()));
                    self.update = Some(Update::Installed { release, binary });
                }
            }
            DataEvent::UpdateFailed(err) => {
                if let Some(Update::Installing { release, .. }) = self.update.take() {
                    self.update = Some(Update::Available(release));
                }
                self.error(format!("Update failed: {}", err.split_whitespace().collect::<Vec<_>>().join(" ")));
            }
            DataEvent::Notice(text) => self.notice(text),
            DataEvent::Failed(text) => self.error(text),
        }
        true
    }

    fn library_part_done(&mut self) {
        self.library_pending = self.library_pending.saturating_sub(1);
        self.library_loading = self.library_pending > 0;
    }

    fn on_tracks(&mut self, view: u64, page: Result<Page<Track>, String>) {
        let unconfirmed: HashMap<String, bool> =
            self.pending_saves.keys().chain(&self.saving).map(|uri| (uri.clone(), self.is_saved(uri))).collect();
        let Some(View::Tracks(v)) = self.view_mut(view) else { return };
        let page = match page {
            Ok(page) => page,
            Err(err) => {
                v.loading = false;
                v.fetch = None;
                match v.incoming.take() {
                    Some(_) => tracing::warn!("refreshing a track list: {err}"),
                    None => v.error = Some(err),
                }
                return;
            }
        };
        let liked = matches!(v.source, TrackSource::Liked);
        let mut items = page.items;
        if liked {
            items.retain(|t| unconfirmed.get(&t.uri) != Some(&false));
        }
        let uris: Vec<String> = items.iter().map(|t| t.uri.clone()).collect();
        let done = page.next_offset.is_none();
        match &mut v.incoming {
            Some(incoming) => incoming.extend(items),
            None => {
                v.tracks.extend(items);
                v.total = Some(page.total);
                v.loading = !done;
            }
        }
        if done {
            if let Some(mut fresh) = v.incoming.take() {
                if liked {
                    let missing: Vec<Track> = v
                        .tracks
                        .iter()
                        .filter(|t| unconfirmed.get(&t.uri) == Some(&true) && !fresh.iter().any(|f| f.uri == t.uri))
                        .cloned()
                        .collect();
                    fresh.splice(0..0, missing);
                }
                v.total = Some(page.total.max(fresh.len() as u32));
                v.replace_tracks(fresh);
            }
            v.fetched_at = Some(Instant::now());
            v.fetch = None;
        }
        if liked {
            self.saved.extend(uris.into_iter().map(|uri| (uri, true)));
        } else {
            self.loader.check_saved(uris);
        }
    }

    pub fn on_tick(&mut self) -> bool {
        let mut redraw = false;

        if self.settings_checked.elapsed() >= SETTINGS_INTERVAL {
            self.settings_checked = Instant::now();
            self.system_settings = system_settings();
            let settings = self.prefs.apply(self.system_settings);
            redraw |= settings != self.settings;
            self.settings = settings;
        }

        if let View::Search(search) = &mut self.view
            && search.typed_at.is_some_and(|t| t.elapsed() >= SEARCH_DEBOUNCE)
        {
            search.typed_at = None;
            let query = search.query.trim().to_owned();
            if query.is_empty() {
                search.entries.clear();
                search.searched.clear();
            } else if query != search.searched {
                search.loading = true;
                self.loader.search(search.id, query);
            }
            redraw = true;
        }

        self.flush_saves();
        self.reload_library();
        if self.last_save.elapsed() >= STATE_SAVE_INTERVAL {
            self.save_state();
            self.save_open_tracks(true);
        }
        if self.data_dirty && self.last_cache_save.elapsed() >= CACHE_SAVE_DELAY {
            self.save_library(true);
        }
        if self.last_eviction.elapsed() >= EVICTION_INTERVAL {
            self.evict_images();
        }
        if self.trim_due.is_some_and(|due| Instant::now() >= due) {
            self.trim_due = None;
            crate::release_freed_memory();
        }

        if let Some((_, _, at)) = self.pressed {
            if at.elapsed() >= PRESS_FLASH {
                self.pressed = None;
            }
            redraw = true;
        }

        redraw |= self.ask_for_star();

        if self.status.as_ref().is_some_and(|s| s.since.elapsed() >= STATUS_TTL) {
            self.status = None;
            redraw = true;
        }

        if let Some(volume) = self.volume_sync.expire() {
            self.control(|c| c.set_volume(volume));
        }
        if self.transfer.as_ref().is_some_and(|t| t.since.elapsed() >= TRANSFER_TIMEOUT) {
            self.transfer = None;
            redraw = true;
        }
        if self.now.start_failed() {
            let failed_here = self.now.pending.as_ref().is_some_and(|p| !p.skip) && !self.remote_active;
            self.cancel_start();
            if failed_here {
                self.error("Playback didn't start.".into());
            }
            redraw = true;
        }

        if self.player().is_some_and(Player::supports_devices) && !self.polling && Instant::now() >= self.next_poll {
            self.polling = true;
            self.poll_started = Instant::now();
            let interval = if self.last_push.is_some() { PUSHED_POLL_INTERVAL } else { POLL_INTERVAL };
            self.next_poll = Instant::now() + interval;
            self.loader.playback(self.active);
        }

        if self.upcoming_due.is_some_and(|due| Instant::now() >= due) {
            self.ask_upcoming();
            redraw = true;
        }

        if self.zen {
            self.request_zen_cover();
            redraw |= self.scroll_lyrics();
        }

        let frame = self.frame();
        if frame != self.last_frame && self.is_animating() {
            self.last_frame = frame;
            redraw = true;
        }
        redraw || self.accent.is_fading() || self.effects.is_running()
    }

    fn is_animating(&self) -> bool {
        let loading = match &self.view {
            View::Home(v) => v.loading && v.home.is_none(),
            View::Library(_) | View::Settings(_) => false,
            View::Artist(v) => v.page.is_none() && v.error.is_none(),
            View::Tracks(v) => v.loading,
            View::Search(v) => v.loading,
            View::Queue(v) => v.loading,
        };
        let lyrics_loading = self.zen && self.lyrics.as_ref().is_some_and(|l| l.result.is_none());
        self.now.playing
            || self.now.pending.is_some()
            || self.transfer.is_some()
            || matches!(self.overlay, Some(Overlay::Devices { refreshing: true, .. }))
            || loading
            || lyrics_loading
            || self.playlists_loading
            || self.library_loading
            || (!self.is_connected() && self.player_error().is_none())
    }

    fn request_zen_cover(&mut self) {
        if !self.prefs.covers {
            return;
        }
        let (Some(size), Some(track)) = (self.zen_cover_want, &self.now.track) else { return };
        let Some(url) = track.cover_url_large.clone() else { return };
        if self.zen_cover_want_since.is_none_or(|(wanted, _)| wanted != size) {
            self.zen_cover_want_since = Some((size, Instant::now()));
        }
        let shown = self.zen_cover.as_ref().is_some_and(|c| c.url == url);
        let loaded = shown && self.zen_cover.as_ref().is_some_and(|c| c.size == size);
        let pending = self.zen_cover_pending.as_ref().is_some_and(|(u, _)| *u == url);
        let failed = self
            .zen_cover_failed
            .as_ref()
            .is_some_and(|(u, s, at)| *u == url && *s == size && at.elapsed() < ZEN_COVER_RETRY);
        let settled = self.zen_cover_want_since.is_some_and(|(_, since)| since.elapsed() >= ZEN_COVER_SETTLE);
        if loaded || pending || failed || (shown && !settled) {
            return;
        }
        self.zen_cover_pending = Some((url.clone(), size));
        self.loader.large_cover(url, size);
    }

    fn scroll_lyrics(&mut self) -> bool {
        let target = self
            .lyrics
            .as_ref()
            .and_then(|l| l.result.as_ref())
            .and_then(|r| r.as_ref().ok())
            .and_then(|l| l.as_ref())
            .and_then(|l| l.current_line(self.now.position_ms()))
            .map_or(0.0, |line| line as f32);
        let distance = target - self.lyrics_scroll;
        if distance.abs() < 0.02 || self.calm() {
            self.lyrics_scroll = target;
            return distance.abs() >= 0.02;
        }
        self.lyrics_scroll += distance * 0.25;
        true
    }


    fn on_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::F(12) {
            self.snapshot_requested = true;
            return;
        }
        if self.overlay.is_some() {
            if Command::from_key(&key) == Some(Command::Quit) {
                self.quit = true;
            } else if matches!(self.overlay, Some(Overlay::Help)) {
                self.overlay = None;
            } else if matches!(self.overlay, Some(Overlay::Star { .. })) {
                self.on_star_key(key);
            } else if matches!(self.overlay, Some(Overlay::Menu(_))) {
                self.on_menu_key(key);
            } else {
                self.on_picker_key(key);
            }
            return;
        }
        if self.focus == Focus::SearchInput && !self.zen {
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            match Command::from_key(&key) {
                Some(command) if ctrl => self.run(command),
                _ => self.on_search_input(key),
            }
            return;
        }
        if key.code == KeyCode::Char('r') && self.view.has_error() {
            self.reload_failed_view();
            return;
        }
        if let Some(command) = Command::from_key(&key) {
            self.run(command);
            return;
        }
        if self.zen {
            if key.code == KeyCode::Esc {
                self.toggle_zen();
            }
            return;
        }
        match key.code {
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = if self.focus == Focus::Nav { Focus::Content } else { Focus::Nav };
            }
            _ => match self.focus {
                Focus::Nav => self.on_nav_key(key),
                Focus::Content => self.on_content_key(key),
                Focus::SearchInput => {}
            },
        }
    }

    fn on_nav_key(&mut self, key: KeyEvent) {
        let len = self.nav_len();
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.nav.move_by(-1, len),
            KeyCode::Down | KeyCode::Char('j') => self.nav.move_by(1, len),
            KeyCode::Home | KeyCode::Char('g') => self.nav.selected = 0,
            KeyCode::End | KeyCode::Char('G') => self.nav.selected = len.saturating_sub(1),
            KeyCode::Enter | KeyCode::Char('l') => self.open_nav_item(),
            KeyCode::Left => self.cycle_filter(-1),
            KeyCode::Right => self.cycle_filter(1),
            _ => {}
        }
    }

    fn on_content_key(&mut self, key: KeyEvent) {
        if matches!(self.view, View::Settings(_)) {
            return self.on_settings_key(key);
        }
        if let View::Artist(artist) = &mut self.view {
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => artist.move_row(-1),
                KeyCode::Down | KeyCode::Char('j') => artist.move_row(1),
                KeyCode::Right | KeyCode::Char('l') => {
                    artist.move_item(1);
                }
                KeyCode::Left | KeyCode::Char('h') => {
                    if !artist.move_item(-1) {
                        self.focus = Focus::Nav;
                    }
                }
                KeyCode::Home | KeyCode::Char('g') => artist.move_row(isize::MIN / 2),
                KeyCode::End | KeyCode::Char('G') => artist.move_row(isize::MAX / 2),
                KeyCode::Enter => self.activate(),
                KeyCode::Char('p') => {
                    let uri = artist.uri.clone();
                    self.play(uri, None);
                }
                KeyCode::Char('F') => {
                    let (uri, name) = (artist.uri.clone(), artist.name.clone());
                    self.toggle_library(uri, name);
                }
                KeyCode::Esc => self.back(),
                _ => {}
            }
            return;
        }
        if let View::Library(library) = &mut self.view {
            let columns = library.columns.max(1) as isize;
            let delta = match key.code {
                KeyCode::Left | KeyCode::Char('h') if library.cursor % library.columns.max(1) == 0 => {
                    self.focus = Focus::Nav;
                    return;
                }
                KeyCode::Left | KeyCode::Char('h') => -1,
                KeyCode::Right | KeyCode::Char('l') => 1,
                KeyCode::Up | KeyCode::Char('k') => -columns,
                KeyCode::Down | KeyCode::Char('j') => columns,
                KeyCode::Home | KeyCode::Char('g') => isize::MIN / 2,
                KeyCode::End | KeyCode::Char('G') => isize::MAX / 2,
                KeyCode::Char('<') => return self.cycle_page_filter(-1),
                KeyCode::Char('>') => return self.cycle_page_filter(1),
                KeyCode::Enter => return self.activate(),
                KeyCode::Esc => return self.back(),
                _ => return,
            };
            let filter = library.filter;
            let len = self.library_entries_for(filter).len();
            if let View::Library(library) = &mut self.view {
                library.detached = false;
                library.cursor = (library.cursor as isize + delta).clamp(0, len.saturating_sub(1) as isize) as usize;
            }
            return;
        }
        if let View::Home(home) = &mut self.view {
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => home.move_shelf(-1),
                KeyCode::Down | KeyCode::Char('j') => home.move_shelf(1),
                KeyCode::Right | KeyCode::Char('l') => {
                    home.move_item(1);
                }
                KeyCode::Left | KeyCode::Char('h') => {
                    if !home.move_item(-1) {
                        self.focus = Focus::Nav;
                    }
                }
                KeyCode::Home | KeyCode::Char('g') => home.move_shelf(isize::MIN / 2),
                KeyCode::End | KeyCode::Char('G') => home.move_shelf(isize::MAX / 2),
                KeyCode::Enter => self.activate(),
                KeyCode::Char('p') => self.play_home_item(),
                KeyCode::Esc => self.back(),
                _ => {}
            }
            return;
        }
        let page = self.page_height.max(1) as isize;
        let delta = match key.code {
            KeyCode::Up | KeyCode::Char('k') => Some(-1),
            KeyCode::Down | KeyCode::Char('j') => Some(1),
            KeyCode::PageUp => Some(-page),
            KeyCode::PageDown => Some(page),
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => Some(-page / 2),
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => Some(page / 2),
            KeyCode::Home | KeyCode::Char('g') => Some(isize::MIN / 2),
            KeyCode::End | KeyCode::Char('G') => Some(isize::MAX / 2),
            _ => None,
        };
        if let Some(delta) = delta {
            self.move_content(delta);
            return;
        }
        match key.code {
            KeyCode::Enter => self.activate(),
            KeyCode::Left | KeyCode::Char('h') => self.focus = Focus::Nav,
            KeyCode::Esc => self.back(),
            KeyCode::Delete | KeyCode::Backspace if matches!(self.view, View::Queue(_)) => {
                self.remove_selected_from_queue();
            }
            _ => {}
        }
    }

    fn scroll_content(&mut self, delta: isize) {
        match &mut self.view {
            View::Tracks(v) => v.list.scroll(delta),
            View::Search(v) => {
                let rows = v.entry_rows.max(1) as usize;
                v.list.scroll(delta.signum() * delta.unsigned_abs().div_ceil(rows) as isize);
            }
            View::Queue(v) => {
                let rows = v.entry_rows.max(1) as usize;
                v.list.scroll(delta.signum() * delta.unsigned_abs().div_ceil(rows) as isize);
            }
            View::Home(v) => v.scroll_by(delta),
            View::Artist(v) => v.scroll_by(delta),
            View::Library(v) => {
                v.top = (v.top as isize + delta).max(0) as usize;
                v.detached = true;
            }
            View::Settings(v) => {
                v.top = (v.top as isize + delta).max(0) as usize;
                v.detached = true;
            }
        }
    }

    fn move_content(&mut self, delta: isize) {
        let library_len = match &self.view {
            View::Library(v) => self.library_entries_for(v.filter).len(),
            _ => 0,
        };
        match &mut self.view {
            View::Home(v) => v.move_shelf(delta.signum()),
            View::Library(v) => {
                let step = delta.signum() * v.columns.max(1) as isize;
                v.cursor = (v.cursor as isize + step).clamp(0, library_len.saturating_sub(1) as isize) as usize;
            }
            View::Artist(v) => v.move_row(delta.signum()),
            View::Tracks(v) => v.list.move_by(delta, v.tracks.len()),
            View::Search(v) => v.move_by(delta),
            View::Settings(v) => v.move_by(delta.signum()),
            View::Queue(v) => v.move_by(delta),
        }
    }

    fn on_search_input(&mut self, key: KeyEvent) {
        let View::Search(search) = &mut self.view else {
            self.focus = Focus::Content;
            return;
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('u' | 'w') if ctrl => search.query.clear(),
            KeyCode::Char(c) if !ctrl => search.query.push(c),
            KeyCode::Backspace => {
                search.query.pop();
            }
            KeyCode::Enter | KeyCode::Down | KeyCode::Tab => {
                if !search.entries.is_empty() {
                    self.focus = Focus::Content;
                }
                return;
            }
            KeyCode::Esc => {
                if search.entries.is_empty() {
                    self.back();
                } else {
                    self.focus = Focus::Content;
                }
                return;
            }
            _ => return,
        }
        search.typed_at = Some(Instant::now());
    }

    fn on_picker_key(&mut self, key: KeyEvent) {
        let outcome = match &mut self.overlay {
            Some(Overlay::Palette(picker)) => picker.on_key(key).map(Choice::Palette),
            Some(Overlay::AddToPlaylist { picker, .. }) => picker.on_key(key).map(Choice::Playlist),
            Some(Overlay::Devices { picker, .. }) => picker.on_key(key).map(Choice::Device),
            Some(Overlay::Dj { picker, suggestions, .. }) => {
                let query = picker.query.clone();
                let outcome = picker.on_key(key);
                if picker.query != query {
                    let query = picker.query.clone();
                    picker.set_items(dj_items(&query, suggestions), |_| false);
                }
                outcome.map(Choice::Dj)
            }
            _ => PickerOutcome::Stay,
        };
        self.apply_choice(outcome);
    }

    fn on_star_key(&mut self, key: KeyEvent) {
        let Some(Overlay::Star { cursor }) = &mut self.overlay else { return };
        match key.code {
            KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::BackTab | KeyCode::Char('h' | 'l') => {
                *cursor = 1 - *cursor;
            }
            KeyCode::Enter => {
                let star = *cursor == 0;
                self.answer_star(star);
            }
            KeyCode::Char('s') => self.answer_star(true),
            KeyCode::Esc | KeyCode::Char('n') => self.answer_star(false),
            _ => {}
        }
    }

    pub(crate) fn ask_for_star_after(&mut self, delay: Duration) {
        self.star_due = Some(Instant::now() + delay);
    }

    fn ask_for_star(&mut self) -> bool {
        let calm = self.overlay.is_none() && self.focus != Focus::SearchInput && !self.zen && !self.demo;
        if !calm || !self.star_due.is_some_and(|due| Instant::now() >= due) {
            return false;
        }
        self.star_due = None;
        self.usage.star_asked = true;
        self.save_state();
        self.overlay = Some(Overlay::Star { cursor: 0 });
        self.overlay_fx_pending = true;
        true
    }

    fn answer_star(&mut self, star: bool) {
        self.overlay = None;
        if star {
            self.open_in_browser(tunebox_core::update::REPO_URL);
            self.notice("Thank you!".into());
        }
    }

    fn open_in_browser(&mut self, url: &str) {
        #[cfg(not(test))]
        if let Err(err) = open::that_detached(url) {
            self.error(format!("Can't open {url}: {err}"));
        }
        #[cfg(test)]
        self.opened.push(url.to_owned());
    }

    fn on_menu_key(&mut self, key: KeyEvent) {
        let Some(Overlay::Menu(menu)) = &mut self.overlay else { return };
        let len = menu.items.len();
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => menu.cursor = menu.cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => menu.cursor = (menu.cursor + 1).min(len.saturating_sub(1)),
            KeyCode::Enter => {
                let cursor = menu.cursor;
                self.choose_menu_item(cursor);
            }
            KeyCode::Esc | KeyCode::Char('.') => self.overlay = None,
            _ => {}
        }
    }

    fn choose_menu_item(&mut self, index: usize) {
        let Some(Overlay::Menu(menu)) = self.overlay.take() else { return };
        if let Some(&(action, _)) = menu.items.get(index) {
            self.run_menu(action, menu.target);
        }
    }

    fn apply_choice(&mut self, outcome: PickerOutcome<Choice>) {
        match outcome {
            PickerOutcome::Select(Choice::Palette(action)) => self.run_palette(action),
            PickerOutcome::Select(Choice::Playlist(index)) => self.add_to_playlist(index),
            PickerOutcome::Select(Choice::Device(index)) => self.transfer_to(index),
            PickerOutcome::Select(Choice::Dj(choice)) => self.choose_dj(choice),
            PickerOutcome::Close => self.overlay = None,
            PickerOutcome::Stay => {}
        }
    }


    fn on_mouse(&mut self, mouse: MouseEvent) -> bool {
        let position = Position::new(mouse.column, mouse.row);
        self.mouse = Some(position);
        if let Some(target) = self.dragging {
            match mouse.kind {
                MouseEventKind::Drag(MouseButton::Left) => {
                    self.drag(target, mouse.column, false);
                    return true;
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    self.drag(target, mouse.column, true);
                    self.dragging = None;
                    return true;
                }
                _ => {}
            }
        }
        if matches!(mouse.kind, MouseEventKind::Moved | MouseEventKind::Drag(_)) {
            let hovered = self.hit_at(position).filter(Hit::is_interactive);
            let changed = hovered != self.hovered;
            self.hovered = hovered;
            return changed;
        }
        let hit = self.hit_at(position);
        let in_overlay = matches!(hit, Some(Hit::OverlayArea | Hit::OverlayItem(_)));
        let sideways = mouse.modifiers.contains(KeyModifiers::SHIFT);
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if self.overlay.is_some() && !in_overlay {
                    self.overlay = None;
                } else if let Some(hit @ (Hit::Seek { .. } | Hit::Volume { .. })) = hit {
                    self.dragging = Some(hit);
                    self.drag(hit, mouse.column, false);
                } else if let Some(hit) = hit {
                    if hit.is_interactive()
                        && let Some(&(area, _)) = self.hits.iter().rev().find(|(area, _)| area.contains(position))
                    {
                        self.pressed = Some((hit, area, Instant::now()));
                    }
                    self.click(hit, mouse.column);
                }
            }
            MouseEventKind::Down(MouseButton::Right) => {
                self.overlay = None;
                let hit = hit.map(|hit| match hit {
                    Hit::Link(Link::TrackArtist { row, .. } | Link::TrackAlbum(row)) => Hit::Row(row),
                    Hit::CardPlay { shelf, item } => Hit::Card { shelf, item },
                    Hit::RowPlay(index) => Hit::Row(index),
                    hit => hit,
                });
                let anchor = (mouse.column, mouse.row);
                match hit {
                    Some(hit @ (Hit::Row(_) | Hit::Card { .. } | Hit::Nav(_))) => {
                        self.select(hit);
                        self.open_menu(anchor);
                    }
                    Some(
                        Hit::Cover
                        | Hit::LikePlaying
                        | Hit::Link(Link::PlayingArtist(_) | Link::PlayingAlbum | Link::PlayingContext),
                    ) => self.open_playing_menu(anchor),
                    _ => {}
                }
            }
            MouseEventKind::ScrollDown if sideways => self.scroll_sideways(hit, 1),
            MouseEventKind::ScrollUp if sideways => self.scroll_sideways(hit, -1),
            MouseEventKind::ScrollRight => self.scroll_sideways(hit, 1),
            MouseEventKind::ScrollLeft => self.scroll_sideways(hit, -1),
            MouseEventKind::ScrollDown => self.scroll(hit, SCROLL_STEP),
            MouseEventKind::ScrollUp => self.scroll(hit, -SCROLL_STEP),
            _ => return false,
        }
        true
    }

    fn drag(&mut self, target: Hit, column: u16, release: bool) {
        match target {
            Hit::Seek { x, width } => {
                let Some(track) = &self.now.track else { return };
                let ratio = column.saturating_sub(x) as f64 / width.max(1) as f64;
                let position = (ratio.clamp(0.0, 1.0) * track.duration_ms as f64) as u32;
                if release {
                    self.seek_preview = None;
                    self.seek_to(position);
                } else {
                    self.seek_preview = Some(position);
                }
            }
            Hit::Volume { x, width } => {
                let ratio = (column.saturating_sub(x) + 1) as f64 / width.max(1) as f64;
                let volume = (ratio.clamp(0.0, 1.0) * u16::MAX as f64) as u16;
                self.now.volume = volume;
                if release || !self.remote_active {
                    self.set_volume(volume);
                }
            }
            _ => {}
        }
    }

    fn scroll_sideways(&mut self, hit: Option<Hit>, delta: isize) {
        let shelf = match hit {
            Some(
                Hit::Card { shelf, .. }
                | Hit::CardPlay { shelf, .. }
                | Hit::ShelfScroll { shelf, .. }
                | Hit::ShelfPlay(shelf),
            ) => shelf,
            _ => return,
        };
        match &mut self.view {
            View::Home(v) => v.scroll_shelf(shelf, delta),
            View::Artist(v) => v.scroll_shelf(shelf, delta),
            _ => {}
        }
    }

    fn play_selected(&mut self) {
        if let Some(target) = self.context_target() {
            self.run_menu(MenuAction::Play, target);
        }
    }

    pub fn hit_at(&self, position: Position) -> Option<Hit> {
        self.hits.iter().rev().find(|(area, _)| area.contains(position)).map(|(_, hit)| *hit)
    }

    pub fn sync_hover(&mut self) {
        self.hovered = self.mouse.and_then(|position| self.hit_at(position)).filter(Hit::is_interactive);
    }

    pub fn hovered_track_uri(&self) -> Option<String> {
        let hit = self.hovered?;
        match hit {
            Hit::Row(index) => match &self.view {
                View::Tracks(v) => v.tracks.get(index).map(|t| t.uri.clone()),
                View::Search(v) => match v.entries.get(index) {
                    Some(SearchEntry::Track(t)) => Some(t.uri.clone()),
                    _ => None,
                },
                View::Queue(v) => match v.rows.get(index) {
                    Some(QueueRow::Playing(t) | QueueRow::Next { track: t, .. }) => Some(t.uri.clone()),
                    _ => None,
                },
                View::Artist(v) => {
                    let page = v.page.as_ref()?;
                    if index < page.top_tracks.len() { Some(page.top_tracks[index].track.uri.clone()) } else { None }
                }
                _ => None,
            },
            Hit::Card { shelf, item } => match &self.view {
                View::Home(v) => {
                    let home = v.home.as_ref()?;
                    let shelf = home.shelves.get(shelf)?;
                    shelf.items.get(item).and_then(|i| i.is_song().then(|| i.context_uri.clone()))
                }
                View::Artist(v) => {
                    let page = v.page.as_ref()?;
                    let shelf = page.shelves.get(shelf)?;
                    shelf.items.get(item).and_then(|i| i.is_song().then(|| i.context_uri.clone()))
                }
                _ => None,
            },
            _ => None,
        }
    }

    pub fn selected_track_uri(&self) -> Option<String> {
        match &self.view {
            View::Tracks(v) => v.tracks.get(v.list.selected).map(|t| t.uri.clone()),
            View::Search(v) => match v.selected() {
                Some(SearchEntry::Track(t)) => Some(t.uri.clone()),
                _ => None,
            },
            View::Queue(v) => match v.selected() {
                Some(QueueRow::Playing(t) | QueueRow::Next { track: t, .. }) => Some(t.uri.clone()),
                _ => None,
            },
            View::Home(v) => v.selected().and_then(|item| item.is_song().then(|| item.context_uri.clone())),
            View::Artist(v) => match v.selected() {
                Some(ArtistSelection::Track(t)) => Some(t.track.uri.clone()),
                Some(ArtistSelection::Item(item)) => item.is_song().then(|| item.context_uri.clone()),
                None => None,
            },
            _ => None,
        }
    }

    fn select(&mut self, hit: Hit) {
        match hit {
            Hit::Nav(index) => {
                self.focus = Focus::Nav;
                self.nav.selected = index;
            }
            Hit::Row(index) => {
                self.focus = Focus::Content;
                match &mut self.view {
                    View::Tracks(v) => v.list.selected = index,
                    View::Search(v) => {
                        if !matches!(v.entries.get(index), Some(SearchEntry::Header(_)) | None) {
                            v.list.selected = index;
                        }
                    }
                    View::Queue(v) => {
                        if !matches!(v.rows.get(index), Some(QueueRow::Header(_)) | None) {
                            v.list.selected = index;
                        }
                    }
                    View::Artist(v) => {
                        v.row = index;
                        v.item = 0;
                        v.detached = true;
                    }
                    View::Library(v) => {
                        v.cursor = index;
                        v.detached = true;
                    }
                    View::Settings(v) => {
                        v.selected = index;
                        v.detached = true;
                    }
                    View::Home(_) => {}
                }
            }
            Hit::Card { shelf, item } => {
                self.focus = Focus::Content;
                match &mut self.view {
                    View::Home(v) => {
                        v.shelf = shelf;
                        v.item = item;
                        v.detached = true;
                    }
                    View::Artist(v) => {
                        v.row = shelf;
                        v.item = item;
                        v.detached = true;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn click(&mut self, hit: Hit, column: u16) {
        let double = self.last_click.is_some_and(|(at, last)| last == hit && at.elapsed() < DOUBLE_CLICK);
        self.last_click = Some((Instant::now(), hit));
        match hit {
            Hit::Nav(_) => {
                self.select(hit);
                self.open_nav_item();
            }
            Hit::Card { .. } => {
                self.select(hit);
                self.activate();
            }
            Hit::Row(_) => {
                self.select(hit);
                if double || self.opens_on_click() {
                    self.activate();
                }
            }
            Hit::CardPlay { shelf, item } => {
                self.select(Hit::Card { shelf, item });
                self.play_selected();
            }
            Hit::RowPlay(index) => {
                self.select(Hit::Row(index));
                self.play_selected();
            }
            Hit::ShelfScroll { shelf, forward } => {
                let page = |cards: usize| if forward { cards as isize } else { -(cards as isize) };
                match &mut self.view {
                    View::Home(v) => v.scroll_shelf(shelf, page(v.visible(shelf))),
                    View::Artist(v) => v.scroll_shelf(shelf, page(v.cards)),
                    _ => {}
                }
            }
            Hit::ShelfPlay(shelf) => self.play_shelf(shelf),
            Hit::PinnedCopy => self.on_pinned(false),
            Hit::PinnedClose => self.on_pinned(true),
            Hit::HomeChip(index) => {
                self.focus = Focus::Content;
                self.pick_chip(index);
            }
            Hit::Link(link) => self.follow_link(link),
            Hit::Hint(action) => match action {
                HintAction::Run(command) => self.run(command),
                HintAction::Activate if self.focus == Focus::Nav => self.open_nav_item(),
                HintAction::Activate => self.activate(),
                HintAction::Back => {
                    if self.zen {
                        self.toggle_zen();
                    } else {
                        self.back();
                    }
                }
                HintAction::ToggleFocus => {
                    self.focus = if self.focus == Focus::Nav { Focus::Content } else { Focus::Nav };
                }
                HintAction::Key(code) => self.on_key(KeyEvent::new(code, KeyModifiers::NONE)),
            },
            Hit::Header(button) => self.header_button(button),
            Hit::LikePlaying => {
                if let Some(track) = &self.now.track {
                    let (uri, name) = (track.uri.clone(), track.name.clone());
                    self.toggle_library(uri, name);
                }
            }
            Hit::SearchInput => self.run(Command::Search),
            Hit::Service(service) => self.show_service(service),
            Hit::PageChip(filter) => self.set_page_filter(filter),
            Hit::Chip(filter) => {
                let filter = if self.library_filter == filter { LibraryFilter::All } else { filter };
                self.set_filter(filter);
            }
            Hit::Command(command) => self.run(command),
            Hit::Seek { .. } | Hit::Volume { .. } => self.drag(hit, column, true),
            Hit::Cover => self.toggle_zen(),
            Hit::SettingStep { index, forward } => {
                self.select(Hit::Row(index));
                if let View::Settings(v) = &self.view {
                    let setting = v.setting();
                    self.change_setting(setting, forward);
                }
            }
            Hit::OverlayItem(position) if matches!(self.overlay, Some(Overlay::Menu(_))) => {
                self.choose_menu_item(position);
            }
            Hit::OverlayItem(position) if matches!(self.overlay, Some(Overlay::Star { .. })) => {
                self.answer_star(position == 0);
            }
            Hit::OverlayItem(position) => {
                let outcome = match &self.overlay {
                    Some(Overlay::Palette(picker)) => picker.select(position).map(Choice::Palette),
                    Some(Overlay::AddToPlaylist { picker, .. }) => picker.select(position).map(Choice::Playlist),
                    Some(Overlay::Devices { picker, .. }) => picker.select(position).map(Choice::Device),
                    Some(Overlay::Dj { picker, .. }) => picker.select(position).map(Choice::Dj),
                    _ => PickerOutcome::Stay,
                };
                self.apply_choice(outcome);
            }
            Hit::NavArea | Hit::ContentArea | Hit::OverlayArea => {}
        }
    }

    fn opens_on_click(&self) -> bool {
        match &self.view {
            View::Library(_) => true,
            View::Search(v) => !matches!(v.selected(), Some(SearchEntry::Track(_)) | None),
            _ => false,
        }
    }

    fn scroll(&mut self, hit: Option<Hit>, delta: isize) {
        match hit {
            Some(Hit::OverlayArea | Hit::OverlayItem(_)) => match &mut self.overlay {
                Some(Overlay::Menu(menu)) => {
                    menu.cursor =
                        (menu.cursor as isize + delta.signum()).clamp(0, menu.items.len() as isize - 1) as usize;
                }
                Some(Overlay::Palette(picker)) => picker.cursor.move_by(delta.signum(), picker.matches.len()),
                Some(Overlay::AddToPlaylist { picker, .. }) | Some(Overlay::Devices { picker, .. }) => {
                    picker.cursor.move_by(delta.signum(), picker.matches.len())
                }
                Some(Overlay::Dj { picker, .. }) => picker.cursor.move_by(delta.signum(), picker.matches.len()),
                _ => {}
            },
            Some(Hit::NavArea | Hit::Nav(_) | Hit::Chip(_)) => self.nav.scroll(delta.signum()),
            Some(
                Hit::ContentArea
                | Hit::Row(_)
                | Hit::Card { .. }
                | Hit::CardPlay { .. }
                | Hit::RowPlay(_)
                | Hit::ShelfScroll { .. }
                | Hit::ShelfPlay(_)
                | Hit::HomeChip(_)
                | Hit::PageChip(_)
                | Hit::Header(_)
                | Hit::SettingStep { .. },
            ) => self.scroll_content(delta),
            _ => {}
        }
    }


    pub fn run(&mut self, command: Command) {
        match command {
            Command::Home => {
                self.zen = false;
                if !matches!(self.view, View::Home(_)) {
                    self.open_home();
                }
            }
            Command::Library => {
                self.zen = false;
                if !matches!(self.view, View::Library(_)) {
                    self.open_library_view();
                }
            }
            Command::Quit => self.quit = true,
            Command::Update => self.run_update(),
            Command::PlayPause if self.remote_active => {
                let playing = !self.now.playing;
                self.now.set_playing(playing);
                self.remote_intent.playing = Some(playing);
                self.remote(if playing { Remote::Play } else { Remote::Pause });
            }
            Command::PlayPause if self.resume.is_some() && self.is_connected() => {
                if let Some(last) = self.resume.take() {
                    let uri = last.uri.clone();
                    if self.start_on(&uri, move |p| {
                        p.resume(last.context, last.uri, last.position_ms)?;
                        if !last.queued.is_empty() {
                            p.clear_queue()?;
                            for queued in last.queued {
                                p.add_to_queue(&queued)?;
                            }
                        }
                        Ok(())
                    }) {
                        self.now.start(Some(uri));
                    }
                }
            }
            Command::PlayPause if self.idle() && self.is_connected() => self.play_shown(),
            Command::PlayPause => {
                let has_track = self.now.track.is_some();
                self.control(|p| p.toggle(has_track));
            }
            Command::Next | Command::Previous | Command::Shuffle | Command::Repeat
                if self.idle() && self.is_connected() =>
            {
                self.notice("Nothing is playing here yet · press space to start".into());
            }
            Command::Next => self.skip(true),
            Command::Previous => self.skip(false),
            Command::SeekBack => self.seek_by(-SEEK_STEP_MS),
            Command::SeekForward => self.seek_by(SEEK_STEP_MS),
            Command::VolumeUp => self.change_volume(1),
            Command::VolumeDown => self.change_volume(-1),
            Command::Shuffle => {
                let on = !self.now.shuffle;
                if self.remote_active {
                    self.now.shuffle = on;
                    self.remote_intent.shuffle = Some(on);
                    self.remote(Remote::Shuffle(on));
                } else {
                    self.control(|p| p.shuffle(on));
                }
            }
            Command::Repeat => {
                let next = match self.now.repeat {
                    Repeat::Off => Repeat::Context,
                    Repeat::Context => Repeat::Track,
                    Repeat::Track => Repeat::Off,
                };
                if self.remote_active {
                    self.now.repeat = next;
                    self.remote_intent.repeat = Some(next);
                    self.remote(Remote::Repeat(next));
                } else {
                    self.control(|p| p.repeat(next));
                }
            }
            Command::Search => {
                if self.zen {
                    self.toggle_zen();
                }
                self.open_search();
            }
            Command::NowPlaying => self.toggle_zen(),
            Command::ShowQueue => self.toggle_queue(),
            Command::Like => self.toggle_saved(),
            Command::Queue => match self.target() {
                Some(target) if target.is_track => self.queue(target.uri, target.name),
                Some(_) => self.error("Only songs can be added to the queue.".into()),
                None => self.error("No song selected.".into()),
            },
            Command::AddToPlaylist => self.open_playlist_picker(),
            Command::Menu => {
                let area = self.content_area;
                self.open_menu((area.x + area.width / 3, area.y + area.height / 3));
            }
            Command::Dj if self.dj_playing() => self.open_dj(),
            Command::Dj => match self.service().and_then(backend::dj_context) {
                Some(dj) => {
                    self.notice("Starting DJ…".into());
                    self.play(dj.into(), None);
                }
                None => self.error(format!("{} has no DJ", self.service_name())),
            },
            Command::Crossfade if !self.supports_crossfade() => {
                self.error(format!("{} doesn't crossfade", self.service_name()));
            }
            Command::Crossfade => {
                self.change_setting(Setting::Crossfade, true);
                let text = match self.prefs.crossfade {
                    0 => "Crossfade off".to_owned(),
                    seconds => format!("Crossfade: {seconds} s"),
                };
                self.notice(text);
            }
            Command::Settings => self.open_settings(),
            Command::Devices if !self.supports_devices() => {
                self.error(format!("{} doesn't play on other devices", self.service_name()));
            }
            Command::Devices => {
                self.loader.devices(self.service());
                match self.devices_cache.clone().filter(|d| !d.is_empty()) {
                    Some(devices) => self.open_devices(devices, true),
                    None => {
                        self.devices_wanted = true;
                        self.notice("Finding devices…".into());
                    }
                }
            }
            Command::Palette => self.open_palette(),
            Command::Help => self.open_overlay(Overlay::Help),
            Command::SwitchService => self.switch_service(),
        }
    }

    fn run_palette(&mut self, action: PaletteAction) {
        self.overlay = None;
        match action {
            PaletteAction::Run(command) => self.run(command),
            PaletteAction::OpenHome => {
                self.zen = false;
                self.select_nav(NavItem::Home);
                self.open_home();
            }
            PaletteAction::OpenLiked => {
                self.zen = false;
                self.open_library_entry(LibraryEntry::Liked);
            }
            PaletteAction::OpenPlaylist(index) => {
                self.zen = false;
                self.open_library_entry(LibraryEntry::Playlist(index));
            }
        }
    }

    fn open_palette(&mut self) {
        let mut items: Vec<_> = Command::PALETTE
            .iter()
            .filter(|&&command| self.offers(command))
            .map(|&command| PickItem {
                label: match command {
                    Command::Dj if self.dj_playing() => "Ask DJ…".to_owned(),
                    Command::Update => self.update_label(),
                    Command::SwitchService => match self.next_service() {
                        Some(next) => format!("Switch to {}", next.name()),
                        None => command.label().to_owned(),
                    },
                    _ => command.label().to_owned(),
                },
                detail: command.key().to_owned(),
                image_url: None,
                value: PaletteAction::Run(command),
            })
            .collect();
        if matches!(self.update, Some(Update::Available(_) | Update::Installed { .. }))
            && let Some(index) = items.iter().position(|item| matches!(item.value, PaletteAction::Run(Command::Update)))
        {
            let item = items.remove(index);
            items.insert(0, item);
        }
        items.push(PickItem {
            label: "Home".into(),
            detail: "open".into(),
            image_url: None,
            value: PaletteAction::OpenHome,
        });
        items.push(PickItem {
            label: "Liked Songs".into(),
            detail: "open".into(),
            image_url: None,
            value: PaletteAction::OpenLiked,
        });
        items.extend(self.playlists.iter().enumerate().map(|(index, playlist)| PickItem {
            label: playlist.name.clone(),
            detail: "Playlist".into(),
            image_url: None,
            value: PaletteAction::OpenPlaylist(index),
        }));
        self.open_overlay(Overlay::Palette(Picker::new(items)));
    }

    fn open_playlist_picker(&mut self) {
        match self.target().filter(|t| t.is_track) {
            Some(target) => self.open_playlist_picker_for(target.uri),
            None => self.error("Select a song first.".into()),
        }
    }

    pub(super) fn open_playlist_picker_for(&mut self, uri: String) {
        if let Some(note) = self.library_note() {
            return self.error(note);
        }
        let items: Vec<_> = self
            .playlists
            .iter()
            .enumerate()
            .filter(|(_, p)| p.collaborative || self.owns(p))
            .map(|(index, p)| PickItem {
                label: p.name.clone(),
                detail: p.total.map(|total| format!("{total} songs")).unwrap_or_default(),
                image_url: p.image_url.clone(),
                value: index,
            })
            .collect();
        if items.is_empty() {
            return self.error("You don’t own any playlists yet.".into());
        }
        self.open_overlay(Overlay::AddToPlaylist { uris: vec![uri], picker: Picker::new(items) });
    }

    pub fn owns(&self, playlist: &PlaylistInfo) -> bool {
        self.me.as_ref().is_some_and(|me| match playlist.owner_id.as_str() {
            "" => playlist.owner_name == me.name,
            id => id == me.id,
        })
    }

    fn add_to_playlist(&mut self, index: usize) {
        let Some(Overlay::AddToPlaylist { uris, .. }) = self.overlay.take() else { return };
        if let Some(playlist) = self.playlists.get_mut(index) {
            playlist.total = playlist.total.map(|total| total + uris.len() as u32);
            let text = format!("Added to “{}”", playlist.name);
            self.loader.add_to_playlist(playlist, uris);
            self.notice(text);
        }
    }

    pub(super) fn queue(&mut self, uri: String, name: String) {
        self.loader.queue(uri);
        self.notice(format!("“{name}” added to queue"));
    }

    fn open_overlay(&mut self, overlay: Overlay) {
        self.overlay = Some(overlay);
        self.overlay_fx_pending = true;
    }

    pub fn animate_overlay(&mut self, area: Rect) {
        let p = theme::palette();
        self.overlay_fx_pending = false;
        let effect = if self.settings.reduce_motion {
            Effect::fade_in(p.raised, 150, area)
        } else {
            Effect::sweep_down(p.raised, 6, 180, area)
        };
        self.animate(Fx::Overlay, effect);
    }

    fn toggle_saved(&mut self) {
        match self.target() {
            Some(target) => self.toggle_library(target.uri, target.name),
            None => self.error("Nothing selected.".into()),
        }
    }

    pub(super) fn toggle_library(&mut self, uri: String, name: String) {
        let saved = !self.is_saved(&uri);
        self.saved.insert(uri.clone(), saved);
        self.show_library_change_of(&uri, saved);
        self.pending_saves.insert(uri, (saved, name, Instant::now()));
    }

    fn show_library_change_of(&mut self, uri: &str, saved: bool) {
        match Service::of(uri) {
            Some(service) => self.with_service(service, |app| app.show_library_change(uri, saved)),
            None => self.show_library_change(uri, saved),
        }
    }

    fn flush_saves(&mut self) {
        let ready: Vec<String> = self
            .pending_saves
            .iter()
            .filter(|(uri, (_, _, at))| at.elapsed() >= SAVE_DEBOUNCE && !self.saving.contains(*uri))
            .map(|(uri, _)| uri.clone())
            .collect();
        for uri in ready {
            if let Some((saved, name, _)) = self.pending_saves.remove(&uri) {
                self.saving.insert(uri.clone());
                self.loader.set_saved(uri, name, saved);
            }
        }
    }

    fn save_in_progress(&self, uri: &str) -> bool {
        self.pending_saves.contains_key(uri) || self.saving.contains(uri)
    }

    fn target(&self) -> Option<Target> {
        let track = |uri: &str, name: &str, is_track| Target { uri: uri.into(), name: name.into(), is_track };
        if !self.zen && self.focus == Focus::Content {
            let selected = match &self.view {
                View::Home(v) => v.selected().map(|i| track(&i.uri, &i.title, false)),
                View::Library(v) => self
                    .library_entries_for(v.filter)
                    .get(v.cursor)
                    .and_then(|entry| self.library_target(*entry))
                    .map(|t| track(&t.uri, &t.name, false)),
                View::Artist(v) => v.selected().map(|selection| match selection {
                    ArtistSelection::Track(t) => track(&t.track.uri, &t.track.name, true),
                    ArtistSelection::Item(i) => track(&i.uri, &i.title, false),
                }),
                View::Tracks(v) => v.tracks.get(v.list.selected).map(|t| track(&t.uri, &t.name, true)),
                View::Search(v) => v.selected().and_then(|entry| match entry {
                    SearchEntry::Track(t) => Some(track(&t.uri, &t.name, true)),
                    SearchEntry::Album(a) => Some(track(&a.uri, &a.name, false)),
                    SearchEntry::Artist(a) => Some(track(&a.uri, &a.name, false)),
                    SearchEntry::Playlist(p) => Some(track(&p.uri, &p.name, false)),
                    SearchEntry::Header(_) => None,
                }),
                View::Settings(_) => None,
                View::Queue(v) => v.selected().and_then(QueueRow::track).map(|t| track(&t.uri, &t.name, true)),
            };
            if selected.is_some() {
                return selected;
            }
        }
        self.now.track.as_ref().map(|t| track(&t.uri, &t.name, true))
    }

    fn toggle_zen(&mut self) {
        let p = theme::palette();
        self.zen = !self.zen;
        let screen = self.screen;
        self.animate(Fx::Screen, Effect::fade_in(p.bg, 320, screen));
    }

    fn animate(&mut self, slot: Fx, effect: Effect) {
        if self.settings.low_power {
            return;
        }
        self.effects.start(slot, effect);
        self.last_draw = Instant::now();
    }


    pub fn library_filters(&self) -> Vec<LibraryFilter> {
        let mut filters = vec![LibraryFilter::Playlists];
        let kinds = [
            (LibraryFilter::Albums, self.albums.is_empty()),
            (LibraryFilter::Artists, self.artists.is_empty()),
            (LibraryFilter::Podcasts, self.shows.is_empty()),
        ];
        filters.extend(kinds.into_iter().filter(|(_, empty)| !empty).map(|(filter, _)| filter));
        filters
    }

    pub fn library_entries(&self) -> Vec<LibraryEntry> {
        self.library_entries_for(self.library_filter)
    }

    pub fn library_entries_for(&self, filter: LibraryFilter) -> Vec<LibraryEntry> {
        let mut entries = Vec::new();
        if filter.shows(LibraryFilter::Playlists) {
            if self.library_note().is_none() {
                entries.push(LibraryEntry::Liked);
            }
            entries.extend((0..self.playlists.len()).map(LibraryEntry::Playlist));
        }
        if filter.shows(LibraryFilter::Albums) {
            entries.extend((0..self.albums.len()).map(LibraryEntry::Album));
        }
        if filter.shows(LibraryFilter::Artists) {
            entries.extend((0..self.artists.len()).map(LibraryEntry::Artist));
        }
        if filter.shows(LibraryFilter::Podcasts) {
            entries.extend((0..self.shows.len()).map(LibraryEntry::Show));
        }
        entries
    }

    pub fn library_note(&self) -> Option<String> {
        let service = self.loader.service()?;
        if let Some(problem) = &self.problem {
            return Some(format!("{} isn't available: {problem}", service.name()));
        }
        self.signed_out.then(|| {
            format!(
                "Sign in to {} to see your library here: run `tunebox login {}` in a terminal.",
                service.name(),
                service.key()
            )
        })
    }

    pub fn nav_items(&self) -> Vec<NavItem> {
        self.library_entries().into_iter().map(NavItem::Library).collect()
    }

    fn nav_len(&self) -> usize {
        NavItem::FIXED + self.library_entries().len()
    }

    fn select_nav(&mut self, item: NavItem) {
        if let Some(index) = self.nav_items().iter().position(|i| *i == item) {
            self.nav.selected = index;
        }
    }

    fn set_filter(&mut self, filter: LibraryFilter) {
        self.library_filter = filter;
        self.nav = ListCursor { selected: NavItem::FIXED, ..ListCursor::default() };
        self.nav.move_by(0, self.nav_len());
    }

    fn cycle_filter(&mut self, delta: isize) {
        let filters: Vec<_> = std::iter::once(LibraryFilter::All).chain(self.library_filters()).collect();
        let current = filters.iter().position(|f| *f == self.library_filter).unwrap_or(0) as isize;
        let next = (current + delta).rem_euclid(filters.len() as isize) as usize;
        self.set_filter(filters[next]);
    }

    pub fn playing_context(&self) -> Option<&str> {
        self.now.track.as_ref()?;
        self.last_context.as_deref()
    }

    pub fn dj_playing(&self) -> bool {
        self.local_active && self.playing_context().is_some_and(backend::is_dj)
    }

    fn listed_track(&self, row: usize) -> Option<&Track> {
        match &self.view {
            View::Tracks(v) => v.tracks.get(row),
            View::Search(v) => match v.entries.get(row) {
                Some(SearchEntry::Track(track)) => Some(track),
                _ => None,
            },
            View::Queue(v) => v.rows.get(row).and_then(QueueRow::track),
            _ => None,
        }
    }

    fn follow_link(&mut self, link: Link) {
        let (uri, name) = match link {
            Link::TrackArtist { row, artist } => {
                let Some(track) = self.listed_track(row) else { return };
                match (track.artist_uris.get(artist), track.artists.get(artist)) {
                    (Some(uri), Some(name)) => (uri.clone(), name.clone()),
                    _ => return,
                }
            }
            Link::TrackAlbum(row) => {
                let Some(track) = self.listed_track(row).filter(|t| !t.album_uri.is_empty()) else { return };
                let (uri, name) = (track.album_uri.clone(), track.album.clone());
                let (artists, cover) = (track.artists.clone(), track.image_url.clone());
                return self.open_album(uri, name, artists, cover);
            }
            Link::PlayingArtist(artist) => {
                let Some(track) = &self.now.track else { return };
                match (track.artist_uris.get(artist), track.artists.get(artist)) {
                    (Some(uri), Some(name)) => (uri.clone(), name.clone()),
                    _ => return,
                }
            }
            Link::PlayingAlbum => {
                let Some(track) = &self.now.track else { return };
                let Some(uri) = track.album_uri.clone() else { return };
                let (name, artists, cover) = (track.album.clone(), track.artists.clone(), track.cover_url.clone());
                return self.open_album(uri, name, artists, cover);
            }
            Link::PlayingContext => {
                if let Some(uri) = self.last_context.clone() {
                    self.open_context(uri);
                }
                return;
            }
        };
        self.open_artist(uri, name);
    }

    pub fn can_go_back(&self) -> bool {
        !self.history.is_empty()
    }

    pub fn is_open(&self, item: NavItem) -> bool {
        let NavItem::Library(entry) = item else {
            return matches!((item, &self.view), (NavItem::Home, View::Home(_)) | (NavItem::Search, View::Search(_)));
        };
        let View::Tracks(view) = &self.view else { return false };
        match (entry, &view.source) {
            (LibraryEntry::Liked, TrackSource::Liked) => true,
            (LibraryEntry::Playlist(i), TrackSource::Playlist(p)) => {
                self.playlists.get(i).is_some_and(|q| q.id == p.id)
            }
            (LibraryEntry::Album(i), TrackSource::Album(a)) => self.albums.get(i).is_some_and(|b| b.id == a.id),
            _ => false,
        }
    }

    fn open_nav_item(&mut self) {
        let Some(item) = self.nav_items().get(self.nav.selected).copied() else { return };
        if self.is_open(item) {
            self.focus = if item == NavItem::Search { Focus::SearchInput } else { Focus::Content };
            return;
        }
        match item {
            NavItem::Home => self.open_home(),
            NavItem::Search => self.open_search(),
            NavItem::Library(entry) => self.open_library_entry(entry),
        }
    }

    fn open_library_entry(&mut self, entry: LibraryEntry) {
        if !self.nav_items().contains(&NavItem::Library(entry)) {
            self.library_filter = LibraryFilter::All;
        }
        self.select_nav(NavItem::Library(entry));
        match entry {
            LibraryEntry::Liked => self.open_tracks(TrackSource::Liked),
            LibraryEntry::Playlist(i) => {
                if let Some(playlist) = self.playlists.get(i).cloned() {
                    self.open_tracks(TrackSource::Playlist(playlist));
                }
            }
            LibraryEntry::Album(i) => {
                if let Some(album) = self.albums.get(i).cloned() {
                    self.open_tracks(TrackSource::Album(album));
                }
            }
            LibraryEntry::Artist(i) => {
                if let Some(artist) = self.artists.get(i).cloned() {
                    self.open_artist(artist.uri, artist.name);
                }
            }
            LibraryEntry::Show(i) => {
                if let Some(show) = self.shows.get(i).cloned() {
                    self.play(show.uri, None);
                }
            }
        }
    }

    fn open_home(&mut self) {
        let id = self.next_id();
        let mut view = HomeView::new(id, self.home_cache.clone());
        if self.home_fetched_at.is_some_and(|at| at.elapsed() < HOME_FRESH) {
            view.loading = false;
        } else {
            self.loader.home(id, None);
        }
        self.push_view(View::Home(view));
        self.focus = Focus::Content;
    }

    pub fn cover(&mut self, url: Option<&str>, size: Size, round: bool) -> Option<&Picture> {
        self.image(url?, size, round)
    }

    pub fn begin_frame(&mut self) {
        self.draws += 1;
        self.frame_started = Instant::now();
    }

    pub fn end_frame(&mut self) {
        self.loader.frame_drawn(self.draws);
        let mut queue = std::mem::take(&mut self.prefetch);
        for (url, size, round) in queue.drain(..) {
            let key = image_key(&url, size);
            if self.images.contains_key(&key) {
                continue;
            }
            self.images.insert(key.clone(), ImageSlot::Loading(Arc::new(AtomicU64::new(self.draws))));
            self.image_used.insert(key.clone(), Instant::now());
            self.loader.prefetch_image(key, url, size, round);
        }
        self.prefetch = queue;

        let target = self
            .selected_track_uri()
            .map(|uri| (uri, Duration::from_millis(50)))
            .or_else(|| self.hovered_track_uri().map(|uri| (uri, Duration::from_millis(300))));
        if let Some((uri, debounce)) = target
            && self.player().is_some()
            && backend::is_playable(&uri)
            && self
                .track_prefetch
                .as_ref()
                .map_or(true, |(last, when)| last != &uri || when.elapsed() >= debounce)
        {
            if let Some(player) = self.player() {
                let _ = player.prefetch_track(&uri);
            }
            self.track_prefetch = Some((uri, Instant::now()));
        }
    }

    pub fn prefetch(&mut self, url: Option<&str>, size: Size, round: bool) {
        if !self.prefs.covers {
            return;
        }
        let Some(url) = url else { return };
        self.prefetch.push((url.to_owned(), size, round));
    }

    pub fn auto_image_protocol(&self) -> ImageProtocol {
        self.loader.auto_image_protocol()
    }

    pub fn set_image_protocol(&mut self, protocol: ImageProtocol) {
        if protocol == self.loader.image_protocol() {
            return;
        }
        self.loader.set_image_protocol(protocol);
        for (_, slot) in self.images.drain() {
            if let ImageSlot::Ready(picture) = slot {
                self.terminal_out.extend(picture.terminal_delete());
            }
        }
        self.image_used.clear();
        if let Some(cover) = self.now.cover.take() {
            self.terminal_out.extend(cover.picture.terminal_delete());
            self.load_cover(cover.url);
        }
        if let Some(cover) = self.zen_cover.take() {
            self.terminal_out.extend(cover.picture.terminal_delete());
        }
        self.zen_cover_pending = None;
        self.zen_cover_failed = None;
    }

    pub fn image(&mut self, url: &str, size: Size, round: bool) -> Option<&Picture> {
        if !self.prefs.covers {
            return None;
        }
        let key = image_key(url, size);
        self.image_used.insert(key.clone(), Instant::now());
        match self.images.get(&key) {
            Some(ImageSlot::Loading(drawn)) => drawn.store(self.draws, Ordering::Relaxed),
            Some(ImageSlot::Ready(_)) => {}
            Some(ImageSlot::Failed(at)) if at.elapsed() < IMAGE_RETRY => {}
            None | Some(ImageSlot::Failed(_)) => {
                let drawn = Arc::new(AtomicU64::new(self.draws));
                self.images.insert(key.clone(), ImageSlot::Loading(drawn.clone()));
                self.loader.image(key.clone(), url.to_owned(), size, round, drawn);
            }
        }
        match self.images.get(&key) {
            Some(ImageSlot::Ready(picture)) => Some(picture),
            _ => None,
        }
    }

    pub fn placeholder_image(&mut self, name: &str, size: Size, round: bool) -> Option<&Picture> {
        if !self.prefs.covers {
            return None;
        }
        let key = format!("name-cover:{name}:{}x{}:{round}", size.width, size.height);
        self.image_used.insert(key.clone(), Instant::now());
        match self.images.get(&key) {
            Some(ImageSlot::Loading(drawn)) => drawn.store(self.draws, Ordering::Relaxed),
            Some(ImageSlot::Ready(_)) => {}
            Some(ImageSlot::Failed(at)) if at.elapsed() < IMAGE_RETRY => {}
            None | Some(ImageSlot::Failed(_)) => {
                let drawn = Arc::new(AtomicU64::new(self.draws));
                self.images.insert(key.clone(), ImageSlot::Loading(drawn.clone()));
                self.loader.placeholder(key.clone(), name.to_owned(), size, round, drawn);
            }
        }
        match self.images.get(&key) {
            Some(ImageSlot::Ready(picture)) => Some(picture),
            _ => None,
        }
    }

    fn evict_images(&mut self) {
        self.last_eviction = Instant::now();
        let mut stale = Vec::new();
        let mut held = 0;
        let mut off_screen = Vec::new();
        for (key, used) in &self.image_used {
            let Some(ImageSlot::Ready(picture)) = self.images.get(key) else { continue };
            if !picture.in_terminal() {
                if used.elapsed() >= IMAGE_TTL {
                    stale.push(key.clone());
                }
                continue;
            }
            held += picture.terminal_bytes;
            if *used < self.frame_started {
                off_screen.push((*used, key.clone(), picture.terminal_bytes));
            }
        }
        off_screen.sort_unstable_by_key(|(used, _, _)| *used);
        for (_, key, bytes) in off_screen {
            if held <= TERMINAL_IMAGE_BUDGET {
                break;
            }
            held -= bytes;
            stale.push(key);
        }
        if stale.is_empty() {
            return;
        }
        for key in &stale {
            if let Some(ImageSlot::Ready(picture)) = self.images.remove(key) {
                self.terminal_out.extend(picture.terminal_delete());
            }
            self.image_used.remove(key);
        }
        crate::release_freed_memory();
    }

    fn open_library_view(&mut self) {
        let id = self.next_id();
        let filter = self.library_filter;
        self.push_view(View::Library(LibraryView { id, filter, cursor: 0, columns: 1, top: 0, detached: false }));
        self.focus = Focus::Content;
    }

    pub(crate) fn set_page_filter(&mut self, filter: LibraryFilter) {
        if let View::Library(v) = &mut self.view {
            v.filter = if v.filter == filter { LibraryFilter::All } else { filter };
            v.cursor = 0;
            v.top = 0;
        }
    }

    fn cycle_page_filter(&mut self, delta: isize) {
        let View::Library(v) = &self.view else { return };
        let filters: Vec<_> = std::iter::once(LibraryFilter::All).chain(self.library_filters()).collect();
        let current = filters.iter().position(|f| *f == v.filter).unwrap_or(0) as isize;
        let next = filters[(current + delta).rem_euclid(filters.len() as isize) as usize];
        if let View::Library(v) = &mut self.view {
            v.filter = next;
            v.cursor = 0;
            v.top = 0;
        }
    }

    fn open_search(&mut self) {
        if let View::Search(_) = self.view {
            self.focus = Focus::SearchInput;
            return;
        }
        let id = self.next_id();
        self.push_view(View::Search(SearchView {
            id,
            query: String::new(),
            searched: String::new(),
            entries: Vec::new(),
            loading: false,
            error: None,
            typed_at: None,
            list: ListCursor::default(),
            entry_rows: 1,
        }));
        self.focus = Focus::SearchInput;
    }

    fn open_tracks(&mut self, source: TrackSource) {
        match &source {
            TrackSource::Liked => {}
            TrackSource::Playlist(PlaylistInfo { uri, .. }) | TrackSource::Album(AlbumInfo { uri, .. }) => {
                self.show_service_of(uri)
            }
        }
        if let View::Tracks(v) = &self.view
            && v.source.same(&source)
        {
            self.focus = Focus::Content;
            return;
        }
        let page_uri = match &source {
            TrackSource::Liked => None,
            TrackSource::Playlist(p) => Some(p.uri.clone()),
            TrackSource::Album(a) => Some(a.uri.clone()),
        };
        if let Some(uri) = page_uri.filter(|uri| !self.saved.contains_key(uri)) {
            self.loader.check_saved(vec![uri]);
        }
        let loaded = self.take_loaded_copy(&source).or_else(|| self.cached_copy(&source));
        self.retire_copies(&source);
        let view = match loaded {
            Some(view) if view.loading || view.fetched_at.is_some_and(|at| at.elapsed() < PAGE_FRESH) => view,
            Some(mut view) => {
                view.refresh(self.next_id(), |id, source| self.loader.tracks(id, source));
                view
            }
            None => {
                let mut view = TrackView::new(source);
                view.load(self.next_id(), |id, source| self.loader.tracks(id, source));
                view
            }
        };
        self.push_view(View::Tracks(view));
        self.focus = Focus::Content;
    }

    fn take_loaded_copy(&mut self, source: &TrackSource) -> Option<TrackView> {
        let index = self.history.iter().rposition(
            |view| matches!(view, View::Tracks(v) if v.source.same(source) && !v.stale && v.error.is_none()),
        )?;
        let View::Tracks(v) = &mut self.history[index] else { return None };
        let stand_in = TrackView { list: v.list.clone(), ..TrackView::new(v.source.clone()) };
        let loaded = std::mem::replace(v, stand_in);
        self.next_view_id += 1;
        v.retire(self.next_view_id);
        Some(loaded)
    }

    fn retire_copies(&mut self, source: &TrackSource) {
        for view in &mut self.history {
            if let View::Tracks(v) = view
                && v.source.same(source)
                && !v.stale
            {
                self.next_view_id += 1;
                v.retire(self.next_view_id);
            }
        }
    }

    fn push_view(&mut self, view: View) {
        let previous = std::mem::replace(&mut self.view, view);
        self.history.push(previous);
        if self.history.len() > HISTORY_LIMIT {
            self.history.remove(0);
        }
        let quick = self.last_nav.is_some_and(|at| at.elapsed() < QUICK_NAV);
        self.last_nav = Some(Instant::now());
        if !quick {
            self.animate_content();
        }
    }

    fn back(&mut self) {
        match self.history.pop() {
            Some(view) => {
                self.view = view;
                if let View::Tracks(v) = &mut self.view
                    && v.stale
                {
                    self.next_view_id += 1;
                    v.load(self.next_view_id, |id, source| self.loader.tracks(id, source));
                }
                self.check_queue();
                self.focus = Focus::Content;
                self.last_nav = Some(Instant::now());
                self.effects.cancel(&Fx::Content);
            }
            None => self.focus = Focus::Nav,
        }
    }

    fn animate_content(&mut self) {
        let p = theme::palette();
        let area = self.content_area;
        self.animate(Fx::Content, Effect::fade_in(p.bg, 120, area));
    }

    fn next_id(&mut self) -> u64 {
        self.next_view_id += 1;
        self.next_view_id
    }

    fn view_mut(&mut self, id: u64) -> Option<&mut View> {
        let shown = std::iter::once(&mut self.view).chain(self.history.iter_mut());
        shown.chain(self.parked.values_mut().flat_map(browsing::Browsing::views_mut)).find(|v| v.id() == id)
    }


    fn activate(&mut self) {
        match &self.view {
            View::Home(v) => {
                if let Some(chip) = v.chip {
                    return self.pick_chip(chip);
                }
                if let Some(item) = v.selected().cloned() {
                    if item.is_song() {
                        self.play_item(item);
                    } else {
                        self.open_home_item(item);
                    }
                }
            }
            View::Library(v) => {
                if let Some(entry) = self.library_entries_for(v.filter).get(v.cursor).copied() {
                    self.open_library_entry(entry);
                }
            }
            View::Artist(v) => match v.selected() {
                Some(ArtistSelection::Track(t)) => {
                    let (context, track) = (v.uri.clone(), t.track.clone());
                    self.play(context, Some(track.uri.clone()));
                    self.show_early(&track, None);
                }
                Some(ArtistSelection::Item(item)) => {
                    let item = item.clone();
                    self.open_home_item(item);
                }
                None => {}
            },
            View::Tracks(v) => {
                let Some(track) = v.tracks.get(v.list.selected).cloned() else { return };
                let (context, cover_url) = match &v.source {
                    TrackSource::Liked => match self.liked_context() {
                        Some(context) => (context, None),
                        None => return self.not_connected(),
                    },
                    TrackSource::Playlist(p) => (p.uri.clone(), None),
                    TrackSource::Album(a) => (a.uri.clone(), a.image_url.clone()),
                };
                self.play(context, Some(track.uri.clone()));
                self.show_early(&track, cover_url);
            }
            View::Search(v) => match v.selected().cloned() {
                Some(SearchEntry::Track(t)) if t.album_uri.is_empty() => {
                    self.play(t.uri.clone(), None);
                    self.show_early(&t, t.image_url.clone());
                }
                Some(SearchEntry::Track(t)) => {
                    self.play(t.album_uri.clone(), Some(t.uri.clone()));
                    self.show_early(&t, None);
                }
                Some(SearchEntry::Artist(a)) => self.open_artist(a.uri, a.name),
                Some(SearchEntry::Album(a)) => self.open_tracks(TrackSource::Album(a)),
                Some(SearchEntry::Playlist(p)) => self.open_tracks(TrackSource::Playlist(p)),
                Some(SearchEntry::Header(_)) | None => {}
            },
            View::Settings(v) => {
                let setting = v.setting();
                self.change_setting(setting, true);
            }
            View::Queue(v) => {
                if let Some(row) = v.selected().cloned() {
                    self.play_queue_row(row);
                }
            }
        }
    }

    fn open_home_item(&mut self, item: HomeItem) {
        if backend::is_dj(&item.uri) {
            return self.run(Command::Dj);
        }
        let id = item.uri.rsplit(':').next().unwrap_or_default().to_owned();
        match item.kind {
            ItemKind::Playlist => self.open_tracks(TrackSource::Playlist(PlaylistInfo {
                id,
                uri: item.uri,
                name: item.title,
                owner_id: item.owner_id,
                owner_name: item.owner_name,
                collaborative: false,
                total: None,
                image_url: item.image_url,
            })),
            ItemKind::Album => self.open_tracks(TrackSource::Album(AlbumInfo {
                id,
                uri: item.uri,
                name: item.title,
                artists: vec![item.subtitle],
                year: String::new(),
                total: 0,
                image_url: item.image_url,
            })),
            ItemKind::Artist => self.open_artist(item.uri, item.title),
            _ => self.play_item(item),
        }
    }

    fn open_artist(&mut self, uri: String, name: String) {
        self.show_service_of(&uri);
        if let View::Artist(v) = &self.view
            && v.uri == uri
        {
            self.focus = Focus::Content;
            return;
        }
        let id = self.next_id();
        let loaded = self.history.iter().rev().find_map(|view| match view {
            View::Artist(v) if v.uri == uri && v.page.is_some() => Some(v.clone()),
            _ => None,
        });
        let view = match loaded {
            Some(view) => {
                if !view.fetched_at.is_some_and(|at| at.elapsed() < PAGE_FRESH) {
                    self.loader.artist(id, uri);
                }
                ArtistView { id, ..view }
            }
            None => {
                self.loader.artist(id, uri.clone());
                ArtistView::new(id, uri, name)
            }
        };
        self.push_view(View::Artist(view));
        self.focus = Focus::Content;
    }

    fn pick_chip(&mut self, index: usize) {
        let View::Home(v) = &mut self.view else { return };
        if let Some(chip) = v.pick_chip(index) {
            let id = v.id;
            self.loader.home(id, chip);
        }
    }

    fn play_shelf(&mut self, shelf: usize) {
        let View::Home(v) = &self.view else { return };
        let Some(context) = v.home.as_ref().and_then(|h| h.shelves.get(shelf)).and_then(|s| s.play_all.clone()) else {
            return;
        };
        self.play(context, None);
    }

    fn play_home_item(&mut self) {
        if let View::Home(v) = &self.view
            && let Some(item) = v.selected().cloned()
        {
            self.play_item(item);
        }
    }

    fn play_item(&mut self, item: HomeItem) {
        match item.kind {
            ItemKind::Audiobook => self.error("tunebox can’t play audiobooks yet.".into()),
            ItemKind::Episode => self.play(item.context_uri, Some(item.uri)),
            _ => self.play(item.context_uri, None),
        }
    }

    fn play(&mut self, context: String, track: Option<String>) {
        let before = (self.resume.take(), self.last_context.replace(context.clone()));
        let sent = if self.remote_active && self.plays_here(&context) {
            self.remote(Remote::PlayContext { context, track: track.clone(), shuffle: false });
            true
        } else {
            let uri = track.clone().unwrap_or_else(|| context.clone());
            self.start_on(&uri, |p| p.play(context, track.clone()))
        };
        if sent {
            self.now.start(track);
        } else {
            (self.resume, self.last_context) = before;
        }
    }

    fn idle(&self) -> bool {
        !self.local_active && !self.remote_active && self.now.track.is_some() && self.now.pending.is_none()
    }

    fn play_shown(&mut self) {
        let Some(track) = &self.now.track else { return };
        let (context, uri, position_ms) = (self.last_context.clone(), track.uri.clone(), self.now.position_ms());
        if self.start_on(&uri, |p| p.resume(context, uri.clone(), position_ms)) {
            self.now.start(Some(uri));
        }
    }

    fn play_shuffled(&mut self, context: String) {
        let before = (self.resume.take(), self.last_context.replace(context.clone()));
        let sent = if self.remote_active && self.plays_here(&context) {
            self.remote(Remote::PlayContext { context, track: None, shuffle: true });
            true
        } else {
            self.start_on(&context.clone(), |p| p.play_shuffled(context))
        };
        if sent {
            self.now.start(None);
        } else {
            (self.resume, self.last_context) = before;
        }
    }

    fn skip(&mut self, forward: bool) {
        let restart = !forward && self.now.position_ms() >= RESTART_AFTER_MS;
        let sent = match (self.remote_active, forward) {
            (true, true) => {
                self.remote(Remote::Next);
                true
            }
            (true, false) => {
                if restart {
                    self.remote_intent.seek = Some((0, Instant::now()));
                }
                self.remote(Remote::Previous);
                true
            }
            (false, true) => self.control(Player::next),
            (false, false) => self.control(Player::previous),
        };
        if !sent || !(self.remote_active || self.local_active) {
            return;
        }
        if !restart {
            self.now.skip();
        } else if self.remote_active {
            self.now.set_position(0);
        } else {
            self.now.seek(0);
        }
    }

    fn show_early(&mut self, track: &Track, cover_url: Option<String>) {
        let before = self.now.track.as_ref().map(|t| t.uri.clone());
        self.now.show_early(CurrentTrack {
            uri: track.uri.clone(),
            name: track.name.clone(),
            artists: track.artists.clone(),
            artist_uris: track.artist_uris.clone(),
            album: track.album.clone(),
            album_uri: Some(track.album_uri.clone()).filter(|uri| !uri.is_empty()),
            duration_ms: track.duration_ms,
            cover_url: cover_url.clone(),
            cover_url_large: cover_url.clone(),
        });
        if self.now.track.as_ref().map(|t| &t.uri) == before.as_ref() {
            return;
        }
        if let Some(url) = cover_url {
            self.load_cover(url);
        }
        self.after_track_change(before);
    }

    fn cancel_start(&mut self) {
        let before = self.now.track.as_ref().map(|t| t.uri.clone());
        self.now.cancel_start();
        self.after_track_change(before);
    }

    fn header_button(&mut self, button: HeaderButton) {
        let Some(target) = self.page_target() else { return };
        match button {
            HeaderButton::Play => self.play(target.uri, None),
            HeaderButton::Shuffle => self.play_shuffled(target.uri),
            HeaderButton::Save => self.run_menu(crate::overlay::MenuAction::Save, target),
            HeaderButton::More => {
                let Some(&(area, _)) = self.hits.iter().find(|(_, hit)| *hit == Hit::Header(button)) else { return };
                let items = menu::page_menu_items(&target, self.is_saved(&target.uri));
                let anchor = (area.x, area.bottom());
                self.overlay = Some(Overlay::Menu(crate::overlay::Menu { target, anchor, items, cursor: 0 }));
                self.overlay_fx_pending = true;
            }
        }
    }

    fn remote(&mut self, command: Remote) {
        self.remote_intent.sent();
        self.last_command = Instant::now();
        self.loader.remote(self.active, command);
    }

    fn change_volume(&mut self, steps: i32) {
        let step = u16::MAX as i32 / 20;
        let volume = (self.now.volume as i32 + steps * step).clamp(0, u16::MAX as i32) as u16;
        self.set_volume(volume);
    }

    fn set_volume(&mut self, volume: u16) {
        self.now.volume = volume;
        if self.remote_active {
            self.remote_intent.volume = Some(volume);
            self.remote(Remote::Volume((volume as u32 * 100 / u16::MAX as u32) as u8));
            return;
        }
        self.restored_volume = Some(volume);
        if self.local_active {
            if let Some(volume) = self.volume_sync.request(volume) {
                self.control(|p| p.set_volume(volume));
            }
        } else {
            self.volume_sync.defer(volume);
        }
    }

    fn seek_by(&mut self, delta_ms: i64) {
        let Some(track) = &self.now.track else { return };
        let target = (self.now.position_ms() as i64 + delta_ms).clamp(0, track.duration_ms as i64);
        self.seek_to(target as u32);
    }

    fn seek_to(&mut self, position_ms: u32) {
        let Some(track) = &self.now.track else { return };
        let position_ms = if track.duration_ms > 0 { position_ms.min(track.duration_ms) } else { position_ms };
        if self.remote_active {
            self.now.set_position(position_ms);
            self.remote_intent.seek = Some((position_ms, Instant::now()));
            self.remote(Remote::Seek(position_ms));
        } else if self.local_active {
            self.now.seek(position_ms);
            self.control(|p| p.seek(position_ms));
        } else {
            self.now.set_position(position_ms);
            if let Some(last) = &mut self.resume {
                last.position_ms = position_ms;
            }
        }
    }

    fn open_dj(&mut self) {
        let Some(track) = &self.now.track else { return };
        self.loader.dj_suggestions(track.uri.clone());
        self.status = None;
        let picker = Picker::free_text(dj_items("", &[]));
        self.open_overlay(Overlay::Dj { picker, suggestions: Vec::new(), loading: true });
    }

    fn choose_dj(&mut self, choice: DjChoice) {
        let Some(Overlay::Dj { picker, suggestions, .. }) = self.overlay.take() else { return };
        match choice {
            DjChoice::Jump => {
                if self.control(Player::next_segment) {
                    self.notice("Getting different DJ picks…".into());
                    self.now.start(None);
                }
            }
            DjChoice::Ask => self.ask_dj(picker.query.trim().to_owned(), false),
            DjChoice::Suggestion(index) => {
                if let Some(prompt) = suggestions.into_iter().nth(index) {
                    self.ask_dj(prompt, true);
                }
            }
        }
    }

    fn ask_dj(&mut self, prompt: String, suggested: bool) {
        let Some(player) = self.player() else { return self.not_connected() };
        match player.ask_dj(prompt, suggested) {
            Ok(answer) => {
                self.loader.ask_dj(answer);
                self.notice("Asking DJ…".into());
                self.now.start(None);
            }
            Err(err) => self.error(format!("DJ: {err:#}")),
        }
    }

    fn open_devices(&mut self, devices: Vec<Device>, refreshing: bool) {
        if devices.is_empty() && !refreshing {
            return self.error(format!("No {} devices found.", self.service_name()));
        }
        let items = device_items(&devices, self.device_id.as_deref());
        self.status = None;
        self.open_overlay(Overlay::Devices { devices, picker: Picker::new(items), refreshing });
    }

    fn refresh_devices(&mut self, devices: Vec<Device>) {
        if devices.is_empty() {
            self.overlay = None;
            return self.error(format!("No {} devices found.", self.service_name()));
        }
        let items = device_items(&devices, self.device_id.as_deref());
        let Some(Overlay::Devices { devices: shown, picker, refreshing }) = &mut self.overlay else { return };
        let selected = picker.selected().and_then(|&index| shown.get(index)).map(|d| d.id.clone());
        picker.set_items(items, |&index| devices.get(index).map(|d| &d.id) == selected.as_ref());
        *shown = devices;
        *refreshing = false;
    }

    fn transfer_to(&mut self, index: usize) {
        let Some(Overlay::Devices { devices, .. }) = self.overlay.take() else { return };
        let Some(device) = devices.into_iter().nth(index) else { return };
        self.transfer =
            Some(Transfer { device_id: device.id.clone(), name: device.name.clone(), since: Instant::now() });
        self.loader.transfer(self.service(), device);
        self.last_command = Instant::now();
    }

    pub fn transferring_to(&self) -> Option<&str> {
        self.transfer.as_ref().map(|t| t.name.as_str())
    }

    fn control(&mut self, command: impl FnOnce(&Player) -> anyhow::Result<()>) -> bool {
        let Some(player) = self.player() else {
            self.not_connected();
            return false;
        };
        if let Err(err) = command(player) {
            self.error(format!("{err:#}"));
            return false;
        }
        true
    }

    fn start_on(&mut self, uri: &str, command: impl FnOnce(&Player) -> anyhow::Result<()>) -> bool {
        match Service::of(uri) {
            Some(service) if self.players.contains_key(&service) => self.take_over(service),
            Some(service) if !self.is_active(service) => {
                self.not_connected_to(Some(service));
                return false;
            }
            _ => {}
        }
        self.control(command)
    }

    fn take_over(&mut self, service: Service) {
        let previous = self.active.replace(service);
        self.device_id = self.players.get(&service).and_then(Player::device_id);
        let Some(previous) = previous.filter(|previous| *previous != service) else { return };
        if self.remote_active {
            self.loader.remote(Some(previous), Remote::Pause);
        } else if let Some(player) = self.players.get(&previous) {
            let _ = player.pause();
        }
        self.local_active = false;
        self.remote_active = false;
        self.active_device = None;
        self.now.device = None;
        self.volume_sync = VolumeSync::default();
        self.volume_sync.defer(self.now.volume);
        self.set_queue(Queue::default());
    }

    fn plays_here(&self, uri: &str) -> bool {
        Service::of(uri).is_none_or(|service| self.is_active(service))
    }

    fn liked_context(&self) -> Option<String> {
        let service = self.loader.service()?;
        let user = self.players.get(&service).and_then(Player::user);
        backend::liked_context(service, user.as_deref())
    }

    fn not_connected(&mut self) {
        self.not_connected_to(self.service());
    }

    fn not_connected_to(&mut self, service: Option<Service>) {
        let error =
            service.and_then(|service| self.problem_of(service).or_else(|| self.player_errors.get(&service).cloned()));
        let text = match error {
            Some(err) => format!("Playback unavailable: {err}"),
            None if self.demo => "This is the demo: nothing plays here".into(),
            None => format!("Still connecting to {}…", service.map_or("your music service", Service::name)),
        };
        self.error(text);
    }

    pub(super) fn notice(&mut self, text: String) {
        self.status = Some(Status { text, error: false, since: Instant::now() });
    }

    pub(crate) fn error(&mut self, text: String) {
        if let Some(command) = sign_in_command(&text) {
            self.pinned = Some(Pinned { text, command: Some(command), copied: false, cause: Cause::SignIn, service: self.service() });
            return;
        }
        self.status = Some(Status { text, error: true, since: Instant::now() });
    }

    fn unpin(&mut self, cause: Cause) {
        if self.pinned.as_ref().is_some_and(|p| p.cause == cause) {
            self.pinned = None;
        }
    }

    fn on_pinned(&mut self, close: bool) {
        if close {
            self.pinned = None;
        } else if let Some(pinned) = &mut self.pinned
            && let Some(command) = &pinned.command
        {
            menu::copy_to_clipboard(command);
            pinned.copied = true;
        }
    }
}

fn sign_in_command(text: &str) -> Option<String> {
    let start = text.find("`tunebox login")? + 1;
    let end = start + text[start..].find('`')?;
    Some(text[start..end].to_owned())
}

fn image_key(url: &str, size: Size) -> String {
    format!("{}x{}:{url}", size.width, size.height)
}

fn device_items(devices: &[Device], own: Option<&str>) -> Vec<PickItem<usize>> {
    devices
        .iter()
        .enumerate()
        .map(|(index, device)| {
            let icon = crate::icons::device(&device.kind);
            let this = if Some(device.id.as_str()) == own { " (this device)" } else { "" };
            let state = if device.active { "● playing" } else { device.kind.as_str() };
            PickItem {
                label: format!("{icon}  {}{this}", device.name),
                detail: state.to_owned(),
                image_url: None,
                value: index,
            }
        })
        .collect()
}

#[derive(Debug, Clone, Copy)]
enum Choice {
    Palette(PaletteAction),
    Playlist(usize),
    Device(usize),
    Dj(DjChoice),
}
