//! The window frame, for the platforms that do not draw one.
//!
//! A window needs three things a program cannot do without help: something to
//! drag to move it, edges to drag to resize it, and a button to close it.
//! Where the platform provides them - macOS, Windows, X11 under a window
//! manager, a Wayland compositor that implements server-side decorations -
//! [`gpui::Decorations::Server`] is reported and this module draws nothing.
//! Under `Decorations::Client`, which is what GNOME's Wayland session gives
//! every application, they are the application's to draw, and this is them:
//!
//! * a titlebar that moves the window, maximises it on a double click, and
//!   raises the compositor's own menu on a right click,
//! * minimise, maximise/restore and close buttons at its right,
//! * a six-pixel grab zone around the whole window, with the cursor for
//!   whichever edge or corner the pointer is over.
//!
//! The window's content is inset by that zone rather than drawn under it, so a
//! fader that reaches the edge of the window cannot swallow a resize - and a
//! resize cannot swallow the fader.

use gpui::{
    canvas, div, point, prelude::*, px, rgb, Bounds, CursorStyle, Decorations, Div, HitboxBehavior,
    MouseButton, MouseDownEvent, Pixels, Point, ResizeEdge, Size, Stateful, Tiling, Window,
};

use super::theme::*;

/// How far in from an edge counts as a grab. Six pixels is the width of a
/// window border people have been aiming at for thirty years; wider starts
/// eating clicks meant for the content.
pub const RESIZE_MARGIN: Pixels = px(6.0);
/// Corner radius of the frame we draw ourselves.
const ROUNDING: Pixels = px(8.0);

/// Which edge or corner the pointer is over, if any.
///
/// A tiled edge is not one of them: a window snapped to the left half of the
/// screen shares its right edge with the compositor's own grip, and its left
/// edge with nothing at all. `tiling` says which sides are in that state, and
/// they are excluded here rather than at every call site.
pub fn resize_edge(
    pos: Point<Pixels>,
    margin: Pixels,
    size: Size<Pixels>,
    tiling: Tiling,
) -> Option<ResizeEdge> {
    let top = pos.y <= margin && !tiling.top;
    let bottom = pos.y >= size.height - margin && !tiling.bottom;
    let left = pos.x <= margin && !tiling.left;
    let right = pos.x >= size.width - margin && !tiling.right;

    // Corners first: at a corner both an edge and a corner match, and the
    // corner is the one that was aimed at.
    Some(match (top, bottom, left, right) {
        (true, _, true, _) => ResizeEdge::TopLeft,
        (true, _, _, true) => ResizeEdge::TopRight,
        (_, true, true, _) => ResizeEdge::BottomLeft,
        (_, true, _, true) => ResizeEdge::BottomRight,
        (true, ..) => ResizeEdge::Top,
        (_, true, ..) => ResizeEdge::Bottom,
        (_, _, true, _) => ResizeEdge::Left,
        (_, _, _, true) => ResizeEdge::Right,
        _ => return None,
    })
}

pub fn cursor_for(edge: ResizeEdge) -> CursorStyle {
    match edge {
        ResizeEdge::Top | ResizeEdge::Bottom => CursorStyle::ResizeUpDown,
        ResizeEdge::Left | ResizeEdge::Right => CursorStyle::ResizeLeftRight,
        ResizeEdge::TopLeft | ResizeEdge::BottomRight => CursorStyle::ResizeUpLeftDownRight,
        ResizeEdge::TopRight | ResizeEdge::BottomLeft => CursorStyle::ResizeUpRightDownLeft,
    }
}

