// SPDX-License-Identifier: GPL-3.0-only
#![allow(hidden_glob_reexports)]

use libcosmic as cosmic;
use cosmic::app::{Core, Task};
use cosmic::iced::window::Id;
use cosmic::iced::{Alignment, ContentFit, Length, Subscription};
use cosmic::surface::action::{app_popup, destroy_popup};
use cosmic::widget::{self, list_column};
use cosmic::Element;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ops::Deref;
use std::path::PathBuf;
use std::time::Duration;
use zbus::zvariant::{OwnedValue, Value};

const APP_ID: &str = "com.krul.CosmicAppletSpotify";
const VERSION: &str = env!("CARGO_PKG_VERSION");
const MPRIS_BUS: &str = "org.mpris.MediaPlayer2.spotify";
const MPRIS_PATH: &str = "/org/mpris/MediaPlayer2";
const MPRIS_PLAYER: &str = "org.mpris.MediaPlayer2.Player";
const SPOTIFY_FLATPAK: &str = "com.spotify.Client";

// ─── Config ───────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone)]
struct AppConfig {
    /// "icon_only" | "icon_track" | "icon_artist_track"
    panel_display: String,
    /// Show play/pause button in panel
    show_play_panel: bool,
    /// Show next button in panel
    show_next_panel: bool,
    /// Show previous button in panel
    show_prev_panel: bool,
    /// Max characters for panel label before truncation (0 = no limit)
    max_label_chars: usize,
    /// "note_symbolic" | "spotify_colored" | "spotify_symbolic"
    icon_style: String,
    /// Fetch and display album art in popup
    show_album_art: bool,
    /// D-Bus poll interval in seconds
    poll_interval_secs: u32,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            panel_display: "icon_artist_track".to_string(),
            show_play_panel: true,
            show_next_panel: true,
            show_prev_panel: false,
            max_label_chars: 40,
            icon_style: "note_symbolic".to_string(),
            show_album_art: true,
            poll_interval_secs: 3,
        }
    }
}

fn config_path() -> PathBuf {
    let mut p = dirs::config_dir().unwrap_or_else(|| PathBuf::from("~/.config"));
    p.push("cosmic-ext-applet-spotify");
    p.push("config.json");
    p
}

