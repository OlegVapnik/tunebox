//! tunebox в терминале: Home, библиотека, поиск, страницы треков и плеер.
#![cfg_attr(not(feature = "spotify"), allow(dead_code))]

mod app;
mod art;
mod backend;
mod clock;
mod colors;
mod commands;
mod datacache;
pub mod demo;
mod fx;
mod graphics;
#[cfg(test)]
mod harness;
mod icons;
mod loader;
mod media;
mod notify;
pub mod omarchy;
mod overlay;
mod playback;
mod preferences;
mod queue;
#[cfg(test)]
mod scenarios;
mod snapshot;
mod state;
mod theme;
mod ui;
mod views;

use std::{
    io::{BufWriter, Stdout, Write},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use anyhow::{Result, bail};
use crossterm::{
    event::{DisableFocusChange, DisableMouseCapture, EnableFocusChange, EnableMouseCapture, EventStream},
    terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate},
};
use futures::{FutureExt, StreamExt};
use ratatui::{Terminal, backend::CrosstermBackend, buffer::Buffer};
use tokio::{
    sync::{Notify, mpsc},
    time::MissedTickBehavior,
};
use tunebox_core::{config::Config, paths::Paths, service::Service};

use crate::clock::Instant;
pub use crate::graphics::ImageProtocol;
use crate::{
    app::{App, AppEvent},
    backend::{Account, Catalog, Launch},
    graphics::ImageTerminal,
    loader::{DataEvent, Loader},
    preferences::Preferences,
};

pub const SERVICES: &[Service] = backend::COMPILED;
pub const VARIANT: Option<&str> = backend::VARIANT;

const TICK: Duration = Duration::from_millis(25);
const IMAGES_PER_FRAME: usize = 12;

type Term = Terminal<CrosstermBackend<BufWriter<Stdout>>>;

const LOW_POWER_TICK: Duration = Duration::from_millis(100);

pub async fn run(paths: &Paths) -> Result<Option<PathBuf>> {
    let config = Config::load(paths)?;
    let services = config.enabled_services(SERVICES);
    if services.is_empty() {
        bail!("{}", no_service(paths));
    }
    let mut accounts = Vec::new();
    let mut first_error = None;
    for service in services {
        let account = match Account::open(service, paths, &config).await {
            Ok(account) => Ok(account),
            Err(err) => {
                tracing::warn!("{}: {err:#}", service.name());
                let problem = format!("{err:#}");
                first_error.get_or_insert(err);
                Err(problem)
            }
        };
        accounts.push((service, account));
    }
    if accounts.iter().all(|(_, account)| account.is_err())
        && let Some(err) = first_error
    {
        return Err(err);
    }
    run_ui(paths, config, accounts, None).await
}

pub fn no_service(paths: &Paths) -> String {
    if SERVICES.is_empty() {
        return "This tunebox has no music service built in. `tunebox demo` shows what it looks like.".into();
    }
    let keys: Vec<String> = SERVICES.iter().map(|service| format!("\"{}\"", service.key())).collect();
    format!(
        "No music service is enabled. Add the ones to use to {}:\n\n  services = [{}]",
        paths.config_file().display(),
        keys.join(", ")
    )
}

pub async fn demo(paths: &Paths) -> Result<Option<PathBuf>> {
    let demo = Arc::new(demo::Demo::sample()?);
    let config = Config::load(paths).unwrap_or_default();
    run_ui(paths, config, Vec::new(), Some(demo)).await
}

type Accounts = Vec<(Service, Result<Account, String>)>;

async fn run_ui(
    paths: &Paths,
    config: Config,
    accounts: Accounts,
    demo: Option<Arc<demo::Demo>>,
) -> Result<Option<PathBuf>> {
    #[cfg(unix)]
    let _stderr = StderrToLog::redirect(&paths.log_file());
    icons::init(config.icons.as_deref());
    let _ = ratatui::init();
    let mut terminal = Terminal::new(CrosstermBackend::new(BufWriter::with_capacity(64 * 1024, std::io::stdout())))?;
    let images = ImageTerminal::detect();
    crossterm::execute!(std::io::stdout(), EnableMouseCapture, EnableFocusChange)?;
    let restore = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture, DisableFocusChange);
        restore(info);
    }));
    let prefs = Preferences::from_config(&config);
    let image_protocol = ImageProtocol::from_env().unwrap_or(prefs.images);
    let notifier = if demo.is_some() { None } else { notify::Notifier::detect() };
    let result = run_app(&mut terminal, paths, accounts, images, image_protocol, prefs, notifier, demo).await;
    let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture, DisableFocusChange);
    ratatui::restore();
    result
}

