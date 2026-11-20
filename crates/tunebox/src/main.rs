#![cfg_attr(
    not(any(feature = "spotify", feature = "ytmusic", feature = "soundcloud")),
    allow(unused_variables, unreachable_code, clippy::never_loop)
)]

mod doctor;
#[cfg(any(feature = "spotify", feature = "ytmusic", feature = "soundcloud"))]
mod headless;
#[cfg(any(feature = "spotify", feature = "ytmusic", feature = "soundcloud"))]
mod media;
#[cfg(target_os = "linux")]
mod omarchy;
#[cfg(any(feature = "ytmusic", feature = "soundcloud"))]
mod prompt;
#[cfg(feature = "soundcloud")]
mod soundcloud;
#[cfg(feature = "spotify")]
mod spotify;
mod style;
mod update;
#[cfg(feature = "ytmusic")]
mod ytmusic;

use std::sync::LazyLock;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, builder::PossibleValuesParser};
use tracing_subscriber::{filter::Targets, fmt, prelude::*};
use tunebox_core::{config::Config, paths::Paths, service::Service};
use tunebox_tui::SERVICES;

#[derive(Parser)]
#[command(name = "tunebox", version = version(), about = about())]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    Login {
        #[arg(value_parser = PossibleValuesParser::new(Service::ALL.map(Service::key)))]
        service: Option<String>,
        #[arg(long)]
        force: bool,
        #[arg(long, conflicts_with = "force")]
        check: bool,
    },
    #[cfg(any(feature = "ytmusic", feature = "soundcloud"))]
    Logout {
        #[arg(value_parser = PossibleValuesParser::new([
            #[cfg(feature = "ytmusic")]
            Service::YouTubeMusic.key(),
            #[cfg(feature = "soundcloud")]
            Service::SoundCloud.key(),
        ]))]
        service: String,
    },
    Doctor,
    #[cfg_attr(not(any(feature = "spotify", feature = "ytmusic")), command(hide = true))]
    Update,
    Play {
        uri: String,
        #[arg(short, long)]
        shuffle: bool,
    },
    #[cfg(feature = "spotify")]
    #[command(hide = true)]
    Query {
        operation: String,
        #[arg(default_value = "{}")]
        variables: String,
    },
    #[cfg(feature = "spotify")]
    #[command(hide = true)]
    DjSample {
        dir: std::path::PathBuf,
    },
    Demo,
    #[cfg(feature = "spotify")]
    Connect {
        #[arg(long, default_value = "tunebox")]
        name: String,
    },
    #[cfg(target_os = "linux")]
    Omarchy {
        #[command(subcommand)]
        action: omarchy::Action,
    },
}

fn version() -> &'static str {
    static VERSION: LazyLock<String> = LazyLock::new(|| {
        let services: Vec<&str> = SERVICES.iter().map(|service| service.key()).collect();
        let services = if services.is_empty() { "demo only".to_owned() } else { services.join(", ") };
        format!("{} ({services})", env!("CARGO_PKG_VERSION"))
    });
    &VERSION
}

fn about() -> &'static str {
    static ABOUT: LazyLock<String> = LazyLock::new(|| {
        let names: Vec<&str> = SERVICES.iter().map(|service| service.name()).collect();
        let names = if names.is_empty() { "Music".to_owned() } else { names.join(" and ") };
        format!("{names} in your terminal")
    });
    &ABOUT
}

fn main() -> Result<()> {
    unsafe {
        std::env::set_var("PULSE_PROP_application.name", "tunebox");
        std::env::set_var("PULSE_PROP_application.icon_name", "audio-x-generic");
        std::env::set_var("PULSE_PROP_media.role", "music");
    }
    limit_malloc_arenas();
    tunebox_os::run_main(|| tokio::runtime::Runtime::new()?.block_on(run()))
}

fn limit_malloc_arenas() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    unsafe {
        libc::mallopt(libc::M_ARENA_MAX, 2);
        libc::mallopt(libc::M_MMAP_THRESHOLD, 256 * 1024);
    }
}