fn load_config() -> AppConfig {
    std::fs::read_to_string(config_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_config(cfg: &AppConfig) {
    let path = config_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(s) = serde_json::to_string_pretty(cfg) {
        let _ = std::fs::write(&path, s);
    }
}

// ─── Data structures ──────────────────────────────────────────────────────────

#[derive(Clone, Debug, Default)]
pub struct TrackInfo {
    title: String,
    artist: String,
    album: String,
    art_url: Option<String>,
    position_us: i64,  // microseconds
    length_us: i64,    // microseconds
    is_playing: bool,
    track_id: String,
}

#[derive(Clone, Debug)]
pub enum PlayerState {
    NotRunning,
    Stopped,
    Active(TrackInfo),
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn fmt_duration(us: i64) -> String {
    let secs = (us / 1_000_000).max(0);
    format!("{}:{:02}", secs / 60, secs % 60)
}

fn truncate(s: &str, max: usize) -> String {
    if max == 0 || s.chars().count() <= max {
        return s.to_string();
    }
    let t: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", t)
}

fn art_cache_path(url: &str) -> PathBuf {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    url.hash(&mut h);
    let mut p = dirs::cache_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
    p.push("cosmic-ext-applet-spotify");
    p.push(format!("{:x}.jpg", h.finish()));
    p
}

fn panel_icon_name(cfg: &AppConfig) -> &'static str {
    match cfg.icon_style.as_str() {
        "spotify_colored" => "com.spotify.Client",
        "spotify_symbolic" => "com.spotify.Client-symbolic",
        _ => "audio-x-generic-symbolic",
    }
}

fn play_pause_icon(is_playing: bool) -> &'static str {
    if is_playing {
        "media-playback-pause-symbolic"
    } else {
        "media-playback-start-symbolic"
    }
}

fn panel_label_text(track: &TrackInfo, cfg: &AppConfig) -> String {
    match cfg.panel_display.as_str() {
        "icon_only" => String::new(),
        "icon_track" => truncate(&track.title, cfg.max_label_chars),
        _ => {
            let full = if track.artist.is_empty() {
                track.title.clone()
            } else {
                format!("{} – {}", track.artist, track.title)
            };
            truncate(&full, cfg.max_label_chars)
        }
    }
}

// ─── zvariant helpers ─────────────────────────────────────────────────────────

fn str_val(m: &HashMap<String, OwnedValue>, k: &str) -> Option<String> {
    m.get(k).and_then(|v| match v.deref() {
        Value::Str(s) => Some(s.to_string()),
        _ => None,
    })
}

fn arr_first_str(m: &HashMap<String, OwnedValue>, k: &str) -> Option<String> {
    m.get(k).and_then(|v| match v.deref() {
        Value::Array(arr) => arr.iter().find_map(|v| match v {
            Value::Str(s) => Some(s.to_string()),
            _ => None,
        }),
        _ => None,
    })
}

fn i64_val(m: &HashMap<String, OwnedValue>, k: &str) -> Option<i64> {
    m.get(k).and_then(|v| match v.deref() {
        Value::I64(n) => Some(*n),
        _ => None,
    })
}

// ─── D-Bus / MPRIS ────────────────────────────────────────────────────────────

async fn query_player() -> PlayerState {
    match try_query().await {
        Ok(state) => state,
        Err(_) => PlayerState::NotRunning,
    }
}

async fn try_query() -> zbus::Result<PlayerState> {
    let conn = zbus::Connection::session().await?;
    let proxy = zbus::Proxy::new(&conn, MPRIS_BUS, MPRIS_PATH, MPRIS_PLAYER).await?;

    let status: String = proxy.get_property("PlaybackStatus").await?;
    if status == "Stopped" {
        return Ok(PlayerState::Stopped);
    }

    let metadata: HashMap<String, OwnedValue> = proxy.get_property("Metadata").await?;
    let position: i64 = proxy.get_property("Position").await.unwrap_or(0);

    let title = str_val(&metadata, "xesam:title").unwrap_or_default();
    let artist = arr_first_str(&metadata, "xesam:artist").unwrap_or_default();
    let album = str_val(&metadata, "xesam:album").unwrap_or_default();
    let art_url = str_val(&metadata, "mpris:artUrl");
    let length = i64_val(&metadata, "mpris:length").unwrap_or(0);
    let track_id = str_val(&metadata, "mpris:trackid").unwrap_or_default();

    Ok(PlayerState::Active(TrackInfo {
        title,
        artist,
        album,
        art_url,
        position_us: position,
        length_us: length,
        is_playing: status == "Playing",
        track_id,
    }))
}

async fn mpris_call(method: &str) {
    let _ = try_mpris_call(method).await;
}

async fn try_mpris_call(method: &str) -> zbus::Result<()> {
    let conn = zbus::Connection::session().await?;
    let proxy = zbus::Proxy::new(&conn, MPRIS_BUS, MPRIS_PATH, MPRIS_PLAYER).await?;
    proxy.call_method(method, &()).await?;
    Ok(())
}

// ─── Album art ────────────────────────────────────────────────────────────────

/// Download URL and cache to disk. Returns path on success.
async fn fetch_art(url: String) -> Option<PathBuf> {
    let cache = art_cache_path(&url);
    if cache.exists() {
        return Some(cache);
    }
    let out = tokio::process::Command::new("curl")
        .args(["-s", "--max-time", "10", "-L", &url])
        .output()
        .await
        .ok()?;
    if out.status.success() && !out.stdout.is_empty() {
        if let Some(parent) = cache.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if std::fs::write(&cache, &out.stdout).is_ok() {
            return Some(cache);
        }
    }
    None
}

// ─── Messages ─────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub enum Message {
    PopupClosed(Id),
    Surface(cosmic::surface::Action),
    Tick,
    PlayerState(PlayerState),
    AlbumArt(Option<PathBuf>),
    PlayPause,
    Next,
    Previous,
    LaunchSpotify,
    ToggleSettings,
    // Config mutations
    SetPanelDisplay(String),
    SetIconStyle(String),
    ToggleShowPlayPanel,
    ToggleShowNextPanel,
    ToggleShowPrevPanel,
    ToggleShowAlbumArt,
    SetMaxLabelChars(usize),
    SetPollInterval(u32),
}

