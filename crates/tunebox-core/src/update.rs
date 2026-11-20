
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::paths::Paths;

const REPO: &str = "OlegVapnik/tunebox";
pub const REPO_URL: &str = "https://github.com/OlegVapnik/tunebox";
const USER_AGENT: &str = concat!("tunebox/", env!("CARGO_PKG_VERSION"));
const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(5 * 60);
pub const INSTALLER: &str = "curl -fsSL https://raw.githubusercontent.com/OlegVapnik/tunebox/main/install.sh | bash";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageManager {
    Homebrew,
    System,
    Nix,
    Mise,
}

impl PackageManager {
    pub fn of_running() -> Option<Self> {
        Self::of(&std::env::current_exe().ok()?.canonicalize().ok()?)
    }

    fn of(path: &Path) -> Option<Self> {
        if path.starts_with("/nix/store") {
            Some(Self::Nix)
        } else if path.components().any(|part| part.as_os_str() == "Cellar") {
            Some(Self::Homebrew)
        } else if path.starts_with("/usr/bin") {
            Some(Self::System)
        } else if path
            .components()
            .map(|part| part.as_os_str())
            .collect::<Vec<_>>()
            .windows(2)
            .any(|pair| pair == ["mise", "installs"])
        {
            Some(Self::Mise)
        } else {
            None
        }
    }

    pub fn how_to_update(self, variant: &str) -> String {
        match self {
            Self::Homebrew => format!("Update with brew upgrade {}", homebrew_formula(variant)),
            Self::System => "Update with your package manager".into(),
            Self::Nix => "Update with Nix".into(),
            Self::Mise => "Update with mise upgrade".into(),
        }
    }
}

fn homebrew_formula(variant: &str) -> String {
    match variant {
        "ytmusic" | "full" => format!("tunebox-{variant}"),
        _ => "tunebox".into(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    pub tag: String,
}

impl Release {
    pub fn version(&self) -> &str {
        self.tag.trim_start_matches('v')
    }
}

pub fn current_version() -> String {
    std::env::var("TUNEBOX_PRETEND_VERSION").unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_owned())
}

pub async fn check(paths: &Paths, now: bool) -> Result<Option<Release>> {
    let file = paths.update_check();
    let remembered = fs::read(&file)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Checked>(&bytes).ok())
        .filter(|checked| !now && seconds_since_epoch().saturating_sub(checked.at) < CHECK_INTERVAL.as_secs());
    let latest = match remembered {
        Some(checked) => checked.latest,
        None => {
            let latest = latest().await?;
            let checked = Checked { at: seconds_since_epoch(), latest: latest.clone() };
            if let Err(err) = remember(paths, &checked) {
                tracing::warn!("Couldn't remember the update check: {err}");
            }
            latest
        }
    };
    Ok(newer(latest.version(), &current_version()).then_some(latest))
}

fn remember(paths: &Paths, checked: &Checked) -> io::Result<()> {
    fs::create_dir_all(&paths.cache)?;
    fs::write(paths.update_check(), serde_json::to_vec(checked)?)
}

#[derive(Serialize, Deserialize)]
struct Checked {
    at: u64,
    latest: Release,
}

fn seconds_since_epoch() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

pub async fn latest() -> Result<Release> {
    #[derive(Deserialize)]
    struct Latest {
        tag_name: String,
    }
    let latest: Latest = crate::http::client(USER_AGENT)
        .build()?
        .get(format!("https://api.github.com/repos/{REPO}/releases/latest"))
        .header("accept", "application/vnd.github+json")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(Release { tag: latest.tag_name })
}

pub fn newer(version: &str, current: &str) -> bool {
    fn parts(version: &str) -> Option<(u64, u64, u64)> {
        let core = version.trim_start_matches('v').split(['-', '+']).next()?;
        let mut numbers = core.split('.').map(str::parse);
        Some((numbers.next()?.ok()?, numbers.next()?.ok()?, numbers.next().unwrap_or(Ok(0)).ok()?))
    }
    matches!((parts(version), parts(current)), (Some(version), Some(current)) if version > current)
}

