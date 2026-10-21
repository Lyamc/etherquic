//! Link, carrier, and codec comparison opened from the Carrier (?) control.
//!
//! Numbers here are the ones in `performance.md`: frame budgets, Quinn and
//! capture constants, the codec catalog, and the measured NDIS / Npcap copy.

use gpui::{div, prelude::*, px, rgb, AnyView, App, ClickEvent, Context, SharedString, Window};

const CARD: u32 = 0x1b1f29;
const CARD_EDGE: u32 = 0x2c3340;
const TEXT: u32 = 0xe7ecf3;
const MUTED: u32 = 0x9aa3b2;
const PLAYER_BLUE: u32 = 0x4cc2ff;

pub const HOVER: &str = "60 fps is 16.7 ms. Auto encode is 15 fps software and 30 fps hardware. Click for the link, carrier, and codec comparison.";

struct Tip {
    text: SharedString,
}

impl Render for Tip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .max_w(px(360.))
            .rounded_sm()
            .border_1()
            .border_color(rgb(CARD_EDGE))
            .bg(rgb(CARD))
            .text_xs()
            .text_color(rgb(TEXT))
            .whitespace_normal()
            .child(self.text.clone())
    }
}

/// `?` beside Carrier. Hover is the one-line budget. Click opens [`panel`].
pub fn button(
    open: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id("carrier-latency-help")
        .flex()
        .flex_none()
        .w(px(22.))
        .h(px(22.))
        .items_center()
        .justify_center()
        .rounded_sm()
        .border_1()
        .border_color(rgb(if open { PLAYER_BLUE } else { CARD_EDGE }))
        .bg(rgb(if open { 0x243044 } else { CARD }))
        .text_xs()
        .font_weight(gpui::FontWeight::BOLD)
        .text_color(rgb(TEXT))
        .cursor_pointer()
        .tooltip(|_: &mut Window, cx: &mut App| -> AnyView {
            cx.new(|_| Tip {
                text: HOVER.into(),
            })
            .into()
        })
        .child("?")
        .on_click(on_click)
}

struct Row {
    name: &'static str,
    rank: &'static str,
    cost: &'static str,
    best: &'static str,
}

const LINKS: &[Row] = &[
    Row {
        name: "Membrane NDIS",
        rank: "1",
        cost: "Measured 4.8 MiB/s on a 2 MB copy. About 10 ms for a 50 kB picture.",
        best: "Windows LAN, device already open",
    },
    Row {
        name: "Linux poll",
        rank: "2",
        cost: "pcap fd and the send wake share one poll. No 1 ms sleep on that path.",
        best: "Linux with a raw socket",
    },
    Row {
        name: "UDP",
        rank: "3",
        cost: "No raw NIC. QUIC uses the service port. RTP uses the discovered input port.",
        best: "Phone, or no Ethernet open",
    },
    Row {
        name: "macOS BPF",
        rank: "4",
        cost: "BPF receive, AF_NDRV send. The host daemon opens the device.",
        best: "Mac on the LAN",
    },
    Row {
        name: "Npcap fallback",
        rank: "5",
        cost: "Measured 0.09 MiB/s on that same 2 MB copy. About 0.5 s for 50 kB.",
        best: "Only while Membrane is not open",
    },
];

const CARRIERS: &[Row] = &[
    Row {
        name: "QUIC datagrams",
        rank: "1",
        cost: "1 ms ACK floor, 256 KiB window, on L2 or UDP under 2 ms. A hole does not stall the next picture.",
        best: "LAN picture and audio",
    },
    Row {
        name: "UDP/RTP",
        rank: "2",
        cost: "No Quinn socket. Picture hole is one frame, at least 2 ms. Default profile is gcm.",
        best: "LAN without QUIC",
    },
    Row {
        name: "QUIC streams",
        rank: "3",
        cost: "Quinn's 25 ms max ACK delay. CUBIC starts at 14,720 bytes. One loss stalls that stream.",
        best: "A long path, or an old peer",
    },
    Row {
        name: "AVB",
        rank: "4",
        cost: "Shaped at 200 Mbit/s. Class B credit is 250 µs. A failed reservation ends the session.",
        best: "A reserved LAN, picture inside 200 Mbit/s",
    },
    Row {
        name: "ST 2110",
        rank: "5",
        cost: "Each row is paced across the frame. Uncompressed 1080p60 is about 2.5 Gbit/s.",
        best: "10 Gbit/s studio, JPEG XS or uncompressed",
    },
];