// ─── App state ────────────────────────────────────────────────────────────────

pub struct AppModel {
    core: Core,
    popup: Option<Id>,
    config: AppConfig,
    player: PlayerState,
    show_settings: bool,
    /// Cached art path for the current track_id
    album_art_path: Option<PathBuf>,
    /// Track id for which art was last fetched (avoids re-fetching same art)
    art_track_id: String,
}

// ─── Application impl ─────────────────────────────────────────────────────────

impl cosmic::Application for AppModel {
    type Executor = cosmic::executor::Default;
    type Flags = ();
    type Message = Message;
    const APP_ID: &'static str = APP_ID;

    fn core(&self) -> &Core { &self.core }
    fn core_mut(&mut self) -> &mut Core { &mut self.core }

    fn init(core: Core, _flags: ()) -> (Self, Task<Self::Message>) {
        let config = load_config();
        let task = cosmic::task::future(async { Message::PlayerState(query_player().await) });
        (
            AppModel {
                core,
                popup: None,
                config,
                player: PlayerState::NotRunning,
                show_settings: false,
                album_art_path: None,
                art_track_id: String::new(),
            },
            task,
        )
    }

    fn on_close_requested(&self, id: cosmic::iced_runtime::core::window::Id) -> Option<Message> {
        Some(Message::PopupClosed(id))
    }

