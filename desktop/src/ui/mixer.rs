//! The mixer panel: what the person at the desk looks at.
//!
//! Left is the desk - where to listen, how deep the buffer is, what the master
//! and the feedback shifter are doing, and which addresses to give the phones.
//! Right is one strip per microphone, which is the part that has to be readable
//! from across a room: name, meter, buffer depth, loss.

use gpui::{div, prelude::*, px, rgb, Context, Entity, IntoElement};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::Input;
use gpui_component::slider::{Slider, SliderState};
use gpui_component::{h_flex, v_flex, Selectable, Sizable};

use lanmic::mixer::{SourceSnapshot, MAX_SOURCES};
use lanmic::receiver::{RxStats, MAX_JITTER_MS, MIN_JITTER_MS};

use crate::audio::Direction;

use super::theme::*;
use super::widgets::*;
use super::{ms, LanMic};

impl LanMic {
    pub(super) fn render_mixer(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let running = self.server.is_some();
        let stats = self
            .server
            .as_ref()
            .map(|s| s.stats())
            .unwrap_or_else(|| RxStats {
                running: false,
                limiter_gain: 1.0,
                ..Default::default()
            });

        h_flex()
            .gap_3()
            .flex_1()
            .min_h(px(0.))
            .items_start()
            .child(
                v_flex()
                    .gap_3()
                    .w(px(320.))
                    .flex_none()
                    .child(self.mixer_transport(running, cx))
                    .child(self.mixer_master(&stats, cx))
                    .child(self.mixer_output_device(running, cx))
                    .child(self.mixer_addresses()),
            )
            .child(self.mixer_strips(&stats, cx))
    }

    fn mixer_transport(&mut self, running: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let jitter = self.options.server.jitter_ms;

        panel()
            .child(heading("Mixer"))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        v_flex()
                            .gap_1()
                            .w(px(96.))
                            .flex_none()
                            .child(label("port"))
                            .child(Input::new(&self.server_port).small()),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .flex_1()
                            .child(label("name phones will see"))
                            .child(Input::new(&self.server_name).small()),
                    ),
            )
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .child(label(if running {
                        "jitter buffer (next start)"
                    } else {
                        "jitter buffer"
                    }))
                    .child(
                        h_flex()
                            .gap_1()
                            .items_center()
                            .child(
                                Button::new("jitter-down")
                                    .label("-")
                                    .outline()
                                    .xsmall()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.nudge_jitter(-5);
                                        cx.notify();
                                    })),
                            )
                            .child(readout_right(format!("{jitter} ms"), W_MS))
                            .child(
                                Button::new("jitter-up")
                                    .label("+")
                                    .outline()
                                    .xsmall()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.nudge_jitter(5);
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .child(label(format!(
                "{MIN_JITTER_MS}..{MAX_JITTER_MS} ms. Raise it if the under counter climbs."
            )))
            .child(
                Button::new("server-toggle")
                    .label(if running { "STOP" } else { "START MIXER" })
                    .map(|button| {
                        if running {
                            button.danger()
                        } else {
                            button.primary()
                        }
                    })
                    .w_full()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.toggle_server(cx);
                        cx.notify();
                    })),
            )
    }

    fn mixer_master(&mut self, stats: &RxStats, cx: &mut Context<Self>) -> impl IntoElement {
        let limiting = stats.limiter_gain < 0.999;
        let _ = cx;

        panel()
            .child(heading("Master"))
            .child(meter(stats.master_peak, px(10.)))
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .child(label("gain"))
                    .child(readout_right(format!("{:.2}x", self.master_gain), W_GAIN)),
            )
            .child(Slider::new(&self.master_fader))
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .child(label("feedback shift"))
                    .child(readout_right(
                        if self.feedback_hz <= 0.0 {
                            "off".to_string()
                        } else {
                            format!("{:.1} Hz", self.feedback_hz)
                        },
                        W_HZ,
                    )),
            )
            .child(Slider::new(&self.feedback_fader))
            .child(
                readings()
                    .child(stat(
                        "limiter",
                        format!("{:.2}x", stats.limiter_gain),
                        W_GAIN,
                    ))
                    .child(stat("pkts", stats.packets.to_string(), W_COUNT))
                    .child(stat("bad", stats.bad_packets.to_string(), W_COUNT))
                    .child(stat("xrun", stats.xruns.to_string(), W_COUNT))
                    .child(stat("out", format!("{:.1} ms", stats.latency_ms), W_MS)),
            )
            .when(limiting, |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(rgb(WARN))
                        .child("limiter working - pull the master down"),
                )
            })
    }

    fn mixer_output_device(&mut self, running: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let chosen = self.options.server.device.clone();
        let live_name = self
            .server
            .as_ref()
            .map(|s| format!("{} ({} ch)", s.device_name(), s.output_channels()));

        panel()
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .child(heading("Output"))
                    .child(
                        Button::new("rescan-out")
                            .label("rescan")
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.rescan_devices();
                                cx.notify();
                            })),
                    ),
            )
            .when_some(live_name, |this, name| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(rgb(LIVE))
                        .child(format!("playing out of {name}")),
                )
            })
            .child(
                v_flex()
                    .id("output-devices")
                    .gap_1()
                    .max_h(px(150.))
                    .overflow_y_scroll()
                    .children(device_rows(
                        &self.output_devices,
                        chosen.as_deref(),
                        Direction::Output,
                        running,
                        cx,
                    )),
            )
    }

    fn mixer_addresses(&self) -> impl IntoElement {
        let addresses = &self.addresses;
        panel()
            .child(heading("Point the phones at"))
            .child(if addresses.is_empty() {
                div()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child("no non-loopback address - is Wi-Fi up?")
            } else {
                div()
                    .text_xs()
                    .text_color(rgb(TEXT))
                    .child(addresses.join("   "))
            })
            .child(label(if self.options.server.discovery {
                "Find server on a phone will land here."
            } else {
                "Discovery is off; the address has to be typed in."
            }))
    }

    fn mixer_strips(&mut self, stats: &RxStats, cx: &mut Context<Self>) -> impl IntoElement {
        let sources = self.sources.clone();
        let running = self.server.is_some();
        let faders: Vec<Option<Entity<SliderState>>> = sources
            .iter()
            .map(|source| self.fader_for(source.ssrc).cloned())
            .collect();

        v_flex()
            .gap_2()
            .flex_1()
            .min_w(px(0.))
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .child(heading(format!(
                        "Microphones {} / {MAX_SOURCES}",
                        stats.active_sources
                    )))
                    .child(label("gain, mute and the buffer each one is running")),
            )
            .child(
                v_flex()
                    .id("strips")
                    .gap_2()
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .when(sources.is_empty(), |this| {
                        this.child(panel().flex_1().justify_center().items_center().child(
                            div().text_xs().text_color(rgb(MUTED)).child(if running {
                                "listening - nothing has said hello yet"
                            } else {
                                "start the mixer, then GO LIVE on a phone"
                            }),
                        ))
                    })
                    .children(
                        sources
                            .iter()
                            .zip(faders)
                            .map(|(source, fader)| strip(source, fader, cx)),
                    ),
            )
    }
}

