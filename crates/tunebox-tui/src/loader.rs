
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result};
use futures::{StreamExt, stream};
use image::imageops::FilterType;
use ratatui::layout::Size;
use ratatui_image::{
    Resize,
    picker::{Picker, ProtocolType},
    protocol::Protocol,
};
use sha2::{Digest, Sha256};
use tokio::sync::{Semaphore, mpsc::UnboundedSender, watch};
use tunebox_core::{
    artist::ArtistPage,
    catalog::{AlbumInfo, ArtistInfo, Page, PlaylistInfo, SearchResults, ShowInfo, Track, User},
    connect::{Device, Playback},
    home::Home,
    lyrics::{self, Lyrics, LyricsQuery},
    paths::Paths,
    player::CurrentTrack,
    update::{self, Release},
};

use crate::{
    app::AppEvent,
    art,
    backend::{self, Catalog, Listed, Remote, Service},
    demo::Demo,
    graphics::{ImageProtocol, ImageTerminal, Inline},
    queue::Upcoming,
    theme::{self, Rgb},
    views::TrackSource,
};

const IMAGE_CACHE_AGE: Duration = Duration::from_secs(60 * 24 * 60 * 60);

pub struct Fetch(tokio::task::AbortHandle);

impl Drop for Fetch {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct Pending(Arc<AtomicUsize>);

impl Pending {
    fn new(count: &Arc<AtomicUsize>) -> Self {
        count.fetch_add(1, Ordering::SeqCst);
        Self(count.clone())
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

pub struct Picture {
    pub graphic: Graphic,
    pub source: Option<Arc<image::DynamicImage>>,
    pub terminal_bytes: usize,
}

pub enum Graphic {
    Cells(Protocol),
    Inline(Arc<Inline>),
}

pub enum DataEvent {
    For(Service, Box<DataEvent>),
    Me(Result<User, String>),
    Home {
        view: u64,
        chip: Option<String>,
        result: Result<Home, String>,
    },
    Artist {
        view: u64,
        result: Result<ArtistPage, String>,
    },
    Image {
        key: String,
        result: Result<Picture, String>,
    },
    ImageSkipped {
        key: String,
    },
    Playlists(Result<Vec<PlaylistInfo>, String>),
    LikedCount(Result<u32, String>),
    Albums(Result<Vec<AlbumInfo>, String>),
    Artists(Result<Vec<ArtistInfo>, String>),
    Shows(Result<Vec<ShowInfo>, String>),
    Tracks {
        view: u64,
        page: Result<Page<Track>, String>,
    },
    Search {
        view: u64,
        query: String,
        result: Result<SearchResults, String>,
    },
    Cover {
        url: String,
        result: Result<(Picture, Option<Rgb>), String>,
    },
    LargeCover {
        url: String,
        size: Size,
        result: Result<Picture, String>,
    },
    Artwork {
        url: String,
        result: Result<Arc<[u8]>, String>,
    },
    Lyrics {
        uri: String,
        result: Result<Option<Lyrics>, String>,
    },
    Saved(Vec<(String, bool)>),
    SaveDone {
        uri: String,
        notice: String,
    },
    SaveFailed {
        uri: String,
        saved: bool,
        error: String,
    },
    AddFailed {
        playlist_id: String,
        count: u32,
        error: String,
    },
    Playback(Result<Option<Playback>, String>),
    RemoteDone {
        commands: usize,
        result: Result<(), String>,
    },
    Devices(Result<Vec<Device>, String>),
    Transferred {
        name: String,
        result: Result<(), String>,
    },
    QueueTracks {
        asked: Vec<String>,
        tracks: Vec<Track>,
        reached: bool,
    },
    Upcoming {
        playing: String,
        result: Result<Vec<Track>, String>,
    },
    SkippedTo(Result<(), String>),
    DjSuggestions(Result<Vec<String>, String>),
    DjAsked(Result<(), String>),
    UpdateAvailable(Release),
    UpdateProgress(u8),
    UpdateInstalled(PathBuf),
    UpdateFailed(String),
    Notice(String),
    Failed(String),
}

#[derive(Clone)]
pub struct Loader {
    services: Arc<[Service]>,
    catalogs: Arc<[Catalog]>,
    problems: Arc<[(Service, String)]>,
    service: Option<Service>,
    tx: UnboundedSender<AppEvent>,
    http: reqwest::Client,
    image_terminal: Arc<ImageTerminal>,
    picker: Arc<watch::Sender<(ImageProtocol, Picker)>>,
    keep_sources: bool,
    image_cache: PathBuf,
    encode_turns: Arc<Semaphore>,
    prefetch_turns: Arc<Semaphore>,
    frames_drawn: Arc<AtomicU64>,
    demo: Option<Arc<Demo>>,
    pending: Arc<AtomicUsize>,
}

enum ImageSource {
    Downloaded(Vec<u8>),
    Drawn(Arc<dyn Fn() -> image::DynamicImage + Send + Sync>),
}

impl Loader {
    pub fn new(
        started: Vec<(Service, Result<Catalog, String>)>,
        service: Option<Service>,
        tx: UnboundedSender<AppEvent>,
        image_terminal: ImageTerminal,
        image_cache: PathBuf,
        keep_sources: bool,
    ) -> Self {
        let http =
            tunebox_core::http::client(concat!("tunebox/", env!("CARGO_PKG_VERSION"))).build().unwrap_or_default();
        let turns = std::thread::available_parallelism().map_or(2, |n| n.get().saturating_sub(2).clamp(1, 4));
        let picker = image_terminal.picker(ImageProtocol::Auto);
        let services = started.iter().map(|(service, _)| *service).collect();
        let (catalogs, problems): (Vec<_>, Vec<_>) = started.into_iter().partition(|(_, started)| started.is_ok());
        Self {
            services,
            catalogs: catalogs.into_iter().filter_map(|(_, started)| started.ok()).collect(),
            problems: problems.into_iter().filter_map(|(service, started)| Some((service, started.err()?))).collect(),
            service,
            tx,
            http,
            image_terminal: Arc::new(image_terminal),
            picker: Arc::new(watch::Sender::new((ImageProtocol::Auto, picker))),
            keep_sources,
            image_cache,
            encode_turns: Arc::new(Semaphore::new(turns)),
            prefetch_turns: Arc::new(Semaphore::new(2)),
            frames_drawn: Arc::default(),
            demo: None,
            pending: Arc::default(),
        }
    }

    #[cfg(test)]
    pub fn for_tests(services: &[Service], problems: &[(Service, &str)], tx: UnboundedSender<AppEvent>) -> Self {
        let images = std::env::temp_dir().join("tunebox-test-images");
        let mut loader =
            Self::new(Vec::new(), services.first().copied(), tx, ImageTerminal::halfblocks(), images, false);
        loader.services = services.into();
        loader.problems = problems.iter().map(|(service, problem)| (*service, problem.to_string())).collect();
        loader
    }

    pub fn with_demo(mut self, demo: Option<Arc<Demo>>) -> Self {
        self.demo = demo;
        self
    }

    pub fn image_protocol(&self) -> ImageProtocol {
        self.picker.borrow().0
    }

    pub fn auto_image_protocol(&self) -> ImageProtocol {
        self.image_terminal.auto()
    }

    pub fn set_image_protocol(&self, protocol: ImageProtocol) {
        self.picker.send_replace((protocol, self.image_terminal.picker(protocol)));
    }

    pub fn image_kind(&self) -> ProtocolType {
        self.picker.borrow().1.protocol_type()
    }

    fn demo(&self) -> Option<&Demo> {
        self.demo.as_deref()
    }

    pub fn service(&self) -> Option<Service> {
        self.service
    }

    pub fn services(&self) -> Vec<Service> {
        self.services.to_vec()
    }

    pub fn problem(&self, service: Service) -> Option<&str> {
        self.problems.iter().find(|(s, _)| *s == service).map(|(_, problem)| problem.as_str())
    }

    pub fn set_service(&mut self, service: Option<Service>) {
        self.service = service;
    }

    pub fn signed_in(&self) -> bool {
        self.demo.is_some() || self.shown().is_ok_and(|catalog| catalog.signed_in())
    }

    fn catalog(&self, service: Option<Service>) -> Result<Catalog> {
        let service = service.context("No music service to ask")?;
        if let Some(problem) = self.problem(service) {
            anyhow::bail!("{problem}");
        }
        let catalog = self.catalogs.iter().find(|catalog| catalog.service() == service);
        catalog.cloned().with_context(|| format!("{} isn't enabled", service.name()))
    }

    fn catalog_of(&self, uri: &str) -> Result<Catalog> {
        self.catalog(Service::of(uri))
    }

    fn shown(&self) -> Result<Catalog> {
        self.catalog(self.service)
    }

    pub fn frame_drawn(&self, number: u64) {
        self.frames_drawn.store(number, Ordering::Relaxed);
    }

    fn send(&self, event: DataEvent) {
        let _ = self.tx.send(AppEvent::Data(event));
    }

    fn send_for(&self, event: DataEvent) {
        match self.service {
            Some(service) => self.send(DataEvent::For(service, Box::new(event))),
            None => self.send(event),
        }
    }

    fn spawn<F>(&self, task: impl FnOnce(Self) -> F + Send + 'static)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        tokio::spawn(self.counted(task(self.clone())));
    }

    fn spawn_fetch<F>(&self, task: impl FnOnce(Self) -> F + Send + 'static) -> Fetch
    where
        F: Future<Output = ()> + Send + 'static,
    {
        Fetch(tokio::spawn(self.counted(task(self.clone()))).abort_handle())
    }

    fn counted<F: Future<Output = ()>>(&self, task: F) -> impl Future<Output = ()> + use<F> {
        let pending = Pending::new(&self.pending);
        async move {
            let _pending = pending;
            task.await;
        }
    }

    #[cfg(test)]
    pub fn is_idle(&self) -> bool {
        self.pending.load(Ordering::SeqCst) == 0
    }

    pub fn me(&self) {
        if let Some(demo) = self.demo() {
            return self.send_for(DataEvent::Me(Ok(demo.user.clone())));
        }
        self.spawn(|this| async move {
            let result = async { this.shown()?.me().await }.await;
            this.send_for(DataEvent::Me(result.map_err(|e| format!("{e:#}"))));
        });
    }

    pub fn check_update(&self, paths: Paths, asked: bool) {
        self.spawn(move |this| async move {
            match update::check(&paths, asked).await {
                Ok(Some(release)) => this.send(DataEvent::UpdateAvailable(release)),
                Ok(None) if asked => {
                    this.send(DataEvent::Notice(format!("tunebox {} is the latest version", update::current_version())))
                }
                Ok(None) => {}
                Err(err) if asked => this.send(DataEvent::Failed(format!("Couldn't look for updates: {err:#}"))),
                Err(err) => tracing::warn!("Update check failed: {err:#}"),
            }
        });
    }

    pub fn install_update(&self, release: Release) {
        let Some(variant) = backend::VARIANT else {
            return self.send(DataEvent::UpdateFailed("This tunebox has no release to update to".into()));
        };
        self.spawn(move |this| async move {
            let tx = this.tx.clone();
            let mut shown = None;
            let result = update::install(&release, variant, |done, total| {
                let percent = total.filter(|total| *total > 0).map(|total| (done * 100 / total) as u8);
                if percent != shown {
                    shown = percent;
                    if let Some(percent) = percent {
                        let _ = tx.send(AppEvent::Data(DataEvent::UpdateProgress(percent)));
                    }
                }
            })
            .await;
            this.send(match result {
                Ok(binary) => DataEvent::UpdateInstalled(binary),
                Err(err) => DataEvent::UpdateFailed(format!("{err:#}")),
            });
        });
    }

    pub fn home(&self, view: u64, chip: Option<String>) {
        if let Some(demo) = self.demo() {
            return self.send_for(DataEvent::Home { view, chip, result: Ok(demo.home.clone()) });
        }
        self.spawn(move |this| async move {
            let result = async { this.shown()?.home(chip.as_deref()).await }.await;
            this.send_for(DataEvent::Home { view, chip, result: result.map_err(|e| format!("{e:#}")) });
        });
    }

    pub fn artist(&self, view: u64, uri: String) {
        if let Some(demo) = self.demo() {
            let result = demo.artist_pages.get(&uri).cloned().ok_or_else(|| "This artist isn't in the demo".to_owned());
            return self.send(DataEvent::Artist { view, result });
        }
        self.spawn(move |this| async move {
            let result = async { this.catalog_of(&uri)?.artist(&uri).await }.await;
            this.send(DataEvent::Artist { view, result: result.map_err(|e| format!("{e:#}")) });
        });
    }

    pub fn image(&self, key: String, url: String, size: Size, round: bool, drawn: Arc<AtomicU64>) {
        self.spawn(move |this| async move {
            let on_screen = || drawn.load(Ordering::Relaxed) >= this.frames_drawn.load(Ordering::Relaxed);
            let result = async {
                let source = this.fetch_image(&url).await?;
                if !on_screen() {
                    return Ok(None);
                }
                let _turn = this.encode_turns.acquire().await?;
                if !on_screen() {
                    return Ok(None);
                }
                this.encode_image(source, size, mask(round)).await.map(|(picture, ())| Some(picture))
            }
            .await;
            this.send(match result {
                Ok(Some(picture)) => DataEvent::Image { key, result: Ok(picture) },
                Ok(None) => DataEvent::ImageSkipped { key },
                Err(err) => DataEvent::Image { key, result: Err(format!("{err:#}")) },
            });
        });
    }

    pub fn placeholder(&self, key: String, name: String, size: Size, round: bool, drawn: Arc<AtomicU64>) {
        self.spawn(move |this| async move {
            let result = async {
                let _turn = this.encode_turns.acquire().await?;
                if drawn.load(Ordering::Relaxed) < this.frames_drawn.load(Ordering::Relaxed) {
                    return Ok(None);
                }
                let draw: Arc<dyn Fn() -> image::DynamicImage + Send + Sync> = Arc::new(move || art::name_cover(&name));
                this.encode_image(ImageSource::Drawn(draw), size, mask(round)).await.map(|(picture, ())| Some(picture))
            }
            .await;
            this.send(match result {
                Ok(Some(picture)) => DataEvent::Image { key, result: Ok(picture) },
                Ok(None) => DataEvent::ImageSkipped { key },
                Err(err) => DataEvent::Image { key, result: Err(format!("{err:#}")) },
            });
        });
    }

    pub fn prefetch_image(&self, key: String, url: String, size: Size, round: bool) {
        self.spawn(move |this| async move {
            let result = async {
                let _turn = this.prefetch_turns.acquire().await?;
                let source = this.fetch_image(&url).await?;
                this.encode_image(source, size, mask(round)).await.map(|(picture, ())| picture)
            }
            .await;
            this.send(DataEvent::Image { key, result: result.map_err(|e| format!("{e:#}")) });
        });
    }

    pub fn library(&self) {
        self.liked_count();
        self.albums();
        self.artists();
        self.shows();
    }

    pub fn liked_count(&self) {
        if let Some(demo) = self.demo() {
            return self.send_for(DataEvent::LikedCount(Ok(demo.liked.len() as u32)));
        }
        self.spawn(|this| async move {
            let result = async { this.shown()?.liked_count().await }.await.map_err(|e| format!("{e:#}"));
            this.send_for(DataEvent::LikedCount(result));
        });
    }

    pub fn albums(&self) {
        if let Some(demo) = self.demo() {
            return self.send_for(DataEvent::Albums(Ok(demo.albums.clone())));
        }
        self.spawn(|this| async move {
            let result = async { this.shown()?.saved_albums().await }.await.map_err(|e| format!("{e:#}"));
            this.send_for(DataEvent::Albums(result));
        });
    }

    pub fn artists(&self) {
        if let Some(demo) = self.demo() {
            return self.send_for(DataEvent::Artists(Ok(demo.artists.clone())));
        }
        self.spawn(|this| async move {
            let result = async { this.shown()?.followed_artists().await }.await.map_err(|e| format!("{e:#}"));
            this.send_for(DataEvent::Artists(result));
        });
    }

    pub fn shows(&self) {
        if let Some(demo) = self.demo() {
            return self.send_for(DataEvent::Shows(Ok(demo.shows.clone())));
        }
        self.spawn(|this| async move {
            let result = async { this.shown()?.saved_shows().await }.await.map_err(|e| format!("{e:#}"));
            this.send_for(DataEvent::Shows(result));
        });
    }

    pub fn playlists(&self) {
        if let Some(demo) = self.demo() {
            return self.send_for(DataEvent::Playlists(Ok(demo.playlists.clone())));
        }
        self.spawn(|this| async move {
            let result = async { this.shown()?.playlists().await }.await.map_err(|e| format!("{e:#}"));
            this.send_for(DataEvent::Playlists(result));
        });
    }

    pub fn tracks(&self, view: u64, source: TrackSource) -> Fetch {
        if let Some(demo) = self.demo() {
            let items = match &source {
                TrackSource::Liked => demo.liked.clone(),
                TrackSource::Playlist(playlist) => demo.tracks(&playlist.uri),
                TrackSource::Album(album) => demo.tracks(&album.uri),
            };
            let total = items.len() as u32;
            self.send(DataEvent::Tracks { view, page: Ok(Page { items, total, next_offset: None }) });
            return self.spawn_fetch(|_| async {});
        }
        self.spawn_fetch(move |this| async move {
            let service = match &source {
                TrackSource::Liked => this.service,
                TrackSource::Playlist(p) => Service::of(&p.uri),
                TrackSource::Album(a) => Service::of(&a.uri),
            };
            let mut pages = match this.catalog(service) {
                Ok(catalog) => catalog.tracks(source),
                Err(err) => stream::iter([Err(err)]).boxed(),
            };
            while let Some(page) = pages.next().await {
                let failed = page.is_err();
                this.send(DataEvent::Tracks { view, page: page.map_err(|e| format!("{e:#}")) });
                if failed {
                    return;
                }
            }
        })
    }

    pub fn search(&self, view: u64, query: String) {
        if let Some(demo) = self.demo() {
            let result = Ok(demo.search(&query));
            return self.send(DataEvent::Search { view, query, result });
        }
        self.spawn(move |this| async move {
            let result = async { this.shown()?.search(&query).await }.await.map_err(|e| format!("{e:#}"));
            this.send(DataEvent::Search { view, query, result });
        });
    }

    pub fn cover(&self, url: String, size: Size) {
        self.spawn(move |this| async move {
            let result = this.load_image(&url, size, |image| theme::dominant_color(image)).await;
            this.send(DataEvent::Cover { url, result: result.map_err(|e| format!("{e:#}")) });
        });
    }

    pub fn artwork(&self, url: String) {
        self.spawn(move |this| async move {
            let result = this.download_image(&url).await.map(Arc::from).map_err(|e| format!("{e:#}"));
            this.send(DataEvent::Artwork { url, result });
        });
    }

    pub fn large_cover(&self, url: String, size: Size) {
        self.spawn(move |this| async move {
            let result = this.load_image(&url, size, |_| ()).await.map(|(picture, ())| picture);
            this.send(DataEvent::LargeCover { url, size, result: result.map_err(|e| format!("{e:#}")) });
        });
    }

    async fn load_image<T: Send + 'static>(
        &self,
        url: &str,
        size: Size,
        process: impl FnOnce(&mut image::DynamicImage) -> T + Send + 'static,
    ) -> Result<(Picture, T)> {
        let source = self.fetch_image(url).await?;
        self.encode_image(source, size, process).await
    }

    async fn fetch_image(&self, url: &str) -> Result<ImageSource> {
        Ok(match url {
            art::LIKED_COVER => ImageSource::Drawn(Arc::new(art::liked_cover)),
            art::PLACEHOLDER_COVER => ImageSource::Drawn(Arc::new(art::placeholder_cover)),
            _ if cfg!(test) => ImageSource::Drawn(Arc::new(art::placeholder_cover)),
            _ => ImageSource::Downloaded(self.download_image(url).await?),
        })
    }

    async fn encode_image<T: Send + 'static>(
        &self,
        source: ImageSource,
        size: Size,
        process: impl FnOnce(&mut image::DynamicImage) -> T + Send + 'static,
    ) -> Result<(Picture, T)> {
        let picker = self.picker.borrow().1.clone();
        let keep_source = self.keep_sources;
        tokio::task::spawn_blocking(move || {
            let mut image = match source {
                ImageSource::Downloaded(bytes) => image::load_from_memory(&bytes)?,
                ImageSource::Drawn(draw) => draw(),
            };
            let (width, height) = (image.width(), image.height());
            if width != height {
                let side = width.min(height);
                image = image.crop_imm((width - side) / 2, (height - side) / 2, side, side);
            }
            if featureless(&image) {
                return Err(anyhow::anyhow!("featureless cover"));
            }
            let extra = process(&mut image);
            let _ = keep_source;
            let source = Some(Arc::new(image.clone()));
            let (graphic, terminal_bytes) = match picker.protocol_type() {
                ProtocolType::Kitty => {
                    let font = picker.font_size();
                    let width = u32::from(size.width) * u32::from(font.width);
                    let height = u32::from(size.height) * u32::from(font.height);
                    image = image.resize_exact(width, height, FilterType::Triangle);
                    let terminal_bytes = image.width() as usize * image.height() as usize * 4;
                    (Graphic::Cells(picker.new_protocol_cells(image, size)?), terminal_bytes)
                }
                kind @ (ProtocolType::Sixel | ProtocolType::Iterm2) => {
                    let inline = Inline::new(kind, &image, size, picker.font_size(), picker.tmux_detected())?;
                    (Graphic::Inline(Arc::new(inline)), 0)
                }
                ProtocolType::Halfblocks => {
                    (Graphic::Cells(picker.new_protocol(image, size, Resize::Scale(Some(FilterType::Triangle)))?), 0)
                }
            };
            Ok((Picture { graphic, source, terminal_bytes }, extra))
        })
        .await?
    }

    async fn download_image(&self, url: &str) -> Result<Vec<u8>> {
        if cfg!(test) {
            anyhow::bail!("Tests stay offline: {url}");
        }
        let path = self.image_cache.join(cache_name(url));
        if let Ok(bytes) = tokio::fs::read(&path).await {
            return Ok(bytes);
        }
        let bytes = self.http.get(url).send().await?.error_for_status()?.bytes().await?.to_vec();
        let _ = tokio::fs::create_dir_all(&self.image_cache).await;
        let _ = tokio::fs::write(&path, &bytes).await;
        Ok(bytes)
    }

    pub fn prune_image_cache(&self) {
        let dir = self.image_cache.clone();
        tokio::task::spawn_blocking(move || {
            let Ok(entries) = std::fs::read_dir(&dir) else { return };
            for entry in entries.flatten() {
                if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
                    continue;
                }
                let current = entry.file_name().to_str().is_some_and(|name| {
                    name.len() == 64 && name.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                });
                let age = entry.metadata().ok().and_then(|meta| {
                    let modified = meta.modified().ok()?;
                    meta.accessed().map_or(modified, |accessed| accessed.max(modified)).elapsed().ok()
                });
                if !current || age.is_some_and(|age| age > IMAGE_CACHE_AGE) {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        });
    }

    pub fn lyrics(&self, track: &CurrentTrack) {
        if let Some(demo) = self.demo() {
            let result = Ok(demo.lyrics.get(&track.uri).cloned());
            return self.send(DataEvent::Lyrics { uri: track.uri.clone(), result });
        }
        let uri = track.uri.clone();
        let title = track.name.clone();
        let artist = track.artists.first().cloned().unwrap_or_default();
        let album = track.album.clone();
        let duration_ms = track.duration_ms;
        self.spawn(move |this| async move {
            let query = LyricsQuery { uri: &uri, title: &title, artist: &artist, album: &album, duration_ms };
            let result = match this.catalog_of(&uri) {
                Ok(catalog) => catalog.lyrics(query).await,
                Err(_) => lyrics::lrclib(&this.http, &query).await,
            };
            this.send(DataEvent::Lyrics { uri, result: result.map_err(|e| format!("{e:#}")) });
        });
    }

    pub fn check_saved(&self, uris: Vec<String>) {
        if uris.is_empty() {
            return;
        }
        if let Some(demo) = self.demo() {
            let flags = uris.into_iter().map(|uri| {
                let saved = demo.is_saved(&uri);
                (uri, saved)
            });
            return self.send(DataEvent::Saved(flags.collect()));
        }
        self.spawn(move |this| async move {
            for (service, uris) in by_service(uris) {
                let Ok(catalog) = this.catalog(Some(service)) else { continue };
                let mut checks = catalog.check_saved(uris);
                while let Some(result) = checks.next().await {
                    match result {
                        Ok(flags) => this.send(DataEvent::Saved(flags)),
                        Err(err) => tracing::warn!("library check failed: {err:#}"),
                    }
                }
            }
        });
    }

    pub fn set_saved(&self, uri: String, name: String, saved: bool) {
        if self.demo.is_some() {
            let notice = if saved { format!("“{name}” saved") } else { format!("“{name}” removed") };
            return self.send(DataEvent::SaveDone { uri, notice });
        }
        self.spawn(move |this| async move {
            let result = async { this.catalog_of(&uri)?.set_saved(&uri, saved).await }.await;
            match result {
                Ok(()) => {
                    let notice = if saved { format!("“{name}” saved") } else { format!("“{name}” removed") };
                    this.send(DataEvent::SaveDone { uri, notice });
                }
                Err(err) => this.send(DataEvent::SaveFailed { uri, saved: !saved, error: format!("{err:#}") }),
            }
        });
    }

    pub fn playback(&self, service: Option<Service>) {
        if self.demo.is_some() {
            return self.send(DataEvent::Playback(Ok(None)));
        }
        self.spawn(move |this| async move {
            let result = async { this.catalog(service)?.playback().await }.await.map_err(|e| format!("{e:#}"));
            this.send(DataEvent::Playback(result));
        });
    }

    pub fn devices(&self, service: Option<Service>) {
        if self.demo.is_some() {
            return self.send(DataEvent::Devices(Ok(Vec::new())));
        }
        self.spawn(move |this| async move {
            let result = async { this.catalog(service)?.devices().await }.await.map_err(|e| format!("{e:#}"));
            this.send(DataEvent::Devices(result));
        });
    }

    pub fn transfer(&self, service: Option<Service>, device: Device) {
        if self.demo.is_some() {
            return;
        }
        self.spawn(move |this| async move {
            let result = async { this.catalog(service)?.transfer(&device.id).await }.await;
            this.send(DataEvent::Transferred { name: device.name, result: result.map_err(|e| format!("{e:#}")) });
        });
    }

    pub fn dj_suggestions(&self, track_uri: String) {
        if self.demo.is_some() {
            return self.send(DataEvent::DjSuggestions(Ok(Vec::new())));
        }
        self.spawn(move |this| async move {
            let suggestions = async { this.catalog_of(&track_uri)?.dj_suggestions(&track_uri).await };
            let result = suggestions.await.map_err(|e| format!("{e:#}"));
            this.send(DataEvent::DjSuggestions(result));
        });
    }

    pub fn ask_dj(&self, answer: impl Future<Output = anyhow::Result<()>> + Send + 'static) {
        self.spawn(move |this| async move {
            let result = answer.await.map_err(|e| format!("{e:#}"));
            this.send(DataEvent::DjAsked(result));
        });
    }

    pub fn remote(&self, service: Option<Service>, command: Remote) {
        if self.demo.is_some() {
            return;
        }
        if let Ok(catalog) = self.catalog(service) {
            catalog.remote(command);
        }
    }

    pub fn queue(&self, uri: String) {
        if self.demo.is_some() {
            return;
        }
        self.spawn(move |this| async move {
            if let Err(err) = async { this.catalog_of(&uri)?.add_to_queue(&uri).await }.await {
                this.send(DataEvent::Failed(format!("Queue: {err:#}")));
            }
        });
    }

    pub fn queue_tracks(&self, uris: Vec<String>) {
        if let Some(demo) = self.demo() {
            let tracks = uris.iter().filter_map(|uri| demo.track(uri)).cloned().collect();
            return self.send(DataEvent::QueueTracks { asked: uris, tracks, reached: true });
        }
        self.spawn(move |this| async move {
            let (known, unknown): (Vec<_>, Vec<_>) =
                uris.into_iter().partition(|uri| Service::of(uri).is_some_and(|s| this.catalog(Some(s)).is_ok()));
            if !unknown.is_empty() {
                this.send(DataEvent::QueueTracks { asked: unknown, tracks: Vec::new(), reached: true });
            }
            for (service, uris) in by_service(known) {
                let Ok(catalog) = this.catalog(Some(service)) else { continue };
                let mut found = catalog.queue_tracks(uris);
                while let Some(found) = found.next().await {
                    let (asked, tracks, reached) = (found.asked, found.tracks, found.reached);
                    this.send(DataEvent::QueueTracks { asked, tracks, reached });
                }
            }
        });
    }

    pub fn upcoming(&self, playing: String) {
        if self.demo.is_some() {
            return;
        }
        self.spawn(move |this| async move {
            let result = async { this.catalog_of(&playing)?.upcoming().await }.await.map_err(|e| format!("{e:#}"));
            this.send(DataEvent::Upcoming { playing, result });
        });
    }

    pub fn skip_to(&self, device: String, entry: Upcoming) {
        if self.demo.is_some() {
            return;
        }
        self.spawn(move |this| async move {
            let skip = async { this.catalog_of(&entry.uri)?.skip_to(&device, &entry).await };
            let result = skip.await.map_err(|e| format!("{e:#}"));
            this.send(DataEvent::SkippedTo(result));
        });
    }

    pub fn set_queue(&self, service: Option<Service>, device: String, listed: Listed) {
        if self.demo.is_some() {
            return;
        }
        self.spawn(move |this| async move {
            let set = async { this.catalog(service)?.set_queue(&device, &listed).await };
            if let Err(err) = set.await {
                this.send(DataEvent::Failed(format!("Queue: {err:#}")));
            }
        });
    }

    pub fn add_to_playlist(&self, playlist: &PlaylistInfo, uris: Vec<String>) {
        if self.demo.is_some() {
            return;
        }
        let (playlist_uri, playlist_id) = (playlist.uri.clone(), playlist.id.clone());
        self.spawn(move |this| async move {
            let added = async { this.catalog_of(&playlist_uri)?.add_to_playlist(&playlist_id, &uris).await };
            if let Err(err) = added.await {
                let error = format!("Playlist: {err:#}");
                this.send_for(DataEvent::AddFailed { playlist_id, count: uris.len() as u32, error });
            }
        });
    }
}

fn by_service(uris: Vec<String>) -> Vec<(Service, Vec<String>)> {
    let mut groups: Vec<(Service, Vec<String>)> = Vec::new();
    for uri in uris {
        let Some(service) = Service::of(&uri) else { continue };
        match groups.iter_mut().find(|(s, _)| *s == service) {
            Some((_, group)) => group.push(uri),
            None => groups.push((service, vec![uri])),
        }
    }
    groups
}

fn cache_name(url: &str) -> String {
    Sha256::digest(url.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect()
}

impl Picture {
    pub fn size(&self) -> Size {
        match &self.graphic {
            Graphic::Cells(protocol) => protocol.size(),
            Graphic::Inline(inline) => inline.size(),
        }
    }

    pub fn kind(&self) -> ProtocolType {
        match &self.graphic {
            Graphic::Cells(Protocol::Kitty(_)) => ProtocolType::Kitty,
            Graphic::Cells(Protocol::Sixel(_)) => ProtocolType::Sixel,
            Graphic::Cells(Protocol::ITerm2(_)) => ProtocolType::Iterm2,
            Graphic::Cells(Protocol::Halfblocks(_)) => ProtocolType::Halfblocks,
            Graphic::Inline(inline) => inline.kind(),
        }
    }

    pub fn in_terminal(&self) -> bool {
        matches!(&self.graphic, Graphic::Cells(Protocol::Kitty(kitty)) if kitty.transmitted())
    }

    pub fn terminal_delete(&self) -> Option<String> {
        match &self.graphic {
            Graphic::Cells(Protocol::Kitty(kitty)) if kitty.transmitted() => Some(kitty.delete_sequence()),
            _ => None,
        }
    }
}

fn mask(round: bool) -> impl FnOnce(&mut image::DynamicImage) + Send + 'static {
    move |image| {
        if round {
            *image = circle(image);
        } else {
            *image = rounded(image);
        }
    }
}

fn circle(image: &image::DynamicImage) -> image::DynamicImage {
    let mut rgba = image.to_rgba8();
    let (width, height) = rgba.dimensions();
    let radius = width.min(height) as f32 / 2.0;
    let (cx, cy) = (width as f32 / 2.0, height as f32 / 2.0);
    for (x, y, pixel) in rgba.enumerate_pixels_mut() {
        let distance = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
        let coverage = (radius - distance + 0.5).clamp(0.0, 1.0);
        pixel.0[3] = (pixel.0[3] as f32 * coverage) as u8;
    }
    rgba.into()
}

fn rounded(image: &image::DynamicImage) -> image::DynamicImage {
    let mut rgba = image.to_rgba8();
    let (width, height) = rgba.dimensions();
    let radius = (width.min(height) as f32 / 8.0).max(2.0);
    let (cx, cy) = (width as f32 / 2.0, height as f32 / 2.0);
    for (x, y, pixel) in rgba.enumerate_pixels_mut() {
        let dx = ((x as f32 + 0.5 - cx).abs() - (cx - radius)).max(0.0);
        let dy = ((y as f32 + 0.5 - cy).abs() - (cy - radius)).max(0.0);
        let coverage = (radius - dx.hypot(dy) + 0.5).clamp(0.0, 1.0);
        pixel.0[3] = (pixel.0[3] as f32 * coverage) as u8;
    }
    rgba.into()
}

fn featureless(image: &image::DynamicImage) -> bool {
    let rgb = image.to_rgb8();
    let (width, height) = rgb.dimensions();
    if width == 0 || height == 0 {
        return true;
    }
    let step_x = (width / 16).max(1) as usize;
    let step_y = (height / 16).max(1) as usize;
    let mut min = [255u8; 3];
    let mut max = [0u8; 3];
    for y in (0..height).step_by(step_y) {
        for x in (0..width).step_by(step_x) {
            let pixel = rgb.get_pixel(x, y).0;
            for channel in 0..3 {
                min[channel] = min[channel].min(pixel[channel]);
                max[channel] = max[channel].max(pixel[channel]);
            }
        }
    }
    (0..3).all(|channel| max[channel].saturating_sub(min[channel]) <= 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn covers_get_rounded_corners() {
        let opaque = image::RgbaImage::from_pixel(64, 64, image::Rgba([255, 255, 255, 255])).into();
        let masked = rounded(&opaque).into_rgba8();
        assert_eq!(masked.get_pixel(0, 0).0[3], 0, "the corner is cut away");
        assert_eq!(masked.get_pixel(32, 32).0[3], 255, "the middle is untouched");
        assert_eq!(masked.get_pixel(2, 32).0[3], 255, "so is an edge that is not a corner");
    }

    #[test]
    fn a_flat_cover_counts_as_missing() {
        let flat = image::RgbImage::from_pixel(32, 32, image::Rgb([0, 0, 0])).into();
        assert!(featureless(&flat));
        let mut detailed = image::RgbImage::from_pixel(32, 32, image::Rgb([0, 0, 0]));
        detailed.put_pixel(10, 10, image::Rgb([255, 0, 0]));
        assert!(!featureless(&image::DynamicImage::ImageRgb8(detailed)));
    }

    #[test]
    fn requests_go_to_the_service_of_each_uri() {
        let uris = ["spotify:track:1", "ytmusic:track:2", "file:3", "spotify:track:4"].map(String::from).to_vec();
        let groups = by_service(uris);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0], (Service::Spotify, vec!["spotify:track:1".to_owned(), "spotify:track:4".to_owned()]));
        assert_eq!(groups[1], (Service::YouTubeMusic, vec!["ytmusic:track:2".to_owned()]));
    }
}
