
use std::{
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use tokio::process::Command;

use crate::paths::Paths;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Browser {
    pub name: String,
    pub spec: String,
}

struct Known {
    ids: &'static [&'static str],
    name: &'static str,
    reader: &'static str,
    dir: &'static str,
    fork: bool,
}

const fn known(ids: &'static [&'static str], name: &'static str, reader: &'static str, dir: &'static str) -> Known {
    Known { ids, name, reader, dir, fork: false }
}

const fn fork(ids: &'static [&'static str], name: &'static str, reader: &'static str, dir: &'static str) -> Known {
    Known { ids, name, reader, dir, fork: true }
}

#[cfg(target_os = "macos")]
const KNOWN: &[Known] = &[
    known(&["org.mozilla.firefox"], "Firefox", "firefox", "Library/Application Support/Firefox"),
    known(&["com.google.chrome"], "Google Chrome", "chrome", "Library/Application Support/Google/Chrome"),
    known(&["com.microsoft.edgemac"], "Microsoft Edge", "edge", "Library/Application Support/Microsoft Edge"),
    known(&["com.brave.browser"], "Brave", "brave", "Library/Application Support/BraveSoftware/Brave-Browser"),
    known(&["org.chromium.chromium"], "Chromium", "chromium", "Library/Application Support/Chromium"),
    known(&["com.vivaldi.vivaldi"], "Vivaldi", "vivaldi", "Library/Application Support/Vivaldi"),
    known(&["com.operasoftware.opera"], "Opera", "opera", "Library/Application Support/com.operasoftware.Opera"),
    known(&["com.apple.safari"], "Safari", "safari", "Library/Containers/com.apple.Safari"),
    fork(&["app.zen-browser.zen"], "Zen", "firefox", "Library/Application Support/zen"),
    fork(
        &["org.mozilla.librewolf", "io.gitlab.librewolf-community"],
        "LibreWolf",
        "firefox",
        "Library/Application Support/librewolf",
    ),
];
#[cfg(not(target_os = "macos"))]
const KNOWN: &[Known] = &[
    known(&["firefox.desktop"], "Firefox", "firefox", ".mozilla/firefox"),
    known(&["firefox.desktop"], "Firefox", "firefox", ".config/mozilla/firefox"),
    known(&["google-chrome.desktop"], "Google Chrome", "chrome", ".config/google-chrome"),
    known(&["microsoft-edge.desktop"], "Microsoft Edge", "edge", ".config/microsoft-edge"),
    known(&["brave-browser.desktop", "brave.desktop"], "Brave", "brave", ".config/BraveSoftware/Brave-Browser"),
    fork(&["brave-origin.desktop"], "Brave Origin", "brave", ".config/BraveSoftware/Brave-Origin"),
    known(&["chromium.desktop", "chromium-browser.desktop"], "Chromium", "chromium", ".config/chromium"),
    known(&["vivaldi-stable.desktop", "vivaldi.desktop"], "Vivaldi", "vivaldi", ".config/vivaldi"),
    known(&["opera.desktop"], "Opera", "opera", ".config/opera"),
    fork(&["zen.desktop", "zen-browser.desktop"], "Zen", "firefox", ".zen"),
    fork(&["librewolf.desktop"], "LibreWolf", "firefox", ".librewolf"),
    fork(&["floorp.desktop"], "Floorp", "firefox", ".floorp"),
];

