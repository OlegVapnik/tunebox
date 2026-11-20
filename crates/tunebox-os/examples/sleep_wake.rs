
fn main() {
    tunebox_os::run_main(|| {
        let (tx, rx) = std::sync::mpsc::channel();
        let watching = tunebox_os::power::watch(move |event| {
            let _ = tx.send(event);
        });
        if !watching {
            eprintln!("This system doesn't report sleep and wake.");
            return;
        }
        println!("Watching for sleep and wake; Ctrl+C quits.");
        for event in rx {
            println!("{event:?}");
        }
    });
}
