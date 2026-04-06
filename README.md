# COSMIC Spotify Applet

A panel applet for the [COSMIC desktop](https://github.com/pop-os/cosmic-epoch) that shows the currently playing Spotify track and provides playback controls.

## Features

- Track title, artist, and album art in the popup
- Play/pause, next, previous controls (panel and popup)
- Clicking the panel icon **raises** the Spotify window; clicking again **minimizes** it
- Clicking the track label opens the popup
- Configurable panel label (icon only / track / artist + track)
- Configurable poll interval, label truncation, and icon style

## Requirements

- COSMIC desktop (Wayland)
- Spotify installed as a Flatpak (`com.spotify.Client`) or native package
- `curl` (album art download)

## Build & Install

```bash
cargo build --release
```

Copy the binary somewhere on your `$PATH` and add it to your COSMIC panel via **Panel Settings → Add Applet**.

### Desktop entry

Create `~/.local/share/applications/cosmic-ext-applet-spotify.desktop`:

```ini
[Desktop Entry]
Type=Application
Name=Spotify Applet
Exec=/path/to/cosmic-ext-applet-spotify
Icon=multimedia-audio-player
```

## Development

```bash
./dev-reload.sh   # build + restart panel to pick up new binary
```

The script avoids triggering `cosmic-session`'s exponential restart backoff by killing only the manually-launched panel (if present) and re-launching it directly. On a fresh session it falls back to killing the session-managed panel, which costs one backoff increment.

## Architecture

- **`src/main.rs`** — libcosmic `Application` impl, MPRIS D-Bus queries, album art, popup UI
- **`src/toplevel.rs`** — Wayland thread tracking Spotify's window focus state via `ext_foreign_toplevel_list_v1` + `zcosmic_toplevel_info_v1`; sends `set_minimized` via `zcosmic_toplevel_manager_v1`

### Raise / minimize flow

```
icon click
  → Message::RaiseSpotify
    → spotify_activated?
      yes → TopCmd::Minimize("spotify") → zcosmic_toplevel_manager_v1::set_minimized
      no  → TokenRequest (XDG activation token)
              → sh -c "flatpak run com.spotify.Client" with XDG_ACTIVATION_TOKEN set
                (Spotify is single-instance — the second launch signals the running one to raise)
```

### Wayland toplevel tracking

The toplevel handler thread uses the regular `$WAYLAND_DISPLAY` socket (not the panel's privileged socket, which does not expose the toplevel globals). It performs four roundtrips on startup to populate the initial window list, then enters a `calloop` event loop.

Spotify reports `app_id = "spotify"` to the Wayland compositor.
