//! Parsec-style console: host this PC, or sit down as the player.
//!
//! The taskbar button is applied from a deferred callback so the Win32 call
//! is not inside the render borrow. It runs only when the desired state changes.

use std::time::Duration;

use etherquic_ui::{
    format_mac, lanes, list_nics, HexMac, Link, NicChoice, Role, CLIENT_MAC, HOST_MAC,
};
use gpui::{
    div, prelude::*, px, rgb, size, App, Application, ClickEvent, Context, FocusHandle,
    KeyDownEvent, SharedString, TitlebarOptions, Window, WindowBounds, WindowOptions,
};

const BG: u32 = 0x12141a;
const CARD: u32 = 0x1b1f29;
const CARD_EDGE: u32 = 0x2c3340;
const TEXT: u32 = 0xe7ecf3;
const MUTED: u32 = 0x9aa3b2;
const HOST_GREEN: u32 = 0x3dd68c;
const PLAYER_BLUE: u32 = 0x4cc2ff;
const DANGER: u32 = 0xff6b6b;
const INK: u32 = 0x0e1116;

pub fn run() {
    Application::new().run(|cx: &mut App| {
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let bounds = WindowBounds::centered(size(px(920.), px(640.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(bounds),
                titlebar: Some(TitlebarOptions {
                    title: Some("etherquic".into()),
                    ..Default::default()
                }),
                app_id: Some("etherquic".into()),
                window_min_size: Some(size(px(760.), px(560.))),
                ..Default::default()
            },
            |_window, cx| cx.new(Desk::new),
        )
        .expect("open etherquic window");
        cx.activate(true);
    });
}

struct Desk {
    link: Link,
    nics: Vec<NicChoice>,
    nic_error: Option<String>,
    nic: Option<String>,
    peer: HexMac,
    focus: FocusHandle,
    message: Option<String>,
    /// `true` matches a freshly created normal window (`WS_EX_APPWINDOW`).
    taskbar_on: bool,
}

impl Desk {
    fn new(cx: &mut Context<Self>) -> Self {
        let (nics, nic_error) = match list_nics() {
            Ok(nics) => (nics, None),
            Err(err) => (Vec::new(), Some(err)),
        };
        let desk = Self {
            link: Link::idle(),
            nics,
            nic_error,
            nic: None,
            peer: HexMac::from_bytes(HOST_MAC),
            focus: cx.focus_handle(),
            message: None,
            taskbar_on: true,
        };
        desk.start_poll(cx);
        desk
    }

    fn start_poll(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| loop {
            gpui::Timer::after(Duration::from_millis(400)).await;
            if this
                .update(cx, |desk, cx| {
                    desk.link.reap();
                    cx.notify();
                })
                .is_err()
            {
                break;
            }
        })
        .detach();
    }

    fn sync_taskbar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let want = self.link.shows_taskbar();
        if want == self.taskbar_on {
            return;
        }
        let Some(hwnd) = crate::taskbar::hwnd_of(window) else {
            return;
        };
        self.taskbar_on = want;
        window.set_window_title(if want { "etherquic host" } else { "etherquic" });
        cx.defer(move |_cx| crate::taskbar::apply(hwnd, want));
    }

    fn on_host(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.link.reap();
        if self.link.role() == Role::Client {
            return;
        }
        if self.link.role() == Role::Host {
            self.link.stop();
            self.message = None;
            cx.notify();
            return;
        }
        match self.link.host(self.nic.clone()) {
            Ok(()) => self.message = None,
            Err(err) => self.message = Some(err),
        }
        cx.notify();
    }

    fn on_connect(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.link.reap();
        if self.link.role() == Role::Host {
            return;
        }
        if self.link.role() == Role::Client {
            self.link.stop();
            self.message = None;
            cx.notify();
            return;
        }
        let Some(peer) = self.peer.parse() else {
            self.message = Some("Peer address needs 12 hex digits.".into());
            cx.notify();
            return;
        };
        match self.link.connect(self.nic.clone(), peer) {
            Ok(()) => self.message = None,
            Err(err) => self.message = Some(err),
        }
        cx.notify();
    }

    fn on_peer_key(&mut self, ev: &KeyDownEvent) {
        let key = ev.keystroke.key.as_str();
        let mods = &ev.keystroke.modifiers;
        if key == "backspace" {
            if mods.control || mods.alt || mods.platform {
                self.peer.clear();
            } else {
                self.peer.backspace();
            }
            return;
        }
        if ev.is_held || mods.control || mods.alt || mods.platform {
            return;
        }
        if let Some(ch) = ev
            .keystroke
            .key_char
            .as_deref()
            .and_then(|text| text.chars().next())
            .or_else(|| (key.len() == 1).then(|| key.chars().next()).flatten())
        {
            self.peer.push(ch);
        }
    }

    fn status_line(&self) -> String {
        self.message.clone().unwrap_or_else(|| self.link.status())
    }

    fn status_color(&self) -> u32 {
        if self.link.shows_taskbar() {
            return HOST_GREEN;
        }
        if self.link.role() == Role::Client {
            return PLAYER_BLUE;
        }
        let text = self.status_line().to_ascii_lowercase();
        if text.contains("fail")
            || text.contains("timed")
            || text.contains("missing")
            || text.contains("needs")
            || text.contains("denied")
            || text.contains("error")
        {
            DANGER
        } else {
            MUTED
        }
    }

    fn host_card(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let hosting = self.link.role() == Role::Host;
        let enabled = self.link.role() != Role::Client;
        let label = if hosting { "Stop hosting" } else { "Host" };
        let edge = if hosting { HOST_GREEN } else { CARD_EDGE };
        div()
            .flex()
            .flex_col()
            .flex_1()
            .gap_2()
            .p_4()
            .bg(rgb(CARD))
            .border_1()
            .border_color(rgb(edge))
            .rounded_md()
            .child(
                div()
                    .text_color(rgb(HOST_GREEN))
                    .child("Host"),
            )
            .child(
                div()
                    .text_color(rgb(MUTED))
                    .whitespace_normal()
                    .child("Share this PC as a play session. A player connects to this machine. Input is sent first, then audio, then video."),
            )
            .child(
                div()
                    .text_color(rgb(TEXT))
                    .child(format!("This PC appears as {}", format_mac(HOST_MAC))),
            )
            .child(action_button("host-btn", label, enabled, HOST_GREEN, cx.listener(|desk, _, window, cx| {
                desk.on_host(window, cx);
            })))
    }

    fn connect_card(&mut self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let playing = self.link.role() == Role::Client;
        let enabled = self.link.role() != Role::Host;
        let label = if playing { "Disconnect" } else { "Connect" };
        let edge = if playing { PLAYER_BLUE } else { CARD_EDGE };
        let focused = self.focus.is_focused(window);
        let typed = self.peer.display();
        let field_color = if typed.is_empty() { MUTED } else { TEXT };
        let field_text = if typed.is_empty() {
            "click and type the host address".to_string()
        } else {
            typed
        };
        div()
            .flex()
            .flex_col()
            .flex_1()
            .gap_2()
            .p_4()
            .bg(rgb(CARD))
            .border_1()
            .border_color(rgb(edge))
            .rounded_md()
            .child(div().text_color(rgb(PLAYER_BLUE)).child("Connect"))
            .child(
                div()
                    .text_color(rgb(MUTED))
                    .whitespace_normal()
                    .child("Join a host as the player. This window stays off the taskbar."),
            )
            .child(
                div()
                    .text_color(rgb(TEXT))
                    .child(format!("This PC appears as {}", format_mac(CLIENT_MAC))),
            )
            .child(
                div()
                    .id("peer-mac")
                    .flex()
                    .h(px(36.))
                    .px_2()
                    .items_center()
                    .bg(rgb(BG))
                    .border_1()
                    .border_color(rgb(if focused { PLAYER_BLUE } else { CARD_EDGE }))
                    .rounded_sm()
                    .cursor_pointer()
                    .text_color(rgb(field_color))
                    .track_focus(&self.focus)
                    .child(field_text)
                    .on_click(cx.listener(|desk, _: &ClickEvent, window, cx| {
                        window.focus(&desk.focus);
                        cx.notify();
                    }))
                    .on_key_down(cx.listener(|desk, ev: &KeyDownEvent, _, cx| {
                        desk.on_peer_key(ev);
                        cx.notify();
                    })),
            )
            .child(action_button(
                "connect-btn",
                label,
                enabled,
                PLAYER_BLUE,
                cx.listener(|desk, _, window, cx| {
                    desk.on_connect(window, cx);
                }),
            ))
    }

    fn nic_row(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self.nic.clone();
        let mut row = div().flex().flex_row().flex_wrap().gap_2().w_full();
        let auto_on = selected.is_none();
        row = row.child(nic_chip(
            "nic-auto",
            "Auto",
            auto_on,
            cx.listener(|desk, _, _, cx| {
                desk.nic = None;
                cx.notify();
            }),
        ));
        for (index, nic) in self.nics.iter().enumerate() {
            let name = nic.name.clone();
            let on = selected.as_deref() == Some(nic.name.as_str());
            let label = nic_label(nic);
            row = row.child(nic_chip(
                SharedString::from(format!("nic-{index}")),
                label,
                on,
                cx.listener(move |desk, _, _, cx| {
                    desk.nic = Some(name.clone());
                    cx.notify();
                }),
            ));
        }
        row
    }
}

