use std::{fs, io, path::Path};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
/// A value to [`store`] in the config file.
pub use toml_edit::Value;

use crate::{paths::Paths, service::Service};

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Client ID of the user's own app from developer.spotify.com (kept for librespot streaming).
    pub client_id: Option<String>,
    /// The `sp_dc` cookie of a browser signed in to Spotify's web player, used for Web API calls.
    pub sp_dc: Option<String>,
    /// Seconds consecutive tracks overlap (0–12); off when missing.
    pub crossfade: Option<u64>,
    /// `"nerd"` or `"text"`; detected from the terminal when missing.
    pub icons: Option<String>,
    /// `"keychain"` keeps sign-ins in the system's keychain; files when missing.
    pub credentials: Option<String>,
    /// Desktop notifications for new tracks while tunebox is in the background; on when missing.
    pub notifications: Option<bool>,
    /// Looks for a new release at start and offers to update; on when missing.
    pub update_check: Option<bool>,
    /// Cover art on every page and in the player; on when missing.
    pub covers: Option<bool>,
    /// `"on"` or `"off"`: fewer redraws and no animations; follows the system when missing.
    pub power_saving: Option<String>,
    /// A color name such as `"violet"`; follows the playing cover when missing.
    pub accent: Option<String>,
    /// `"tunebox"` or `"terminal"`: tunebox's own colors, or the terminal's background and text.
    /// Omarchy's theme where there is one, tunebox's own elsewhere, when missing.
    pub colors: Option<String>,
    /// How covers reach the terminal: `"kitty"`, `"sixel"`, `"iterm2"` or `"blocks"`; detected
    /// from the terminal when missing.
    pub images: Option<String>,
    /// The music services to use, by key: `["spotify", "ytmusic"]`. The installer writes it;
    /// every service built in when missing.
    pub services: Option<Vec<String>>,
}

impl Config {
    /// Reads `config.toml`; `TUNEBOX_CLIENT_ID` overrides the file.
    pub fn load(paths: &Paths) -> Result<Self> {
        let path = paths.config_file();
        let mut config = match fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).with_context(|| format!("{} is not valid TOML", path.display()))?,
            Err(err) if err.kind() == io::ErrorKind::NotFound => Self::default(),
            Err(err) => return Err(err).with_context(|| format!("{} is not readable", path.display())),
        };
        if let Ok(id) = std::env::var("TUNEBOX_CLIENT_ID") {
            config.client_id = Some(id);
        }
        if let Ok(cookie) = std::env::var("TUNEBOX_SPOTIFY_SP_DC") {
            config.sp_dc = Some(cookie);
        }
        Ok(config)
    }

    /// Whether sign-ins go to the system's keychain rather than files.
    pub fn keychain(&self) -> Result<bool> {
        match self.credentials.as_deref() {
            None | Some("files") => Ok(false),
            Some("keychain") if tunebox_os::secrets::available() => Ok(true),
            Some("keychain") => bail!("credentials = \"keychain\" works on macOS only for now"),
            Some(other) => bail!("credentials = \"{other}\" is unknown: use \"keychain\" or \"files\""),
        }
    }

    /// The services to use, of those `compiled` in: the ones `services` lists, in its order, or
    /// all of them when it is missing. The config file is shared by every build, so it may name
    /// a service this one lacks; that one is left out.
    pub fn enabled_services(&self, compiled: &[Service]) -> Vec<Service> {
        let Some(keys) = &self.services else { return compiled.to_vec() };
        let mut enabled = Vec::new();
        for key in keys {
            match Service::from_key(key) {
                Some(service) if !compiled.contains(&service) => {
                    tracing::warn!("{} is enabled but not built into this tunebox", service.name());
                }
                Some(service) if !enabled.contains(&service) => enabled.push(service),
                Some(_) => {}
                None => tracing::warn!("services: \"{key}\" is not a music service tunebox knows"),
            }
        }
        enabled
    }

    /// The Spotify app's client ID, from the config or from `TUNEBOX_SPOTIFY_CLIENT_ID`, which
    /// helps where the config is managed elsewhere.
    pub fn client_id(&self, paths: &Paths) -> Result<String> {
        if let Some(id) = self.client_id.as_deref().map(str::trim).filter(|id| !id.is_empty()) {
            return Ok(id.to_owned());
        }
        if let Ok(id) = std::env::var("TUNEBOX_SPOTIFY_CLIENT_ID") {
            let id = id.trim();
            if !id.is_empty() {
                return Ok(id.to_owned());
            }
        }
        bail!(
            "No client ID configured. Add it to {}:\n\n  client_id = \"…\"\n\nor set TUNEBOX_SPOTIFY_CLIENT_ID. \
             `tunebox login spotify` walks you through making a Spotify app.",
            paths.config_file().display()
        )
    }

    /// The `sp_dc` cookie of Spotify's web player, from the config or `TUNEBOX_SPOTIFY_SP_DC`.
    pub fn sp_dc(&self, paths: &Paths) -> Result<String> {
        if let Some(cookie) = self.sp_dc.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
            return Ok(cookie.to_owned());
        }
        if let Ok(cookie) = std::env::var("TUNEBOX_SPOTIFY_SP_DC") {
            let cookie = cookie.trim();
            if !cookie.is_empty() {
                return Ok(cookie.to_owned());
            }
        }
        bail!(
            "No sp_dc cookie configured. Sign in to open.spotify.com in a browser, copy the sp_dc cookie, then \
             add `sp_dc = \"…\"` to {} or run `tunebox login spotify`.",
            paths.config_file().display()
        )
    }
}

