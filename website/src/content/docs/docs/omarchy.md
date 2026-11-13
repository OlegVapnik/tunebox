---
title: Omarchy
description: tunebox on Omarchy, with the theme's colors, a bar widget, the menu, the launcher and Super+Shift+M.
---

On [Omarchy](https://omarchy.org), tunebox takes its colors from the theme and follows it when you switch, without a restart. Light themes work as well as dark ones. Song notifications come from the desktop with the cover, in any terminal, and a click on one brings tunebox to the front.

`tunebox omarchy install` adds the rest, and the installer offers to run it:

- **Bar widget.** Next to the clock: the service, a small equalizer while a song plays, and the title and artist, which scroll by when they don't fit. A click opens a card with the cover, the position, the controls and the volume. Middle click plays or pauses, right click brings tunebox to the front, and scrolling skips. Its settings in Omarchy choose the width, whether long titles scroll or fade out, and whether it shows while tunebox isn't open.
- **Theme.** A template Omarchy renders for every theme. Edit it in `~/.config/omarchy/themed/tunebox.toml.tpl` to change how every theme looks in tunebox, or give a theme its own `tunebox.toml`.
- **Menu.** Music in the Omarchy menu: open tunebox, play or pause, next and previous.
- **Launcher.** tunebox with its icon in the app launcher. It opens tunebox, or brings it to the front.
- **Super+Shift+M.** Opens tunebox instead of Spotify, if you say yes or add `--bind-music-key`. A binding of your own stays as it is.

`tunebox omarchy status` shows what is set up, and `tunebox omarchy remove` takes out exactly what `install` added. Running `install` again changes nothing, and running the installer again brings it up to date.

## The accent

The **Theme** accent in [Settings](/docs/settings/) takes the theme's accent color, and follows it when you switch themes.

## Media keys

Omarchy binds the media keys already, so they play, pause and skip in tunebox, and so do `playerctl` and the bar.