impl Render for Desk {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.link.reap();
        self.sync_taskbar(window, cx);
        let status = self.status_line();
        let status_color = self.status_color();
        let host = self.host_card(cx);
        let connect = self.connect_card(window, cx);
        let nics = self.nic_row(cx);
        let nic_error = self.nic_error.clone();

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .text_sm()
            .p_4()
            .gap_3()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(div().text_color(rgb(TEXT)).child("etherquic"))
                    .child(
                        div().text_color(rgb(MUTED)).whitespace_normal().child(
                            "Play this PC over the LAN. The taskbar icon means this machine is hosting. A player does not take a taskbar slot.",
                        ),
                    ),
            )
            .child(
                div()
                    .text_color(rgb(status_color))
                    .whitespace_normal()
                    .child(status),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_3()
                    .w_full()
                    .flex_1()
                    .child(host)
                    .child(connect),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(div().text_color(rgb(MUTED)).child("Adapter"))
                    .when_some(nic_error, |row, err| {
                        row.child(div().text_color(rgb(DANGER)).whitespace_normal().child(err))
                    })
                    .child(nics),
            )
            .child(lane_row())
    }
}

fn action_button(
    id: impl Into<gpui::ElementId>,
    label: &'static str,
    enabled: bool,
    accent: u32,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .flex_none()
        .h(px(34.))
        .px_4()
        .items_center()
        .justify_center()
        .bg(rgb(accent))
        .text_color(rgb(INK))
        .rounded_sm()
        .cursor_pointer()
        .when(!enabled, |this| this.opacity(0.4))
        .active(|this| this.opacity(0.85))
        .child(label)
        .on_click(on_click)
}

