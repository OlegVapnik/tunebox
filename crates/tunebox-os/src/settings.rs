
use crate::os;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Settings {
    pub reduce_motion: bool,
    pub low_power: bool,
}

pub fn current() -> Settings {
    os::settings::current()
}
