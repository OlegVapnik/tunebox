# Changelog

All notable changes to tunebox. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.5.5] - 2026-11-17

YouTube Music playback uses the sign-in you already have, so YouTube's bot check stops blocking songs.

### Fixed

- **YouTube Music streams** are now resolved by yt-dlp with your saved cookies. Without them YouTube asked this connection to prove it wasn't a bot and refused every song; with them the same request succeeds. Signed-out playback still works when there is no saved sign-in.
- `tunebox doctor` checks the stream with the same cookies, so it reports playback the way the player sees it.

## [0.5.4] - 2026-11-17

SoundCloud becomes a full service next to Spotify and YouTube Music, songs start sooner, and the shell gets a calmer, steadier look.

### Added

- **SoundCloud.** Sign in from your browser with one command, browse charts and search, see liked songs, playlists and albums, and switch to it with <kbd>S</kbd>. A playlist without its own cover uses the first song's artwork, then the uploader's picture.
- **Cookies read from your browser.** `tunebox login spotify` and `tunebox login ytmusic` offer Firefox and Chromium browsers first; yt-dlp handles locked databases and keyrings. Spotify can also open a Chromium window, inject your `sp_dc` and take the token the web player gets for itself when its token endpoint refuses a plain request.
- **Saved queue.** The songs you queued, and your place in them, come back after a restart.
- **`r` retries a failed page.** Home, artist, tracks, search and playlists can be reloaded from the keyboard.
- **Generated covers.** Missing artwork becomes a smooth gradient image with a large initial drawn into it, for Latin and Cyrillic names, sent through the same image pipeline as real covers.

### Changed

- **Spotify's Web API** now signs in through your own web-player session (`sp_dc`) instead of a developer app whose owner must have Premium. When Spotify blocks the token endpoint, `tunebox login spotify` opens a browser window and captures the web player's own token.
- **A calmer look.** Catppuccin Mocha colors, one mauve accent, clearer weight and spacing, quieter hints, a 2×2 labelled navigation grid, and more polished Now Playing and Queue pages.
- **Faster starts.** Resolved stream URLs are cached on disk, the selected track is prefetched as you move the cursor or mouse, and mpv buffers ahead. Pressing Enter no longer waits for yt-dlp on a track you have already hovered.
- **YouTube Music sign-in heals itself.** Cookies are re-read from the browser that supplied them, for the same account. A failed try is retried after 30 seconds instead of waiting for a restart.
- **SoundCloud Home** shows only Trending now — no greeting, "Playlists to try" or "Albums on SoundCloud" — and keeps the service buttons clear of the search field in narrow windows.

### Fixed

- Card titles wrap by whole words, one-letter SoundCloud subtitles are hidden, and the play button is centred without a dark hole beside it.
- Selection is a dark wash with an accent bar; a playing track is marked with the accent and ♪; the highlighted card is the first one again.
- Service buttons no longer merge with the search field. Errors show `—` instead of `0 songs · 0 min`, and a sign-in problem appears only on its own service.
- YouTube Music's refresh works before the first profile request and tolerates a browser whose cookie store is briefly locked.

## [0.5.2] - 2026-11-10

YouTube Music's Home looks and works as in its own apps, and tunebox asks once for a star.

### Added

- **Moods on YouTube Music's Home.** Chips like Relax, Workout or Focus above the shelves show the Home of that mood, as on music.youtube.com, and picked again the whole Home. <kbd>↑</kbd> from the first shelf reaches them.
- **Quick picks as songs.** Quick picks and Trending songs show as songs in columns of four, with a small cover and the artists under the title. <kbd>↑</kbd> <kbd>↓</kbd> move within a column, <kbd>←</kbd> <kbd>→</kbd> from column to column. **Play all** plays the whole shelf in order.
- **The line above a shelf,** like "THE BEST HITS FOR INSTANT MORNING ENERGY.", where YouTube Music has one.
- **A star on GitHub.** On the second day you use tunebox, it asks once whether you'd give it a star on GitHub. Whatever you answer, it doesn't ask again.

### Changed

- **Songs on YouTube Music's Home play at once,** with their radio after them, as in YouTube Music's own apps, instead of opening the radio as a page. Their menu is a song's: add to queue, like, add to playlist.

### Fixed