/// The outermost element: the window's own border and its resize grip.
///
/// Under server-side decorations this is a plain box and the compositor does
/// the rest. Under client-side it grows a border, rounded corners on the sides
/// that are not tiled, and the grab zone.
pub fn shell(decorations: Decorations) -> Stateful<Div> {
    let base = div().id("window-shell").size_full().bg(rgb(BG));
    let Decorations::Client { tiling } = decorations else {
        return base;
    };

    base
        // The content sits inside this, which is what keeps the grab zone
        // clear of everything that wants a click of its own.
        .p(RESIZE_MARGIN)
        .border_color(rgb(BORDER_STRONG))
        // A tiled edge is flush against something; rounding or bordering it
        // would draw a seam down the middle of the screen.
        .when(!tiling.top, |this| this.border_t(px(1.0)))
        .when(!tiling.bottom, |this| this.border_b(px(1.0)))
        .when(!tiling.left, |this| this.border_l(px(1.0)))
        .when(!tiling.right, |this| this.border_r(px(1.0)))
        .when(!(tiling.top || tiling.left), |this| {
            this.rounded_tl(ROUNDING)
        })
        .when(!(tiling.top || tiling.right), |this| {
            this.rounded_tr(ROUNDING)
        })
        .when(!(tiling.bottom || tiling.left), |this| {
            this.rounded_bl(ROUNDING)
        })
        .when(!(tiling.bottom || tiling.right), |this| {
            this.rounded_br(ROUNDING)
        })
        // The cursor is decided at paint time from where the pointer is, so
        // the frame has to redraw as it moves. This is the same trade GPUI's
        // own `window_shadow` example makes: a repaint per mouse move, for a
        // cursor that says what the edge will do before it is dragged.
        .on_mouse_move(|_, window, _| window.refresh())
        .on_mouse_down(
            MouseButton::Left,
            move |event: &MouseDownEvent, window, _| {
                let size = window.window_bounds().get_bounds().size;
                if let Some(edge) = resize_edge(event.position, RESIZE_MARGIN, size, tiling) {
                    window.start_window_resize(edge);
                }
            },
        )
        .child(grip(tiling))
}

/// A transparent overlay whose only job is the cursor. It is the first child,
/// so every real control paints over it and keeps its own hover behaviour.
fn grip(tiling: Tiling) -> impl IntoElement {
    canvas(
        |_, window, _| {
            let size = window.window_bounds().get_bounds().size;
            window.insert_hitbox(
                Bounds::new(point(px(0.0), px(0.0)), size),
                HitboxBehavior::Normal,
            )
        },
        move |_, hitbox, window, _| {
            let size = window.window_bounds().get_bounds().size;
            if let Some(edge) = resize_edge(window.mouse_position(), RESIZE_MARGIN, size, tiling) {
                window.set_cursor_style(cursor_for(edge), &hitbox);
            }
        },
    )
    .absolute()
    .size_full()
}

/// Makes an element the thing you drag the window by: move on a drag,
/// maximise or restore on a double click, and the compositor's window menu on
/// a right click. A no-op under server-side decorations, where the real
/// titlebar already does all three.
pub fn draggable(element: Stateful<Div>, decorations: Decorations) -> Stateful<Div> {
    if matches!(decorations, Decorations::Server) {
        return element;
    }
    element
        .on_mouse_down(MouseButton::Left, |event: &MouseDownEvent, window, _| {
            // A double click is the shortcut for the maximise button, and
            // starting a move on it would fight the compositor.
            if event.click_count >= 2 {
                window.zoom_window();
            } else {
                window.start_window_move();
            }
        })
        .on_mouse_down(MouseButton::Right, |event: &MouseDownEvent, window, _| {
            window.show_window_menu(event.position);
        })
}

/// Minimise, maximise/restore and close, in the order every desktop but macOS
/// puts them - and macOS reports server-side decorations, so it never gets
/// here. Each is drawn only if the platform says it has it.
pub fn controls(decorations: Decorations, window: &Window) -> Vec<Stateful<Div>> {
    if matches!(decorations, Decorations::Server) {
        return Vec::new();
    }
    let available = window.window_controls();
    let maximized = window.is_maximized();
    let mut buttons = Vec::new();

    if available.minimize {
        buttons.push(
            control("window-minimize", "\u{2013}", false)
                .on_click(|_, window, _| window.minimize_window()),
        );
    }
    if available.maximize {
        buttons.push(
            // The same button either way: it is the window's size that
            // toggles, and `zoom_window` is what toggles it.
            control(
                "window-maximize",
                if maximized { "\u{2750}" } else { "\u{25a1}" },
                false,
            )
            .on_click(|_, window, _| window.zoom_window()),
        );
    }
    buttons.push(
        control("window-close", "\u{00d7}", true).on_click(|_, window, _| window.remove_window()),
    );
    buttons
}

