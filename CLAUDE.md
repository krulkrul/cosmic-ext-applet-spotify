# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build commands

```bash
cargo build                   # debug build
cargo build --release         # release build (size-optimised via Cargo.toml profile)
cargo clippy                  # lint
```

Run directly (requires a COSMIC/Wayland session):
```bash
./target/debug/cosmic-ext-applet-spotify
```

## Architecture

Single-file Rust applet (`src/main.rs`) using **libcosmic** (pinned to commit `c52ef976` — see Cargo.toml). Follows the exact same patterns as sibling applets `../cosmic-ext-applet-crypto` and `../cosmic-ext-applet-weather`.

### Key libcosmic patterns used here

**Popup rendering** — the popup is NOT rendered in `view_window`. Instead it is a closure passed to `app_popup::<AppModel>()` inside `view()`. The second argument to `app_popup` is `Some(Box::new(|state| { ... }.map(cosmic::Action::App)))`.

**Popup toggle** — the panel button uses `on_press_with_rectangle` (not `on_press`) so the system knows where to anchor the popup. Destroy via `destroy_popup(id)`. Both are wrapped in `Message::Surface(...)` and dispatched through `cosmic::task::message(cosmic::Action::Cosmic(cosmic::app::Action::Surface(action)))`.

**Async tasks** — use `cosmic::task::future(async { Message::Foo(...) })`, not raw `tokio::spawn`.

**Settings view** — shown inside the same popup by toggling `AppModel::show_settings`. The `build_main_view` / `build_settings_view` free functions each return `Element<'_, Message>` and are called from the `app_popup` view closure.

### Player state flow

```
subscription (every N secs) → Message::Tick
  → cosmic::task::future → query_player() via zbus
    → Message::PlayerState(PlayerState)
      → if track_id changed: fetch_art(url) → Message::AlbumArt(Option<PathBuf>)
```

`PlayerState` has three variants: `NotRunning`, `Stopped`, `Active(TrackInfo)`.

### D-Bus / MPRIS

`query_player()` / `try_query()` connect to `org.mpris.MediaPlayer2.spotify` on the session bus via **zbus 4** (pinned in Cargo.lock). If Spotify is not running the property getter fails and `NotRunning` is returned — no explicit bus-name check needed.

Controls (`PlayPause`, `Next`, `Previous`) call `mpris_call(method)` then re-query after a short delay so the panel updates immediately.

### Album art

Downloaded with `curl` into `~/.cache/cosmic-ext-applet-spotify/<hash>.jpg`. The file path is stored as `AppModel::album_art_path` (`Option<PathBuf>`). In view, a `Handle::from_path` is created on demand — no bytes are kept in memory. Art is only re-fetched when `TrackInfo::track_id` changes.

### Config

`AppConfig` is serialised to `~/.config/cosmic-ext-applet-spotify/config.json`. `save_config` is called inline in every `Message::Set*` / `Message::Toggle*` handler.

### App ID

`com.krul.CosmicAppletSpotify`

### Dependency note

`Cargo.lock` was seeded from `../cosmic-ext-applet-crypto/Cargo.lock` to pin the libcosmic dependency graph. Do not delete it. Adding new dependencies should be done with `cargo add` and the lock file committed.