    fn update(&mut self, message: Self::Message) -> Task<Self::Message> {
        match message {
            Message::PopupClosed(id) => {
                if self.popup.as_ref() == Some(&id) {
                    self.popup = None;
                }
            }

            Message::Surface(action) => {
                return cosmic::task::message(cosmic::Action::Cosmic(
                    cosmic::app::Action::Surface(action),
                ));
            }

            Message::Tick => {
                return cosmic::task::future(async {
                    Message::PlayerState(query_player().await)
                });
            }

            Message::PlayerState(state) => {
                let new_id = match &state {
                    PlayerState::Active(t) => t.track_id.clone(),
                    _ => String::new(),
                };
                let new_art_url = match &state {
                    PlayerState::Active(t) => t.art_url.clone(),
                    _ => None,
                };

                // Clear art if track changed or Spotify stopped
                if new_id != self.art_track_id {
                    self.album_art_path = None;
                    self.art_track_id = new_id;
                }

                self.player = state;

                // Fetch art if needed
                if self.config.show_album_art
                    && self.album_art_path.is_none()
                    && !self.art_track_id.is_empty()
                {
                    if let Some(url) = new_art_url {
                        return cosmic::task::future(async move {
                            Message::AlbumArt(fetch_art(url).await)
                        });
                    }
                }
            }

            Message::AlbumArt(path) => {
                self.album_art_path = path;
            }

            Message::PlayPause => {
                return cosmic::task::future(async {
                    mpris_call("PlayPause").await;
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    Message::PlayerState(query_player().await)
                });
            }

            Message::Next => {
                return cosmic::task::future(async {
                    mpris_call("Next").await;
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    Message::PlayerState(query_player().await)
                });
            }

            Message::Previous => {
                return cosmic::task::future(async {
                    mpris_call("Previous").await;
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    Message::PlayerState(query_player().await)
                });
            }

            Message::LaunchSpotify => {
                let _ = std::process::Command::new("flatpak")
                    .args(["run", SPOTIFY_FLATPAK])
                    .spawn();
            }

            Message::ToggleSettings => {
                self.show_settings = !self.show_settings;
            }

            Message::SetPanelDisplay(v) => {
                self.config.panel_display = v;
                save_config(&self.config);
            }
            Message::SetIconStyle(v) => {
                self.config.icon_style = v;
                save_config(&self.config);
            }
            Message::ToggleShowPlayPanel => {
                self.config.show_play_panel = !self.config.show_play_panel;
                save_config(&self.config);
            }
            Message::ToggleShowNextPanel => {
                self.config.show_next_panel = !self.config.show_next_panel;
                save_config(&self.config);
            }
            Message::ToggleShowPrevPanel => {
                self.config.show_prev_panel = !self.config.show_prev_panel;
                save_config(&self.config);
            }
            Message::ToggleShowAlbumArt => {
                self.config.show_album_art = !self.config.show_album_art;
                save_config(&self.config);
                if self.config.show_album_art {
                    // Re-fetch art for current track
                    if let PlayerState::Active(t) = &self.player {
                        if let Some(url) = t.art_url.clone() {
                            return cosmic::task::future(async move {
                                Message::AlbumArt(fetch_art(url).await)
                            });
                        }
                    }
                } else {
                    self.album_art_path = None;
                }
            }
            Message::SetMaxLabelChars(n) => {
                self.config.max_label_chars = n;
                save_config(&self.config);
            }
            Message::SetPollInterval(s) => {
                self.config.poll_interval_secs = s;
                save_config(&self.config);
            }
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Self::Message> {
        let (_, v_pad) = self.core.applet.suggested_padding(true);

        match &self.player {
            // Spotify not running: single icon that launches it
            PlayerState::NotRunning => {
                let icon_size = self.core.applet.suggested_size(true).0;
                let icon = widget::icon::from_name(panel_icon_name(&self.config)).size(icon_size);
                let btn = cosmic::widget::button::custom(icon)
                    .padding([v_pad, 8])
                    .class(cosmic::theme::Button::AppletIcon)
                    .on_press(Message::LaunchSpotify);
                self.core.applet.autosize_window(btn).into()
            }

            // Spotify running: label + optional controls
            PlayerState::Stopped | PlayerState::Active(_) => {
                let label_text = match &self.player {
                    PlayerState::Active(t) => panel_label_text(t, &self.config),
                    _ => String::new(),
                };

                let is_playing = matches!(&self.player, PlayerState::Active(t) if t.is_playing);
                let have_popup = self.popup;

                // Main toggle area: icon [+ label]
                let icon_size = self.core.applet.suggested_size(true).0;
                let mut label_row = widget::row()
                    .spacing(4)
                    .align_y(Alignment::Center)
                    .push(widget::icon::from_name(panel_icon_name(&self.config)).size(icon_size));
                if !label_text.is_empty() {
                    label_row = label_row.push(self.core.applet.text(label_text));
                }

                let toggle_btn = cosmic::widget::button::custom(label_row)
                    .padding([v_pad, 6])
                    .class(cosmic::theme::Button::AppletIcon)
                    .on_press_with_rectangle(move |_, _| {
                        if let Some(id) = have_popup {
                            Message::Surface(destroy_popup(id))
                        } else {
                            Message::Surface(app_popup::<AppModel>(
                                move |state: &mut AppModel| {
                                    let new_id = Id::unique();
                                    state.popup = Some(new_id);
                                    let mut s = state.core.applet.get_popup_settings(
                                        state.core.main_window_id().unwrap(),
                                        new_id,
                                        None,
                                        None,
                                        None,
                                    );
                                    s.positioner.size_limits = cosmic::iced::Limits::NONE
                                        .min_width(300.0)
                                        .max_width(380.0)
                                        .min_height(80.0)
                                        .max_height(600.0);
                                    s
                                },
                                Some(Box::new(|state: &AppModel| {
                                    if state.show_settings {
                                        build_settings_view(state).map(cosmic::Action::App)
                                    } else {
                                        build_main_view(state).map(cosmic::Action::App)
                                    }
                                })),
                            ))
                        }
                    });

                let tooltip = Element::from(self.core.applet.applet_tooltip::<Message>(
                    toggle_btn,
                    "Spotify",
                    self.popup.is_some(),
                    |a| Message::Surface(a),
                    None,
                ));

                let mut panel_row = widget::row()
                    .spacing(0)
                    .align_y(Alignment::Center)
                    .push(tooltip);

                if self.config.show_prev_panel {
                    panel_row = panel_row.push(
                        cosmic::widget::button::custom(
                            widget::icon::from_name("media-skip-backward-symbolic").size(14),
                        )
                        .padding([v_pad, 4])
                        .class(cosmic::theme::Button::AppletIcon)
                        .on_press(Message::Previous),
                    );
                }

                if self.config.show_play_panel {
                    panel_row = panel_row.push(
                        cosmic::widget::button::custom(
                            widget::icon::from_name(play_pause_icon(is_playing)).size(14),
                        )
                        .padding([v_pad, 4])
                        .class(cosmic::theme::Button::AppletIcon)
                        .on_press(Message::PlayPause),
                    );
                }

                if self.config.show_next_panel {
                    panel_row = panel_row.push(
                        cosmic::widget::button::custom(
                            widget::icon::from_name("media-skip-forward-symbolic").size(14),
                        )
                        .padding([v_pad, 4])
                        .class(cosmic::theme::Button::AppletIcon)
                        .on_press(Message::Next),
                    );
                }

                self.core.applet.autosize_window(panel_row).into()
            }
        }
    }

    fn view_window(&self, _id: Id) -> Element<'_, Self::Message> {
        widget::text("").into()
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        cosmic::iced::time::every(Duration::from_secs(self.config.poll_interval_secs as u64))
            .map(|_| Message::Tick)
    }

    fn style(&self) -> Option<cosmic::iced_core::theme::Style> {
        Some(cosmic::applet::style())
    }
}

// ─── Popup: main / now-playing view ──────────────────────────────────────────

fn build_main_view(state: &AppModel) -> Element<'_, Message> {
    let mut content = list_column();

