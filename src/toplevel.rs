/// Wayland handler thread for tracking toplevel window state and sending minimize requests.
///
/// Protocol flow (zcosmic_toplevel_info_v1 v2+):
///   ext_foreign_toplevel_list_v1  →  Toplevel  →  ext_foreign_toplevel_handle_v1  (app_id)
///   zcosmic_toplevel_info_v1      →  get_cosmic_toplevel(foreign)  →  zcosmic_toplevel_handle_v1  (state)
///   zcosmic_toplevel_manager_v1   →  set_minimized(cosmic_handle)
use libcosmic::cctk::{
    cosmic_protocols::{
        toplevel_info::v1::client::{zcosmic_toplevel_handle_v1, zcosmic_toplevel_info_v1},
        toplevel_management::v1::client::zcosmic_toplevel_manager_v1,
    },
    sctk::reexports::{calloop, calloop_wayland_source::WaylandSource},
    wayland_client::{
        Connection, Dispatch, QueueHandle, event_created_child,
        protocol::wl_registry,
    },
    wayland_protocols::ext::foreign_toplevel_list::v1::client::{
        ext_foreign_toplevel_handle_v1, ext_foreign_toplevel_list_v1,
    },
};
use libcosmic::iced_futures::futures::channel::mpsc::UnboundedSender;

// ─── Public API ───────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub enum TopCmd {
    Minimize(String),
}

#[derive(Clone, Debug)]
pub enum TopUpdate {
    Init(calloop::channel::Sender<TopCmd>),
    AppActivated { app_id: String, active: bool },
}

// ─── Internal state ───────────────────────────────────────────────────────────

struct Toplevel {
    foreign: ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
    cosmic: Option<zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1>,
    app_id: String,
    activated: bool,
}

struct AppData {
    exit: bool,
    foreign_list: Option<ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1>,
    cosmic_info: Option<zcosmic_toplevel_info_v1::ZcosmicToplevelInfoV1>,
    manager: Option<zcosmic_toplevel_manager_v1::ZcosmicToplevelManagerV1>,
    toplevels: Vec<Toplevel>,
    tx: UnboundedSender<TopUpdate>,
    watch_app_id: String,
    watch_activated: bool,
}

impl AppData {
    fn notify_if_changed(&mut self) {
        let now_active = self
            .toplevels
            .iter()
            .any(|t| t.app_id == self.watch_app_id && t.activated);
        if now_active != self.watch_activated {
            self.watch_activated = now_active;
            let _ = self.tx.unbounded_send(TopUpdate::AppActivated {
                app_id: self.watch_app_id.clone(),
                active: now_active,
            });
        }
    }

    fn request_cosmic_for_foreign(
        &mut self,
        foreign: &ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
        qh: &QueueHandle<Self>,
    ) {
        if let Some(info) = &self.cosmic_info {
            let cosmic = info.get_cosmic_toplevel(foreign, qh, ());
            if let Some(t) = self.toplevels.iter_mut().find(|t| &t.foreign == foreign) {
                t.cosmic = Some(cosmic);
            }
        }
    }
}

// ─── Registry dispatch ────────────────────────────────────────────────────────

impl Dispatch<wl_registry::WlRegistry, ()> for AppData {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name, interface, version,
        } = event
        {
            match interface.as_str() {
                "ext_foreign_toplevel_list_v1" => {
                    state.foreign_list = Some(registry.bind(name, 1, qh, ()));
                }
                "zcosmic_toplevel_info_v1" if version >= 2 => {
                    state.cosmic_info = Some(registry.bind(name, 2, qh, ()));
                }
                "zcosmic_toplevel_manager_v1" => {
                    state.manager = Some(registry.bind(name, 1, qh, ()));
                }
                _ => {}
            }
        }
    }
}

// ─── ext_foreign_toplevel_list_v1 ────────────────────────────────────────────

impl Dispatch<ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1, ()> for AppData {
    fn event(
        state: &mut Self,
        _: &ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1,
        event: ext_foreign_toplevel_list_v1::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let ext_foreign_toplevel_list_v1::Event::Toplevel { toplevel } = event {
            state.toplevels.push(Toplevel {
                foreign: toplevel.clone(),
                cosmic: None,
                app_id: String::new(),
                activated: false,
            });
            state.request_cosmic_for_foreign(&toplevel, qh);
        }
    }

    event_created_child!(AppData, ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1, [
        ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1, ()),
    ]);
}

// ─── ext_foreign_toplevel_handle_v1 ──────────────────────────────────────────

