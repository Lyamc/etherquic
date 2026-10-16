//! Grey operating-system marks. The version number sits on the bottom right.

use etherquic::discover::OsFace;
use gpui::{div, prelude::*, px, rgb, AnyElement, FontWeight};

const GREY: u32 = 0x9aa3b2;
const GREY_SOFT: u32 = 0xc5cad3;
const PLATE: u32 = 0x12141a;

/// A 34px mark. `subscript` is drawn over the bottom-right corner.
pub fn os_badge(face: OsFace, subscript: &str) -> AnyElement {
    let mark = subscript.trim();
    div()
        .relative()
        .flex_none()
        .w(px(34.))
        .h(px(34.))
        .child(
            div()
                .absolute()
                .top(px(0.))
                .left(px(0.))
                .w(px(24.))
                .h(px(24.))
                .child(face_icon(face)),
        )
        .when(!mark.is_empty(), |badge| {
            badge.child(
                div()
                    .absolute()
                    .bottom(px(0.))
                    .right(px(0.))
                    .px(px(2.))
                    .rounded_sm()
                    .bg(rgb(PLATE))
                    .text_xs()
                    .font_weight(FontWeight::BOLD)
                    .text_color(rgb(GREY_SOFT))
                    .child(mark.to_string()),
            )
        })
        .into_any_element()
}

fn face_icon(face: OsFace) -> AnyElement {
    match face {
        OsFace::WindowsXp => flag_panes(),
        OsFace::WindowsVista => orb(false),
        OsFace::Windows7 => orb(true),
        OsFace::Windows8 => tiles(0., 0.),
        OsFace::Windows81 => tiles(2., 0.),
        OsFace::Windows10 => perspective_flag(),
        OsFace::Windows11 => tiles(2., 3.),
        OsFace::Windows => tiles(1., 1.),
        OsFace::Mac => apple(),
        OsFace::Android => android(),
        OsFace::Other => other(),
    }
}

fn tiles(gap: f32, radius: f32) -> AnyElement {
    let cell = |radius: f32| div().flex_1().h_full().bg(rgb(GREY)).rounded(px(radius));
    div()
        .flex()
        .flex_col()
        .w_full()
        .h_full()
        .gap(px(gap))
        .child(
            div()
                .flex()
                .flex_row()
                .flex_1()
                .gap(px(gap))
                .child(cell(radius))
                .child(cell(radius)),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .flex_1()
                .gap(px(gap))
                .child(cell(radius))
                .child(cell(radius)),
        )
        .into_any_element()
}

/// Four panes, the right column inset, so it reads as the Windows 10 flag.
fn perspective_flag() -> AnyElement {
    let cell = || div().flex_1().w_full().bg(rgb(GREY));
    div()
        .flex()
        .flex_row()
        .w_full()
        .h_full()
        .gap(px(1.))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .h_full()
                .gap(px(1.))
                .child(cell())
                .child(cell()),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .w(px(9.))
                .h_full()
                .pt(px(3.))
                .gap(px(1.))
                .child(cell())
                .child(cell()),
        )
        .into_any_element()
}

/// Uneven panes, the XP flag in grey.
fn flag_panes() -> AnyElement {
    let pane = |w: f32, h: f32| div().w(px(w)).h(px(h)).bg(rgb(GREY));
    div()
        .flex()
        .flex_col()
        .w_full()
        .h_full()
        .justify_center()
        .child(
            div()
                .flex()
                .flex_row()
                .items_end()
                .gap(px(1.))
                .child(pane(14., 11.))
                .child(pane(8., 9.)),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .items_start()
                .gap(px(1.))
                .mt(px(1.))
                .child(pane(8., 8.))
                .child(pane(14., 10.)),
        )
        .into_any_element()
}

fn orb(shine: bool) -> AnyElement {
    div()
        .relative()
        .w_full()
        .h_full()
        .child(
            div()
                .absolute()
                .top(px(1.))
                .left(px(1.))
                .w(px(22.))
                .h(px(22.))
                .rounded(px(11.))
                .bg(rgb(GREY)),
        )
        .when(shine, |orb| {
            orb.child(
                div()
                    .absolute()
                    .top(px(4.))
                    .left(px(5.))
                    .w(px(8.))
                    .h(px(8.))
                    .rounded(px(4.))
                    .bg(rgb(GREY_SOFT)),
            )
        })
        .into_any_element()
}

fn apple() -> AnyElement {
    div()
        .relative()
        .w_full()
        .h_full()
        .child(
            div()
                .absolute()
                .top(px(6.))
                .left(px(3.))
                .w(px(16.))
                .h(px(16.))
                .rounded(px(8.))
                .bg(rgb(GREY)),
        )
        .child(
            div()
                .absolute()
                .top(px(2.))
                .left(px(12.))
                .w(px(6.))
                .h(px(5.))
                .rounded(px(3.))
                .bg(rgb(GREY)),
        )
        .child(
            div()
                .absolute()
                .top(px(7.))
                .left(px(14.))
                .w(px(8.))
                .h(px(8.))
                .rounded(px(4.))
                .bg(rgb(PLATE)),
        )
        .into_any_element()
}

fn android() -> AnyElement {
    div()
        .relative()
        .w_full()
        .h_full()
        .child(
            div()
                .absolute()
                .top(px(1.))
                .left(px(6.))
                .w(px(2.))
                .h(px(5.))
                .bg(rgb(GREY)),
        )
        .child(
            div()
                .absolute()
                .top(px(0.))
                .left(px(5.))
                .w(px(4.))
                .h(px(4.))
                .rounded(px(2.))
                .bg(rgb(GREY)),
        )
        .child(
            div()
                .absolute()
                .top(px(1.))
                .left(px(16.))
                .w(px(2.))
                .h(px(5.))
                .bg(rgb(GREY)),
        )
        .child(
            div()
                .absolute()
                .top(px(0.))
                .left(px(15.))
                .w(px(4.))
                .h(px(4.))
                .rounded(px(2.))
                .bg(rgb(GREY)),
        )
        .child(
            div()
                .absolute()
                .top(px(7.))
                .left(px(2.))
                .w(px(20.))
                .h(px(14.))
                .rounded(px(4.))
                .bg(rgb(GREY)),
        )
        .child(
            div()
                .absolute()
                .top(px(11.))
                .left(px(6.))
                .w(px(3.))
                .h(px(3.))
                .bg(rgb(PLATE)),
        )
        .child(
            div()
                .absolute()
                .top(px(11.))
                .left(px(15.))
                .w(px(3.))
                .h(px(3.))
                .bg(rgb(PLATE)),
        )
        .into_any_element()
}

fn other() -> AnyElement {
    div()
        .relative()
        .w_full()
        .h_full()
        .rounded_sm()
        .border_2()
        .border_color(rgb(GREY))
        .child(
            div()
                .absolute()
                .bottom(px(3.))
                .left(px(4.))
                .right(px(4.))
                .h(px(3.))
                .bg(rgb(GREY)),
        )
        .into_any_element()
}