const CODECS: &[Row] = &[
    Row {
        name: "PyroWave",
        rank: "1",
        cost: "Catalog ~1 frame. GPU compute both ends. Full picture when pixels change.",
        best: "LAN with the bandwidth",
    },
    Row {
        name: "MJPEG",
        rank: "2",
        cost: "Catalog ~1 frame. Intra. Dirty tiles when that switch is on.",
        best: "A CPU that finishes inside the frame",
    },
    Row {
        name: "JPEG XS",
        rank: "3",
        cost: "Catalog ~1 frame. Intra. The ST 2110 picture that fits a smaller link.",
        best: "Studio, or a 10G stand-in",
    },
    Row {
        name: "OpenAPV",
        rank: "4",
        cost: "Catalog ~1–2 frames. Intra through ffmpeg.",
        best: "When XS is absent and the frame budget allows it",
    },
    Row {
        name: "MJ2K",
        rank: "5",
        cost: "Catalog ~1–2 frames. Intra, smaller than MJPEG. Dirty tiles when on.",
        best: "Same as MJPEG, fewer bits",
    },
    Row {
        name: "H.264",
        rank: "6",
        cost: "Catalog ~2 frames. B-frames stay off. Hardware encode is the path that holds 60.",
        best: "A tighter link, hardware on the host",
    },
    Row {
        name: "H.265",
        rank: "7",
        cost: "Catalog ~2–3 frames. Encode ASIC required.",
        best: "Bandwidth first, 30 or 60",
    },
    Row {
        name: "AV1",
        rank: "8",
        cost: "Catalog ~3–4 frames. Encode ASIC required.",
        best: "The smallest stream, not 120 or 144",
    },
];

/// The three comparisons. Rank 1 is the path that can feed a 60 fps picture.
pub fn panel() -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .w_full()
        .min_w_0()
        .p_2()
        .rounded_sm()
        .border_1()
        .border_color(rgb(CARD_EDGE))
        .bg(rgb(0x141820))
        .child(note(
            "60 fps is 16.7 ms. 120 is 8.3 ms. 144 is 6.9 ms. Auto encode is 15 fps software and 30 fps hardware. Match follows the display. Capture and present can each wait one refresh.",
        ))
        .child(table("Link", LINKS))
        .child(table("Carrier", CARRIERS))
        .child(table("Codec", CODECS))
        .child(note(
            "Opus is a 10 ms frame and does not gate the picture. Npcap and QUIC streams are what miss 60 on a large picture. Frame generation draws a synthesised picture between arrivals.",
        ))
}

fn note(text: &'static str) -> impl IntoElement {
    div()
        .w_full()
        .min_w_0()
        .text_xs()
        .text_color(rgb(MUTED))
        .whitespace_normal()
        .child(text)
}

fn table(title: &'static str, rows: &'static [Row]) -> impl IntoElement {
    let mut body = div().flex().flex_col().gap_1().w_full().min_w_0();
    for row in rows {
        body = body.child(
            div()
                .flex()
                .flex_col()
                .w_full()
                .min_w_0()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_2()
                        .items_baseline()
                        .child(
                            div()
                                .flex_none()
                                .text_xs()
                                .text_color(rgb(PLAYER_BLUE))
                                .child(row.rank),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .font_weight(gpui::FontWeight::BOLD)
                                .text_color(rgb(TEXT))
                                .child(row.name),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(TEXT))
                        .whitespace_normal()
                        .child(row.cost),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(MUTED))
                        .whitespace_normal()
                        .child(row.best),
                ),
        );
    }
    div()
        .flex()
        .flex_col()
        .gap_1()
        .w_full()
        .min_w_0()
        .child(
            div()
                .text_xs()
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(rgb(MUTED))
                .child(title),
        )
        .child(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_comparison_names_every_carrier_and_the_frame_budget() {
        assert!(HOVER.contains("16.7 ms"));
        assert!(HOVER.contains("15 fps"));
        let names: Vec<&str> = CARRIERS.iter().map(|row| row.name).collect();
        assert!(names.contains(&"QUIC datagrams"));
        assert!(names.contains(&"UDP/RTP"));
        assert!(names.contains(&"AVB"));
        assert!(names.contains(&"ST 2110"));
        assert_eq!(LINKS[0].name, "Membrane NDIS");
        assert_eq!(LINKS.last().unwrap().name, "Npcap fallback");
        assert_eq!(CODECS[0].name, "PyroWave");
        assert_eq!(CODECS.last().unwrap().name, "AV1");
    }
}
