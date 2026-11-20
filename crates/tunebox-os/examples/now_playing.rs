
use std::{sync::Arc, time::Duration};

use tunebox_os::media::{Command, Controls, State, Track};

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut paused, mut youtube, mut title, mut artwork) = (false, false, "Example Song".to_owned(), None);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--paused" => paused = true,
            "--youtube" => youtube = true,
            "--title" => title = args.next().expect("--title takes the title"),
            path => artwork = Some(Arc::from(std::fs::read(path).expect("cover is readable"))),
        }
    }
    let link = if youtube {
        "https://music.youtube.com/watch?v=dQw4w9WgXcQ"
    } else {
        "https://open.spotify.com/track/4uLU6hMCjMI75M1A2tKUQC"
    };
    tunebox_os::run_main(move || {
        let (tx, rx) = std::sync::mpsc::channel();
        let controls = Controls::new(move |command| {
            let _ = tx.send(command);
        })
        .expect("this system has no media controls tunebox can use");
        let track = Track {
            title,
            artist: "tunebox".into(),
            album: "Media Controls".into(),
            duration: Duration::from_secs(215),
            artwork,
            link: Some(link.into()),
        };
        let mut state = State { track: Some(track), playing: !paused, position: Duration::ZERO, volume: u16::MAX / 2 };
        let mut since = std::time::Instant::now();
        controls.update(state.clone());
        println!("Showing a track. Try the media keys, Control Center or playerctl; Ctrl+C quits.");
        for command in rx {
            println!("{command:?}");
            if state.playing {
                state.position += since.elapsed();
            }
            since = std::time::Instant::now();
            match command {
                Command::Play => state.playing = true,
                Command::Pause => state.playing = false,
                Command::Toggle => state.playing = !state.playing,
                Command::Seek(position) => state.position = position,
                Command::Next | Command::Previous => state.position = Duration::ZERO,
                Command::Volume(volume) => state.volume = volume,
            }
            controls.update(state.clone());
        }
    });
}
