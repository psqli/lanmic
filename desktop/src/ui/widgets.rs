//! The handful of controls this window needs, none of which GPUI ships.
//!
//! GPUI gives elements, layout and events; a slider and an editable field are
//! the application's business. Both are small enough to be worth having
//! exactly, rather than pulling in a component library for two of them.

use std::rc::Rc;

use gpui::{
    canvas, div, prelude::*, px, relative, rgb, Div, ElementId, Keystroke, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, SharedString, Stateful, WeakEntity,
};

use super::theme::*;

/// A card: the boxes the window is made of.
pub fn panel() -> Div {
    div()
        .flex()
        .flex_col()
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

/// `name  value`, the shape every counter on the screen is shown in.
pub fn stat(name: impl Into<SharedString>, value: impl Into<SharedString>) -> Div {
    div()
        .flex()
        .flex_row()
        .gap_1()
        .items_baseline()
        .child(label(name))
        .child(div().text_xs().text_color(rgb(TEXT)).child(value.into()))
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    /// The one thing this panel is for: GO LIVE, START.
    Primary,
    Secondary,
    Danger,
    /// A two-state control - mute, a mode tab - drawn lit or unlit.
    Toggle(bool),
}

pub fn button(
    id: impl Into<ElementId>,
    text: impl Into<SharedString>,
    kind: ButtonKind,
) -> Stateful<Div> {
    let (bg, fg, border) = match kind {
        ButtonKind::Primary => (ACCENT, 0xffffff, ACCENT),
        ButtonKind::Secondary => (PANEL_ALT, TEXT, BORDER_STRONG),
        ButtonKind::Danger => (DANGER, 0x1a0d0d, DANGER),
        ButtonKind::Toggle(true) => (ACCENT, 0xffffff, ACCENT),
        ButtonKind::Toggle(false) => (PANEL_ALT, MUTED, BORDER),
    };
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .px_3()
        .py_1p5()
        .rounded_md()
        .text_xs()
        .bg(rgb(bg))
        .text_color(rgb(fg))
        .border_1()
        .border_color(rgb(border))
        .cursor_pointer()
        .hover(|this| this.border_color(rgb(ACCENT)))
        .child(text.into())
}

/// A button that is visibly not available - a start with no address to send to,
/// a device list while a stream is running. Drawn dimmed and given no click
/// handler by the caller.
pub fn disabled(id: impl Into<ElementId>, text: impl Into<SharedString>) -> Stateful<Div> {
    button(id, text, ButtonKind::Secondary)
        .opacity(0.45)
        .cursor_default()
}

/// Which end of a drag this is. The move handler is global once a drag starts -
/// that is what lets the pointer leave the track without dropping the drag -
/// so the view has to know which slider owns it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragPhase {
    Down,
    Move,
    Up,
}