#[allow(clippy::too_many_arguments)]
async fn run_app(
    terminal: &mut Term,
    paths: &Paths,
    accounts: Accounts,
    images: ImageTerminal,
    image_protocol: ImageProtocol,
    prefs: Preferences,
    notifier: Option<notify::Notifier>,
    demo: Option<Arc<demo::Demo>>,
) -> Result<Option<PathBuf>> {
    let (tx, mut events) = mpsc::unbounded_channel();
    let snapshotter = snapshot::Snapshotter::from_env().map(Arc::new);
    let notices = tx.clone();
    let media = if demo.is_some() {
        None
    } else {
        tunebox_os::media::Controls::new({
            let tx = tx.clone();
            move |command| {
                let _ = tx.send(app::AppEvent::Media(command));
            }
        })
    };
    let retries: Vec<Arc<Notify>> = accounts.iter().map(|_| Arc::default()).collect();
    tunebox_os::power::watch({
        let (tx, retries) = (tx.clone(), retries.clone());
        move |event| {
            if event == tunebox_os::power::Event::Wake {
                retries.iter().for_each(|retry_now| retry_now.notify_one());
            }
            let _ = tx.send(app::AppEvent::Power(event));
        }
    });
    let (start, started) = match &demo {
        Some(demo) => (app::Start::Demo(demo), Vec::new()),
        None => {
            let saved = state::load(&paths.state_file());
            let crossfade = Duration::from_secs(prefs.crossfade);
            let started: Vec<(Service, Result<Catalog, String>)> = accounts
                .into_iter()
                .zip(retries)
                .map(|((service, account), retry_now)| {
                    let (paths, tx, volume) = (paths.clone(), tx.clone(), saved.volume);
                    (service, account.map(|account| account.start(Launch { paths, crossfade, volume, retry_now, tx })))
                })
                .collect();
            (app::Start::Restore(Box::new(saved)), started)
        }
    };
    let service = match &demo {
        Some(demo) => Service::of(&demo.playing.track.uri),
        None => started.first().map(|(service, _)| *service),
    };
    let colors = colors::Colors::new(prefs.colors, images.colors(), &paths.config);
    let loader = Loader::new(started, service, tx.clone(), images, paths.cache.join("images"), snapshotter.is_some())
        .with_demo(demo.clone());
    loader.set_image_protocol(image_protocol);
    loader.prune_image_cache();
    let update_check = prefs.update_check && demo.is_none();
    let mut app = App::new(loader, prefs, start, paths.clone(), media, notifier, Some(colors));
    if update_check {
        app.check_for_update(false);
    }

    let mut input = EventStream::new();
    let mut closed = shutdown_signals()?;
    let mut ticker = new_ticker(app.settings.low_power);

    draw(terminal, &mut app, false)?;
    let (recording_since, mut last_frame) = (Instant::now(), Instant::now());
    while !app.quit {
        let redraw = tokio::select! {
            biased;
            event = input.next() => match event {
                Some(event) => {
                    let mut redraw = app.on_input(event?);
                    while let Some(Some(event)) = input.next().now_or_never() {
                        redraw |= app.on_input(event?);
                    }
                    redraw
                }
                None => break,
            },
            _ = closed.recv() => break,
            Some(event) = events.recv() => {
                let mut redraw = app.on_event(event);
                let mut images = 0;
                while images < IMAGES_PER_FRAME
                    && let Ok(event) = events.try_recv()
                {
                    images += usize::from(matches!(event, AppEvent::Data(DataEvent::Image { .. })));
                    redraw |= app.on_event(event);
                }
                redraw
            }
            _ = ticker.tick() => app.on_tick(),
        };
        let redraw = app.poll_colors() || redraw;
        app.sync_media();
        app.announce_track();
        if ticker.period() != ticker_period(app.settings.low_power) {
            ticker = new_ticker(app.settings.low_power);
        }
        let record = snapshotter.as_ref().and_then(|s| s.record_every()).filter(|every| last_frame.elapsed() >= *every);
        if let (Some(_), Some(snapshotter)) = (record, &snapshotter) {
            last_frame = Instant::now();
            let buffer = draw(terminal, &mut app, true)?.expect("asked for the frame");
            let placements = app.placements.clone();
            let (snapshotter, at) = (snapshotter.clone(), recording_since.elapsed().as_millis() as u64);
            let mouse = app.mouse.map(|position| (position.x, position.y));
            tokio::task::spawn_blocking(move || snapshotter.save_frame(&buffer, &placements, at, mouse));
        } else if redraw || app.snapshot_requested {
            let snapshot = std::mem::take(&mut app.snapshot_requested) && snapshotter.is_some();
            if let Some(buffer) = draw(terminal, &mut app, snapshot)?
                && let Some(snapshotter) = &snapshotter
            {
                let placements = app.placements.clone();
                let (snapshotter, notices) = (snapshotter.clone(), notices.clone());
                tokio::task::spawn_blocking(move || {
                    let text = match snapshotter.save(&buffer, &placements) {
                        Ok(path) => format!("Snapshot: {}", path.display()),
                        Err(err) => format!("Snapshot failed: {err:#}"),
                    };
                    let _ = notices.send(app::AppEvent::Data(loader::DataEvent::Notice(text)));
                });
            }
        }
        if !app.terminal_out.is_empty() {
            let out = terminal.backend_mut();
            for sequence in app.terminal_out.drain(..) {
                out.write_all(sequence.as_bytes())?;
            }
            out.flush()?;
        }
    }
    app.shutdown();
    Ok(app.restart.take())
}

