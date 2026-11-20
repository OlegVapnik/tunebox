
pub mod audio;
pub mod media;
pub mod power;
pub mod secrets;
pub mod settings;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as os;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as os;
#[cfg(target_os = "linux")]
pub use linux::notifications;

#[cfg(not(target_os = "macos"))]
mod fallback;
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
use fallback as os;

pub fn run_main<R: Send + 'static>(work: impl FnOnce() -> R + Send + 'static) -> R {
    os::run_main(work)
}
