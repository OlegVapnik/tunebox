//! Prints the system settings tunebox follows each time they change. Turn on Reduce Motion or
//! power saving to try it:
//!
//! ```sh
//! cargo run -p tunebox-os --example settings
//! ```

use std::{thread, time::Duration};

fn main() {
    tunebox_os::run_main(|| {
        println!("Watching the settings; Ctrl+C quits.");
        let mut shown = None;
        loop {
            let settings = tunebox_os::settings::current();
            if shown != Some(settings) {
                println!("{settings:?}");
                shown = Some(settings);
            }
            thread::sleep(Duration::from_millis(500));
        }
    });
}
