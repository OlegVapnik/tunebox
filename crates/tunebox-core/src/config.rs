use std::{fs, io, path::Path};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
pub use toml_edit::Value;

use crate::{paths::Paths, service::Service};

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub client_id: Option<String>,
    pub sp_dc: Option<String>,
    pub crossfade: Option<u64>,
    pub icons: Option<String>,
    pub credentials: Option<String>,
    pub notifications: Option<bool>,
    pub update_check: Option<bool>,
    pub covers: Option<bool>,
    pub power_saving: Option<String>,
    pub accent: Option<String>,
    pub colors: Option<String>,
    pub images: Option<String>,
    pub services: Option<Vec<String>>,
}

fn normalize_cookie(raw: &str) -> String {
    let mut value = raw.trim().to_owned();
    for _ in 0..4 {
        let trimmed = value.trim().trim_end_matches(';').trim();
        let lower = trimmed.to_ascii_lowercase();
        let stripped = ["cookie:", "sp_dc:", "sp_dc="]
            .iter()
            .find_map(|prefix| lower.strip_prefix(prefix).map(|_| &trimmed[prefix.len()..]))
            .unwrap_or(trimmed);
        let stripped = stripped.trim().trim_end_matches(';').trim();
        let unquoted = if stripped.len() >= 2
            && ((stripped.starts_with('"') && stripped.ends_with('"'))
                || (stripped.starts_with('\'') && stripped.ends_with('\'')))
        {
            &stripped[1..stripped.len() - 1]
        } else {
            stripped
        };
        if unquoted == value {
            break;
        }
        value = unquoted.to_owned();
    }
    value.trim().trim_end_matches(';').trim().to_owned()
}

impl Config {
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

    pub fn keychain(&self) -> Result<bool> {
        match self.credentials.as_deref() {
            None | Some("files") => Ok(false),
            Some("keychain") if tunebox_os::secrets::available() => Ok(true),
            Some("keychain") => bail!("credentials = \"keychain\" works on macOS only for now"),
            Some(other) => bail!("credentials = \"{other}\" is unknown: use \"keychain\" or \"files\""),
        }
    }

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

    pub fn sp_dc(&self, paths: &Paths) -> Result<String> {
        if let Some(cookie) = self.sp_dc.as_deref().map(normalize_cookie).filter(|c| !c.is_empty()) {
            return Ok(cookie);
        }
        if let Ok(cookie) = std::env::var("TUNEBOX_SPOTIFY_SP_DC") {
            let cookie = normalize_cookie(&cookie);
            if !cookie.is_empty() {
                return Ok(cookie);
            }
        }
        bail!(
            "No sp_dc cookie configured. Sign in to open.spotify.com in a browser, copy the sp_dc cookie, then \
             add `sp_dc = \"…\"` to {} or run `tunebox login spotify`.",
            paths.config_file().display()
        )
    }
}

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
    let target = fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
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
    fn clears_the_decorations_copied_with_a_cookie() {
        assert_eq!(normalize_cookie("  AQAPR1ReWl0Z  "), "AQAPR1ReWl0Z");
        assert_eq!(normalize_cookie("sp_dc:AQAPR1ReWl0Z"), "AQAPR1ReWl0Z");
        assert_eq!(normalize_cookie("sp_dc=\"AQAPR1ReWl0Z\""), "AQAPR1ReWl0Z");
        assert_eq!(normalize_cookie("sp_dc:'AQAPR1ReWl0Z'"), "AQAPR1ReWl0Z");
        assert_eq!(normalize_cookie("Cookie: sp_dc=AQAPR1ReWl0Z;"), "AQAPR1ReWl0Z");
        assert_eq!(normalize_cookie("\"sp_dc: 'AQAPR1ReWl0Z'\""), "AQAPR1ReWl0Z");
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
