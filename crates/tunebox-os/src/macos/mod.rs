pub mod audio;
pub mod media;
pub mod power;
pub mod secrets;
pub mod settings;

use std::{panic, thread};

use dispatch2::DispatchQueue;
use objc2_core_foundation::CFRunLoop;

const WORKER_STACK: usize = 8 << 20;

pub fn run_main<R: Send + 'static>(work: impl FnOnce() -> R + Send + 'static) -> R {
    let worker = thread::Builder::new()
        .name("tunebox".into())
        .stack_size(WORKER_STACK)
        .spawn(move || {
            let _stop = StopMainLoop;
            work()
        })
        .expect("cannot start the worker thread");
    CFRunLoop::run();
    worker.join().unwrap_or_else(|payload| panic::resume_unwind(payload))
}

struct StopMainLoop;

impl Drop for StopMainLoop {
    fn drop(&mut self) {
        DispatchQueue::main().exec_async(|| {
            if let Some(main) = CFRunLoop::main() {
                main.stop();
            }
        });
    }
}
