//! Pointer injection through libei when the compositor has no fake-input and no
//! virtual pointer. KWin Plasma 6 hides `org_kde_kwin_fake_input` from ordinary
//! clients. The remote-desktop portal hands back an EIS socket, and absolute
//! motion is in that device's logical pixels ([`etherquic_play::ei_absolute`]).

use std::collections::HashMap;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::thread;
use std::time::{Duration, Instant};

use crate::{ei_absolute, wayland_wheel, PointerPlace};
use reis::ei::{self, Connection};
use reis::PendingRequestResult;

struct SeatData {
    capabilities: HashMap<String, u64>,
}

struct Region {
    offset_x: u32,
    offset_y: u32,
    width: u32,
    height: u32,
}

struct DeviceData {
    interfaces: HashMap<String, reis::Object>,
    regions: Vec<Region>,
    resumed: bool,
    emulating: bool,
}

impl DeviceData {
    fn interface<T: reis::Interface>(&self) -> Option<T> {
        self.interfaces.get(T::NAME)?.clone().downcast()
    }
}

pub struct EiSeat {
    context: ei::Context,
    connection: Connection,
    seats: HashMap<ei::Seat, SeatData>,
    devices: HashMap<ei::Device, DeviceData>,
    sequence: u32,
    last_serial: u32,
    started: Instant,
    /// Portal proxies and the zbus reader. `None` when `LIBEI_SOCKET` was set.
    portal: Option<PortalSession>,
}

impl EiSeat {
    /// Open an EIS sender. This asks the remote-desktop portal when no EIS
    /// socket is in the environment, and that portal shows a permission dialog.
    pub fn open() -> Result<Self, String> {
        let (context, portal) = open_context()?;
        let reis::handshake::HandshakeResp {
            connection, serial, ..
        } = reis::handshake::ei_handshake_blocking(
            &context,
            "etherquic-play",
            ei::handshake::ContextType::Sender,
        )
        .map_err(|error| format!("libei handshake: {error}"))?;
        let mut seat = Self {
            context,
            connection,
            seats: HashMap::new(),
            devices: HashMap::new(),
            sequence: serial,
            last_serial: serial,
            started: Instant::now(),
            portal,
        };
        let deadline = Instant::now() + Duration::from_secs(3);
        while !seat.has_absolute() {
            seat.pump()?;
            if seat.has_absolute() {
                break;
            }
            if Instant::now() > deadline {
                return Err("libei offered no absolute pointer".into());
            }
            thread::sleep(Duration::from_millis(10));
        }
        seat.start_emulating()?;
        Ok(seat)
    }

    pub fn absolute(&mut self, place: PointerPlace) -> Result<(), String> {
        self.pump()?;
        let (device, data) = self
            .absolute_device()
            .ok_or("libei absolute pointer is gone")?;
        let (offset_x, offset_y, width, height) = data
            .regions
            .first()
            .map(|region| {
                (
                    region.offset_x,
                    region.offset_y,
                    region.width,
                    region.height,
                )
            })
            .unwrap_or((0, 0, place.x_extent, place.y_extent));
        let (x, y) = ei_absolute(place, offset_x, offset_y, width, height);
        let pointer = data
            .interface::<ei::PointerAbsolute>()
            .ok_or("libei device has no absolute pointer")?;
        pointer.motion_absolute(x, y);
        self.frame(device)?;
        self.pump()
    }

    pub fn relative(&mut self, dx: i32, dy: i32) -> Result<(), String> {
        self.pump()?;
        let (device, data) = self
            .relative_device()
            .ok_or("libei relative pointer is gone")?;
        let pointer = data
            .interface::<ei::Pointer>()
            .ok_or("libei device has no relative pointer")?;
        pointer.motion_relative(dx as f32, dy as f32);
        self.frame(device)?;
        self.pump()
    }