    // Header row: title + settings button
    content = content.add(
        widget::row()
            .push(widget::text("Spotify").size(13).width(Length::Fill))
            .push(
                cosmic::widget::button::custom(
                    widget::icon::from_name("preferences-system-symbolic").size(18),
                )
                .class(cosmic::theme::Button::Icon)
                .on_press(Message::ToggleSettings),
            )
            .spacing(4)
            .padding([8, 12, 4, 12])
            .align_y(Alignment::Center),
    );

    match &state.player {
        PlayerState::NotRunning => {
            content = content.add(
                widget::column()
                    .push(widget::text("Spotify is not running").size(13))
                    .push(
                        widget::button::standard("Launch Spotify")
                            .on_press(Message::LaunchSpotify),
                    )
                    .spacing(8)
                    .padding([12, 12]),
            );
        }

        PlayerState::Stopped => {
            content = content.add(
                widget::column()
                    .push(widget::text("Stopped").size(13))
                    .push(
                        widget::row()
                            .push(
                                cosmic::widget::button::custom(
                                    widget::icon::from_name("media-playback-start-symbolic")
                                        .size(22),
                                )
                                .class(cosmic::theme::Button::Standard)
                                .on_press(Message::PlayPause),
                            )
                            .spacing(8)
                            .align_y(Alignment::Center),
                    )
                    .spacing(8)
                    .padding([12, 12]),
            );
        }

        PlayerState::Active(track) => {
            // Album art
            if state.config.show_album_art {
                if let Some(path) = &state.album_art_path {
                    let handle =
                        cosmic::iced::widget::image::Handle::from_path(path);
                    content = content.add(
                        widget::container(
                            cosmic::iced_widget::image::Image::new(handle)
                                .width(Length::Fill)
                                .height(Length::Fixed(280.0))
                                .content_fit(ContentFit::Cover),
                        )
                        .width(Length::Fill),
                    );
                } else {
                    // Placeholder while art is loading
                    content = content.add(
                        widget::container(widget::text("♪").size(48))
                            .width(Length::Fill)
                            .height(Length::Fixed(120.0))
                            .align_x(cosmic::iced::alignment::Horizontal::Center)
                            .align_y(cosmic::iced::alignment::Vertical::Center),
                    );
                }
            }

            // Track info — centred to match controls
            content = content.add(
                widget::column()
                    .push(
                        widget::text(&track.title)
                            .size(15)
                            .width(Length::Fill)
                            .align_x(cosmic::iced::alignment::Horizontal::Center),
                    )
                    .push(
                        widget::text(if track.artist.is_empty() {
                            "Unknown artist".to_string()
                        } else {
                            track.artist.clone()
                        })
                        .size(12)
                        .width(Length::Fill)
                        .align_x(cosmic::iced::alignment::Horizontal::Center),
                    )
                    .push(
                        widget::text(if track.album.is_empty() {
                            String::new()
                        } else {
                            track.album.clone()
                        })
                        .size(11)
                        .width(Length::Fill)
                        .align_x(cosmic::iced::alignment::Horizontal::Center),
                    )
                    .spacing(2)
                    .padding([8, 12, 4, 12]),
            );

            // Progress: position / duration
            if track.length_us > 0 {
                let pos = fmt_duration(track.position_us);
                let dur = fmt_duration(track.length_us);
                content = content.add(
                    widget::container(
                        widget::text(format!("{pos} / {dur}"))
                            .size(11)
                            .align_x(cosmic::iced::alignment::Horizontal::Center),
                    )
                    .width(Length::Fill)
                    .align_x(cosmic::iced::alignment::Horizontal::Center)
                    .padding([0, 12, 4, 12]),
                );
            }

            // Transport controls: ⏮  ⏯  ⏭
            let controls = widget::row()
                .spacing(12)
                .align_y(Alignment::Center)
                .push(
                    cosmic::widget::button::custom(
                        widget::icon::from_name("media-skip-backward-symbolic").size(22),
                    )
                    .class(cosmic::theme::Button::Standard)
                    .on_press(Message::Previous),
                )
                .push(
                    cosmic::widget::button::custom(
                        widget::icon::from_name(play_pause_icon(track.is_playing)).size(28),
                    )
                    .class(cosmic::theme::Button::Suggested)
                    .on_press(Message::PlayPause),
                )
                .push(
                    cosmic::widget::button::custom(
                        widget::icon::from_name("media-skip-forward-symbolic").size(22),
                    )
                    .class(cosmic::theme::Button::Standard)
                    .on_press(Message::Next),
                );

            content = content.add(
                widget::container(controls)
                    .width(Length::Fill)
                    .align_x(cosmic::iced::alignment::Horizontal::Center)
                    .padding([4, 12, 12, 12]),
            );
        }
    }