async fn run() -> Result<()> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let cli = Cli::parse();
    let mut paths = Paths::resolve()?;
    paths.keychain = Config::load(&paths)?.keychain()?;
    init_logging(&paths, matches!(cli.command, None | Some(Command::Demo)))?;
    match cli.command {
        Some(Command::Login { service, check: true, .. }) => {
            if !signed_in(&paths, service.as_deref()).await? {
                std::process::exit(1);
            }
            Ok(())
        }
        Some(Command::Login { service, force, .. }) => login(&paths, service.as_deref(), force).await,
        #[cfg(feature = "ytmusic")]
        Some(Command::Logout { service }) if service == Service::YouTubeMusic.key() => ytmusic::logout(&paths),
        #[cfg(feature = "soundcloud")]
        Some(Command::Logout { service }) if service == Service::SoundCloud.key() => soundcloud::logout(&paths),
        #[cfg(any(feature = "ytmusic", feature = "soundcloud"))]
        Some(Command::Logout { service }) => bail!("“{service}” has no sign-out"),
        Some(Command::Doctor) => doctor::run(&paths, &services(&paths, None)?).await,
        Some(Command::Update) => update::run().await,
        Some(Command::Play { uri, shuffle }) => play(&paths, &uri, shuffle).await,
        Some(Command::Demo) => update::restart_if_asked(tunebox_tui::demo(&paths).await?),
        #[cfg(feature = "spotify")]
        Some(Command::Connect { name }) => spotify::connect(&paths, name).await,
        #[cfg(feature = "spotify")]
        Some(Command::Query { operation, variables }) => spotify::query(&paths, &operation, &variables).await,
        #[cfg(feature = "spotify")]
        Some(Command::DjSample { dir }) => spotify::dj_sample(&paths, &dir).await,
        #[cfg(target_os = "linux")]
        Some(Command::Omarchy { action }) => omarchy::run(action),
        None => update::restart_if_asked(tunebox_tui::run(&paths).await?),
    }
}

fn services(paths: &Paths, named: Option<&str>) -> Result<Vec<Service>> {
    let Some(key) = named else {
        let enabled = Config::load(paths)?.enabled_services(SERVICES);
        if enabled.is_empty() {
            bail!("{}", tunebox_tui::no_service(paths));
        }
        return Ok(enabled);
    };
    let service = Service::from_key(key).with_context(|| format!("“{key}” is not a music service tunebox knows"))?;
    if !SERVICES.contains(&service) {
        bail!("This tunebox was built without {}.", service.name());
    }
    Ok(vec![service])
}

fn init_logging(paths: &Paths, tui: bool) -> Result<()> {
    let logs = tracing_subscriber::registry().with(log_filter(std::env::var("TUNEBOX_LOG").ok().as_deref()));
    if tui {
        std::fs::create_dir_all(&paths.cache)?;
        std::fs::File::create(paths.log_file())?;
        let file = std::fs::OpenOptions::new().append(true).open(paths.log_file())?;
        logs.with(fmt::layer().with_ansi(false).with_writer(std::sync::Mutex::new(file))).init();
    } else {
        logs.with(fmt::layer().with_writer(std::io::stderr)).init();
    }
    Ok(())
}

async fn login(paths: &Paths, named: Option<&str>, force: bool) -> Result<()> {
    let services = services(paths, named)?;
    for &service in &services {
        if services.len() > 1 {
            println!("\n  {}", style::bold(service.name()));
        }
        let signed_in: bool = match service {
            #[cfg(feature = "spotify")]
            Service::Spotify => spotify::login(paths, &Config::load(paths)?, force).await.map(|()| true)?,
            #[cfg(feature = "ytmusic")]
            Service::YouTubeMusic => {
                if services.len() == 1 {
                    println!();
                }
                ytmusic::login(paths, force, services.len() > 1).await?
            }
            #[cfg(feature = "soundcloud")]
            Service::SoundCloud => soundcloud::login(paths, force).await?,
            #[allow(unreachable_patterns)]
            other => bail!("{} isn't built into this tunebox", other.name()),
        };
        if !signed_in && services.len() == 1 {
            return Ok(());
        }
    }
    println!("  Done. Test everything with {}\n", style::bold("tunebox doctor"));
    Ok(())
}

