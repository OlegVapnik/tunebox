
use std::{
    fs,
    sync::{
        Once,
        atomic::{AtomicBool, AtomicU8, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use futures::StreamExt;
use zbus::{Proxy, Variant, proxy};

use super::bus;
use crate::settings::Settings;

const FIRST_READ: Duration = Duration::from_millis(250);
const PROFILES: [(&str, &str); 2] = [
    ("org.freedesktop.UPower.PowerProfiles", "/org/freedesktop/UPower/PowerProfiles"),
    ("net.hadess.PowerProfiles", "/net/hadess/PowerProfiles"),
];
const PLATFORM_PROFILE: &str = "/sys/firmware/acpi/platform_profile";

static POWER_SAVER: AtomicU8 = AtomicU8::new(UNKNOWN);
const UNKNOWN: u8 = 0;
const NO: u8 = 1;
const YES: u8 = 2;
static REDUCE_MOTION: AtomicBool = AtomicBool::new(false);

pub fn current() -> Settings {
    static WATCH: Once = Once::new();
    WATCH.call_once(|| {
        let (ready, first_read) = mpsc::channel();
        bus::spawn(watch_power_profile(ready.clone()));
        bus::spawn(watch_motion(ready));
        let deadline = Instant::now() + FIRST_READ;
        while first_read.recv_timeout(deadline.saturating_duration_since(Instant::now())).is_ok() {}
    });
    let low_power = match POWER_SAVER.load(Ordering::Relaxed) {
        UNKNOWN => fs::read_to_string(PLATFORM_PROFILE).is_ok_and(|profile| profile.trim() == "low-power"),
        saver => saver == YES,
    };
    Settings { reduce_motion: REDUCE_MOTION.load(Ordering::Relaxed), low_power }
}

async fn watch_power_profile(ready: mpsc::Sender<()>) {
    let Some(system) = bus::system().await else { return };
    for (name, path) in PROFILES {
        let Ok(proxy) = Proxy::new(&system, name, path, name).await else { continue };
        let Ok(profile) = proxy.get_property::<String>("ActiveProfile").await else { continue };
        set_power_saver(&profile);
        drop(ready);
        let mut changes = proxy.receive_property_changed::<String>("ActiveProfile").await;
        while let Some(change) = changes.next().await {
            if let Ok(profile) = change.get().await {
                set_power_saver(&profile);
            }
        }
        return;
    }
    tracing::debug!("no power-profiles-daemon; following {PLATFORM_PROFILE}");
}

fn set_power_saver(profile: &str) {
    POWER_SAVER.store(if profile == "power-saver" { YES } else { NO }, Ordering::Relaxed);
}

#[proxy(
    interface = "org.freedesktop.portal.Settings",
    default_service = "org.freedesktop.portal.Desktop",
    default_path = "/org/freedesktop/portal/desktop"
)]
trait Portal {
    fn read_one(&self, namespace: &str, key: &str) -> zbus::Result<zbus::OwnedVariant>;

    fn read(&self, namespace: &str, key: &str) -> zbus::Result<zbus::OwnedVariant>;

    #[zbus(signal)]
    fn setting_changed(&self, namespace: &str, key: &str, value: Variant<'_>) -> zbus::Result<()>;
}

const REDUCED_MOTION: (&str, &str) = ("org.freedesktop.appearance", "reduced-motion");
const ANIMATIONS: (&str, &str) = ("org.gnome.desktop.interface", "enable-animations");

async fn watch_motion(ready: mpsc::Sender<()>) {
    let Some(session) = bus::session().await else { return };
    let Ok(portal) = PortalProxy::new(&session).await else { return };
    let Ok(mut changes) = portal.receive_setting_changed().await else { return };
    let mut motion = Motion::default();
    for (namespace, key) in [REDUCED_MOTION, ANIMATIONS] {
        let value = match portal.read_one(namespace, key).await {
            Ok(value) => Ok(value),
            Err(_) => portal.read(namespace, key).await,
        };
        if let Ok(value) = value {
            motion.set(namespace, key, &value);
        }
    }
    REDUCE_MOTION.store(motion.reduce(), Ordering::Relaxed);
    drop(ready);
    while let Some(change) = changes.next().await {
        if let Ok(args) = change.args()
            && motion.set(args.namespace(), args.key(), args.value())
        {
            REDUCE_MOTION.store(motion.reduce(), Ordering::Relaxed);
        }
    }
}

#[derive(Debug, Default)]
struct Motion {
    reduced: bool,
    animations_off: bool,
}

impl Motion {
    fn set(&mut self, namespace: &str, key: &str, value: &Variant<'_>) -> bool {
        match (namespace, key, value) {
            (_, _, Variant::Variant(inner)) => self.set(namespace, key, inner),
            (_, _, Variant::U32(preference)) if (namespace, key) == REDUCED_MOTION => {
                self.reduced = *preference == 1;
                true
            }
            (_, _, Variant::Bool(enabled)) if (namespace, key) == ANIMATIONS => {
                self.animations_off = !enabled;
                true
            }
            _ => false,
        }
    }

    fn reduce(&self) -> bool {
        self.reduced || self.animations_off
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follows_either_motion_setting() {
        let mut motion = Motion::default();
        assert!(motion.set("org.freedesktop.appearance", "reduced-motion", &Variant::U32(1)));
        assert!(motion.reduce());
        motion.set("org.freedesktop.appearance", "reduced-motion", &Variant::U32(0));
        assert!(!motion.reduce());
        motion.set("org.gnome.desktop.interface", "enable-animations", &Variant::Bool(false));
        assert!(motion.reduce());
        motion.set("org.gnome.desktop.interface", "enable-animations", &Variant::Bool(true));
        assert!(!motion.reduce());
    }

    #[test]
    fn reads_values_from_older_portals() {
        let mut motion = Motion::default();
        let wrapped = Variant::Variant(Box::new(Variant::U32(1)));
        assert!(motion.set("org.freedesktop.appearance", "reduced-motion", &wrapped));
        assert!(motion.reduce());
    }

    #[test]
    fn ignores_other_settings() {
        let mut motion = Motion::default();
        assert!(!motion.set("org.freedesktop.appearance", "color-scheme", &Variant::U32(1)));
        assert!(!motion.set("org.gnome.desktop.interface", "enable-animations", &Variant::U32(0)));
        motion.set("org.freedesktop.appearance", "reduced-motion", &Variant::U32(7));
        assert!(!motion.reduce());
    }
}
