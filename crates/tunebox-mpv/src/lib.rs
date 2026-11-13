//! mpv-backed playback for the services that stream through it: YouTube Music, SoundCloud and
//! Spotify without Premium.
//!
//! [`Player`] owns mpv and the queue; a service implements [`Source`] to turn its URIs into
//! songs, context pages and stream URLs. [`ytdlp`] resolves URLs through yt-dlp, which
//! SoundCloud and Spotify's fallback use.

mod engine;
mod mpv;
mod queue;
pub mod ytdlp;

pub use engine::{Player, Source};
pub use ytdlp::{BotCheck, Stream};