async fn signed_in(paths: &Paths, named: Option<&str>) -> Result<bool> {
    for service in services(paths, named)? {
        let signed_in: bool = match service {
            #[cfg(feature = "spotify")]
            Service::Spotify => spotify::signed_in(paths, &Config::load(paths)?).await?,
            #[cfg(feature = "ytmusic")]
            Service::YouTubeMusic => ytmusic::signed_in(paths)?,
            #[cfg(feature = "soundcloud")]
            Service::SoundCloud => soundcloud::signed_in(paths)?,
            #[allow(unreachable_patterns)]
            other => bail!("{} isn't built into this tunebox", other.name()),
        };
        if !signed_in {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn play(paths: &Paths, input: &str, shuffle: bool) -> Result<()> {
    match link_service(input).or(SERVICES.first().copied()) {
        #[cfg(feature = "spotify")]
        Some(Service::Spotify) => spotify::play(paths, input, shuffle).await,
        #[cfg(feature = "ytmusic")]
        Some(Service::YouTubeMusic) => ytmusic::play(paths, input, shuffle).await,
        #[cfg(feature = "soundcloud")]
        Some(Service::SoundCloud) => soundcloud::play(paths, input, shuffle).await,
        #[allow(unreachable_patterns)]
        Some(other) => bail!("{} isn't built into this tunebox", other.name()),
        None => bail!("{}", tunebox_tui::no_service(paths)),
    }
}

fn link_service(input: &str) -> Option<Service> {
    let youtube = ["music.youtube.com/", "www.youtube.com/", "youtube.com/", "m.youtube.com/", "youtu.be/"];
    let site = input.strip_prefix("https://").or_else(|| input.strip_prefix("http://"));
    if input.starts_with("https://open.spotify.com/") {
        Some(Service::Spotify)
    } else if site.is_some_and(|site| youtube.iter().any(|host| site.starts_with(host))) {
        Some(Service::YouTubeMusic)
    } else if site.is_some_and(|site| site.starts_with("soundcloud.com/") || site.starts_with("www.soundcloud.com/")) {
        Some(Service::SoundCloud)
    } else {
        Service::of(input)
    }
}

fn log_filter(spec: Option<&str>) -> Targets {
    let errors = || Targets::new().with_default(tracing::Level::ERROR);
    let directives: Vec<&str> = spec.unwrap_or_default().split(',').map(str::trim).filter(|d| !d.is_empty()).collect();
    if directives.is_empty() { errors() } else { directives.join(",").parse().unwrap_or_else(|_| errors()) }
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn log_filters_leave_out_empty_directives() {
        use tracing::Level;
        let on = |spec, target, level| log_filter(spec).would_enable(target, &level);
        assert!(on(None, "tunebox", Level::ERROR) && !on(None, "tunebox", Level::WARN));
        assert!(on(Some(""), "tunebox", Level::ERROR), "empty is unset");
        assert!(on(Some("info,"), "tunebox", Level::INFO), "a trailing comma keeps info");
        assert!(on(Some(" info , librespot=debug"), "librespot", Level::DEBUG));
        assert!(!on(Some("info,librespot=debug"), "tunebox", Level::DEBUG));
        assert!(on(Some("=not a level"), "tunebox", Level::ERROR), "unreadable is unset");
    }

    #[test]
    fn links_go_to_their_service() {
        assert_eq!(link_service("https://open.spotify.com/album/2noRn2Aes5aoNVsU6iWThc"), Some(Service::Spotify));
        assert_eq!(link_service("spotify:track:1"), Some(Service::Spotify));
        assert_eq!(link_service("https://music.youtube.com/watch?v=dQw4w9WgXcQ"), Some(Service::YouTubeMusic));
        assert_eq!(link_service("https://www.youtube.com/watch?v=dQw4w9WgXcQ"), Some(Service::YouTubeMusic));
        assert_eq!(link_service("https://youtu.be/dQw4w9WgXcQ"), Some(Service::YouTubeMusic));
        assert_eq!(link_service("ytmusic:album:MPREb_1"), Some(Service::YouTubeMusic));
        assert_eq!(link_service("https://soundcloud.com/artist/song"), Some(Service::SoundCloud));
        assert_eq!(link_service("soundcloud:track:artist/song"), Some(Service::SoundCloud));
        assert_eq!(link_service("https://example.com/"), None);
    }

    #[test]
    fn says_which_services_are_built_in() {
        assert!(version().starts_with(env!("CARGO_PKG_VERSION")));
        assert_eq!(version().contains("spotify"), cfg!(feature = "spotify"));
        assert_eq!(version().contains("ytmusic"), cfg!(feature = "ytmusic"));
        Cli::command().debug_assert();
    }
}
