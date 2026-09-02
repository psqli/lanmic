//! The microphone panel: this machine as one more microphone on the desk.
//!
//! It asks the three questions the phone asks (which mixer, which input, how
//! loud) and then shows a level meter and the counters that say whether the
//! network is keeping up.

use gpui::{div, prelude::*, px, rgb, Context, IntoElement};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::Input;
use gpui_component::slider::Slider;
use gpui_component::{h_flex, v_flex, Disableable, Selectable, Sizable};

use lanmic::transmitter::TxStats;

use crate::audio::Direction;

use super::mixer::device_rows;
use super::theme::*;
use super::widgets::*;
use super::LanMic;

impl LanMic {
    pub(super) fn render_mic(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let running = self.mic.is_some();
        let stats = self.mic.as_ref().map(|m| m.stats()).unwrap_or_default();

        h_flex()
            .gap_3()
            .flex_1()
            .min_h(px(0.))
            .items_start()
            .child(
                v_flex()
                    .gap_3()
                    .w(px(360.))
                    .flex_none()
                    .child(self.mic_server(running, cx))
                    .child(self.mic_input_device(running, cx)),
            )
            .child(
                v_flex()
                    .gap_3()
                    .flex_1()
                    .min_w(px(0.))
                    .child(self.mic_level(&stats, cx))
                    .child(self.mic_counters(&stats)),
            )
    }

    fn mic_server(&mut self, running: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let searching = self.searching;
        let found = self.found.clone();
        let packet = self.options.mic.frames_per_packet;

        panel()
            .child(heading("Server"))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        v_flex()
                            .gap_1()
                            .flex_1()
                            .child(label("mixer address"))
                            .child(Input::new(&self.mic_host).small()),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .w(px(84.))
                            .flex_none()
                            .child(label("port"))
                            .child(Input::new(&self.mic_port).small()),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Button::new("find-server")
                            .label(if searching { "searching..." } else { "Find server" })
                            .outline()
                            .small()
                            .disabled(searching)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.find_servers(window, cx);
                                cx.notify();
                            })),
                    )
                    .child(label("broadcasts on the discovery port")),
            )
            .children(found.iter().enumerate().map(|(index, server)| {
                let host = server.host();
                h_flex()
                    .id(("found", index))
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
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.set_host(host.clone(), window, cx);
                        cx.notify();
                    }))
                    .child(div().truncate().child(server.name.clone()))
                    .child(div().text_color(rgb(MUTED)).child(server.host()))
            }))
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .child(label(if running {
                        "packet size (next start)"
                    } else {
                        "packet size"
                    }))
                    .child(
                        Button::new("packet-size")
                            .label(packet_label(packet))
                            .outline()
                            .xsmall()
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
                Button::new("mic-toggle")
                    .label(if running { "STOP" } else { "GO LIVE" })
                    .map(|button| {
                        if running {
                            button.danger()
                        } else {
                            button.primary()
                        }
                    })
                    .w_full()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.toggle_mic(cx);
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
                h_flex()
                    .items_center()
                    .justify_between()
                    .child(heading("Input"))
                    .child(
                        Button::new("rescan-in")
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
                        .child(format!("capturing from {name}")),
                )
            })
            .child(
                v_flex()
                    .id("input-devices")
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
        let muted = self.mic_muted;
        let target = self
            .mic
            .as_ref()
            .map(|m| format!("{}  ssrc {:08X}", m.target(), m.ssrc()));

        panel()
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .child(heading("Level"))
                    .child(
                        Button::new("mic-mute")
                            .label(if muted { "MUTED" } else { "mute" })
                            .outline()
                            .xsmall()
                            .selected(muted)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.toggle_mic_mute();
                                cx.notify();
                            })),
                    ),
            )
            .child(meter(if muted { 0.0 } else { stats.peak }, px(16.)))
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .child(label("gain"))
                    .child(readout_right(format!("{:.2}x", self.mic_gain), W_GAIN)),
            )
            .child(Slider::new(&self.mic_fader))
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
                readings()
                    .child(stat("sent", stats.packets_sent.to_string(), W_COUNT))
                    .child(stat("dropped", stats.frames_dropped.to_string(), W_COUNT))
                    .child(stat("errors", stats.send_errors.to_string(), W_COUNT))
                    .child(stat("xrun", stats.xruns.to_string(), W_COUNT))
                    .child(stat("in", format!("{:.1} ms", stats.latency_ms), W_MS)),
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
