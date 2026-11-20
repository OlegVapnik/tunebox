
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend};
use tokio::sync::mpsc::{self, UnboundedReceiver};
use tunebox_core::paths::Paths;
use unicode_width::UnicodeWidthStr;

use crate::{
    app::{App, AppEvent, Start},
    backend::{Catalog, Service, fake},
    clock,
    demo::Demo,
    graphics::ImageTerminal,
    loader::Loader,
    preferences::Preferences,
    snapshot::Snapshotter,
    ui,
};

const SETTLE_LIMIT: Duration = Duration::from_secs(10);

pub struct Harness {
    pub app: App,
    events: UnboundedReceiver<AppEvent>,
    terminal: Terminal<TestBackend>,
    root: PathBuf,
    fakes: Vec<fake::Fake>,
}

impl Harness {
    pub async fn demo() -> Self {
        Self::demo_sized(120, 36).await
    }

    pub async fn demo_sized(width: u16, height: u16) -> Self {
        let (root, paths) = new_paths();
        let demo = Arc::new(Demo::sample().expect("the demo sample loads"));
        let (tx, events) = mpsc::unbounded_channel();
        let service = Service::of(&demo.playing.track.uri);
        let images = paths.cache.join("images");
        let loader = Loader::new(Vec::new(), service, tx, ImageTerminal::halfblocks(), images, true)
            .with_demo(Some(demo.clone()));
        let app = App::new(loader, Preferences::default(), Start::Demo(&demo), paths, None, None, None);
        Self::start(app, events, root, Vec::new(), (width, height)).await
    }

    pub async fn fakes(services: &[Service]) -> Self {
        let mut harness = Self::fakes_connecting(services).await;
        for fake in harness.fakes.clone() {
            fake.connect();
        }
        harness.settle().await;
        harness
    }

    pub async fn fakes_connecting(services: &[Service]) -> Self {
        let (root, paths) = new_paths();
        let (tx, events) = mpsc::unbounded_channel();
        let fakes: Vec<_> = services.iter().map(|service| fake::Fake::new(*service, tx.clone())).collect();
        let started =
            fakes.iter().map(|fake| (fake.service(), Ok(Catalog::Fake(fake::Catalog(fake.clone()))))).collect();
        let images = paths.cache.join("images");
        let loader = Loader::new(started, services.first().copied(), tx, ImageTerminal::halfblocks(), images, true);
        let start = Start::Restore(Box::default());
        let app = App::new(loader, Preferences::default(), start, paths, None, None, None);
        Self::start(app, events, root, fakes, (120, 36)).await
    }

    async fn start(
        app: App,
        events: UnboundedReceiver<AppEvent>,
        root: PathBuf,
        fakes: Vec<fake::Fake>,
        (width, height): (u16, u16),
    ) -> Self {
        let terminal = Terminal::new(TestBackend::new(width, height)).expect("a test terminal");
        let mut harness = Self { app, events, terminal, root, fakes };
        harness.settle().await;
        harness
    }

    pub fn fake(&self, service: Service) -> &fake::Fake {
        self.fakes.iter().find(|fake| fake.service() == service).expect("a fake of that service")
    }

    pub async fn press(&mut self, code: KeyCode) -> &mut Self {
        self.input(Event::Key(KeyEvent::new(code, KeyModifiers::NONE))).await
    }

