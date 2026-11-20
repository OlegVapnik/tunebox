
use std::{panic, thread, time::Duration};

fn main() {
    assert_eq!(tunebox_os::run_main(|| 42), 42);

    let late = tunebox_os::run_main(|| {
        thread::sleep(Duration::from_millis(50));
        "late"
    });
    assert_eq!(late, "late");

    let report = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));
    let result = panic::catch_unwind(|| tunebox_os::run_main(|| panic!("expected panic")));
    panic::set_hook(report);
    assert!(result.is_err());

    println!("run_main: ok");
}
