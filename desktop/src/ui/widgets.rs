//! The few pieces this window needs that a component library does not have.
//!
//! Buttons, sliders, text inputs, the titlebar and the window border come from
//! [`gpui_component`]. What is left here is the part that is about audio
//! rather than about widgets: a peak meter coloured by how close it is to
//! clipping, and the readouts around it.
//!
//! # Why the numbers live in fixed-width boxes
//!
//! Every readout on this screen changes several times a second, and some of
//! them change width when they do: a buffer that goes from `9 ms` to `10 ms`,
//! a packet count that reaches ten thousand. In a flex row a wider child
//! pushes everything after it along, so a mixer left running would have its
//! mute buttons twitching sideways all evening.
//!
//! So a number is never laid out by its own width. It goes in a box wide
//! enough for the widest value it can reach, and grows inside that box. The
//! widths are the `W_*` constants below, and they are the only reason this
//! module still exists rather than being a handful of `div()` calls.

use gpui::{div, prelude::*, px, relative, rgb, Div, Pixels, SharedString};
use gpui_component::{h_flex, v_flex};

use super::theme::*;

/// A packet or frame counter: room for seven digits, which is an hour of
/// packets at 200 a second.
pub const W_COUNT: Pixels = px(56.0);
/// A duration in milliseconds, up to `200 ms`.
pub const W_MS: Pixels = px(48.0);
/// A gain or a limiter reading: `2.00x`.
pub const W_GAIN: Pixels = px(40.0);
/// A frequency: `10.0 Hz`, or the word `off`.
pub const W_HZ: Pixels = px(52.0);

/// A card: the boxes the window is made of.
pub fn panel() -> Div {
    v_flex()
        .gap_2()
        .p_3()
        .rounded_md()
        .bg(rgb(PANEL))
        .border_1()
        .border_color(rgb(BORDER))
}

/// A section heading: small, spaced, quiet.
pub fn heading(text: impl Into<SharedString>) -> Div {
    div()
        .text_xs()
        .text_color(rgb(MUTED))
        .child(text.into().to_uppercase())
}

pub fn label(text: impl Into<SharedString>) -> Div {
    div().text_xs().text_color(rgb(MUTED)).child(text.into())
}

/// A number in a box of its own, so that changing it moves nothing else.
/// `width` is the widest the value can get; see the module note.
pub fn readout(value: impl Into<SharedString>, width: Pixels) -> Div {
    div()
        .w(width)
        .flex_none()
        .text_xs()
        .text_color(rgb(TEXT))
        .child(value.into())
}

/// As [`readout`], against the right edge of its box - for a value that sits
/// at the end of a row, where digits should grow leftwards into the space
/// rather than push the row wider.
pub fn readout_right(value: impl Into<SharedString>, width: Pixels) -> Div {
    readout(value, width).text_right()
}

/// `name value`, the shape every counter on the screen is shown in. The name
/// is fixed by its text and the value by `width`, so the whole thing is a
/// fixed-width column whatever the reading.
pub fn stat(name: impl Into<SharedString>, value: impl Into<SharedString>, width: Pixels) -> Div {
    h_flex()
        .flex_none()
        .gap_1()
        .items_baseline()
        .child(label(name))
        .child(readout(value, width))
}

/// A peak meter. `level` is linear with 1.0 full scale, and the engine reports
/// above that - so the bar pins at full while the colour keeps saying "over".
pub fn meter(level: f32, height: Pixels) -> Div {
    let level = if level.is_finite() {
        level.max(0.0)
    } else {
        0.0
    };
    div()
        .h(height)
        .w_full()
        .rounded_sm()
        .bg(rgb(TRACK))
        .overflow_hidden()
        .child(
            div()
                .h_full()
                .w(relative(level.min(1.0)))
                .bg(level_color(level)),
        )
}

/// A row of readings that keeps its shape: every child is `flex_none`, so a
/// value that grows uses the slack at the end of the row rather than the
/// space its neighbours are standing in.
pub fn readings() -> Div {
    h_flex().flex_wrap().gap_3()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_reserved_width_fits_the_widest_value_it_will_hold() {
        // text_xs is 12px, and no digit in a proportional face is wider than
        // about 7.2px at that size. These are the widest strings each box has
        // to take without pushing on the one beside it.
        const DIGIT: f32 = 7.2;
        for (width, widest) in [
            (W_COUNT, "9999999"),
            (W_MS, "200 ms"),
            (W_GAIN, "2.00x"),
            (W_HZ, "10.0 Hz"),
        ] {
            let needed = widest.chars().count() as f32 * DIGIT;
            assert!(
                f32::from(width) >= needed,
                "{width:?} is too narrow for {widest:?}, which needs about {needed}px"
            );
        }
    }
}