fn draw(terminal: &mut Term, app: &mut App, snapshot: bool) -> Result<Option<Buffer>> {
    crossterm::queue!(terminal.backend_mut(), BeginSynchronizedUpdate)?;
    let frame = terminal.draw(|frame| ui::draw(frame, app))?;
    let buffer = snapshot.then(|| frame.buffer.clone());
    for placement in &mut app.placements {
        if let Some(upload) = placement.upload.take() {
            terminal.backend_mut().write_all(upload.as_bytes())?;
        }
    }
    app.inline.write(terminal.backend_mut())?;
    crossterm::execute!(terminal.backend_mut(), EndSynchronizedUpdate)?;
    Ok(buffer)
}

fn ticker_period(low_power: bool) -> Duration {
    if low_power { LOW_POWER_TICK } else { TICK }
}

fn new_ticker(low_power: bool) -> tokio::time::Interval {
    let mut ticker = tokio::time::interval(ticker_period(low_power));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    ticker
}

#[cfg(unix)]
fn shutdown_signals() -> Result<mpsc::Receiver<()>> {
    use tokio::signal::unix::{SignalKind, signal};
    let (tx, rx) = mpsc::channel(1);
    for kind in [SignalKind::hangup(), SignalKind::terminate()] {
        let mut stream = signal(kind)?;
        let tx = tx.clone();
        tokio::spawn(async move {
            stream.recv().await;
            let _ = tx.send(()).await;
        });
    }
    Ok(rx)
}

#[cfg(not(unix))]
fn shutdown_signals() -> Result<mpsc::Receiver<()>> {
    let (tx, rx) = mpsc::channel(1);
    std::mem::forget(tx);
    Ok(rx)
}

#[cfg(unix)]
struct StderrToLog {
    saved: libc::c_int,
}

#[cfg(unix)]
impl StderrToLog {
    fn redirect(path: &std::path::Path) -> Option<Self> {
        use std::os::fd::AsRawFd;
        let file = std::fs::OpenOptions::new().create(true).append(true).open(path).ok()?;
        unsafe {
            let saved = libc::dup(libc::STDERR_FILENO);
            if saved < 0 {
                return None;
            }
            if libc::dup2(file.as_raw_fd(), libc::STDERR_FILENO) < 0 {
                libc::close(saved);
                return None;
            }
            Some(Self { saved })
        }
    }
}

#[cfg(unix)]
impl Drop for StderrToLog {
    fn drop(&mut self) {
        unsafe {
            libc::dup2(self.saved, libc::STDERR_FILENO);
            libc::close(self.saved);
        }
    }
}

pub(crate) fn release_freed_memory() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    unsafe {
        libc::malloc_trim(0);
    }
}