impl Known {
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

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

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

pub fn default_browser() -> Option<Browser> {
    let id = default_browser_id()?;
    let home = home()?;
    KNOWN.iter().filter(|known| known.ids.contains(&id.as_str())).find_map(|known| known.found(&home))
}

#[cfg(not(target_os = "macos"))]
fn default_browser_id() -> Option<String> {
    let asks: [(&str, &[&str]); 3] = [
        ("xdg-settings", &["get", "default-web-browser"]),
        ("xdg-mime", &["query", "default", "x-scheme-handler/https"]),
        ("gio", &["mime", "https", "default"]),
    ];
    asks.into_iter().find_map(|(program, args)| {
        let output = std::process::Command::new(program).args(args).stderr(Stdio::null()).output().ok()?;
        let id = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        (output.status.success() && !id.is_empty()).then_some(id)
    })
}

#[cfg(target_os = "macos")]
fn default_browser_id() -> Option<String> {
    None
}

const EXPORT_TIMEOUT: Duration = Duration::from_secs(120);

pub async fn cookie(paths: &Paths, browser: &mut Browser, site: &str, domain: &str, name: &str) -> Result<Option<String>> {
    let dir = private_dir(paths)?;
    let file = dir.join("browser-export.txt");
    let (mut text, mut stderr) = export(&browser.spec, site, &file).await?;
    if let Some(value) = cookie_in(&text, domain, name) {
        let _ = fs::remove_file(&file);
        return Ok(Some(value));
    }
    if undecrypted(&stderr) && !browser.spec.contains('+') {
        let spec = with_keyring(&browser.spec, "gnomekeyring");
        (text, stderr) = export(&spec, site, &file).await?;
        if stderr.contains("secretstorage not available") {
            bail!(
                "yt-dlp needs Python's secretstorage to read {}'s cookies from the keyring: install \
                 python-secretstorage (Arch) or python3-secretstorage (Debian, Ubuntu, Fedora)",
                browser.name
            );
        }
        browser.spec = spec;
        if let Some(value) = cookie_in(&text, domain, name) {
            let _ = fs::remove_file(&file);
            return Ok(Some(value));
        }
    }
    let _ = fs::remove_file(&file);
    if undecrypted(&stderr) {
        bail!("yt-dlp can't decrypt {}'s cookies{}", browser.name, reason(&stderr));
    }
    if text.trim().is_empty() {
        bail!("No cookies found in {}{}", browser.name, reason(&stderr));
    }
    if locked(&stderr) {
        bail!(
            "{}'s cookie database is locked – close the browser for a second and try again{}",
            browser.name,
            reason(&stderr)
        );
    }
    Ok(None)
}

async fn export(spec: &str, site: &str, file: &Path) -> Result<(String, String)> {
    let _ = fs::remove_file(file);
    let output = Command::new("yt-dlp")
        .args(["--no-config", "--skip-download", "--cookies-from-browser", spec, "--cookies"])
        .arg(file)
        .arg(site)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output();
    let stderr = match tokio::time::timeout(EXPORT_TIMEOUT, output).await {
        Ok(Ok(output)) => String::from_utf8_lossy(&output.stderr).into_owned(),
        Ok(Err(err)) if err.kind() == std::io::ErrorKind::NotFound => {
            bail!("yt-dlp is not installed; it reads the browser's cookies")
        }
        Ok(Err(err)) => return Err(err).context("Can't run yt-dlp"),
        Err(_) => bail!("yt-dlp took too long to read the browser's cookies"),
    };
    let text = fs::read_to_string(file).unwrap_or_default();
    Ok((text, stderr))
}

fn private_dir(paths: &Paths) -> Result<PathBuf> {
    let dir = paths.cache.join("browser");
    fs::create_dir_all(&dir).context("Can't make the browser cache folder")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
    }
    Ok(dir)
}

fn undecrypted(stderr: &str) -> bool {
    stderr.contains("cannot decrypt") || stderr.contains("failed to decrypt")
}

fn locked(stderr: &str) -> bool {
    stderr.contains("database is locked") || stderr.contains("unable to open database file")
}

fn reason(stderr: &str) -> String {
    stderr
        .lines()
        .rfind(|line| line.starts_with("ERROR:") && !line.contains("Unsupported URL"))
        .map(|line| format!(": {}", line.trim_start_matches("ERROR:").trim()))
        .unwrap_or_default()
}

fn with_keyring(spec: &str, keyring: &str) -> String {
    match spec.split_once(':') {
        Some((browser, profile)) => format!("{browser}+{keyring}:{profile}"),
        None => format!("{spec}+{keyring}"),
    }
}

fn cookie_in(text: &str, domain: &str, name: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.strip_prefix("#HttpOnly_").unwrap_or(line);
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() < 7 {
            continue;
        }
        let (cookie_domain, cookie_name, value) = (fields[0], fields[5], fields[6]);
        let host = cookie_domain.trim_start_matches('.');
        let matches = domain == host || domain.strip_suffix(host).is_some_and(|rest| rest.ends_with('.'));
        if matches && cookie_name == name && !value.trim().is_empty() {
            return Some(value.trim().to_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_cookie_and_its_parent_domain() {
        let text = "# Netscape HTTP Cookie File\n\
                    .spotify.com\tTRUE\t/\tTRUE\t0\tsp_dc\tsecret\n\
                    .example.com\tTRUE\t/\tTRUE\t0\tsp_dc\tnope\n";
        assert_eq!(cookie_in(text, "open.spotify.com", "sp_dc").as_deref(), Some("secret"));
        assert_eq!(cookie_in(text, "spotify.com", "sp_dc").as_deref(), Some("secret"));
        assert_eq!(cookie_in(text, "notspotify.com", "sp_dc"), None);
    }

    #[test]
    fn reads_an_http_only_cookie() {
        let text = "#HttpOnly_.spotify.com\tTRUE\t/\tTRUE\t0\tsp_dc\t2-abc\n";
        assert_eq!(cookie_in(text, "open.spotify.com", "sp_dc").as_deref(), Some("2-abc"));
    }

    #[test]
    fn a_browser_without_the_cookie_has_none() {
        assert_eq!(cookie_in("# Netscape HTTP Cookie File\n", "open.spotify.com", "sp_dc"), None);
        assert_eq!(cookie_in(".spotify.com\tTRUE\t/\tTRUE\t0\tother\tx\n", "open.spotify.com", "sp_dc"), None);
    }

    #[test]
    fn knows_where_the_keyring_is() {
        assert_eq!(with_keyring("chrome", "gnomekeyring"), "chrome+gnomekeyring");
        assert_eq!(with_keyring("firefox:/home/o/.zen", "gnomekeyring"), "firefox+gnomekeyring:/home/o/.zen");
    }

    #[test]
    fn explains_a_locked_or_undecryptable_jar() {
        assert!(locked("ERROR: could not lock sqlite database: database is locked"));
        assert!(!locked("WARNING: [generic] Falling back on generic information extractor"));
        assert!(undecrypted("WARNING: cannot decrypt v11 cookies: no key found"));
    }
}