    pub fn button(&mut self, code: u32, down: bool) -> Result<(), String> {
        self.pump()?;
        let (device, data) = self.button_device().ok_or("libei button device is gone")?;
        let button = data
            .interface::<ei::Button>()
            .ok_or("libei device has no buttons")?;
        let state = if down {
            ei::button::ButtonState::Press
        } else {
            ei::button::ButtonState::Released
        };
        button.button(code, state);
        self.frame(device)?;
        self.pump()
    }

    pub fn wheel(&mut self, wheel_y: i16, wheel_x: i16) -> Result<(), String> {
        self.pump()?;
        if wheel_y == 0 && wheel_x == 0 {
            return Ok(());
        }
        let (device, data) = self.scroll_device().ok_or("libei scroll device is gone")?;
        let scroll = data
            .interface::<ei::Scroll>()
            .ok_or("libei device has no scroll")?;
        let (vertical, horizontal) = wayland_wheel(wheel_y, wheel_x);
        scroll.scroll(horizontal as f32, vertical as f32);
        self.frame(device)?;
        self.pump()
    }

    fn frame(&mut self, device: ei::Device) -> Result<(), String> {
        let elapsed = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        device.frame(self.sequence, elapsed);
        self.sequence = self.sequence.wrapping_add(1);
        self.context
            .flush()
            .map_err(|error| format!("libei flush: {error}"))
    }

    fn has_absolute(&self) -> bool {
        self.devices
            .values()
            .any(|data| data.resumed && data.interface::<ei::PointerAbsolute>().is_some())
    }

    fn absolute_device(&self) -> Option<(ei::Device, &DeviceData)> {
        self.devices.iter().find_map(|(device, data)| {
            data.interface::<ei::PointerAbsolute>()
                .map(|_| (device.clone(), data))
        })
    }

    fn relative_device(&self) -> Option<(ei::Device, &DeviceData)> {
        self.devices.iter().find_map(|(device, data)| {
            data.interface::<ei::Pointer>()
                .map(|_| (device.clone(), data))
        })
    }

    fn button_device(&self) -> Option<(ei::Device, &DeviceData)> {
        self.devices.iter().find_map(|(device, data)| {
            data.interface::<ei::Button>()
                .map(|_| (device.clone(), data))
        })
    }

    fn scroll_device(&self) -> Option<(ei::Device, &DeviceData)> {
        self.devices.iter().find_map(|(device, data)| {
            data.interface::<ei::Scroll>()
                .map(|_| (device.clone(), data))
        })
    }

    fn start_emulating(&mut self) -> Result<(), String> {
        let serial = self.last_serial;
        let sequence = self.sequence;
        let pending: Vec<ei::Device> = self
            .devices
            .iter()
            .filter(|(_, data)| data.resumed && !data.emulating)
            .map(|(device, _)| device.clone())
            .collect();
        for device in pending {
            device.start_emulating(sequence, serial);
            if let Some(data) = self.devices.get_mut(&device) {
                data.emulating = true;
            }
            self.sequence = self.sequence.wrapping_add(1);
        }
        self.context
            .flush()
            .map_err(|error| format!("libei emulate: {error}"))?;
        self.pump()
    }

