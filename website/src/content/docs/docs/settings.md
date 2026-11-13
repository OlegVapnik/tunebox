---
title: Settings
description: tunebox's Settings page, and every option of config.toml.
---

<kbd>,</kbd>, the command palette, or a click on your name in the top bar opens the Settings page: crossfade, cover art and how covers are drawn, the colors, the accent color, icons, power saving, song notifications and update checks. Changes apply at once and tunebox saves them in `~/.config/tunebox/config.toml`, so there's no need to edit the file.

## config.toml

You still can; it's read at start:

```toml
client_id = "your client id"
services = ["ytmusic"]     # "spotify", "ytmusic" or both, in the switch's order; every one built in when missing
crossfade = 6              # seconds tracks overlap, 0 to 12; off when missing
covers = false             # no cover art, for slower computers; on when missing
colors = "terminal"        # "tunebox" or "terminal"; Omarchy's theme there, tunebox's own elsewhere, when missing
accent = "violet"          # "theme", "green", "violet", "pink", "blue" or "orange"; follows the cover when missing
images = "blocks"          # "kitty", "sixel", "iterm2" or "blocks"; detected from the terminal when missing
icons = "nerd"             # "nerd" or "text"; detected from the terminal when missing
power_saving = "on"        # "on" or "off"; follows the system when missing
notifications = false      # no desktop notifications for new tracks; on when missing
update_check = false       # don't look for new releases at start; on when missing
credentials = "keychain"   # macOS: sign-ins in the Keychain instead of files (only here)
```

The installer writes `services` with what you listen to. One config file serves every build, so a build leaves out the services it doesn't have. The `"theme"` accent is the one of the colors in use: `theme.toml`'s when it sets one, else the [Omarchy](/docs/omarchy/) theme's with Colors on Auto, and tunebox's green otherwise, also with Terminal.

## Colors

tunebox draws with its own dark colors, and on Omarchy with the [theme's](/docs/omarchy/). Set Colors to **Terminal** and it takes your terminal's background and text instead, with the shades in between mixed from them, so a purple terminal stays purple. Terminals that don't tell their colors keep tunebox's. **tunebox** keeps tunebox's own colors, on Omarchy too.

To pick colors yourself, put any of them in `~/.config/tunebox/theme.toml`. They go over whichever colors the setting gives, and tunebox follows the file as you edit it, without a restart:

```toml
bg = "#2b0a3d"        # behind everything
surface = "#33124a"   # the sidebar
player = "#30104a"    # the player bar
raised = "#3d1a55"    # cards, menus and buttons
text = "#f4eefa"
muted = "#b7a3c4"     # artists, counts and other secondary text
accent = "#e95420"    # the accent while no cover sets one, and the "Theme" accent
```

The others are `faint`, `danger`, `unfocused`, `disabled`, `placeholder`, `thumb`, `lyric_near`, `lyric_far` and `highlight`; [Omarchy's template](https://github.com/OlegVapnik/tunebox/blob/main/integrations/omarchy/tunebox.toml.tpl) says what each is for.

## Cover art

Slow computer? Turn cover art off: tunebox then loads and draws no covers at all. Pages show plain tiles in their place, and Now Playing gives the lyrics the whole screen. Which terminals show real images is on [Terminals](/docs/terminals/).

## Power saving

Power saving stops animations and redraws less often. Unless set otherwise, it turns on by itself with Low Power Mode on macOS and the Power Saver profile on Linux. With animations turned off in the system's settings, slides become fades, lyrics jump to the current line and the equalizer stands still.

## Notifications

Each new track shows a desktop notification while tunebox's window is in the background. kitty, Ghostty, WezTerm and iTerm2 show them themselves. On Linux, every other terminal gets them from the desktop, with the cover: from GNOME, KDE Plasma or a notification daemon like mako or dunst. On Hyprland, [Omarchy](/docs/omarchy/) included, they always come from the desktop, and a click on one brings tunebox to the front. Inside tmux, notifications need `set -g focus-events on`, and the terminal's own also `set -g allow-passthrough all`, in `~/.tmux.conf`.

## Keychain

On macOS, `credentials = "keychain"` keeps sign-ins in the Keychain instead of files: tunebox moves them there on the next start. macOS asks once after each update, because release builds aren't signed yet.

## Where tunebox keeps its files

| What | Where |
| --- | --- |
| Settings | `~/.config/tunebox/config.toml` |
| Your own colors | `~/.config/tunebox/theme.toml` |
| Sign-ins (unless in the Keychain) and session | `~/.local/share/tunebox` |
| Caches and the log | `~/.cache/tunebox` |
