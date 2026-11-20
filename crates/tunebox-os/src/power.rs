
use std::sync::Arc;

use crate::os;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Sleep,
    Wake,
}

pub fn watch(on_event: impl Fn(Event) + Send + Sync + 'static) -> bool {
    os::power::watch(Arc::new(on_event))
}
