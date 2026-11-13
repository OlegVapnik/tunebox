//! Reading SoundCloud's sign-in from a browser's cookies. SoundCloud keeps it in the
//! `oauth_token` cookie, which only the browser can hand over. yt-dlp reads every browser's
//! cookie store and keyring, so tunebox asks it for the cookies and picks that one out.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use tokio::process::Command;
use tunebox_core::paths::Paths;

/// A browser yt-dlp can read cookies from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Browser {
    /// The name people know, e.g. "Brave".
    pub name: String,
    /// What `--cookies-from-browser` takes, e.g. `brave`, or `firefox:<profiles>` for forks.
    pub spec: String,
}

/// A browser tunebox knows.
struct Known {
    name: &'static str,
    /// yt-dlp's name for it, or for the browser it is a fork of.
    reader: &'static str,
    /// Where it keeps its profiles, relative to the home directory.
    dir: &'static str,
    /// Whether yt-dlp needs the folder: it doesn't know forks.
    fork: bool,
}

const fn known(name: &'static str, reader: &'static str, dir: &'static str) -> Known {
    Known { name, reader, dir, fork: false }
}

const fn fork(name: &'static str, reader: &'static str, dir: &'static str) -> Known {
    Known { name, reader, dir, fork: true }
}

#[cfg(target_os = "macos")]
const KNOWN: &[Known] = &[
    known("Google Chrome", "chrome", "Library/Application Support/Google/Chrome"),
    known("Brave", "brave", "Library/Application Support/BraveSoftware/Brave-Browser"),
    known("Chromium", "chromium", "Library/Application Support/Chromium"),
    known("Microsoft Edge", "edge", "Library/Application Support/Microsoft Edge"),
    known("Vivaldi", "vivaldi", "Library/Application Support/Vivaldi"),
    known("Opera", "opera", "Library/Application Support/com.operasoftware.Opera"),
    known("Firefox", "firefox", "Library/Application Support/Firefox"),
    known("Safari", "safari", "Library/Containers/com.apple.Safari"),
    fork("Zen", "firefox", "Library/Application Support/zen"),
    fork("LibreWolf", "firefox", "Library/Application Support/librewolf"),
];
#[cfg(not(target_os = "macos"))]
const KNOWN: &[Known] = &[
    known("Chromium", "chromium", ".config/chromium"),
    known("Brave", "brave", ".config/BraveSoftware/Brave-Browser"),
    // Omarchy's Brave, which keeps its profiles apart from Brave's.
    fork("Brave Origin", "brave", ".config/BraveSoftware/Brave-Origin"),
    known("Google Chrome", "chrome", ".config/google-chrome"),
    known("Microsoft Edge", "edge", ".config/microsoft-edge"),
    known("Vivaldi", "vivaldi", ".config/vivaldi"),
    known("Opera", "opera", ".config/opera"),
    known("Firefox", "firefox", ".mozilla/firefox"),
    known("Firefox", "firefox", ".config/mozilla/firefox"),
    fork("Zen", "firefox", ".zen"),
    fork("LibreWolf", "firefox", ".librewolf"),
    fork("Floorp", "firefox", ".floorp"),
];

impl Known {
    /// The browser, when it has a profile in its folder: a folder alone may be left over, or
    /// made by another program.
    fn found(&self, home: &Path) -> Option<Browser> {
        let dir = home.join(self.dir);
        let profile = match self.reader {
            "firefox" => dir.join("profiles.ini").is_file(),
            "safari" => dir.is_dir(),
            _ => dir.join("Local State").is_file(),
        };
        let spec = if self.fork { format!("{}:{}", self.reader, dir.display()) } else { self.reader.into() };
        profile.then(|| Browser { name: self.name.into(), spec })
    }
}

/// yt-dlp exports the cookies of every site; this many seconds is plenty for that.
const EXPORT_TIMEOUT: Duration = Duration::from_secs(120);

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// The browsers on this computer that have a profile yt-dlp could read cookies from.
pub fn browsers() -> Vec<Browser> {
    let Some(home) = home() else { return Vec::new() };
    let mut found: Vec<Browser> = Vec::new();
    for browser in KNOWN.iter().filter_map(|known| known.found(&home)) {
        if !found.iter().any(|b| b.name == browser.name) {
            found.push(browser);
        }
    }
    found
}

