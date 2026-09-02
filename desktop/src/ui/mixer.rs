//! The mixer panel: what the person at the desk looks at.
//!
//! Left is the desk - where to listen, how deep the buffer is, what the master
//! and the feedback shifter are doing, and which addresses to give the phones.
//! Right is one strip per microphone, which is the part that has to be readable
//! from across a room: name, meter, buffer depth, loss.

use gpui::{div, prelude::*, px, rgb, Context, IntoElement};

use lanmic::mixer::{SourceSnapshot, MAX_FEEDBACK_SHIFT_HZ, MAX_SOURCES};
use lanmic::receiver::{RxStats, MAX_JITTER_MS, MIN_JITTER_MS};

use crate::audio::Direction;

use super::theme::*;
use super::widgets::*;
use super::{ms, Field, LanMic, SliderId, MAX_GAIN};

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

        div()
            .flex()
            .flex_row()
            .gap_3()
            .flex_1()
            .min_h(px(0.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .w(px(320.))
                    .child(self.mixer_transport(running, cx))
                    .child(self.mixer_master(&stats, cx))
                    .child(self.mixer_output_device(running, cx))
                    .child(self.mixer_addresses()),
            )
            .child(self.mixer_strips(&stats, cx))
    }

    fn mixer_transport(&mut self, running: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let jitter = self.options.server.jitter_ms;
        let port_focused = self.focused == Some(Field::ServerPort);
        let name_focused = self.focused == Some(Field::ServerName);

        panel()
            .child(heading("Mixer"))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .w(px(96.))
                            .child(label("port"))
                            .child(
                                field("server-port", self.field(Field::ServerPort), port_focused)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.focused = Some(Field::ServerPort);
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .flex_1()
                            .child(label("name phones will see"))
                            .child(
                                field("server-name", self.field(Field::ServerName), name_focused)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.focused = Some(Field::ServerName);
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .child(label(if running {
                        "jitter buffer (next start)"
                    } else {
                        "jitter buffer"
                    }))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_1()
                            .items_center()
                            .child(button("jitter-down", "-", ButtonKind::Secondary).on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.nudge_jitter(-5);
                                    cx.notify();
                                }),
                            ))
                            .child(
                                div()
                                    .w(px(56.))
                                    .text_xs()
                                    .text_color(rgb(TEXT))
                                    .child(format!("{jitter} ms")),
                            )
                            .child(button("jitter-up", "+", ButtonKind::Secondary).on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.nudge_jitter(5);
                                    cx.notify();
                                }),
                            )),
                    ),
            )
            .child(label(format!(
                "{MIN_JITTER_MS}..{MAX_JITTER_MS} ms. Raise it if the under counter climbs."
            )))
            .child(
                button(
                    "server-toggle",
                    if running { "STOP" } else { "START MIXER" },
                    if running {
                        ButtonKind::Danger
                    } else {
                        ButtonKind::Primary
                    },
                )
                .w_full()
                .py_2()
                .on_click(cx.listener(|this, _, _, cx| {
                    this.toggle_server();
                    cx.notify();
                })),
            )
    }

    fn mixer_master(&mut self, stats: &RxStats, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity().downgrade();
        let limiting = stats.limiter_gain < 0.999;

        panel()
            .child(heading("Master"))
            .child(meter(stats.master_peak, px(10.)))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_between()
                    .child(label("gain"))
                    .child(label(format!("{:.2}x", self.master_gain))),
            )
            .child(slider(
                "master-gain",
                self.master_gain / MAX_GAIN,
                entity.clone(),
                |this: &mut LanMic, phase, at| this.slider_event(SliderId::MasterGain, phase, at),
            ))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_between()
                    .child(label("feedback shift"))
                    .child(label(if self.feedback_hz <= 0.0 {
                        "off".to_string()
                    } else {
                        format!("{:.1} Hz", self.feedback_hz)
                    })),
            )
            .child(slider(
                "feedback-shift",
                self.feedback_hz / MAX_FEEDBACK_SHIFT_HZ,
                entity,
                |this: &mut LanMic, phase, at| {
                    this.slider_event(SliderId::FeedbackShift, phase, at)
                },
            ))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap_3()
                    .child(stat("limiter", format!("{:.2}x", stats.limiter_gain)))
                    .child(stat("pkts", stats.packets.to_string()))
                    .child(stat("bad", stats.bad_packets.to_string()))
                    .child(stat("xrun", stats.xruns.to_string()))
                    .child(stat("out", format!("{:.1} ms", stats.latency_ms))),
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
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .child(heading("Output"))
                    .child(
                        button("rescan-out", "rescan", ButtonKind::Secondary).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.rescan_devices();
                                cx.notify();
                            }),
                        ),
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
                div()
                    .id("output-devices")
                    .flex()
                    .flex_col()
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
        let entity = cx.entity().downgrade();
        let sources = self.sources.clone();
        let running = self.server.is_some();

        div()
            .flex()
            .flex_col()
            .gap_2()
            .flex_1()
            .min_w(px(0.))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .child(heading(format!(
                        "Microphones {} / {MAX_SOURCES}",
                        stats.active_sources
                    )))
                    .child(label("gain, mute and the buffer each one is running")),
            )
            .child(
                div()
                    .id("strips")
                    .flex()
                    .flex_col()
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
                            .map(|source| strip(source, entity.clone(), cx)),
                    ),
            )
    }
}

/// One channel strip.
fn strip(
    source: &SourceSnapshot,
    entity: gpui::WeakEntity<LanMic>,
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
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .w(px(84.))
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
                    button(
                        ("strip-mute", ssrc as u64),
                        if source.muted { "MUTED" } else { "mute" },
                        ButtonKind::Toggle(source.muted),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_source_mute(ssrc);
                        cx.notify();
                    })),
                ),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_3()
                .child(div().w(px(84.)).child(label(format!("{gain:.2}x"))))
                .child(div().flex_1().child(slider(
                    ("strip-gain", ssrc as u64),
                    gain / MAX_GAIN,
                    entity,
                    move |this: &mut LanMic, phase, at| {
                        this.slider_event(SliderId::Source(ssrc), phase, at)
                    },
                )))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_3()
                        .child(stat("buf", format!("{:.0} ms", ms(source.buffer_frames))))
                        .child(stat("lost", source.lost.to_string()))
                        .child(stat("under", source.underruns.to_string()))
                        .child(stat("pkts", source.packets.to_string())),
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
            div()
                .id((prefix, index))
                .flex()
                .flex_row()
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
