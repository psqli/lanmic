//! The microphone panel: this machine as one more microphone on the desk.
//!
//! It asks the three questions the phone asks (which mixer, which input, how
//! loud) and then shows a level meter and the counters that say whether the
//! network is keeping up.

use gpui::{div, prelude::*, px, rgb, Context, IntoElement};

use lanmic::transmitter::TxStats;

use crate::audio::Direction;

use super::mixer::device_rows;
use super::theme::*;
use super::widgets::*;
use super::{Field, LanMic, SliderId, MAX_GAIN};

impl LanMic {
    pub(super) fn render_mic(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let running = self.mic.is_some();
        let stats = self.mic.as_ref().map(|m| m.stats()).unwrap_or_default();

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
                    .w(px(360.))
                    .child(self.mic_server(running, cx))
                    .child(self.mic_input_device(running, cx)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .flex_1()
                    .min_w(px(0.))
                    .child(self.mic_level(&stats, cx))
                    .child(self.mic_counters(&stats))
                    .child(div().flex_1()),
            )
    }

    fn mic_server(&mut self, running: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let host_focused = self.focused == Some(Field::MicHost);
        let port_focused = self.focused == Some(Field::MicPort);
        let searching = self.searching;
        let found = self.found.clone();
        let packet = self.options.mic.frames_per_packet;

        panel()
            .child(heading("Server"))
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
                            .flex_1()
                            .child(label("mixer address"))
                            .child(
                                field("mic-host", self.field(Field::MicHost), host_focused)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.focused = Some(Field::MicHost);
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .w(px(84.))
                            .child(label("port"))
                            .child(
                                field("mic-port", self.field(Field::MicPort), port_focused)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.focused = Some(Field::MicPort);
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .items_center()
                    .child(if searching {
                        disabled("find-server", "searching...")
                    } else {
                        button("find-server", "Find server", ButtonKind::Secondary).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.find_servers(cx);
                                cx.notify();
                            }),
                        )
                    })
                    .child(label("broadcasts on the discovery port")),
            )
            .children(found.iter().enumerate().map(|(index, server)| {
                let host = server.host();
                div()
                    .id(("found", index))
                    .flex()
                    .flex_row()
                    .justify_between()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .text_xs()
                    .bg(rgb(PANEL_ALT))
                    .border_1()
                    .border_color(rgb(BORDER))
                    .cursor_pointer()
                    .hover(|this| this.border_color(rgb(ACCENT)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.field_mut(Field::MicHost).value = host.clone();
                        cx.notify();
                    }))
                    .child(div().truncate().child(server.name.clone()))
                    .child(div().text_color(rgb(MUTED)).child(server.host()))
            }))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .child(label(if running {
                        "packet size (next start)"
                    } else {
                        "packet size"
                    }))
                    .child(
                        button("packet-size", packet_label(packet), ButtonKind::Secondary)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.cycle_packet_size();
                                cx.notify();
                            })),
                    ),
            )
            .child(label(
                "2.5 ms shaves latency and triples the packet rate; 5 ms is the default for a reason.",
            ))
            .child(
                button(
                    "mic-toggle",
                    if running { "STOP" } else { "GO LIVE" },
                    if running {
                        ButtonKind::Danger
                    } else {
                        ButtonKind::Primary
                    },
                )
                .w_full()
                .py_2()
                .on_click(cx.listener(|this, _, _, cx| {
                    this.toggle_mic();
                    cx.notify();
                })),
            )
    }

    fn mic_input_device(&mut self, running: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let chosen = self.options.mic.device.clone();
        let live_name = self
            .mic
            .as_ref()
            .map(|m| format!("{} ({} ch)", m.device_name(), m.input_channels()));

        panel()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .child(heading("Input"))
                    .child(
                        button("rescan-in", "rescan", ButtonKind::Secondary).on_click(cx.listener(
                            |this, _, _, cx| {
                                this.rescan_devices();
                                cx.notify();
                            },
                        )),
                    ),
            )
            .when_some(live_name, |this, name| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(rgb(LIVE))
                        .child(format!("capturing from {name}")),
                )
            })
            .child(
                div()
                    .id("input-devices")
                    .flex()
                    .flex_col()
                    .gap_1()
                    .max_h(px(190.))
                    .overflow_y_scroll()
                    .children(device_rows(
                        &self.input_devices,
                        chosen.as_deref(),
                        Direction::Input,
                        running,
                        cx,
                    )),
            )
    }

    fn mic_level(&mut self, stats: &TxStats, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity().downgrade();
        let muted = self.mic_muted;
        let target = self
            .mic
            .as_ref()
            .map(|m| format!("{}  ssrc {:08X}", m.target(), m.ssrc()));

        panel()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .child(heading("Level"))
                    .child(
                        button(
                            "mic-mute",
                            if muted { "MUTED" } else { "mute" },
                            ButtonKind::Toggle(muted),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.toggle_mic_mute();
                            cx.notify();
                        })),
                    ),
            )
            .child(meter(if muted { 0.0 } else { stats.peak }, px(16.)))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_between()
                    .child(label("gain"))
                    .child(label(format!("{:.2}x", self.mic_gain))),
            )
            .child(slider(
                "mic-gain",
                self.mic_gain / MAX_GAIN,
                entity,
                |this: &mut LanMic, phase, at| this.slider_event(SliderId::MicGain, phase, at),
            ))
            .when(muted, |this| {
                this.child(
                    div().text_xs().text_color(rgb(WARN)).child(
                        "muted - silence is still being sent, so the strip stays on the desk",
                    ),
                )
            })
            .when_some(target, |this, target| {
                this.child(div().text_xs().text_color(rgb(MUTED)).child(target))
            })
    }

    fn mic_counters(&self, stats: &TxStats) -> impl IntoElement {
        let losing = stats.frames_dropped > 0 || stats.send_errors > 0;
        panel()
            .child(heading("Counters"))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap_3()
                    .child(stat("sent", stats.packets_sent.to_string()))
                    .child(stat("dropped", stats.frames_dropped.to_string()))
                    .child(stat("errors", stats.send_errors.to_string()))
                    .child(stat("xrun", stats.xruns.to_string()))
                    .child(stat("in", format!("{:.1} ms", stats.latency_ms))),
            )
            .when(losing, |this| {
                this.child(
                    div().text_xs().text_color(rgb(WARN)).child(
                        "frames are not reaching the wire - the network, not the microphone",
                    ),
                )
            })
    }
}

/// Frames to the label someone thinks in.
pub(super) fn packet_label(frames: usize) -> String {
    format!(
        "{:.1} ms",
        frames as f32 * 1000.0 / lanmic::protocol::SAMPLE_RATE as f32
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_sizes_are_labelled_in_milliseconds() {
        assert_eq!(packet_label(120), "2.5 ms");
        assert_eq!(packet_label(240), "5.0 ms");
        assert_eq!(packet_label(480), "10.0 ms");
        assert_eq!(packet_label(960), "20.0 ms");
    }
}