    fn pump(&mut self) -> Result<(), String> {
        match self.context.read() {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) => return Err(format!("libei read: {error}")),
        }
        while let Some(result) = self.context.pending_event() {
            let request = match result {
                PendingRequestResult::Request(request) => request,
                PendingRequestResult::ParseError(error) => {
                    return Err(format!("libei parse: {error:?}"));
                }
                PendingRequestResult::InvalidObject(_) => continue,
            };
            self.one(request)?;
        }
        self.context
            .flush()
            .map_err(|error| format!("libei flush: {error}"))
    }

    fn one(&mut self, event: ei::Event) -> Result<(), String> {
        match event {
            ei::Event::Connection(_, ei::connection::Event::Ping { ping }) => {
                ping.done(0);
            }
            ei::Event::Connection(
                _,
                ei::connection::Event::Disconnected {
                    reason,
                    explanation,
                    ..
                },
            ) => {
                return Err(format!("libei disconnected: {reason:?} {explanation}"));
            }
            ei::Event::Connection(_, ei::connection::Event::Seat { seat }) => {
                self.seats.insert(
                    seat,
                    SeatData {
                        capabilities: HashMap::new(),
                    },
                );
            }
            ei::Event::Seat(seat, ei::seat::Event::Capability { mask, interface }) => {
                if let Some(data) = self.seats.get_mut(&seat) {
                    data.capabilities.insert(interface, mask);
                }
            }
            ei::Event::Seat(seat, ei::seat::Event::Done) => {
                let bitmask = self
                    .seats
                    .get(&seat)
                    .map(|data| data.capabilities.values().fold(0, |bits, mask| bits | mask))
                    .unwrap_or(0);
                seat.bind(bitmask);
            }
            ei::Event::Seat(seat, ei::seat::Event::Destroyed { .. }) => {
                self.seats.remove(&seat);
            }
            ei::Event::Seat(_, ei::seat::Event::Device { device }) => {
                self.devices.insert(
                    device,
                    DeviceData {
                        interfaces: HashMap::new(),
                        regions: Vec::new(),
                        resumed: false,
                        emulating: false,
                    },
                );
            }
            ei::Event::Device(device, ei::device::Event::Destroyed { .. }) => {
                self.devices.remove(&device);
            }
            ei::Event::Device(
                device,
                ei::device::Event::Region {
                    offset_x,
                    offset_y,
                    width,
                    hight: height,
                    ..
                },
            ) => {
                if let Some(data) = self.devices.get_mut(&device) {
                    data.regions.push(Region {
                        offset_x,
                        offset_y,
                        width,
                        height,
                    });
                }
            }
            ei::Event::Device(device, ei::device::Event::Interface { object }) => {
                if let Some(data) = self.devices.get_mut(&device) {
                    data.interfaces
                        .insert(object.interface().to_string(), object);
                }
            }
            ei::Event::Device(device, ei::device::Event::Resumed { serial }) => {
                self.last_serial = serial;
                if let Some(data) = self.devices.get_mut(&device) {
                    data.resumed = true;
                }
            }
            ei::Event::Device(device, ei::device::Event::Paused { serial }) => {
                self.last_serial = serial;
                if let Some(data) = self.devices.get_mut(&device) {
                    data.resumed = false;
                    data.emulating = false;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

impl Drop for EiSeat {
    fn drop(&mut self) {
        // Stop the EIS client before the portal session. The bus reader has to
        // still be running when the session is closed.
        let serial = self.last_serial;
        for (device, data) in &self.devices {
            if data.emulating {
                device.stop_emulating(serial);
            }
        }
        self.connection.disconnect();
        let _ = self.context.flush();
        if let Some(portal) = self.portal.as_mut() {
            portal.shutdown();
        }
    }
}

/// Current-thread runtime kept inside `block_on` until the seat drops.
///
/// ashpd's session bus connection is created on this runtime. Dropping the
/// runtime aborts zbus's socket reader, the bus client shuts its read side,
/// and xdg-desktop-portal closes the remote-desktop session and the EIS fd.
struct PortalSession {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl PortalSession {
    fn shutdown(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for PortalSession {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn open_context() -> Result<(ei::Context, Option<PortalSession>), String> {
    if let Some(context) =
        ei::Context::connect_to_env().map_err(|error| format!("libei env: {error}"))?
    {
        return Ok((context, None));
    }
    let (fd, portal) = spawn_held_runtime(open_portal)?;
    let stream = UnixStream::from(fd);
    stream
        .set_nonblocking(true)
        .map_err(|error| format!("libei socket: {error}"))?;
    let context = ei::Context::new(stream).map_err(|error| format!("libei context: {error}"))?;
    Ok((context, Some(portal)))
}

/// Run `work` on a private current-thread runtime. `work` sends its value and
/// then waits until the returned session drops, still inside `block_on`.
fn spawn_held_runtime<T, F, Fut>(work: F) -> Result<(T, PortalSession), String>
where
    T: Send + 'static,
    F: FnOnce(
            tokio::sync::oneshot::Receiver<()>,
            std::sync::mpsc::Sender<Result<T, String>>,
        ) -> Fut
        + Send
        + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let (value_tx, value_rx) = std::sync::mpsc::channel();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    let thread = thread::Builder::new()
        .name("etherquic-play-portal".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = value_tx.send(Err(format!("libei runtime: {error}")));
                    return;
                }
            };
            runtime.block_on(work(stop_rx, value_tx));
        })
        .map_err(|error| format!("libei portal thread: {error}"))?;
    let value = match value_rx.recv() {
        Ok(value) => value,
        Err(_) => {
            let _ = stop_tx.send(());
            let _ = thread.join();
            return Err("libei portal thread ended".into());
        }
    };
    match value {
        Ok(value) => Ok((
            value,
            PortalSession {
                stop: Some(stop_tx),
                thread: Some(thread),
            },
        )),
        Err(error) => {
            let _ = stop_tx.send(());
            let _ = thread.join();
            Err(error)
        }
    }
}

async fn open_portal(
    stop: tokio::sync::oneshot::Receiver<()>,
    ready: std::sync::mpsc::Sender<Result<OwnedFd, String>>,
) {
    use ashpd::desktop::remote_desktop::{DeviceType, RemoteDesktop};

    let opened = async {
        let desktop = RemoteDesktop::new()
            .await
            .map_err(|error| format!("remote desktop portal: {error}"))?;
        let session = desktop
            .create_session()
            .await
            .map_err(|error| format!("remote desktop session: {error}"))?;
        desktop
            .select_devices(
                &session,
                DeviceType::Keyboard | DeviceType::Pointer,
                None,
                ashpd::desktop::PersistMode::Application,
            )
            .await
            .map_err(|error| format!("remote desktop devices: {error}"))?;
        desktop
            .start(&session, None)
            .await
            .map_err(|error| format!("remote desktop start: {error}"))?;
        let fd = desktop
            .connect_to_eis(&session)
            .await
            .map_err(|error| format!("remote desktop eis: {error}"))?;
        Ok((desktop, session, fd))
    }
    .await;
    let (desktop, session, fd) = match opened {
        Ok(opened) => opened,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    if ready.send(Ok(fd)).is_err() {
        let _ = session.close().await;
        drop(desktop);
        return;
    }
    // `desktop` and `session` stay alive until the seat drops. `stop.await`
    // keeps this runtime polling, so the zbus reader is not aborted.
    let _ = stop.await;
    let _ = session.close().await;
    drop(session);
    drop(desktop);
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{mpsc, Mutex};
    use std::time::Duration;

    use super::spawn_held_runtime;

    static EIS_SOCKET: Mutex<()> = Mutex::new(());

    #[test]
    fn held_runtime_keeps_polling_until_the_portal_session_drops() {
        let ticks = std::sync::Arc::new(AtomicU64::new(0));
        let clock = ticks.clone();
        let (_value, session) = spawn_held_runtime(
            move |stop, ready: mpsc::Sender<Result<(), String>>| async move {
                let clock = clock.clone();
                tokio::spawn(async move {
                    loop {
                        clock.fetch_add(1, Ordering::Relaxed);
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                });
                let _ = ready.send(Ok(()));
                let _ = stop.await;
            },
        )
        .expect("portal runtime");
        let started = ticks.load(Ordering::Relaxed);
        assert!(
            wait_until(
                || ticks.load(Ordering::Relaxed) > started,
                Duration::from_millis(500),
            ),
            "portal runtime stopped polling while the session was held"
        );
        drop(session);
        let stopped = ticks.load(Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(40));
        assert_eq!(
            ticks.load(Ordering::Relaxed),
            stopped,
            "portal runtime kept polling after the session dropped"
        );
    }

    #[test]
    fn pointer_methods_answer_an_eis_ping_and_place_the_center() {
        let _guard = EIS_SOCKET.lock().unwrap_or_else(|error| error.into_inner());
        let path = std::env::temp_dir().join(format!(
            "etherquic-play-eis-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0)
        ));
        let listener = reis::eis::Listener::bind(&path).expect("eis listener");
        let (ping_tx, ping_rx) = mpsc::channel();
        let (ack_tx, ack_rx) = mpsc::channel();
        let (report_tx, report_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let result = serve_pointer(listener, ping_rx, ack_tx);
            let _ = report_tx.send(result);
        });
        let previous = std::env::var_os("LIBEI_SOCKET");
        let _restore = SocketEnv { previous };
        std::env::set_var("LIBEI_SOCKET", &path);
        let mut seat = super::EiSeat::open().expect("libei seat");
        ping_tx.send(()).expect("ping command");
        ack_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("eis server did not send the ping");
        let place = crate::place_pointer(32_768, 32_768, 1920, 1080);
        seat.absolute(place).expect("absolute");
        seat.relative(4, -5).expect("relative");
        seat.button(0x110, true).expect("button");
        seat.wheel(1, -2).expect("wheel");
        drop(seat);
        let seen = report_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("eis server hung")
            .expect("eis server");
        server.join().expect("eis server thread");
        assert!(
            seen.ping_done,
            "pointer methods never answered ei_pingpong: {seen:?}"
        );
        assert_eq!((seen.x, seen.y), (Some(960.0), Some(540.0)), "{seen:?}");
        assert_eq!(seen.relative, Some((4.0, -5.0)), "{seen:?}");
        assert_eq!(
            seen.button,
            Some((0x110, reis::ei::button::ButtonState::Press)),
            "{seen:?}"
        );
        assert_eq!(seen.scroll, Some((-30.0, -15.0)), "{seen:?}");
        let _ = std::fs::remove_file(path);
    }

    struct SocketEnv {
        previous: Option<std::ffi::OsString>,
    }

    impl Drop for SocketEnv {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => std::env::set_var("LIBEI_SOCKET", value),
                None => std::env::remove_var("LIBEI_SOCKET"),
            }
        }
    }

    fn wait_until(mut ready: impl FnMut() -> bool, limit: Duration) -> bool {
        let deadline = std::time::Instant::now() + limit;
        while std::time::Instant::now() < deadline {
            if ready() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        ready()
    }

    #[derive(Clone, Debug, Default)]
    struct Seen {
        ping_done: bool,
        x: Option<f32>,
        y: Option<f32>,
        relative: Option<(f32, f32)>,
        button: Option<(u32, reis::ei::button::ButtonState)>,
        scroll: Option<(f32, f32)>,
    }

    impl Seen {
        fn complete(&self) -> bool {
            self.ping_done
                && self.x.is_some()
                && self.relative.is_some()
                && self.button.is_some()
                && self.scroll.is_some()
        }
    }

    fn serve_pointer(
        listener: reis::eis::Listener,
        ping: mpsc::Receiver<()>,
        acked: mpsc::Sender<()>,
    ) -> Result<Seen, String> {
        use reis::eis::device::DeviceType;
        use reis::handshake::EisHandshaker;
        use reis::request::{DeviceCapability, EisRequestConverter};
        use reis::PendingRequestResult;

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut accepted = None;
        let mut handshaker: Option<EisHandshaker> = None;
        let mut live: Option<(EisRequestConverter, reis::request::Seat)> = None;
        let mut armed = false;
        let mut ping_sent = false;
        let mut seen = Seen::default();

        while std::time::Instant::now() < deadline && !seen.complete() {
            if accepted.is_none() {
                match listener.accept() {
                    Ok(Some(context)) => {
                        handshaker = Some(EisHandshaker::new(&context, 1));
                        accepted = Some(context);
                    }
                    Ok(None) => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => return Err(format!("eis accept: {error}")),
                }
            }
            let context = accepted.as_ref().expect("eis context");
            if let Err(error) = context.read() {
                return Err(format!("eis read: {error}"));
            }
            let mut finished = None;
            while let Some(result) = context.pending_request() {
                let request = match result {
                    PendingRequestResult::Request(request) => request,
                    PendingRequestResult::ParseError(error) => {
                        return Err(format!("eis parse: {error:?}"));
                    }
                    PendingRequestResult::InvalidObject(id) => {
                        return Err(format!("eis invalid object {id}"));
                    }
                };
                if handshaker.is_some() {
                    let response = handshaker
                        .as_mut()
                        .expect("handshaker")
                        .handle_request(request)
                        .map_err(|error| format!("eis handshake: {error}"))?;
                    if response.is_some() {
                        finished = response;
                        break;
                    }
                    continue;
                }
                note(&mut seen, &request);
                let bind = matches!(
                    request,
                    reis::eis::Request::Seat(_, reis::eis::seat::Request::Bind { .. })
                );
                if let Some((converter, _)) = live.as_mut() {
                    converter
                        .handle_request(request)
                        .map_err(|error| format!("eis request: {error}"))?;
                }
                if bind && !armed {
                    if let Some((converter, seat)) = live.as_ref() {
                        let device = seat.add_device(
                            Some("pointer"),
                            DeviceType::Virtual,
                            &[
                                DeviceCapability::Pointer,
                                DeviceCapability::PointerAbsolute,
                                DeviceCapability::Button,
                                DeviceCapability::Scroll,
                            ],
                            |device| {
                                device.device().region(0, 0, 1920, 1080, 1.0);
                            },
                        );
                        device.resumed();
                        converter
                            .handle()
                            .flush()
                            .map_err(|error| format!("eis device flush: {error}"))?;
                        armed = true;
                    }
                }
            }
            if let Some(response) = finished {
                handshaker = None;
                let converter = EisRequestConverter::new(context, response, 1);
                let seat = converter.handle().add_seat(
                    Some("default"),
                    &[
                        DeviceCapability::Pointer,
                        DeviceCapability::PointerAbsolute,
                        DeviceCapability::Button,
                        DeviceCapability::Scroll,
                    ],
                );
                converter
                    .handle()
                    .flush()
                    .map_err(|error| format!("eis seat flush: {error}"))?;
                live = Some((converter, seat));
            }
            if armed && !ping_sent {
                ping.recv_timeout(Duration::from_secs(3))
                    .map_err(|error| format!("eis ping wait: {error}"))?;
                let (converter, _) = live.as_ref().expect("eis session");
                converter.handle().connection().ping(1);
                converter
                    .handle()
                    .flush()
                    .map_err(|error| format!("eis ping flush: {error}"))?;
                acked.send(()).map_err(|_| "eis ack failed".to_string())?;
                ping_sent = true;
            } else if !seen.complete() {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        if !seen.complete() {
            return Err(format!("eis client stopped early: {seen:?}"));
        }
        Ok(seen)
    }

    fn note(seen: &mut Seen, request: &reis::eis::Request) {
        match request {
            reis::eis::Request::Pingpong(_, reis::eis::pingpong::Request::Done { .. }) => {
                seen.ping_done = true;
            }
            reis::eis::Request::PointerAbsolute(
                _,
                reis::eis::pointer_absolute::Request::MotionAbsolute { x, y },
            ) => {
                seen.x = Some(*x);
                seen.y = Some(*y);
            }
            reis::eis::Request::Pointer(
                _,
                reis::eis::pointer::Request::MotionRelative { x, y },
            ) => {
                seen.relative = Some((*x, *y));
            }
            reis::eis::Request::Button(_, reis::eis::button::Request::Button { button, state }) => {
                seen.button = Some((*button, *state));
            }
            reis::eis::Request::Scroll(_, reis::eis::scroll::Request::Scroll { x, y }) => {
                seen.scroll = Some((*x, *y));
            }
            _ => {}
        }
    }
}