- **An expired YouTube Music sign-in** stays at the bottom until you close it, instead of for a few seconds, and a click copies `tunebox login ytmusic` ([#27](https://github.com/OlegVapnik/tunebox/issues/27)).
- **YouTube's bot check.** When YouTube asks your connection to prove it isn't a bot, no song plays. tunebox said nothing and skipped song after song; now it says why at the bottom until music plays again, and so do `tunebox doctor` and `tunebox play`.

## [0.5.1] - 2026-11-05

tunebox takes on your terminal's colors or your own, and song notifications reach every terminal on Linux.

### Added

- **Your terminal's colors.** Settings has Colors now. **Terminal** takes your terminal's background and text, with the shades in between mixed from them, so tunebox looks at home in a purple Ptyxis on Ubuntu or any other theme. **Auto** stays as before: Omarchy's theme there, tunebox's own dark colors elsewhere, which **tunebox** keeps everywhere.
- **Your own colors.** Any of tunebox's colors in `~/.config/tunebox/theme.toml`, such as `bg = "#2b0a3d"`, go over the others. tunebox follows the file as you edit it.
- **Notifications on Linux in every terminal.** Song notifications come from the desktop, with the cover, in terminals without notifications of their own, like Alacritty, foot, Konsole or GNOME Terminal: from GNOME, KDE Plasma or a notification daemon like mako or dunst. On Hyprland they always come from the desktop, as on Omarchy, and a click on one brings tunebox to the front. Over SSH they stay with the terminal.

### Changed

- **Rust 1.90** or newer builds tunebox from source, for the current D-Bus library on Linux and the library that draws Sixel images.

### Fixed

- **Now Playing in short windows.** tunebox no longer crashes when Now Playing opens in a window under about seven rows tall.
- **Home in short windows** holds still while a song plays, instead of jumping between the greeting and the shelf below it.
- **A song that can't play yet** leaves the player bar as it was. Picking one while tunebox still connects, or in the demo, showed the new playlist or album next to the old song, and ␣ no longer went on with the last one.
- **Spotify's mixes in your library.** The playlists Spotify makes for you, like Club Mix, Hip Hop Mix, the Daily Mixes and Discover Weekly, show up in your library once you've saved them in Spotify, in the order you picked there. So do the other Spotify playlists you follow. Spotify's Web API leaves them out, so tunebox now lists your library the way Spotify's own apps do.
- **Spotify's mixes in search.** Searching for "synthwave" or "club mix" finds your Synthwave Mix or Club Mix first, as in Spotify, and shows the song count wherever Spotify gives it.
- **`tunebox doctor`** checks both.
- **`TUNEBOX_LOG` with a trailing comma,** as in `info,`, logs at that level instead of errors only.

### Security

- **Certificate checks.** tunebox's TLS libraries are updated to their latest fixes. Certificates from intermediates that have since been distrusted, such as emSign's, are now refused.

## [0.5.0] - 2026-11-04

YouTube Music joins Spotify, on its own or side by side, and tunebox becomes part of Omarchy.

### Added

- **YouTube Music.** Home, search, artist, album and playlist pages, lyrics and song radios, without an account. tunebox plays it through mpv, with the streams from yt-dlp, and songs follow each other without a gap. The queue, shuffle, repeat, seeking, the volume and the system's media controls work as with Spotify, the last song comes back after a restart, and when a playlist or album ends, the radio of its last song follows, as on YouTube Music. It needs mpv, yt-dlp 2025.11.12 or newer, and deno. The DJ, devices, crossfade and podcasts stay Spotify's; while YouTube Music plays, the hints, the help and the command palette leave out the DJ, devices and crossfade.
- **Your YouTube Music library.** `tunebox login ytmusic` borrows the sign-in of your default browser, Brave Origin on Omarchy too. Not signed in to YouTube there yet, it opens YouTube's sign-in and goes on once you're signed in. It shows the accounts signed in there by their address, such as a work and a private one of the same name, brand accounts too, and you pick the one to use; **Another account** signs in to one more in the browser first. Another browser from a list, or a Cookie header pasted from the developer tools, works as well. Signed in, tunebox shows your playlists, albums, subscribed artists and liked songs, and you can like songs, save albums and playlists, subscribe to artists and add songs to playlists. Cookies YouTube turns down aren't kept. `tunebox logout ytmusic` forgets the sign-in.
- **Both side by side.** With both services in use, a switch in the top bar names each, the one shown lit. A click on it, `S` or "Switch to …" in the command palette shows the other service's Home, library, search and pages, as you left them. The player bar shows what plays on either, and its links and the queue open in the service the song comes from. Playing on one pauses the other, and tunebox starts on the service you left it on.
- **What you listen to, and nothing else.** The installer first asks: Spotify, YouTube Music or both. Each is a build of its own, so a YouTube Music install carries no Spotify code, and the other way around. `--service spotify`, `ytmusic` or `both` answers ahead, and running it again with another answer swaps the build, keeping settings and sign-ins. For YouTube Music it checks mpv, yt-dlp and deno, offers to install the missing ones with pacman, apt, dnf, zypper or Homebrew where they have them, along with Python's secretstorage, which yt-dlp needs on Linux to read the keyring of Chromium-based browsers, and offers the sign-in.
- **A package for each build.** Homebrew has `tunebox`, `tunebox-ytmusic` and `tunebox-full`, and each release comes with PKGBUILDs for `tunebox-bin`, `tunebox-ytmusic-bin` and `tunebox-full-bin` on Arch Linux. Each replaces the others, and the YouTube Music ones bring mpv, yt-dlp and deno along. With mise, `matching=spotify`, `matching=ytmusic` or `matching=full` picks the build.
- **YouTube Music from the command line.** `tunebox play` takes music.youtube.com, youtube.com and youtu.be links; a song plays with its radio after it. `tunebox doctor` checks mpv, yt-dlp and deno, music.youtube.com, the sign-in with the account's address, and a stream.
- **Omarchy's theme.** On Omarchy, tunebox takes its colors from the theme and follows it when it changes, without a restart. Light themes work as well as dark ones, and a theme can bring its own `tunebox.toml`. The accent has a new choice, Theme, which takes the theme's accent, and tunebox's green elsewhere.
- **tunebox in Omarchy's bar.** `tunebox omarchy install` puts a widget next to the clock: the service, a small equalizer while a song plays, and the title and artist, which scroll by when they don't fit. A click opens a card with the cover, the album, the position to click or drag, the controls and the volume. Middle click plays or pauses, right click brings tunebox to the front, and scrolling skips.
- **Omarchy's menu, launcher and Super+Shift+M.** The same command adds Music to the Omarchy menu, to open tunebox, play or pause and skip, and tunebox with its icon to the app launcher. Asked, or with `--bind-music-key`, Super+Shift+M opens tunebox instead of Spotify. `tunebox omarchy status` shows what is set up, and `tunebox omarchy remove` takes out exactly what it added. The installer offers it on Omarchy, and `--uninstall` takes it out again.
- **Notifications with the cover.** On Omarchy, song notifications come from the desktop with the cover, also in terminals without notifications of their own, like Alacritty. Each replaces the one before, and a click brings tunebox's window to the front.
- **[tunebox.app](https://tunebox.app).** A website with the docs, and a shorter way to install: `curl -fsSL https://raw.githubusercontent.com/OlegVapnik/tunebox/main/install.sh | bash`.
- **More for desktop widgets.** On Linux, they can read and set tunebox's volume and get a link to the song on the web, which also tells them the service. On Hyprland they can bring tunebox's window to the front.

### Changed

- **`tunebox login`** signs in to every service in use, or to the one named, as in `tunebox login spotify`. With YouTube Music among others, it asks before signing in there, as YouTube Music plays without an account. With Spotify only, it works as before.
- **One service doesn't stop the other.** A service that can't start, such as Spotify without a sign-in, no longer keeps tunebox from starting: its pages say why, and tunebox starts on one that works.
- **Updates keep the build.** tunebox updates to the build it is: Spotify, YouTube Music or both. tunebox 0.4 updates to the Spotify build; run the installer to switch.
- **mise** installs tunebox with its github backend, `github:OlegVapnik/tunebox`, where `matching` picks the build.
- **`tunebox --version`** names the services built in, such as `(spotify, ytmusic)`.

### Fixed

- **Spotify refusing the keys to songs.** Spotify turns down librespot's requests for the key to a song's audio at times ([librespot#1649](https://github.com/librespot-org/librespot/issues/1649)), and songs then skipped one after another without a word. tunebox now asks for one key at a time, and after a refusal waits 3, then 5, then 10 seconds before it asks again, as many refusals stop that way. A song that still can't play says so instead of skipping silently. The change comes from an open librespot pull request ([#1844](https://github.com/librespot-org/librespot/pull/1844)).
- Songs LRCLIB has no lyrics for say "No lyrics for this song." instead of "Lyrics are unavailable right now.", as LRCLIB sometimes answers with an error when it has none.
- With bash 5, the installer shows paths in your home folder with `~` too.
- While YouTube Music plays, the system's sound mixer shows the song instead of the address of its stream.

## [0.4.0] - 2026-10-27

Settings move into the app, the queue shows what plays next, and tunebox feels at home on Linux.

### Added

- **Settings.** `,`, the command palette or a click on your name in the top bar opens a Settings page: crossfade, cover art and how covers are drawn, the accent color, icons, power saving, song notifications and update checks. Changes apply at once and are saved to `config.toml`, so nobody has to edit it by hand.
- **Queue.** See what plays next, as in Spotify's apps: the song playing now, the songs you queued under "Next in queue" and the rest of the playlist or album under "Next from". `Q`, the queue button next to the volume or the command palette opens it. It keeps up as songs change and as you add songs with `q`, on tunebox and on the device it controls. Enter or a double click skips ahead to a song, and Delete or its menu takes a song you queued out again.
- **Cover art off.** For slower computers: without cover art, tunebox loads and draws no covers at all. Pages show plain tiles in their place, and Now Playing gives the lyrics the whole screen.
- **Power saving, your way.** Power saving can stay on or off for good; Auto follows the system as before.
- **Accent color.** The accent can stay one color instead of following the cover.
- **At home on Linux.** Media keys, playerctl and the media controls of GNOME, KDE and Waybar play, pause, skip and seek, and show the track with its cover, in `tunebox play` and `tunebox connect` too. tunebox pauses before the computer sleeps and reconnects as soon as it wakes. With animations turned off in the desktop's settings, slides become fades, lyrics jump to the current line and the equalizer stands still. In Power Saver mode animations stop and tunebox wakes the computer less often.
- **Covers in more terminals.** Covers are real images in iTerm2, foot, Windows Terminal, Konsole, VS Code, xterm and other terminals that show Sixel or iTerm2 images, no longer only in kitty, Ghostty and WezTerm. They scroll in and out of view row by row, stay whole under menus, the command palette and the DJ panel, and reach the terminal once instead of with every frame. Inside tmux they stay blocks unless the terminal around it is kitty or Ghostty.
- **Package managers.** Install tunebox with `brew install OlegVapnik/tunebox/tunebox` on macOS and Linux, with `mise use -g ubi:OlegVapnik/tunebox`, or on Arch Linux with `makepkg` from the PKGBUILD that comes with each release. Installed this way, tunebox still tells you about new releases, and leaves installing them to the package manager.

### Changed

- Crossfade (`X`) is saved with the other settings in `config.toml`. The length chosen last moves there by itself.

## [0.3.0] - 2026-10-16

The DJ takes requests, and tunebox updates itself.

### Added

- **Ask DJ.** While DJ plays on tunebox, `D` or the DJ button next to the controls opens the DJ panel, as in Spotify's apps. Type what you'd like to hear or take one of Spotify's suggestions: DJ acknowledges the request and plays what it put together for it. "Pick something for me" jumps to different picks, and DJ announces the change.
- **Updates in one click.** When a new release is out, a button in the top bar says so. Click it or press `U`, and tunebox downloads the release, checks it against its checksum and replaces itself; the same button then restarts into it. `tunebox update` does it from the command line, and `update_check = false` stops tunebox looking.

## [0.2.0] - 2026-10-15

tunebox feels at home on macOS, starts instantly, and can be tried without an account. Thanks to [@yk-sgr](https://github.com/yk-sgr) for everything on macOS.

### Added

- **Media keys.** On macOS, the media keys, AirPods and Control Center play, pause, skip and seek, and Control Center shows the track with its cover. This works in `tunebox play` and `tunebox connect` too.
- **Follows your headphones.** On macOS, playback moves to AirPods or headphones as they connect, and pauses when the output in use disconnects instead of carrying on through the speakers.
- **Reconnecting.** tunebox reconnects to Spotify by itself when the connection drops, after sleep or a network outage, and offers to continue where playback stopped. On macOS it pauses before the Mac sleeps and reconnects as soon as it wakes.
- **Reduce Motion and Low Power Mode.** On macOS with Reduce Motion, slides become fades, lyrics jump to the current line and the equalizer stands still. In Low Power Mode animations stop and tunebox wakes the computer less often.
- **Notifications.** Each new track shows a desktop notification while tunebox's window is in the background, in kitty, Ghostty, WezTerm and iTerm2, also inside tmux with `focus-events on` and `allow-passthrough all`. `notifications = false` turns them off.
- **Keychain.** On macOS, `credentials = "keychain"` keeps sign-ins in the Keychain instead of files. It's off by default, because macOS asks for access again after each update of an unsigned build.
- **Demo.** `tunebox demo` shows the whole interface with a sample library, no account needed.
- **Instant start.** Home, your library and the list you had open show at once from the last run, then refresh in the background.
- **Search with covers.** Results show their artwork, laid out on one grid.

### Changed

- **Smoother screen.** Each frame reaches the terminal whole, so pages and covers no longer tear, and keys and clicks go ahead of loading data.
- **Lighter covers.** Kitty terminals scale covers themselves. Covers load a few at a time, visible ones first and the next page ahead, and non-square ones fill their slot. tunebox needs much less memory for them.
- **Controls answer at once.** Play, pause, skip, seek and volume show on screen right away, on tunebox and on other devices, and don't jump back while Spotify catches up.
- **Other devices.** tunebox follows them through Spotify's own updates instead of asking every 3 seconds.
- **Faster pages.** Pages opened again show at once. Long lists and followed playlists show while they load. Likes, saves and follows show in your library right away.
- **Page changes.** A new page shows at once with a short fade instead of a wipe.
- **Smaller and leaner.** Release builds are smaller and faster, and tracks are buffered on disk instead of in memory.

### Fixed

- On macOS, playback no longer stalls for good when the headphones or AirPods in use disconnect.
- The saved volume applies on start. tunebox always started at 50% and then saved that.
- Help and the command palette no longer crash in small windows.
- Stalled requests give up, rate-limited ones are tried again, and a failed connection to Spotify shows instead of pages that load forever.
- Every key has a mouse action: hint clicks do what their keys do, right-click works on names, play buttons and the player, and clicked targets light up.
- Many layout fixes: columns, highlights, spacing and card rows line up across pages and window sizes.

## [0.1.0] - 2026-10-15

The first release.

### Added

- **Playback.** tunebox plays music itself through librespot and shows up as a Spotify Connect device. It can also control your other devices.
- **Home.** Your shelves and Made For You, the DJ with its voice between tracks, and crossfade.
- **Library.** Playlists, albums, artists and podcasts, in the sidebar and as a full page.
- **Pages and search.** Playlist, album and artist pages, and search.
- **Lyrics.** Synced lyrics in a now-playing view with a large cover.
- **Cover art.** Shown through the Kitty graphics protocol, as half blocks elsewhere.
- **Mouse.** Clicks, hover, the wheel, context menus, and dragging the seek and volume bars.
- **Keyboard.** Shortcuts for everything, plus a command palette.
- **Session restore.** The page, the selection and the last track come back after a restart.
- **Installer.** It sets everything up step by step.

[Unreleased]: https://github.com/OlegVapnik/tunebox/compare/v0.5.5...HEAD
[0.5.5]: https://github.com/OlegVapnik/tunebox/compare/v0.5.4...v0.5.5
[0.5.4]: https://github.com/OlegVapnik/tunebox/compare/v0.5.2...v0.5.4
[0.5.2]: https://github.com/OlegVapnik/tunebox/compare/v0.5.1...v0.5.2
[0.5.1]: https://github.com/OlegVapnik/tunebox/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/OlegVapnik/tunebox/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/OlegVapnik/tunebox/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/OlegVapnik/tunebox/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/OlegVapnik/tunebox/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/OlegVapnik/tunebox/releases/tag/v0.1.0
