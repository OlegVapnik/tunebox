
use std::time::Duration;

use anyhow::Result;
use serde_json::Value;

const TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expect {
    Available,
    Removed,
    Optional,
}

#[derive(Debug)]
pub enum Outcome {
    Ok(String),
    Failed(String),
}

#[derive(Debug)]
pub struct Check {
    pub name: String,
    pub expect: Expect,
    pub outcome: Outcome,
}

#[derive(Debug)]
pub struct Section {
    pub title: &'static str,
    pub checks: Vec<Check>,
}

pub async fn timed(name: &str, probe: impl Future<Output = Result<String>>) -> Check {
    Check { name: name.into(), expect: Expect::Available, outcome: outcome(TIMEOUT, probe).await }
}

pub async fn outcome(timeout: Duration, probe: impl Future<Output = Result<String>>) -> Outcome {
    match tokio::time::timeout(timeout, probe).await {
        Ok(Ok(detail)) => Outcome::Ok(detail),
        Ok(Err(err)) => Outcome::Failed(format!("{err:#}")),
        Err(_) => Outcome::Failed("Timed out".into()),
    }
}

pub async fn external() -> Section {
    let check = timed("Lyrics (LRCLIB)", async {
        let results: Value = reqwest::Client::builder()
            .user_agent(concat!("tunebox/", env!("CARGO_PKG_VERSION")))
            .build()?
            .get("https://lrclib.net/api/search")
            .query(&[("track_name", "One More Time"), ("artist_name", "Daft Punk")])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let synced = results.as_array().into_iter().flatten().filter(|r| r["syncedLyrics"].is_string()).count();
        Ok(format!("{synced} time-synced versions"))
    })
    .await;
    Section { title: "EXTERNAL", checks: vec![check] }
}