/// The `oauth_token` of a browser signed in to SoundCloud, with the browser's name.
pub async fn oauth_token(paths: &Paths) -> Result<Option<(String, String)>> {
    let mut last: Option<anyhow::Error> = None;
    for browser in browsers() {
        match token_from(paths, &browser.spec).await {
            Ok(Some(token)) => return Ok(Some((browser.name, token))),
            Ok(None) => {}
            Err(err) if format!("{err:#}").contains("not installed") => return Err(err),
            Err(err) => last = Some(err),
        }
    }
    if let Some(err) = last {
        tracing::debug!("SoundCloud's cookies: {err:#}");
    }
    Ok(None)
}

/// The `oauth_token` in one browser's cookies, if it is signed in to SoundCloud.
async fn token_from(paths: &Paths, spec: &str) -> Result<Option<String>> {
    let dir = paths.cache.join("soundcloud");
    fs::create_dir_all(&dir).context("Can't make the SoundCloud cache folder")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
    }
    let file = dir.join("browser-export.txt");
    let (token, stderr) = export(spec, &file).await?;
    if token.is_some() {
        return Ok(token);
    }
    // Chromium keeps its cookie key in the keyring, which yt-dlp needs to be told about.
    if undecrypted(&stderr) && !spec.contains('+') {
        let (token, _) = export(&with_keyring(spec), &file).await?;
        return Ok(token);
    }
    Ok(None)
}

/// Runs yt-dlp for the browser's cookies, and reads `oauth_token` out of them.
async fn export(spec: &str, file: &Path) -> Result<(Option<String>, String)> {
    let _ = fs::remove_file(file);
    let output = Command::new("yt-dlp")
        .args(["--no-config", "--skip-download", "--cookies-from-browser", spec, "--cookies"])
        .arg(file)
        .arg("https://soundcloud.com/")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output();
    let stderr = match tokio::time::timeout(EXPORT_TIMEOUT, output).await {
        Ok(Ok(output)) => String::from_utf8_lossy(&output.stderr).into_owned(),
        Ok(Err(err)) if err.kind() == std::io::ErrorKind::NotFound => {
            bail!("yt-dlp is not installed; it reads the browser's SoundCloud sign-in")
        }
        Ok(Err(err)) => return Err(err).context("Can't run yt-dlp"),
        Err(_) => bail!("yt-dlp took too long to read the browser's cookies"),
    };
    let text = fs::read_to_string(file).unwrap_or_default();
    let _ = fs::remove_file(file);
    Ok((token_in(&text), stderr))
}

fn undecrypted(stderr: &str) -> bool {
    stderr.contains("cannot decrypt") || stderr.contains("failed to decrypt")
}

/// Adds the keyring yt-dlp should read Chromium's cookie key from.
fn with_keyring(spec: &str) -> String {
    match spec.split_once(':') {
        Some((reader, profile)) => format!("{reader}+gnomekeyring:{profile}"),
        None => format!("{spec}+gnomekeyring"),
    }
}

/// The `oauth_token` in a Netscape cookies.txt, as yt-dlp writes it.
fn token_in(text: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.strip_prefix("#HttpOnly_").unwrap_or(line);
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() < 7 {
            continue;
        }
        let (domain, name, value) = (fields[0], fields[5], fields[6]);
        if domain.ends_with("soundcloud.com") && name == "oauth_token" && !value.trim().is_empty() {
            return Some(value.trim().to_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_token_out_of_a_cookies_file() {
        let text = "# Netscape HTTP Cookie File\n\
                    .soundcloud.com\tTRUE\t/\tTRUE\t0\toauth_token\t2-1234-5678\n\
                    .example.com\tTRUE\t/\tTRUE\t0\toauth_token\tnope\n";
        assert_eq!(token_in(text).as_deref(), Some("2-1234-5678"));
    }

    #[test]
    fn reads_the_token_of_an_http_only_cookie() {
        let text = "#HttpOnly_.soundcloud.com\tTRUE\t/\tTRUE\t0\toauth_token\t2-abc\n";
        assert_eq!(token_in(text).as_deref(), Some("2-abc"));
    }

    #[test]
    fn a_browser_without_the_token_has_none() {
        assert_eq!(token_in("# Netscape HTTP Cookie File\n"), None);
        assert_eq!(token_in(".soundcloud.com\tTRUE\t/\tTRUE\t0\tother\t2-abc\n"), None);
    }

    #[test]
    fn knows_where_the_keyring_is() {
        assert_eq!(with_keyring("chrome"), "chrome+gnomekeyring");
        assert_eq!(with_keyring("firefox:/home/o/.zen"), "firefox+gnomekeyring:/home/o/.zen");
    }
}