/// The last reading in a strip's row, tagged so a test can measure where it
/// lands. `debug_selector` compiles to nothing outside tests.
pub(super) const STRIP_TAIL: &str = "strip-tail";

/// One channel strip.
fn strip(
    source: &SourceSnapshot,
    fader: Option<Entity<SliderState>>,
    cx: &mut Context<LanMic>,
) -> impl IntoElement {
    let ssrc = source.ssrc;
    let gain = source.gain_milli as f32 / 1000.0;
    let level = source.peak_milli as f32 / 1000.0;
    // A strip whose packets stopped a moment ago is still on the desk for two
    // seconds; saying so is better than a meter that has simply gone quiet.
    let stale = source.age_ms > 250;

    panel()
        .child(
            h_flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .w(px(84.))
                        .flex_none()
                        .text_xs()
                        .text_color(rgb(if stale { MUTED } else { TEXT }))
                        .child(format!("MIC-{:04X}", ssrc & 0xFFFF)),
                )
                .child(
                    div()
                        .flex_1()
                        .child(meter(if source.muted { 0.0 } else { level }, px(12.))),
                )
                .child(
                    Button::new(("strip-mute", ssrc as u64))
                        .label(if source.muted { "MUTED" } else { "mute" })
                        .outline()
                        .xsmall()
                        .selected(source.muted)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.toggle_source_mute(ssrc);
                            cx.notify();
                        })),
                ),
        )
        .child(
            h_flex()
                .items_center()
                .gap_3()
                .child(readout(format!("{gain:.2}x"), W_GAIN))
                .child(div().flex_1().children(fader.as_ref().map(Slider::new)))
                .child(
                    readings()
                        .child(stat(
                            "buf",
                            format!("{:.0} ms", ms(source.buffer_frames)),
                            W_MS,
                        ))
                        .child(stat("lost", source.lost.to_string(), W_COUNT))
                        .child(stat("under", source.underruns.to_string(), W_COUNT))
                        // The last reading in the row: if anything before it
                        // changes width, this is what moves. The tests measure
                        // it for exactly that reason.
                        .child(
                            stat("pkts", source.packets.to_string(), W_COUNT)
                                .debug_selector(|| STRIP_TAIL.into()),
                        ),
                ),
        )
}

/// The device menu, drawn inline: a list beats a popup for something chosen
/// once at the start of a gig.
pub(super) fn device_rows(
    devices: &[crate::audio::DeviceInfo],
    chosen: Option<&str>,
    direction: Direction,
    locked: bool,
    cx: &mut Context<LanMic>,
) -> Vec<impl IntoElement> {
    let prefix = match direction {
        Direction::Output => "out-device",
        Direction::Input => "in-device",
    };
    devices
        .iter()
        .enumerate()
        .map(|(index, device)| {
            // Nothing chosen yet means the host default, so that is the row
            // that should look selected.
            let selected = match chosen {
                Some(name) => name == device.name,
                None => device.is_default,
            };
            let name = device.name.clone();
            h_flex()
                .id((prefix, index))
                .items_center()
                .justify_between()
                .gap_2()
                .px_2()
                .py_1()
                .rounded_md()
                .text_xs()
                .bg(rgb(if selected { PANEL_ALT } else { PANEL }))
                .border_1()
                .border_color(rgb(if selected { ACCENT } else { BORDER }))
                .text_color(rgb(if device.usable { TEXT } else { MUTED }))
                .when(!locked && device.usable, |this| {
                    this.cursor_pointer()
                        .hover(|this| this.border_color(rgb(ACCENT)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.choose_device(direction, name.clone());
                            cx.notify();
                        }))
                })
                .when(locked, |this| this.opacity(0.55))
                .child(div().truncate().child(device.name.clone()))
                .children((!device.usable).then(|| div().text_color(rgb(WARN)).child("no 48 kHz")))
        })
        .collect()
}