    pub async fn ctrl(&mut self, c: char) -> &mut Self {
        self.input(Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))).await
    }

    pub async fn type_text(&mut self, text: &str) -> &mut Self {
        for c in text.chars() {
            self.press(KeyCode::Char(c)).await;
        }
        self
    }

    pub async fn click_on(&mut self, text: &str) -> &mut Self {
        let (column, row) = self.find(text).unwrap_or_else(|| panic!("{text:?} isn't on screen:\n{}", self.screen()));
        for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)] {
            let mouse = MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE };
            self.input(Event::Mouse(mouse)).await;
        }
        self
    }

    pub async fn resize(&mut self, width: u16, height: u16) -> &mut Self {
        self.terminal.backend_mut().resize(width, height);
        self.input(Event::Resize(width, height)).await
    }

    pub async fn wait(&mut self, duration: Duration) -> &mut Self {
        const TICK: Duration = Duration::from_millis(50);
        let mut left = duration;
        while !left.is_zero() {
            let step = left.min(TICK);
            clock::advance(step);
            left -= step;
            for fake in &self.fakes {
                fake.catch_up();
            }
            while let Ok(event) = self.events.try_recv() {
                self.app.on_event(event);
            }
            self.app.on_tick();
        }
        self.settle().await;
        self
    }

    async fn input(&mut self, event: Event) -> &mut Self {
        self.app.on_input(event);
        self.settle().await;
        self
    }

    pub async fn settle(&mut self) {
        let started = std::time::Instant::now();
        loop {
            let mut handled = false;
            while let Ok(event) = self.events.try_recv() {
                self.app.on_event(event);
                handled = true;
            }
            self.redraw();
            self.app.on_tick();
            if !handled && self.events.is_empty() && self.app.is_idle() {
                self.redraw();
                return;
            }
            assert!(started.elapsed() < SETTLE_LIMIT, "Still busy after {SETTLE_LIMIT:?}:\n{}", self.screen());
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }

    pub fn redraw(&mut self) {
        self.terminal.draw(|frame| ui::draw(frame, &mut self.app)).expect("a frame");
        for placement in &mut self.app.placements {
            placement.upload = None;
        }
        self.app.terminal_out.clear();
    }

    pub fn screen(&self) -> String {
        let buffer = self.terminal.backend().buffer();
        let mut screen = String::new();
        for y in 0..buffer.area.height {
            let mut line = String::new();
            let mut skip = 0;
            for x in 0..buffer.area.width {
                if skip > 0 {
                    skip -= 1;
                    continue;
                }
                let symbol = buffer[(x, y)].symbol();
                skip = symbol.width().saturating_sub(1);
                line.push_str(symbol);
            }
            screen.push_str(line.trim_end());
            screen.push('\n');
        }
        screen
    }

    pub fn find(&self, text: &str) -> Option<(u16, u16)> {
        self.screen().lines().enumerate().find_map(|(row, line)| {
            let at = line.find(text)?;
            Some((line[..at].width() as u16, row as u16))
        })
    }

    pub fn player_bar(&self) -> String {
        let screen = self.screen();
        let rows: Vec<_> = screen.lines().collect();
        rows[rows.len() - 7..rows.len() - 2].join("\n")
    }

    pub fn shows(&self, text: &str) -> bool {
        self.find(text).is_some()
    }

    #[track_caller]
    pub fn assert_screen(&self, name: &str) {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/screens").join(format!("{name}.txt"));
        let screen = self.screen();
        let update = std::env::var_os("TUNEBOX_UPDATE_SCREENS").is_some_and(|v| v != "0");
        let saved = std::fs::read_to_string(&path).ok();
        if update || saved.is_none() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &screen).unwrap();
            assert!(update, "New screen {name}, saved to {}; check it and run again", path.display());
            return;
        }
        let saved = saved.unwrap();
        if saved != screen {
            panic!("Screen {name} differs; TUNEBOX_UPDATE_SCREENS=1 takes the new one.\n{}", diff(&saved, &screen));
        }
    }

    pub fn shot(&self, name: &str) {
        let dir = std::env::var_os("TUNEBOX_SHOT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/shots"));
        std::fs::create_dir_all(&dir).expect("a folder for the pictures");
        let snapshotter = match Snapshotter::new(dir) {
            Ok(snapshotter) => snapshotter,
            Err(err) => panic!("snapshots need the dev fonts: {err:#}"),
        };
        let path =
            snapshotter.write(self.terminal.backend().buffer(), &self.app.placements, name).expect("a frame to save");
        println!("{}", path.display());
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn new_paths() -> (PathBuf, Paths) {
    static RUNS: AtomicUsize = AtomicUsize::new(0);
    let run = RUNS.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("tunebox-harness-{}-{run}", std::process::id()));
    let paths =
        Paths { config: root.join("config"), data: root.join("data"), cache: root.join("cache"), keychain: false };
    (root, paths)
}

fn diff(saved: &str, screen: &str) -> String {
    let (saved, screen): (Vec<_>, Vec<_>) = (saved.lines().collect(), screen.lines().collect());
    let mut out = String::new();
    for row in 0..saved.len().max(screen.len()) {
        let (before, after) = (saved.get(row).copied(), screen.get(row).copied());
        if before != after {
            out.push_str(&format!("{row:>3} - {}\n{row:>3} + {}\n", before.unwrap_or(""), after.unwrap_or("")));
        }
    }
    out
}

#[cfg(test)]
mod pictures {
    use std::time::Duration;

    use crossterm::event::KeyCode::{Char, Down, Enter, Esc};

    use super::Harness;

    #[tokio::test]
    #[ignore = "writes pictures and needs the dev fonts"]
    async fn the_views() {
        let mut library = Harness::demo().await;
        library.shot("home");
        library.press(Char('3')).await.wait(Duration::from_millis(600)).await;
        library.shot("library");
        library.press(Down).await;
        library.press(Enter).await.wait(Duration::from_millis(600)).await;
        library.shot("playlist");
        library.press(Char('/')).await;
        library.type_text("daft").await;
        library.wait(Duration::from_millis(400)).await;
        library.shot("search");
        library.press(Esc).await;
        library.press(Char('Q')).await.wait(Duration::from_millis(600)).await;
        library.shot("queue");
        library.press(Char('z')).await.wait(Duration::from_millis(600)).await;
        library.shot("now-playing");
    }
}
