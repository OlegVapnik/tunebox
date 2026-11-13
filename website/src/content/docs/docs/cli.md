---
title: Command line
description: tunebox's commands, for signing in, checking, playing without the interface and more.
---

`tunebox` on its own opens the interface. Everything else:

```sh
tunebox play <uri or link>   # play a Spotify or YouTube link without the interface; --shuffle to shuffle
tunebox connect              # Spotify: wait as a Spotify Connect device until an app picks it
tunebox login                # sign in to every service in use
tunebox login ytmusic        # sign in to one service: spotify or ytmusic
tunebox login --force        # sign in again, with another account if you like
tunebox logout ytmusic       # sign out of YouTube Music
tunebox doctor               # check what each service can reach
tunebox demo                 # the interface with a sample library, no account needed
tunebox omarchy install      # set tunebox up in Omarchy
tunebox update               # update to the latest release
tunebox --version            # the version, and the services built in
```

## play

`tunebox play` takes a URI or a link to a song, album, playlist or artist, and plays it without the interface:

```sh
tunebox play https://open.spotify.com/album/2noRn2Aes5aoNVsU6iWThc
tunebox play https://music.youtube.com/watch?v=d7woYjGWs6k --shuffle
```

Media keys and the system's media controls work while it plays.

## login

`tunebox login` signs in to every service in use, or to the one named. With YouTube Music among others, it asks before signing in there, as YouTube Music plays without an account. `--force` signs in again where already signed in. `--check` only checks, quietly: it exits with 0 when signed in, 1 when not.

## doctor

`tunebox doctor` checks what each service can reach: for Spotify your app, the Web API and playback, for YouTube Music mpv, yt-dlp, deno, music.youtube.com, the sign-in with its account, and a stream. Start here when something doesn't work; see [Troubleshooting](/docs/troubleshooting/).

## omarchy

`tunebox omarchy install` sets tunebox up in [Omarchy](/docs/omarchy/); `--bind-music-key` also makes Super+Shift+M open tunebox, and `--yes` asks nothing. `tunebox omarchy status` shows what is set up, and `tunebox omarchy remove` takes out exactly what `install` added.