/// Sets `key` in the config file, or removes it with `None` so the default applies. The rest
/// of the file stays as it was, comments included.
pub fn store(paths: &Paths, key: &str, value: Option<Value>) -> Result<()> {
    let path = paths.config_file();
    store_at(&path, key, value).with_context(|| format!("Couldn't save the setting to {}", path.display()))
}

fn store_at(path: &Path, key: &str, value: Option<Value>) -> Result<()> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err.into()),
    };
    let mut document: toml_edit::DocumentMut = text.parse()?;
    match value {
        Some(value) => document[key] = toml_edit::value(value),
        None => {
            document.remove(key);
        }
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    // A config linked from elsewhere, such as a dotfiles repository, stays linked: the file it
    // points to gets the change.
    let target = fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    // Write and rename, so a crash mid-write never leaves a broken file behind.
    let temp = target.with_extension("toml.tmp");
    fs::write(&temp, document.to_string())?;
    fs::rename(temp, target)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credentials(value: Option<&str>) -> Config {
        Config { credentials: value.map(Into::into), ..Config::default() }
    }

    #[test]
    fn keeps_sign_ins_in_files_unless_asked() {
        assert!(!credentials(None).keychain().unwrap());
        assert!(!credentials(Some("files")).keychain().unwrap());
        assert_eq!(credentials(Some("keychain")).keychain().is_ok(), cfg!(target_os = "macos"));
        assert!(credentials(Some("vault")).keychain().is_err());
    }

    fn services(keys: Option<&[&str]>) -> Config {
        Config { services: keys.map(|keys| keys.iter().map(|key| key.to_string()).collect()), ..Config::default() }
    }

    #[test]
    fn enables_every_service_built_in_unless_told_otherwise() {
        let both = [Service::Spotify, Service::YouTubeMusic];
        assert_eq!(services(None).enabled_services(&both), both);
        assert_eq!(services(None).enabled_services(&[]), []);
        assert_eq!(services(Some(&["ytmusic"])).enabled_services(&both), [Service::YouTubeMusic]);
        assert_eq!(services(Some(&[])).enabled_services(&both), []);
    }

    #[test]
    fn leaves_out_services_this_build_lacks() {
        let listed = services(Some(&["ytmusic", "deezer", "spotify", "spotify"]));
        assert_eq!(listed.enabled_services(&[Service::Spotify]), [Service::Spotify]);
        assert_eq!(
            listed.enabled_services(&[Service::Spotify, Service::YouTubeMusic]),
            [Service::YouTubeMusic, Service::Spotify]
        );
        let parsed: Config = toml::from_str("services = [\"spotify\"]").unwrap();
        assert_eq!(parsed.enabled_services(&[Service::Spotify, Service::YouTubeMusic]), [Service::Spotify]);
    }

    #[test]
    fn stores_settings_and_keeps_the_rest() {
        let dir = std::env::temp_dir().join(format!("tunebox-config-{}", std::process::id()));
        let path = dir.join("config.toml");
        fs::create_dir_all(&dir).unwrap();
        fs::write(&path, "# my app\nclient_id = \"abc\" # from the dashboard\ncrossfade = 6\n").unwrap();

        store_at(&path, "covers", Some(false.into())).unwrap();
        store_at(&path, "crossfade", None).unwrap();
        store_at(&path, "accent", Some("violet".into())).unwrap();

        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# my app\nclient_id = \"abc\" # from the dashboard\n"), "{text}");
        let config: Config = toml::from_str(&text).unwrap();
        assert_eq!(config.covers, Some(false));
        assert_eq!(config.crossfade, None);
        assert_eq!(config.accent.as_deref(), Some("violet"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn keeps_a_linked_config_linked() {
        let dir = std::env::temp_dir().join(format!("tunebox-config-link-{}", std::process::id()));
        let (real, link) = (dir.join("dotfiles.toml"), dir.join("config.toml"));
        fs::create_dir_all(&dir).unwrap();
        fs::write(&real, "client_id = \"abc\"\n").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();

        store_at(&link, "covers", Some(false.into())).unwrap();

        assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(fs::read_to_string(&real).unwrap(), "client_id = \"abc\"\ncovers = false\n");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn creates_the_file_for_the_first_setting() {
        let dir = std::env::temp_dir().join(format!("tunebox-config-new-{}", std::process::id()));
        let path = dir.join("config.toml");
        store_at(&path, "covers", Some(false.into())).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "covers = false\n");
        fs::remove_dir_all(dir).unwrap();
    }
}