/// One control button. `danger` is the close button, which goes red on hover
/// because it is the one that cannot be undone.
fn control(id: &'static str, glyph: &'static str, danger: bool) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .size(px(22.0))
        .rounded_md()
        .text_xs()
        .text_color(rgb(MUTED))
        .cursor_pointer()
        .hover(move |this| {
            if danger {
                this.bg(rgb(DANGER)).text_color(rgb(0x1a0d0d))
            } else {
                this.bg(rgb(PANEL_ALT)).text_color(rgb(TEXT))
            }
        })
        .child(glyph)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MARGIN: Pixels = px(6.0);

    fn window() -> Size<Pixels> {
        gpui::size(px(1000.0), px(800.0))
    }

    fn at(x: f32, y: f32) -> Point<Pixels> {
        point(px(x), px(y))
    }

    fn edge_at(x: f32, y: f32) -> Option<ResizeEdge> {
        resize_edge(at(x, y), MARGIN, window(), Tiling::default())
    }

    #[test]
    fn the_middle_of_the_window_is_not_an_edge() {
        assert_eq!(edge_at(500.0, 400.0), None);
        // Just inside the grab zone on every side.
        assert_eq!(edge_at(500.0, 7.0), None);
        assert_eq!(edge_at(500.0, 793.0), None);
        assert_eq!(edge_at(7.0, 400.0), None);
        assert_eq!(edge_at(993.0, 400.0), None);
    }

    #[test]
    fn every_edge_and_corner_is_reachable() {
        assert_eq!(edge_at(500.0, 0.0), Some(ResizeEdge::Top));
        assert_eq!(edge_at(500.0, 800.0), Some(ResizeEdge::Bottom));
        assert_eq!(edge_at(0.0, 400.0), Some(ResizeEdge::Left));
        assert_eq!(edge_at(1000.0, 400.0), Some(ResizeEdge::Right));
        assert_eq!(edge_at(0.0, 0.0), Some(ResizeEdge::TopLeft));
        assert_eq!(edge_at(1000.0, 0.0), Some(ResizeEdge::TopRight));
        assert_eq!(edge_at(0.0, 800.0), Some(ResizeEdge::BottomLeft));
        assert_eq!(edge_at(1000.0, 800.0), Some(ResizeEdge::BottomRight));
    }

    #[test]
    fn a_corner_beats_the_two_edges_that_meet_there() {
        // Inside both the top and the left zone: aiming at the corner.
        assert_eq!(edge_at(3.0, 3.0), Some(ResizeEdge::TopLeft));
        assert_eq!(edge_at(997.0, 797.0), Some(ResizeEdge::BottomRight));
    }

    #[test]
    fn a_tiled_edge_cannot_be_grabbed() {
        // Snapped to the left half of the screen: its own left, top and bottom
        // are against the screen, and only the right edge is draggable.
        let snapped = Tiling {
            top: true,
            bottom: true,
            left: true,
            right: false,
        };
        assert_eq!(resize_edge(at(0.0, 400.0), MARGIN, window(), snapped), None);
        assert_eq!(resize_edge(at(500.0, 0.0), MARGIN, window(), snapped), None);
        assert_eq!(
            resize_edge(at(500.0, 800.0), MARGIN, window(), snapped),
            None
        );
        assert_eq!(
            resize_edge(at(1000.0, 400.0), MARGIN, window(), snapped),
            Some(ResizeEdge::Right)
        );
        // And the corner where a tiled edge meets a free one degrades to the
        // free edge rather than disappearing.
        assert_eq!(
            resize_edge(at(1000.0, 0.0), MARGIN, window(), snapped),
            Some(ResizeEdge::Right)
        );
    }

    #[test]
    fn a_fully_tiled_window_has_nothing_to_grab() {
        for (x, y) in [(0.0, 0.0), (500.0, 0.0), (1000.0, 800.0), (0.0, 400.0)] {
            assert_eq!(
                resize_edge(at(x, y), MARGIN, window(), Tiling::tiled()),
                None,
                "({x}, {y}) should not be grabbable when fully tiled"
            );
        }
    }

    #[test]
    fn each_edge_gets_the_cursor_that_describes_it() {
        assert_eq!(cursor_for(ResizeEdge::Top), CursorStyle::ResizeUpDown);
        assert_eq!(cursor_for(ResizeEdge::Bottom), CursorStyle::ResizeUpDown);
        assert_eq!(cursor_for(ResizeEdge::Left), CursorStyle::ResizeLeftRight);
        assert_eq!(cursor_for(ResizeEdge::Right), CursorStyle::ResizeLeftRight);
        assert_eq!(
            cursor_for(ResizeEdge::TopLeft),
            CursorStyle::ResizeUpLeftDownRight
        );
        assert_eq!(
            cursor_for(ResizeEdge::BottomRight),
            CursorStyle::ResizeUpLeftDownRight
        );
        assert_eq!(
            cursor_for(ResizeEdge::TopRight),
            CursorStyle::ResizeUpRightDownLeft
        );
        assert_eq!(
            cursor_for(ResizeEdge::BottomLeft),
            CursorStyle::ResizeUpRightDownLeft
        );
    }

    #[test]
    fn a_narrow_window_still_resolves_its_edges() {
        // Narrower than two margins: the zones overlap and the left one wins,
        // which is at least deterministic rather than a panic or a gap.
        let tiny = gpui::size(px(8.0), px(8.0));
        assert_eq!(
            resize_edge(at(4.0, 4.0), MARGIN, tiny, Tiling::default()),
            Some(ResizeEdge::TopLeft)
        );
    }
}