impl Dispatch<ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1, ()> for AppData {
    fn event(
        state: &mut Self,
        handle: &ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
        event: ext_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_foreign_toplevel_handle_v1::Event::AppId { app_id } => {
                if let Some(t) = state.toplevels.iter_mut().find(|t| &t.foreign == handle) {
                    t.app_id = app_id;
                }
            }
            ext_foreign_toplevel_handle_v1::Event::Done => {
                state.notify_if_changed();
            }
            ext_foreign_toplevel_handle_v1::Event::Closed => {
                state.toplevels.retain(|t| &t.foreign != handle);
                state.notify_if_changed();
            }
            _ => {}
        }
    }
}

// ─── zcosmic_toplevel_info_v1 ─────────────────────────────────────────────────

impl Dispatch<zcosmic_toplevel_info_v1::ZcosmicToplevelInfoV1, ()> for AppData {
    fn event(
        _: &mut Self,
        _: &zcosmic_toplevel_info_v1::ZcosmicToplevelInfoV1,
        _: zcosmic_toplevel_info_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

// ─── zcosmic_toplevel_handle_v1 ───────────────────────────────────────────────

impl Dispatch<zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1, ()> for AppData {
    fn event(
        state: &mut Self,
        handle: &zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1,
        event: zcosmic_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zcosmic_toplevel_handle_v1::Event::State { state: raw } => {
                let activated = raw
                    .chunks_exact(4)
                    .map(|b| u32::from_ne_bytes(b.try_into().unwrap()))
                    .any(|v| v == zcosmic_toplevel_handle_v1::State::Activated as u32);
                if let Some(t) = state
                    .toplevels
                    .iter_mut()
                    .find(|t| t.cosmic.as_ref() == Some(handle))
                {
                    t.activated = activated;
                }
            }
            zcosmic_toplevel_handle_v1::Event::Done => {
                state.notify_if_changed();
            }
            zcosmic_toplevel_handle_v1::Event::Closed => {
                // The foreign handle's Closed event handles list removal
            }
            _ => {}
        }
    }
}

// ─── zcosmic_toplevel_manager_v1 ─────────────────────────────────────────────

impl Dispatch<zcosmic_toplevel_manager_v1::ZcosmicToplevelManagerV1, ()> for AppData {
    fn event(
        _: &mut Self,
        _: &zcosmic_toplevel_manager_v1::ZcosmicToplevelManagerV1,
        _: zcosmic_toplevel_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

// ─── Entry point ──────────────────────────────────────────────────────────────

pub fn wayland_handler(watch_app_id: String, tx: UnboundedSender<TopUpdate>) {
    let (cmd_tx, rx) = calloop::channel::channel::<TopCmd>();
    let _ = tx.unbounded_send(TopUpdate::Init(cmd_tx));

    // Always use the regular Wayland socket — ext_foreign_toplevel_list is not
    // available on the privileged compositor socket used by the panel itself.
    let conn = match Connection::connect_to_env() {
        Ok(c) => c,
        Err(e) => { eprintln!("[toplevel] connect failed: {e}"); return; }
    };

    let display = conn.display();
    let mut event_queue = conn.new_event_queue::<AppData>();
    let qh = event_queue.handle();
    let _registry = display.get_registry(&qh, ());

    let mut app_data = AppData {
        exit: false,
        foreign_list: None,
        cosmic_info: None,
        manager: None,
        toplevels: Vec::new(),
        tx,
        watch_app_id,
        watch_activated: false,
    };

    // Four roundtrips: bind globals → toplevel list → app_id + cosmic handles → state events
    event_queue.roundtrip(&mut app_data).ok();
    event_queue.roundtrip(&mut app_data).ok();
    event_queue.roundtrip(&mut app_data).ok();
    event_queue.roundtrip(&mut app_data).ok();

    let mut event_loop = calloop::EventLoop::<AppData>::try_new().unwrap();
    let wayland_source = WaylandSource::new(conn, event_queue);
    let handle = event_loop.handle();
    if wayland_source.insert(handle.clone()).is_err() {
        return;
    }

    if handle
        .insert_source(rx, |event, _, state| match event {
            calloop::channel::Event::Msg(TopCmd::Minimize(app_id)) => {
                if let Some(manager) = state.manager.as_ref() {
                    if let Some(t) = state.toplevels.iter().find(|t| t.app_id == app_id) {
                        if let Some(cosmic) = &t.cosmic {
                            manager.set_minimized(cosmic);
                        }
                    }
                }
            }
            calloop::channel::Event::Closed => {
                state.exit = true;
            }
        })
        .is_err()
    {
        return;
    }

    loop {
        if app_data.exit {
            break;
        }
        if event_loop.dispatch(None, &mut app_data).is_err() {
            break;
        }
    }
}
