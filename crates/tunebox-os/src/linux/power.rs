
use std::{sync::Arc, time::Duration};

use futures::StreamExt;
use zbus::{OwnedFd, fdo::DBusProxy, names::BusName, proxy};

use super::bus;
use crate::power::Event;

const LOGIND: &str = "org.freedesktop.login1";
const SLEEP_GRACE: Duration = Duration::from_secs(1);

type Callback = Arc<dyn Fn(Event) + Send + Sync>;

#[proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
trait Logind {
    fn inhibit(&self, what: &str, who: &str, why: &str, mode: &str) -> zbus::Result<OwnedFd>;

    #[zbus(signal)]
    fn prepare_for_sleep(&self, start: bool) -> zbus::Result<()>;
}

pub fn watch(on_event: Callback) -> bool {
    let subscribed = bus::run(async {
        let system = bus::system().await?;
        let running = DBusProxy::new(&system).await.ok()?.name_has_owner(BusName::from_static_str(LOGIND).ok()?).await;
        if !running.is_ok_and(|running| running) {
            tracing::debug!("logind isn't running, so tunebox can't tell sleep and wake");
            return None;
        }
        let logind = LogindProxy::new(&system).await.ok()?;
        let signals = logind
            .receive_prepare_for_sleep()
            .await
            .inspect_err(|err| tracing::warn!("cannot watch for sleep and wake: {err}"))
            .ok()?;
        Some((logind, signals))
    });
    let Some((logind, mut signals)) = subscribed else { return false };
    bus::spawn(async move {
        let mut delay = delay_sleep(&logind).await;
        while let Some(signal) = signals.next().await {
            let Ok(args) = signal.args() else { continue };
            if *args.start() {
                on_event(Event::Sleep);
                tokio::time::sleep(SLEEP_GRACE).await;
                drop(delay.take());
            } else {
                on_event(Event::Wake);
                delay = delay_sleep(&logind).await;
            }
        }
    });
    true
}

async fn delay_sleep(logind: &LogindProxy<'_>) -> Option<OwnedFd> {
    logind
        .inhibit("sleep", "tunebox", "Pausing playback", "delay")
        .await
        .inspect_err(|err| tracing::warn!("cannot delay sleep: {err}"))
        .ok()
}