    // Footer
    content = content.add(
        widget::row()
            .push(widget::space::horizontal())
            .push(widget::text(format!("v{VERSION}")).size(10))
            .padding([4, 12]),
    );

    Element::from(state.core.applet.popup_container(content))
}

// ─── Popup: settings view ─────────────────────────────────────────────────────

fn build_settings_view(state: &AppModel) -> Element<'_, Message> {
    let mut content = list_column();

    // Header
    content = content.add(
        widget::row()
            .push(widget::button::text("← Back").on_press(Message::ToggleSettings))
            .push(widget::space::horizontal())
            .push(widget::text("Settings").size(13))
            .padding([8, 12, 4, 12])
            .align_y(Alignment::Center),
    );

    // ── Panel label ────────────────────────────────────────────────────────────
    content = content.add(
        widget::column()
            .push(widget::text("Panel label").size(11))
            .padding([6, 12, 2, 12]),
    );
    let pd = &state.config.panel_display;
    content = content.add(
        widget::column()
            .push(
                widget::row()
                    .push(
                        widget::button::standard("Icon only").on_press_maybe(
                            if pd != "icon_only" {
                                Some(Message::SetPanelDisplay("icon_only".into()))
                            } else {
                                None
                            },
                        ),
                    )
                    .push(
                        widget::button::standard("Icon + Track").on_press_maybe(
                            if pd != "icon_track" {
                                Some(Message::SetPanelDisplay("icon_track".into()))
                            } else {
                                None
                            },
                        ),
                    )
                    .push(
                        widget::button::standard("Artist + Track").on_press_maybe(
                            if pd != "icon_artist_track" {
                                Some(Message::SetPanelDisplay("icon_artist_track".into()))
                            } else {
                                None
                            },
                        ),
                    )
                    .spacing(6)
                    .align_y(Alignment::Center),
            )
            .padding([2, 12, 4, 12]),
    );

    // Max label length
    let ml = state.config.max_label_chars;
    content = content.add(
        widget::column()
            .push(
                widget::row()
                    .push(
                        widget::text(format!("Label max chars: {}", if ml == 0 { "∞".to_string() } else { ml.to_string() }))
                            .size(11)
                            .width(Length::Fill),
                    )
                    .push(widget::button::standard("20").on_press_maybe(if ml != 20 { Some(Message::SetMaxLabelChars(20)) } else { None }))
                    .push(widget::button::standard("30").on_press_maybe(if ml != 30 { Some(Message::SetMaxLabelChars(30)) } else { None }))
                    .push(widget::button::standard("40").on_press_maybe(if ml != 40 { Some(Message::SetMaxLabelChars(40)) } else { None }))
                    .push(widget::button::standard("60").on_press_maybe(if ml != 60 { Some(Message::SetMaxLabelChars(60)) } else { None }))
                    .push(widget::button::standard("∞") .on_press_maybe(if ml != 0  { Some(Message::SetMaxLabelChars(0))  } else { None }))
                    .spacing(4)
                    .align_y(Alignment::Center),
            )
            .padding([2, 12, 6, 12]),
    );

    // ── Panel controls ─────────────────────────────────────────────────────────
    content = content.add(
        widget::column()
            .push(widget::text("Panel controls").size(11))
            .padding([6, 12, 2, 12]),
    );
    let play_label = if state.config.show_play_panel { "Play/pause: ON" } else { "Play/pause: OFF" };
    let next_label = if state.config.show_next_panel { "Next: ON" } else { "Next: OFF" };
    let prev_label = if state.config.show_prev_panel { "Prev: ON" } else { "Prev: OFF" };
    content = content.add(
        widget::column()
            .push(
                widget::row()
                    .push(widget::button::standard(play_label).on_press(Message::ToggleShowPlayPanel))
                    .push(widget::button::standard(next_label).on_press(Message::ToggleShowNextPanel))
                    .push(widget::button::standard(prev_label).on_press(Message::ToggleShowPrevPanel))
                    .spacing(6)
                    .align_y(Alignment::Center),
            )
            .padding([2, 12, 6, 12]),
    );

    // ── Icon style ─────────────────────────────────────────────────────────────
    content = content.add(
        widget::column()
            .push(widget::text("Panel icon").size(11))
            .padding([6, 12, 2, 12]),
    );
    let is = &state.config.icon_style;
    content = content.add(
        widget::column()
            .push(
                widget::row()
                    .push(
                        widget::button::standard("♪ Note (B&W)").on_press_maybe(
                            if is != "note_symbolic" {
                                Some(Message::SetIconStyle("note_symbolic".into()))
                            } else {
                                None
                            },
                        ),
                    )
                    .push(
                        widget::button::standard("Spotify (color)").on_press_maybe(
                            if is != "spotify_colored" {
                                Some(Message::SetIconStyle("spotify_colored".into()))
                            } else {
                                None
                            },
                        ),
                    )
                    .push(
                        widget::button::standard("Spotify (B&W)").on_press_maybe(
                            if is != "spotify_symbolic" {
                                Some(Message::SetIconStyle("spotify_symbolic".into()))
                            } else {
                                None
                            },
                        ),
                    )
                    .spacing(6)
                    .align_y(Alignment::Center),
            )
            .padding([2, 12, 6, 12]),
    );

    // ── Album art ──────────────────────────────────────────────────────────────
    let art_label = if state.config.show_album_art {
        "Album art in popup: ON"
    } else {
        "Album art in popup: OFF"
    };
    content = content.add(
        widget::column()
            .push(widget::button::standard(art_label).on_press(Message::ToggleShowAlbumArt))
            .padding([2, 12, 6, 12]),
    );

    // ── Poll interval ──────────────────────────────────────────────────────────
    content = content.add(
        widget::column()
            .push(widget::text("Refresh interval").size(11))
            .padding([6, 12, 2, 12]),
    );
    let pi = state.config.poll_interval_secs;
    content = content.add(
        widget::column()
            .push(
                widget::row()
                    .push(widget::button::standard("1s") .on_press_maybe(if pi != 1  { Some(Message::SetPollInterval(1))  } else { None }))
                    .push(widget::button::standard("3s") .on_press_maybe(if pi != 3  { Some(Message::SetPollInterval(3))  } else { None }))
                    .push(widget::button::standard("5s") .on_press_maybe(if pi != 5  { Some(Message::SetPollInterval(5))  } else { None }))
                    .push(widget::button::standard("10s").on_press_maybe(if pi != 10 { Some(Message::SetPollInterval(10)) } else { None }))
                    .spacing(6)
                    .align_y(Alignment::Center),
            )
            .padding([2, 12, 8, 12]),
    );

    Element::from(state.core.applet.popup_container(content))
}

// ─── Entry point ─────────────────────────────────────────────────────────────

fn main() -> cosmic::iced::Result {
    cosmic::applet::run::<AppModel>(())
}
