
use std::{
    collections::hash_map::DefaultHasher,
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use tokio::process::Command;

const TIMEOUT: Duration = Duration::from_secs(40);
const MARGIN: Duration = Duration::from_secs(30 * 60);
const LIFETIME: Duration = Duration::from_secs(5 * 60 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BotCheck;

impl std::fmt::Display for BotCheck {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "YouTube asks this connection to prove it isn't a bot, so songs can't play for now. It usually \
             passes within a few hours; another network helps sooner.",
        )
    }
}

impl std::error::Error for BotCheck {}

#[derive(Debug, Clone)]
pub struct Stream {
    pub url: String,
    pub until: Instant,
}

impl Stream {
    pub fn fresh(&self) -> bool {
        Instant::now() < self.until
    }
}

struct TempCookies(Option<PathBuf>);

impl Drop for TempCookies {
    fn drop(&mut self) {
        if let Some(file) = self.0.take() {
            let _ = fs::remove_file(file);
        }
    }
}

pub async fn resolve(target: &str, cache_dir: Option<PathBuf>, service: &str) -> Result<Stream> {
    resolve_with(target, cache_dir, service, None).await
}

pub async fn resolve_with(
    target: &str,
    cache_dir: Option<PathBuf>,
    service: &str,
    cookies: Option<&Path>,
) -> Result<Stream> {
    if let Some(dir) = &cache_dir
        && let Some(stream) = read_cache(dir, target)
    {
        tracing::debug!("resolved {target} from disk cache");
        return Ok(stream);
    }

    let mut command = Command::new("yt-dlp");
    command.args(["--no-config", "--no-playlist", "--no-warnings", "--quiet", "--no-progress", "-f", "bestaudio/best", "-g"]);
    if let Some(dir) = &cache_dir {
        command.arg("--cache-dir").arg(dir);
    }
    let guard = TempCookies(cookies.filter(|file| file.is_file()).and_then(|file| {
        let temp = file.with_extension("yt-dlp.tmp");
        std::fs::copy(file, &temp).is_ok().then_some(temp)
    }));
    if let Some(file) = guard.0.as_deref() {
        command.arg("--cookies").arg(file);
    }
    command.arg(target).stdin(Stdio::null()).kill_on_drop(true);
    let output = match tokio::time::timeout(TIMEOUT, command.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(err)) if err.kind() == std::io::ErrorKind::NotFound => {
            bail!("yt-dlp is not installed; {service} streams come from it")
        }
        Ok(Err(err)) => return Err(err).context("Can't run yt-dlp"),
        Err(_) => bail!("yt-dlp took too long"),
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let url = stdout.lines().find(|line| line.starts_with("http"));
    match url {
        Some(url) if output.status.success() => {
            let stream = Stream { url: url.to_owned(), until: valid_until(url) };
            if let Some(dir) = &cache_dir {
                write_cache(dir, target, &stream);
            }
            Ok(stream)
        }
        _ => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if is_bot_check(&stderr) {
                return Err(BotCheck.into());
            }
            let reason = stderr.lines().rfind(|line| !line.trim().is_empty()).unwrap_or("no URL").trim();
            bail!("{}", reason.strip_prefix("ERROR: ").unwrap_or(reason))
        }
    }
}

fn cache_path(dir: &PathBuf, target: &str) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    target.hash(&mut hasher);
    dir.join("stream-cache").join(format!("{:016x}.json", hasher.finish()))
}

fn read_cache(dir: &PathBuf, target: &str) -> Option<Stream> {
    let path = cache_path(dir, target);
    let data = fs::read_to_string(&path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&data).ok()?;
    let url = value.get("url")?.as_str()?.to_owned();
    let until_epoch = value.get("until")?.as_u64()?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    if until_epoch <= now + 60 {
        let _ = fs::remove_file(&path);
        return None;
    }
    Some(Stream { url, until: Instant::now() + Duration::from_secs(until_epoch - now) })
}

fn write_cache(dir: &PathBuf, target: &str, stream: &Stream) {
    let path = cache_path(dir, target);
    if fs::create_dir_all(path.parent().unwrap_or(dir)).is_err() {
        return;
    }
    let until_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
        + stream.until.saturating_duration_since(Instant::now()).as_secs();
    let value = serde_json::json!({ "url": stream.url, "until": until_epoch });
    let _ = fs::write(&path, value.to_string());
}

fn is_bot_check(stderr: &str) -> bool {
    stderr.contains("confirm you’re not a bot") || stderr.contains("confirm you're not a bot")
}

fn valid_until(url: &str) -> Instant {
    let expires = url::Url::parse(url)
        .ok()
        .and_then(|url| url.query_pairs().find(|(key, _)| key == "expire").and_then(|(_, v)| v.parse::<u64>().ok()));
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let lifetime = match expires {
        Some(expires) => Duration::from_secs(expires.saturating_sub(now)).saturating_sub(MARGIN).min(LIFETIME * 2),
        None => LIFETIME,
    };
    Instant::now() + lifetime
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bot_check_is_told_apart() {
        let stderr = "ERROR: [youtube] plnfIj7dkJE: Sign in to confirm you’re not a bot. Use --cookies-from-browser";
        assert!(is_bot_check(stderr));
        assert!(!is_bot_check("ERROR: [youtube] plnfIj7dkJE: Video unavailable"));
    }

    #[test]
    fn urls_are_good_until_shortly_before_they_expire() {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let url = format!("https://rr3.googlevideo.com/videoplayback?expire={}&itag=251", now + 6 * 3600);
        let left = valid_until(&url) - Instant::now();
        assert!(
            left > Duration::from_secs(5 * 3600 + 1700) && left <= Duration::from_secs(5 * 3600 + 1800),
            "{left:?}"
        );
        let expired = format!("https://rr3.googlevideo.com/videoplayback?expire={}", now - 10);
        assert!(valid_until(&expired) <= Instant::now() + Duration::from_secs(1));
        assert!(valid_until("https://example.com/a.webm") > Instant::now() + Duration::from_secs(4 * 3600));
    }

    #[tokio::test]
    #[ignore = "online"]
    async fn yt_dlp_finds_a_stream() {
        let stream = resolve("https://music.youtube.com/watch?v=d7woYjGWs6k", None, "YouTube Music").await.unwrap();
        assert!(stream.url.starts_with("https://") && stream.fresh(), "{stream:?}");
    }
}