/// A horizontal slider.
///
/// `fraction` is 0..=1 of the track. `on_event` is handed the phase and the
/// fraction the pointer is over; the view decides whether this slider is the
/// one being dragged and what the fraction means in its own units.
pub fn slider<V: 'static>(
    id: impl Into<ElementId>,
    fraction: f32,
    entity: WeakEntity<V>,
    on_event: impl Fn(&mut V, DragPhase, f32) + 'static,
) -> Stateful<Div> {
    let fraction = fraction.clamp(0.0, 1.0);
    let on_event = Rc::new(on_event);

    div()
        .id(id)
        .relative()
        .h(px(18.))
        .w_full()
        .flex()
        .items_center()
        .cursor_pointer()
        .child(
            // The track, drawn as three stacked pieces so the filled part and
            // the knob line up without any measuring.
            div().h(px(4.)).w_full().rounded_sm().bg(rgb(TRACK)).child(
                div()
                    .h_full()
                    .w(relative(fraction))
                    .rounded_sm()
                    .bg(rgb(ACCENT)),
            ),
        )
        .child(
            div()
                .absolute()
                .top(px(4.))
                .left(relative(fraction))
                .child(div().ml(px(-5.)).size(px(10.)).rounded_full().bg(rgb(TEXT))),
        )
        .child(
            // `canvas` is how an element learns its own bounds: the paint pass
            // hands them over, and the mouse handlers registered here can then
            // turn a window position into a position along this track.
            canvas(
                |_, _, _| (),
                move |bounds, _, window, _| {
                    let position_of = move |x: Pixels| {
                        let width = bounds.size.width.max(px(1.));
                        (f32::from(x - bounds.origin.x) / f32::from(width)).clamp(0.0, 1.0)
                    };
                    window.on_mouse_event({
                        let (entity, on_event) = (entity.clone(), on_event.clone());
                        move |ev: &MouseDownEvent, _, _, cx| {
                            if !bounds.contains(&ev.position) {
                                return;
                            }
                            let at = position_of(ev.position.x);
                            entity
                                .update(cx, |view, cx| {
                                    on_event(view, DragPhase::Down, at);
                                    cx.notify();
                                })
                                .ok();
                        }
                    });
                    window.on_mouse_event({
                        let (entity, on_event) = (entity.clone(), on_event.clone());
                        move |ev: &MouseMoveEvent, _, _, cx| {
                            if !ev.dragging() {
                                return;
                            }
                            let at = position_of(ev.position.x);
                            entity
                                .update(cx, |view, cx| {
                                    on_event(view, DragPhase::Move, at);
                                    cx.notify();
                                })
                                .ok();
                        }
                    });
                    window.on_mouse_event({
                        let (entity, on_event) = (entity.clone(), on_event.clone());
                        move |ev: &MouseUpEvent, _, _, cx| {
                            let at = position_of(ev.position.x);
                            entity
                                .update(cx, |view, cx| {
                                    on_event(view, DragPhase::Up, at);
                                    cx.notify();
                                })
                                .ok();
                        }
                    });
                },
            )
            .absolute()
            .size_full(),
        )
}

/// One editable line.
///
/// GPUI has no text input - `examples/input.rs` builds a full IME-aware one in
/// seven hundred lines - and this window needs an address, a port and a name.
/// So this is the small half of that: printable characters, backspace, and a
/// caret. No selection, no clipboard, no IME. Anything more and the right
/// answer would be to lift the example's `TextInput` wholesale.
#[derive(Debug, Clone)]
pub struct TextField {
    pub value: String,
    pub placeholder: SharedString,
    /// Digits only, for the port fields.
    pub numeric: bool,
    pub max_len: usize,
}

/// What a keystroke did, so the caller can act on Enter without re-reading the
/// keystroke itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKey {
    Edited,
    Submitted,
    Ignored,
}

impl TextField {
    pub fn text(placeholder: impl Into<SharedString>, max_len: usize) -> Self {
        Self {
            value: String::new(),
            placeholder: placeholder.into(),
            numeric: false,
            max_len,
        }
    }

    pub fn number(placeholder: impl Into<SharedString>, max_len: usize) -> Self {
        Self {
            numeric: true,
            ..Self::text(placeholder, max_len)
        }
    }

    pub fn with(mut self, value: impl Into<String>) -> Self {
        self.value = value.into();
        self
    }

    pub fn key(&mut self, keystroke: &Keystroke) -> FieldKey {
        // Ctrl-something is a command, not text - except the one command this
        // field has, which is "throw it all away".
        let modifiers = &keystroke.modifiers;
        if modifiers.control || modifiers.platform {
            if keystroke.key == "u" || keystroke.key == "a" {
                self.value.clear();
                return FieldKey::Edited;
            }
            return FieldKey::Ignored;
        }

        match keystroke.key.as_str() {
            "backspace" | "delete" => {
                if self.value.pop().is_some() {
                    FieldKey::Edited
                } else {
                    FieldKey::Ignored
                }
            }
            "enter" => FieldKey::Submitted,
            "escape" | "tab" => FieldKey::Ignored,
            _ => {
                let Some(typed) = keystroke.key_char.as_deref() else {
                    return FieldKey::Ignored;
                };
                let mut edited = false;
                for c in typed.chars() {
                    if c.is_control() || self.value.chars().count() >= self.max_len {
                        continue;
                    }
                    if self.numeric && !c.is_ascii_digit() {
                        continue;
                    }
                    self.value.push(c);
                    edited = true;
                }
                if edited {
                    FieldKey::Edited
                } else {
                    FieldKey::Ignored
                }
            }
        }
    }
}

