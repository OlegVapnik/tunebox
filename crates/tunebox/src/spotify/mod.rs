//! The commands for Spotify: signing in, its checks for `tunebox doctor`, playing without the
//! interface, and a few for development.

mod playback;

use anyhow::{Result, bail};
use tunebox_core::{
    config::Config,
    doctor::{Check, Expect, Outcome, Section},
    paths::Paths,
};
use tunebox_spotify::{doctor, session, webapi::WebApi};

pub use self::playback::{connect, play};
use crate::{prompt, style};

/// Spotify's web player, where the `sp_dc` cookie lives.
const WEB_PLAYER: &str = "https://open.spotify.com";

/// Walks the user through copying the `sp_dc` cookie from a signed-in browser and saves it.
fn setup_sp_dc(paths: &Paths) -> Result<String> {
    if !prompt::interactive() {
        bail!(
            "No Spotify sp_dc cookie yet. Sign in to {WEB_PLAYER} in a browser, copy the sp_dc cookie, then run \
             `tunebox login spotify` again in a terminal. Or set TUNEBOX_SPOTIFY_SP_DC."
        );
    }
    println!("  {} Spotify web player", style::dim("1/2"));
    println!("  A browser opens open.spotify.com – sign in if needed.");
    println!("  {} open Developer Tools (F12) → Application → Cookies → open.spotify.com", style::bold("1."));
    println!("  {} find the {} cookie and copy its value", style::bold("2."), style::bold("sp_dc"));
    println!("  {}", style::dim("Paste it below; it is saved to your config.\n"));
    if open::that_detached(WEB_PLAYER).is_err() {
        println!("  {}\n", style::dim(&format!("Open {WEB_PLAYER} yourself.")));
    }
    loop {
        let Some(entered) = prompt::line("sp_dc")? else {
            bail!("No cookie entered; nothing was saved");
        };
        let cookie = entered.trim().to_owned();
        if !cookie.is_empty() && !cookie.contains(char::is_whitespace) {
            tunebox_core::config::store(paths, "sp_dc", Some(tunebox_core::config::Value::from(cookie.clone())))?;
            println!("  {} saved to {}\n", style::green("✓"), paths.config_file().display());
            return Ok(cookie);
        }
        println!("  {} paste the value of the sp_dc cookie; try again", style::red("✗"));
    }
}

/// Signs in to the Web API with a web-player cookie, then to librespot for streaming.
pub async fn login(paths: &Paths, config: &Config, force: bool) -> Result<()> {
    let sp_dc = match config.sp_dc(paths) {
        Ok(cookie) => cookie,
        Err(_) => setup_sp_dc(paths)?,
    };
    let api = WebApi::new(sp_dc, paths.webapi_token())?;

    println!("\n  {} Web API (web player)", style::dim("1/2"));
    if force || !api.is_logged_in().await {
        api.login().await?;
        println!("  {} connected\n", style::green("✓"));
    } else {
        println!("  {} already connected\n", style::green("✓"));
    }

    println!("  {} Streaming (librespot, Premium)", style::dim("2/2"));
    if force || !session::is_logged_in(paths)? {
        match session::login(paths).await {
            Ok(session) if session::is_premium(&session) => {
                println!("  {} connected as {}\n", style::green("✓"), session.username());
                session.shutdown();
            }
            Ok(session) => {
                // Spotify lets free accounts sign in here but refuses to play their songs,
                // so the credentials are no use; tunebox plays those through YouTube.
                session.shutdown();
                paths.streaming_credentials().delete()?;
                println!("  {} a free account: Spotify songs play through YouTube\n", style::cyan("•"));
            }
            Err(err) => println!(
                "  {} streaming isn't set up ({}); Spotify songs play through YouTube\n",
                style::cyan("•"),
                one_line(&err)
            ),
        }
    } else {
        println!("  {} already connected\n", style::green("✓"));
    }
    Ok(())
}

/// An error's first line, for a message inside a sentence.
fn one_line(err: &anyhow::Error) -> String {
    err.to_string().lines().next().unwrap_or_default().to_owned()
}