fn target() -> Option<&'static str> {
    Some(match (std::env::consts::ARCH, std::env::consts::OS) {
        ("x86_64", "linux") => "x86_64-unknown-linux-gnu",
        ("aarch64", "linux") => "aarch64-unknown-linux-gnu",
        ("x86_64", "macos") => "x86_64-apple-darwin",
        ("aarch64", "macos") => "aarch64-apple-darwin",
        _ => return None,
    })
}

fn archive_name(variant: &str, target: &str) -> String {
    format!("tunebox-{variant}-{target}.tar.gz")
}

pub async fn install(release: &Release, variant: &str, mut progress: impl FnMut(u64, Option<u64>)) -> Result<PathBuf> {
    if let Some(manager) = PackageManager::of_running() {
        bail!("A package manager installed tunebox, so it doesn't replace itself. {}", manager.how_to_update(variant))
    }
    let Some(target) = target() else {
        bail!("There is no prebuilt tunebox for this system; update with `cargo install` instead")
    };
    let binary = std::env::current_exe()?.canonicalize()?;
    let dir = binary.parent().context("The tunebox binary has no folder")?;
    let mut staged = Staged::create(dir)?;

    let archive_name = archive_name(variant, target);
    let base = format!("https://github.com/{REPO}/releases/download/{}", release.tag);
    let http = crate::http::client(USER_AGENT).timeout(DOWNLOAD_TIMEOUT).build()?;
    let mut response = http.get(format!("{base}/{archive_name}")).send().await?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        bail!("tunebox {} has no {variant} build for {target}", release.version());
    }
    response = response.error_for_status()?;
    let total = response.content_length();
    let mut archive = Vec::with_capacity(total.unwrap_or_default() as usize);
    while let Some(chunk) = response.chunk().await? {
        archive.extend_from_slice(&chunk);
        progress(archive.len() as u64, total);
    }
    let checksums = http.get(format!("{base}/{archive_name}.sha256")).send().await?.error_for_status()?.text().await?;
    let expected = checksums.split_whitespace().next().context("The release has an empty checksum")?;
    if !expected.eq_ignore_ascii_case(&hex(&Sha256::digest(&archive))) {
        bail!("The download doesn't match its checksum, so nothing was changed");
    }

    staged.write(&unpack(&archive)?, &binary)?;
    if !reports_version(&staged.path, release.version()).await {
        bail!("tunebox {} doesn't start on this system, so nothing was changed", release.version());
    }
    staged.replace(&binary)?;
    Ok(binary)
}

async fn reports_version(binary: &Path, version: &str) -> bool {
    let output = tokio::process::Command::new(binary).arg("--version").output().await;
    output.is_ok_and(|output| output.status.success() && String::from_utf8_lossy(&output.stdout).contains(version))
}