fn nic_chip(
    id: impl Into<gpui::ElementId>,
    label: impl Into<String>,
    on: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .flex_none()
        .h(px(28.))
        .px_2()
        .items_center()
        .rounded_sm()
        .border_1()
        .border_color(rgb(if on { PLAYER_BLUE } else { CARD_EDGE }))
        .bg(rgb(if on { 0x243044 } else { CARD }))
        .text_color(rgb(if on { TEXT } else { MUTED }))
        .cursor_pointer()
        .child(label.into())
        .on_click(on_click)
}

fn nic_label(nic: &NicChoice) -> String {
    let raw = if nic.description.trim().is_empty() {
        nic.name.as_str()
    } else {
        nic.description.as_str()
    };
    let trimmed = raw.trim();
    if trimmed.chars().count() > 36 {
        let short: String = trimmed.chars().take(34).collect();
        format!("{short}…")
    } else {
        trimmed.to_string()
    }
}

fn lane_row() -> impl IntoElement {
    let mut row = div().flex().flex_row().gap_2().w_full();
    for lane in lanes() {
        row = row.child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .gap_2()
                .p_4()
                .bg(rgb(CARD))
                .rounded_sm()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .justify_between()
                        .child(lane.name)
                        .child(
                            div()
                                .text_color(rgb(HOST_GREEN))
                                .child(format!("{}", lane.priority)),
                        ),
                )
                .child(
                    div()
                        .text_color(rgb(MUTED))
                        .child(format!("service {}", lane.service)),
                ),
        );
    }
    div()
        .flex()
        .flex_col()
        .gap_2()
        .flex_none()
        .child(row)
        .child(
            div().text_color(rgb(MUTED)).whitespace_normal().child(
                "On a shared connection Quinn sends a higher stream priority first: input, then audio, then video. This host listens on 4433. Audio is 4434 and input is 4435.",
            ),
        )
}