/// Whether both sign-ins are saved, the Web API's with the user's app and librespot's.
pub async fn signed_in(paths: &Paths, config: &Config) -> Result<bool> {
    let Ok(sp_dc) = config.sp_dc(paths) else { return Ok(false) };
    let api = WebApi::new(sp_dc, paths.webapi_token())?;
    Ok(api.is_logged_in().await)
}

/// What `tunebox doctor` finds through the Web API and through librespot, each section handed
/// to `report` as soon as it is done.
pub async fn doctor(paths: &Paths, config: &Config, mut report: impl FnMut(Section)) -> Result<()> {
    match config.sp_dc(paths) {
        Ok(sp_dc) => {
            let api = WebApi::new(sp_dc, paths.webapi_token())?;
            let section = if api.is_logged_in().await {
                doctor::web_api(&api).await
            } else {
                not_logged_in("WEB API · web player")
            };
            report(section);
        }
        Err(_) => report(not_logged_in("WEB API · web player")),
    }

    let internal = match session::prepare(paths) {
        Ok(pending) => match pending.connect().await {
            Ok(session) => {
                let pathfinder =
                    tunebox_spotify::pathfinder::Pathfinder::new(session.clone(), paths.pathfinder_hashes());
                let section = doctor::spclient(&session, &pathfinder).await;
                session.shutdown();
                section
            }
            Err(err) => failed_section("LIBRESPOT · Spotify internal", "Session", format!("{err:#}")),
        },
        Err(_) => not_logged_in("LIBRESPOT · Spotify internal"),
    };
    report(internal);
    Ok(())
}

fn not_logged_in(title: &'static str) -> Section {
    failed_section(title, "Login", "not logged in – run `tunebox login`".into())
}

fn failed_section(title: &'static str, name: &str, reason: String) -> Section {
    Section {
        title,
        checks: vec![Check { name: name.into(), expect: Expect::Available, outcome: Outcome::Failed(reason) }],
    }
}

/// Fetches the DJ's lines for the first tracks of a fresh DJ session, without playing anything.
pub async fn dj_sample(paths: &Paths, dir: &std::path::Path) -> Result<()> {
    use anyhow::Context as _;
    let session = session::prepare(paths)?.connect().await?;
    let spclient = session.spclient();
    let dj = spclient.get_context("spotify:playlist:37i9dQZF1EYkqdzj48dyYq").await?;
    println!("context metadata: {:#?}", dj.metadata);
    let lexicon = dj.metadata.get("lexicon_context_url").context("no lexicon_context_url")?;
    let context = spclient.get_context_url(lexicon).await?;
    std::fs::create_dir_all(dir)?;
    let mut count = 0;
    for track in context.pages.iter().flat_map(|page| &page.tracks) {
        let mut narration: Vec<_> = track.metadata.iter().filter(|(key, _)| key.starts_with("narration.")).collect();
        if narration.is_empty() {
            continue;
        }
        narration.sort();
        println!("\n{}", track.uri());
        for (key, value) in &narration {
            println!("  {key} = {value}");
        }
        let field = |key: String| track.metadata.get(&key).map(String::as_str).unwrap_or_default();
        for kind in ["intro", "outro"] {
            let ssml = field(format!("narration.{kind}.ssml"));
            if ssml.is_empty() {
                continue;
            }
            let voice = field(format!("narration.{kind}.voice"));
            let provider = field(format!("narration.{kind}.tts_provider"));
            let mp3 = spclient.get_narration(ssml, voice, provider).await?;
            let file = dir.join(format!("{count}-{kind}.mp3"));
            std::fs::write(&file, &mp3)?;
            println!("  -> {} ({} bytes)", file.display(), mp3.len());
        }
        count += 1;
        if count == 3 {
            break;
        }
    }
    session.shutdown();
    Ok(())
}

/// Runs a pathfinder query and prints the JSON.
pub async fn query(paths: &Paths, operation: &str, variables: &str) -> Result<()> {
    let session = session::prepare(paths)?.connect().await?;
    let pathfinder = tunebox_spotify::pathfinder::Pathfinder::new(session.clone(), paths.pathfinder_hashes());
    let data = pathfinder.query(operation, serde_json::from_str(variables)?).await?;
    println!("{}", serde_json::to_string_pretty(&data)?);
    session.shutdown();
    Ok(())
}