fn unpack(archive: &[u8]) -> Result<Vec<u8>> {
    let mut entries = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    for entry in entries.entries()? {
        let mut entry = entry?;
        if entry.header().entry_type().is_file() && entry.path()?.as_ref() == Path::new("tunebox") {
            let mut binary = Vec::new();
            entry.read_to_end(&mut binary)?;
            return Ok(binary);
        }
    }
    bail!("The release archive holds no tunebox binary")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

struct Staged {
    path: PathBuf,
    file: Option<fs::File>,
}

impl Staged {
    fn create(dir: &Path) -> Result<Self> {
        let path = dir.join(format!(".tunebox-update-{}", std::process::id()));
        let file = fs::OpenOptions::new().write(true).create(true).truncate(true).open(&path);
        match file {
            Ok(file) => Ok(Self { path, file: Some(file) }),
            Err(err) if matches!(err.kind(), io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem) => {
                bail!("tunebox can't write to {}. Update with the installer instead:\n  {INSTALLER}", dir.display())
            }
            Err(err) => Err(err).with_context(|| format!("Couldn't write to {}", dir.display())),
        }
    }

    fn write(&mut self, binary: &[u8], replaces: &Path) -> Result<()> {
        let mut file = self.file.take().context("Already written")?;
        file.write_all(binary)?;
        file.sync_all()?;
        drop(file);
        fs::set_permissions(&self.path, fs::metadata(replaces)?.permissions())?;
        Ok(())
    }

    fn replace(self, binary: &Path) -> Result<()> {
        fs::rename(&self.path, binary).with_context(|| format!("Couldn't replace {}", binary.display()))
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions_by_their_numbers() {
        assert!(newer("v0.3.0", "0.2.0"));
        assert!(newer("0.10.0", "0.9.9"));
        assert!(newer("1.0.0", "0.99.0"));
        assert!(!newer("v0.2.0", "0.2.0"));
        assert!(!newer("0.1.9", "0.2.0"));
        assert!(!newer("0.2.0-beta", "0.2.0"));
        assert!(!newer("nightly", "0.2.0"));
    }

    #[test]
    fn knows_package_managers_by_where_the_binary_is() {
        let of = |path: &str| PackageManager::of(Path::new(path));
        assert_eq!(of("/opt/homebrew/Cellar/tunebox/0.3.0/bin/tunebox"), Some(PackageManager::Homebrew));
        assert_eq!(of("/usr/local/Cellar/tunebox/0.3.0/bin/tunebox"), Some(PackageManager::Homebrew));
        assert_eq!(of("/home/linuxbrew/.linuxbrew/Cellar/tunebox/0.3.0/bin/tunebox"), Some(PackageManager::Homebrew));
        assert_eq!(of("/usr/bin/tunebox"), Some(PackageManager::System));
        assert_eq!(
            of("/home/sam/.local/share/mise/installs/github-OlegVapnik-tunebox/0.3.0/tunebox"),
            Some(PackageManager::Mise)
        );
        assert_eq!(of("/home/sam/.local/bin/tunebox"), None);
        assert_eq!(
            of("/nix/store/0ac6p7kvsyhwlnh7z7mfqmzjy3zcvqqg-tunebox-0.3.0/bin/tunebox"),
            Some(PackageManager::Nix)
        );
        assert_eq!(of("/home/me/.local/bin/tunebox"), None);
        assert_eq!(of("/usr/local/bin/tunebox"), None);
        assert_eq!(of("/home/me/.cargo/bin/tunebox"), None);
        assert_eq!(of("/home/me/tunebox/target/release/tunebox"), None);
        assert_eq!(of("/usr/bin-old/tunebox"), None);
    }

    #[test]
    fn homebrew_upgrades_the_formula_of_the_build() {
        let brew = |variant| PackageManager::Homebrew.how_to_update(variant);
        assert_eq!(brew("spotify"), "Update with brew upgrade tunebox");
        assert_eq!(brew("ytmusic"), "Update with brew upgrade tunebox-ytmusic");
        assert_eq!(brew("full"), "Update with brew upgrade tunebox-full");
        assert_eq!(PackageManager::Mise.how_to_update("full"), "Update with mise upgrade");
    }

    #[test]
    fn each_variant_downloads_its_own_archive() {
        assert_eq!(
            archive_name("spotify", "x86_64-unknown-linux-gnu"),
            "tunebox-spotify-x86_64-unknown-linux-gnu.tar.gz"
        );
        assert_eq!(archive_name("full", "aarch64-apple-darwin"), "tunebox-full-aarch64-apple-darwin.tar.gz");
    }

    #[test]
    fn unpacks_the_binary_from_a_release_archive() {
        let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast()));
        for (name, content) in [("LICENSE", &b"MIT"[..]), ("tunebox", &b"\x7fELF binary"[..])] {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            tar.append_data(&mut header, name, content).unwrap();
        }
        let archive = tar.into_inner().unwrap().finish().unwrap();
        assert_eq!(unpack(&archive).unwrap(), b"\x7fELF binary");
    }
}
