//! Linux pointer injection through a protocol the compositor applies.
//!
//! KWin does not advertise `zwlr_virtual_pointer`. enigo binds `org_kde_kwin_fake_input`
//! at version 1, authenticates, and never sends events, so `move_mouse` returns `Ok`
//! without moving the cursor. Absolute motion on that interface exists since version 3,
//! and current KWin authenticates every client that asks (`setAuthenticated(true)`).
//! Coordinates are global pixels.
//!
//! Compositors that only offer a virtual pointer map `x / x_extent`. The extent has to
//! be the screen size from [`etherquic_play::place_pointer`], not `u32::MAX`.

use std::time::Instant;

use crate::{wayland_wheel, PointerPlace};
use wayland_client::protocol::{wl_pointer, wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols_plasma::fake_input::client::org_kde_kwin_fake_input;
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1, zwlr_virtual_pointer_v1,
};

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;

const AXIS_VERTICAL: u32 = 0;
const AXIS_HORIZONTAL: u32 = 1;

/// Linux event code for an EQIN button bit. `None` when this seat does not send that bit.
pub fn button_code(bit: u8) -> Option<u32> {
    match bit {
        etherquic_input::Buttons::LEFT => Some(BTN_LEFT),
        etherquic_input::Buttons::RIGHT => Some(BTN_RIGHT),
        etherquic_input::Buttons::MIDDLE => Some(BTN_MIDDLE),
        _ => None,
    }
}

enum Backend {
    Fake(org_kde_kwin_fake_input::OrgKdeKwinFakeInput),
    Virtual(zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1),
}

struct SeatState {
    seat: Option<wl_seat::WlSeat>,
    fake: Option<org_kde_kwin_fake_input::OrgKdeKwinFakeInput>,
    pointer_manager: Option<zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1>,
}

impl SeatState {
    fn new() -> Self {
        Self {
            seat: None,
            fake: None,
            pointer_manager: None,
        }
    }
}

pub struct PointerSeat {
    queue: EventQueue<SeatState>,
    state: SeatState,
    backend: Backend,
    started: Instant,
}

impl PointerSeat {
    /// Connect and pick a backend that can place the pointer in absolute pixels.
    ///
    /// Returns an error when neither KWin fake-input (version 3 or newer) nor a
    /// virtual pointer is available. Callers must not treat that as a successful send.
    pub fn open() -> Result<Self, String> {
        let connection =
            Connection::connect_to_env().map_err(|error| format!("wayland connect: {error}"))?;
        let mut queue = connection.new_event_queue();
        let qh = queue.handle();
        connection.display().get_registry(&qh, ());
        let mut state = SeatState::new();
        queue
            .roundtrip(&mut state)
            .map_err(|error| format!("wayland registry: {error}"))?;
        let backend = backend(&mut queue, &mut state)?;
        Ok(Self {
            queue,
            state,
            backend,
            started: Instant::now(),
        })
    }

    pub fn absolute(&mut self, place: PointerPlace) -> Result<(), String> {
        let time = self.time();
        match &self.backend {
            Backend::Fake(fake) => {
                fake.pointer_motion_absolute(f64::from(place.x), f64::from(place.y));
            }
            Backend::Virtual(pointer) => {
                pointer.motion_absolute(time, place.x, place.y, place.x_extent, place.y_extent);
                pointer.frame();
            }
        }
        self.sync()
    }

    pub fn relative(&mut self, dx: i32, dy: i32) -> Result<(), String> {
        let time = self.time();
        match &self.backend {
            Backend::Fake(fake) => {
                fake.pointer_motion(f64::from(dx), f64::from(dy));
            }
            Backend::Virtual(pointer) => {
                pointer.motion(time, f64::from(dx), f64::from(dy));
                pointer.frame();
            }
        }
        self.sync()
    }

    pub fn button(&mut self, code: u32, down: bool) -> Result<(), String> {
        let time = self.time();
        let state = if down { 1 } else { 0 };
        match &self.backend {
            Backend::Fake(fake) => {
                fake.button(code, state);
            }
            Backend::Virtual(pointer) => {
                let button_state = if down {
                    wl_pointer::ButtonState::Pressed
                } else {
                    wl_pointer::ButtonState::Released
                };
                pointer.button(time, code, button_state);
                pointer.frame();
            }
        }
        self.sync()
    }

