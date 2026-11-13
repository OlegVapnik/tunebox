pub mod artist;
pub mod catalog;
pub mod connect;
pub mod doctor;
pub mod home;
pub mod library;
pub mod lyrics;
mod parse;
pub mod pathfinder;
mod player;
pub mod search;
pub mod session;
mod source;
pub mod webapi;

pub use self::{player::Player, source::SpotifySource};
