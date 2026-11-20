
use std::sync::Arc;

use crate::os;

pub struct OutputWatch {
    _watch: os::audio::OutputWatch,
}

impl OutputWatch {
    pub fn new(on_change: impl Fn() + Send + Sync + 'static) -> Option<Self> {
        os::audio::OutputWatch::new(Arc::new(on_change)).map(|watch| Self { _watch: watch })
    }
}