    pub fn wheel(&mut self, wheel_y: i16, wheel_x: i16) -> Result<(), String> {
        if wheel_y == 0 && wheel_x == 0 {
            return Ok(());
        }
        let (vertical, horizontal) = wayland_wheel(wheel_y, wheel_x);
        let time = self.time();
        match &self.backend {
            Backend::Fake(fake) => {
                if wheel_y != 0 {
                    fake.axis(AXIS_VERTICAL, vertical);
                }
                if wheel_x != 0 {
                    fake.axis(AXIS_HORIZONTAL, horizontal);
                }
            }
            Backend::Virtual(pointer) => {
                if wheel_y != 0 {
                    pointer.axis(time, wl_pointer::Axis::VerticalScroll, vertical);
                    pointer.axis_discrete(
                        time,
                        wl_pointer::Axis::VerticalScroll,
                        vertical,
                        -i32::from(wheel_y),
                    );
                }
                if wheel_x != 0 {
                    pointer.axis(time, wl_pointer::Axis::HorizontalScroll, horizontal);
                    pointer.axis_discrete(
                        time,
                        wl_pointer::Axis::HorizontalScroll,
                        horizontal,
                        i32::from(wheel_x),
                    );
                }
                pointer.frame();
            }
        }
        self.sync()
    }

    fn time(&self) -> u32 {
        u32::try_from(self.started.elapsed().as_millis()).unwrap_or(u32::MAX)
    }

    fn sync(&mut self) -> Result<(), String> {
        self.queue
            .roundtrip(&mut self.state)
            .map(|_| ())
            .map_err(|error| format!("wayland roundtrip: {error}"))
    }
}

fn backend(queue: &mut EventQueue<SeatState>, state: &mut SeatState) -> Result<Backend, String> {
    let mut rejected_fake = None;
    if let Some(fake) = state.fake.take() {
        let version = fake.version();
        if version >= 3 {
            // KWin drops motion, buttons, and axis until this request has been applied.
            fake.authenticate("etherquic-play".to_string(), "remote pointer".to_string());
            queue
                .roundtrip(state)
                .map_err(|error| format!("wayland authenticate: {error}"))?;
            return Ok(Backend::Fake(fake));
        }
        rejected_fake = Some(version);
    }
    let Some(manager) = state.pointer_manager.take() else {
        return Err(format!(
            "compositor has no KWin fake-input v3 and no virtual pointer (fake version {rejected_fake:?})"
        ));
    };
    let qh = queue.handle();
    let pointer = manager.create_virtual_pointer(state.seat.as_ref(), &qh, ());
    manager.destroy();
    queue
        .roundtrip(state)
        .map_err(|error| format!("wayland virtual pointer: {error}"))?;
    Ok(Backend::Virtual(pointer))
}

impl Dispatch<wl_registry::WlRegistry, ()> for SeatState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        (): &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        else {
            return;
        };
        if version == 0 {
            return;
        }
        match interface.as_str() {
            "wl_seat" if state.seat.is_none() => {
                state.seat = Some(registry.bind(name, 1, qh, ()));
            }
            "org_kde_kwin_fake_input" if state.fake.is_none() => {
                let max = org_kde_kwin_fake_input::OrgKdeKwinFakeInput::interface().version;
                state.fake = Some(registry.bind(name, version.min(max), qh, ()));
            }
            "zwlr_virtual_pointer_manager_v1" if state.pointer_manager.is_none() => {
                let max = zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1::interface()
                    .version;
                state.pointer_manager = Some(registry.bind(name, version.min(max), qh, ()));
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for SeatState {
    fn event(
        _state: &mut Self,
        _seat: &wl_seat::WlSeat,
        _event: wl_seat::Event,
        (): &(),
        _: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<org_kde_kwin_fake_input::OrgKdeKwinFakeInput, ()> for SeatState {
    fn event(
        _state: &mut Self,
        _fake: &org_kde_kwin_fake_input::OrgKdeKwinFakeInput,
        _event: org_kde_kwin_fake_input::Event,
        (): &(),
        _: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1, ()> for SeatState {
    fn event(
        _state: &mut Self,
        _manager: &zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
        _event: zwlr_virtual_pointer_manager_v1::Event,
        (): &(),
        _: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1, ()> for SeatState {
    fn event(
        _state: &mut Self,
        _pointer: &zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
        _event: zwlr_virtual_pointer_v1::Event,
        (): &(),
        _: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}