/// Layout tests for the frame itself.
///
/// GPUI's test platform always reports `Decorations::Server`, so the
/// client-side branch cannot be reached through the window - which is exactly
/// why everything here takes `Decorations` as an argument rather than reading
/// it. A harness view renders the branch a GNOME session takes, through the
/// real render path, in CI, where no compositor will ever offer it.
#[cfg(test)]
mod layout_tests {
    use super::*;
    use gpui::{Context, Render, TestAppContext};

    /// A window whose whole content is the frame under test.
    struct Harness {
        decorations: Decorations,
    }

    impl Render for Harness {
        fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            shell(self.decorations).child(
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .child(draggable(
                                div().id("drag").flex_1().child("LAN Mic"),
                                self.decorations,
                            ))
                            .children(controls(self.decorations, window)),
                    )
                    .child(div().flex_1().child("content")),
            )
        }
    }

    fn draw(cx: &mut TestAppContext, decorations: Decorations) {
        let (_, cx) = cx.add_window_view(|_, _| Harness { decorations });
        cx.run_until_parked();
    }

    #[gpui::test]
    fn the_server_side_frame_lays_out(cx: &mut TestAppContext) {
        draw(cx, Decorations::Server);
    }

    #[gpui::test]
    fn the_client_side_frame_lays_out_with_its_border_grip_and_controls(cx: &mut TestAppContext) {
        draw(
            cx,
            Decorations::Client {
                tiling: Tiling::default(),
            },
        );
    }

    #[gpui::test]
    fn a_tiled_frame_lays_out_with_its_seams_squared_off(cx: &mut TestAppContext) {
        // Each combination takes a different set of the border and rounding
        // branches, and a window snapped to an edge of the screen is the
        // common case for all of them.
        for (top, bottom, left, right) in [
            (true, false, false, false),
            (false, true, false, false),
            (false, false, true, false),
            (false, false, false, true),
            (true, true, true, true),
        ] {
            draw(
                cx,
                Decorations::Client {
                    tiling: Tiling {
                        top,
                        bottom,
                        left,
                        right,
                    },
                },
            );
        }
    }

    #[gpui::test]
    fn a_server_side_titlebar_grows_no_controls_of_its_own(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        cx.update(|window, _| {
            // The platform draws them; drawing a second set would put two
            // close buttons on one window.
            assert!(controls(Decorations::Server, window).is_empty());
            // And where it does not, all three are there to be drawn.
            let client = Decorations::Client {
                tiling: Tiling::default(),
            };
            assert!(!controls(client, window).is_empty());
        });
    }
}