/// Renders a field. Focus tracking and the key handler belong to the view, so
/// this only draws; `focused` decides the border and whether a caret shows.
pub fn field(id: impl Into<ElementId>, state: &TextField, focused: bool) -> Stateful<Div> {
    let empty = state.value.is_empty();
    div()
        .id(id)
        .flex()
        .items_center()
        .h(px(28.))
        .px_2()
        .rounded_md()
        .bg(rgb(TRACK))
        .border_1()
        .border_color(rgb(if focused { ACCENT } else { BORDER }))
        .text_xs()
        .text_color(rgb(if empty { MUTED } else { TEXT }))
        .cursor_pointer()
        .child(if empty {
            state.placeholder.to_string()
        } else {
            state.value.clone()
        })
        .when(focused, |this| {
            this.child(div().text_color(rgb(ACCENT)).child("▌"))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::Modifiers;

    fn press(key: &str, ch: Option<&str>) -> Keystroke {
        Keystroke {
            modifiers: Modifiers::default(),
            key: key.to_string(),
            key_char: ch.map(str::to_string),
        }
    }

    #[test]
    fn typing_and_deleting_do_what_they_look_like() {
        let mut f = TextField::text("host", 32);
        assert_eq!(f.key(&press("1", Some("1"))), FieldKey::Edited);
        for c in ["9", "2", ".", "1"] {
            f.key(&press(c, Some(c)));
        }
        assert_eq!(f.value, "192.1");
        assert_eq!(f.key(&press("backspace", None)), FieldKey::Edited);
        assert_eq!(f.value, "192.");
    }

    #[test]
    fn backspace_on_an_empty_field_is_not_an_edit() {
        let mut f = TextField::text("host", 32);
        assert_eq!(f.key(&press("backspace", None)), FieldKey::Ignored);
        assert!(f.value.is_empty());
    }

    #[test]
    fn a_port_field_takes_digits_and_nothing_else() {
        let mut f = TextField::number("port", 5);
        for c in ["4", "5", "a", "-", "6", "7", "8"] {
            f.key(&press(c, Some(c)));
        }
        assert_eq!(f.value, "45678");
        // Full: the sixth digit is dropped rather than making an invalid port.
        f.key(&press("9", Some("9")));
        assert_eq!(f.value, "45678");
    }

    #[test]
    fn control_characters_never_reach_the_value() {
        let mut f = TextField::text("name", 32);
        assert_eq!(f.key(&press("enter", Some("\r"))), FieldKey::Submitted);
        assert_eq!(f.key(&press("tab", Some("\t"))), FieldKey::Ignored);
        assert_eq!(f.key(&press("escape", None)), FieldKey::Ignored);
        assert!(f.value.is_empty());
    }

    #[test]
    fn ctrl_u_clears_and_other_chords_are_left_alone() {
        let mut f = TextField::text("host", 32).with("192.168.1.50");
        let mut chord = press("x", Some("x"));
        chord.modifiers.control = true;
        assert_eq!(f.key(&chord), FieldKey::Ignored);
        assert_eq!(f.value, "192.168.1.50");

        let mut clear = press("u", None);
        clear.modifiers.control = true;
        assert_eq!(f.key(&clear), FieldKey::Edited);
        assert!(f.value.is_empty());
    }

    #[test]
    fn a_field_never_grows_past_its_limit() {
        let mut f = TextField::text("name", 4);
        for c in ["a", "b", "c", "d", "e", "f"] {
            f.key(&press(c, Some(c)));
        }
        assert_eq!(f.value, "abcd");
    }
}
