//! The window.
//!
//! One GPUI view holds the whole thing. That is a deliberate choice rather than
//! a shortcut: every control on screen either reads an atomic the audio threads
//! publish or writes one they read, so there is no state to split between
//! entities - and one view means one `notify` per refresh tick instead of a
//! graph of observers.
//!
//! ```text
//!   mod.rs        the view, its state, and everything that starts or stops
//!   mixer.rs      the mixer panel: channel strips, master, feedback
//!   mic.rs        the microphone panel: server, device, level
//!   frame.rs      the window frame, where the platform does not draw one
//!   widgets.rs    meter, slider, button, one-line text field
//!   theme.rs      colours
//! ```
//!
//! The refresh tick is the only thing that redraws: [`REFRESH`] wakes the view,
//! it re-reads the counters, and GPUI diffs the result. Nothing here blocks on
//! audio, and nothing on an audio thread knows this file exists.

mod frame;
mod mic;
mod mixer;
mod theme;
mod widgets;

use std::time::Duration;

use gpui::{
    div, prelude::*, px, rgb, size, App, Application, Bounds, Context, Decorations, FocusHandle,
    KeyDownEvent, SharedString, Task, TitlebarOptions, Window, WindowBounds, WindowDecorations,
    WindowOptions,
};

use lanmic::mixer::{SourceSnapshot, DEFAULT_FEEDBACK_SHIFT_HZ, MAX_FEEDBACK_SHIFT_HZ};
use lanmic::protocol::SAMPLE_RATE;
use lanmic::receiver::{MAX_JITTER_MS, MIN_JITTER_MS};
use lanmic::transmitter::MIN_FRAMES_PER_PACKET;
use lanmic::util::now_ms;

use crate::args::Options;
use crate::audio::{self, DeviceInfo, Direction};
use crate::discovery::{self, Found};
use crate::engine::{Microphone, Server};

use theme::*;
use widgets::*;

/// How often the window rereads what the audio threads publish: under the peak
/// meter's decay, over anything an eye resolves.
const REFRESH: Duration = Duration::from_millis(80);
/// Long enough for every mixer on a quiet LAN to answer, short enough that the
/// button does not feel broken.
const DISCOVERY_WAIT: Duration = Duration::from_millis(1200);
/// Gains run 0..=2 (-inf..+6 dB); the sliders map that range.
const MAX_GAIN: f32 = 2.0;
/// The packet sizes worth offering, in frames at 48 kHz: 2.5, 5, 10, 20 ms.
const PACKET_SIZES: [usize; 4] = [120, 240, 480, 960];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    Mixer,
    Microphone,
}

/// The editable fields. There are four, so they are an enum and an array
/// rather than four handles and a focus graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    ServerPort,
    ServerName,
    MicHost,
    MicPort,
}

impl Field {
    const ALL: [Field; 4] = [
        Field::ServerPort,
        Field::ServerName,
        Field::MicHost,
        Field::MicPort,
    ];

    fn index(self) -> usize {
        Field::ALL.iter().position(|&f| f == self).unwrap_or(0)
    }
}

/// Which slider a drag belongs to. A drag that starts on a slider keeps it
/// until the button comes up, wherever the pointer goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SliderId {
    MasterGain,
    FeedbackShift,
    MicGain,
    /// Per-source, keyed by ssrc rather than by strip position - strips move
    /// when a microphone leaves and a drag must not follow them.
    Source(u32),
}

pub struct LanMic {
    pub(super) options: Options,
    pub(super) panel: Panel,
    pub(super) server: Option<Server>,
    pub(super) mic: Option<Microphone>,
    /// The last thing that went wrong, shown until the next attempt.
    pub(super) error: Option<SharedString>,

    pub(super) fields: [TextField; 4],
    pub(super) focused: Option<Field>,

    pub(super) output_devices: Vec<DeviceInfo>,
    pub(super) input_devices: Vec<DeviceInfo>,
    /// Cached: `getifaddrs` on every one of twelve frames a second, to draw a
    /// line that changes when the Wi-Fi does, would be a syscall for nothing.
    pub(super) addresses: Vec<String>,

    pub(super) found: Vec<Found>,
    pub(super) searching: bool,

    /// Held here as well as in the engine so a stop and a start do not lose
    /// the desk settings - and so the sliders have somewhere to read from
    /// when nothing is running.
    pub(super) master_gain: f32,
    pub(super) feedback_hz: f32,
    pub(super) mic_gain: f32,
    pub(super) mic_muted: bool,
    pub(super) dragging: Option<SliderId>,

    /// Refilled from the source table on every tick.
    pub(super) sources: Vec<SourceSnapshot>,

    focus: FocusHandle,
    _refresh: Task<()>,
}

impl LanMic {
    fn new(options: Options, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window);

        let fields = [
            TextField::number("45678", 5).with(options.server.port.to_string()),
            TextField::text("this machine", 32).with(options.server.name.clone()),
            TextField::text("192.168.1.50", 64).with(options.mic.host.clone()),
            TextField::number("45678", 5).with(options.mic.port.to_string()),
        ];

        let refresh = cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(REFRESH).await;
            // The view is gone: so is the window, and so is this loop.
            if this
                .update(cx, |this, cx| {
                    this.poll();
                    cx.notify();
                })
                .is_err()
            {
                break;
            }
        });

        let mut this = Self {
            panel: if options.mic.host.is_empty() {
                Panel::Mixer
            } else {
                // Started with `--mic HOST`: open on the panel that address is
                // for, with it already filled in.
                Panel::Microphone
            },
            options,
            server: None,
            mic: None,
            error: None,
            fields,
            focused: None,
            output_devices: Vec::new(),
            input_devices: Vec::new(),
            addresses: Vec::new(),
            found: Vec::new(),
            searching: false,
            master_gain: 1.0,
            feedback_hz: DEFAULT_FEEDBACK_SHIFT_HZ,
            mic_gain: 1.0,
            mic_muted: false,
            dragging: None,
            sources: Vec::new(),
            focus,
            _refresh: refresh,
        };
        this.rescan_devices();
        this
    }

    // -- state ------------------------------------------------------------

    pub(super) fn field(&self, which: Field) -> &TextField {
        &self.fields[which.index()]
    }

    pub(super) fn field_mut(&mut self, which: Field) -> &mut TextField {
        &mut self.fields[which.index()]
    }

    /// Reads what the audio threads have published since the last tick. This
    /// is the only thing that writes `sources`, which is what keeps `render` a
    /// function of state rather than a second place state is decided.
    pub(super) fn poll(&mut self) {
        match &self.server {
            Some(server) => {
                server.table().snapshot(now_ms(), &mut self.sources);
                // One strip per source, in a stable order: without this the
                // strips would reshuffle whenever a slot was reused.
                self.sources.sort_by_key(|s| s.ssrc);
            }
            None => self.sources.clear(),
        }
    }

    pub(super) fn is_live(&self) -> bool {
        self.server.is_some() || self.mic.is_some()
    }

    /// Re-reads what this machine has: the device lists and its own addresses.
    /// Called at startup, on the rescan buttons, and whenever a session starts
    /// - the three moments any of it can have changed under us.
    pub(super) fn rescan_devices(&mut self) {
        self.output_devices = audio::devices(Direction::Output);
        self.input_devices = audio::devices(Direction::Input);
        self.addresses = discovery::local_addresses();
    }

    /// Applies the desk settings to a table that has just been built. A new
    /// session starts at unity by construction, which would silently undo a
    /// master fader someone left down.
    fn push_desk_settings(&self) {
        if let Some(server) = &self.server {
            server.table().set_master_gain(self.master_gain);
            server.table().set_feedback_shift_hz(self.feedback_hz);
        }
        if let Some(mic) = &self.mic {
            mic.shared().set_gain(self.mic_gain);
            mic.shared().set_muted(self.mic_muted);
        }
    }

    fn port_from(&self, which: Field) -> Result<u16, String> {
        let raw = self.field(which).value.trim();
        raw.parse::<i32>()
            .ok()
            .and_then(|p| lanmic::net::validate_port(p).ok())
            .ok_or_else(|| format!("{raw:?} is not a port"))
    }

    // -- starting and stopping --------------------------------------------

    pub(super) fn toggle_server(&mut self) {
        if self.server.take().is_some() {
            return;
        }
        // One at a time, as on the phone: a machine is a microphone or a
        // mixer, and being both would mix itself into its own speakers.
        self.mic = None;
        self.error = None;

        let mut config = self.options.server.clone();
        match self.port_from(Field::ServerPort) {
            Ok(port) => config.port = port,
            Err(message) => {
                self.error = Some(format!("port: {message}").into());
                return;
            }
        }
        let name = self.field(Field::ServerName).value.trim().to_string();
        if !name.is_empty() {
            config.name = name;
        }

        match Server::start(&config) {
            Ok(server) => {
                self.options.server = config;
                self.server = Some(server);
                self.push_desk_settings();
                // The addresses to read out are only interesting now, and an
                // interface may have come up since the window opened.
                self.addresses = discovery::local_addresses();
            }
            Err(e) => self.error = Some(e.to_string().into()),
        }
    }

    pub(super) fn toggle_mic(&mut self) {
        if self.mic.take().is_some() {
            return;
        }
        self.server = None;
        self.error = None;

        let mut config = self.options.mic.clone();
        config.host = self.field(Field::MicHost).value.trim().to_string();
        if config.host.is_empty() {
            self.error = Some("no server address: find one, or type it in".into());
            return;
        }
        match self.port_from(Field::MicPort) {
            Ok(port) => config.port = port,
            Err(message) => {
                self.error = Some(format!("port: {message}").into());
                return;
            }
        }

        match Microphone::start(&config) {
            Ok(mic) => {
                self.options.mic = config;
                self.mic = Some(mic);
                self.push_desk_settings();
            }
            Err(e) => self.error = Some(e.to_string().into()),
        }
    }

    pub(super) fn show(&mut self, panel: Panel) {
        if self.panel == panel {
            return;
        }
        // Leaving a panel stops what it started: an invisible live microphone
        // is how a room gets fed back into itself.
        match panel {
            Panel::Mixer => self.mic = None,
            Panel::Microphone => self.server = None,
        }
        self.panel = panel;
        self.focused = None;
        // Whatever went wrong was about the panel being left.
        self.error = None;
    }

    pub(super) fn find_servers(&mut self, cx: &mut Context<Self>) {
        if self.searching {
            return;
        }
        self.searching = true;
        self.error = None;
        self.found.clear();

        let port = self.options.server.discovery_port;
        cx.spawn(async move |this, cx| {
            // A broadcast and a second of listening: off the UI thread, where
            // a second of anything is a second of a frozen window.
            let result = cx
                .background_executor()
                .spawn(async move { discovery::probe(port, DISCOVERY_WAIT) })
                .await;
            this.update(cx, |this, cx| {
                this.searching = false;
                match result {
                    Ok(found) => {
                        if found.is_empty() {
                            this.error = Some("no mixer answered; type the address in".into());
                        }
                        // One answer is not a choice: take it.
                        if let [only] = found.as_slice() {
                            this.field_mut(Field::MicHost).value = only.host();
                        }
                        this.found = found;
                    }
                    Err(e) => this.error = Some(format!("discovery failed: {e}").into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // -- controls ---------------------------------------------------------

    pub(super) fn set_master_gain(&mut self, gain: f32) {
        self.master_gain = gain.clamp(0.0, MAX_GAIN);
        if let Some(server) = &self.server {
            server.table().set_master_gain(self.master_gain);
        }
    }

    pub(super) fn set_feedback_hz(&mut self, hz: f32) {
        self.feedback_hz = hz.clamp(0.0, MAX_FEEDBACK_SHIFT_HZ);
        if let Some(server) = &self.server {
            server.table().set_feedback_shift_hz(self.feedback_hz);
        }
    }

    pub(super) fn set_mic_gain(&mut self, gain: f32) {
        self.mic_gain = gain.clamp(0.0, MAX_GAIN);
        if let Some(mic) = &self.mic {
            mic.shared().set_gain(self.mic_gain);
        }
    }

    pub(super) fn toggle_mic_mute(&mut self) {
        self.mic_muted = !self.mic_muted;
        if let Some(mic) = &self.mic {
            mic.shared().set_muted(self.mic_muted);
        }
    }

    pub(super) fn set_source_gain(&mut self, ssrc: u32, gain: f32) {
        if let Some(server) = &self.server {
            server
                .table()
                .set_source_gain(ssrc, gain.clamp(0.0, MAX_GAIN));
        }
    }

    pub(super) fn toggle_source_mute(&mut self, ssrc: u32) {
        let muted = self.sources.iter().any(|s| s.ssrc == ssrc && s.muted);
        if let Some(server) = &self.server {
            server.table().set_source_muted(ssrc, !muted);
        }
    }

    /// Nudges the jitter target. It only takes effect on the next start - the
    /// buffers are sized when the session is built - which the panel says.
    pub(super) fn nudge_jitter(&mut self, delta: i32) {
        self.options.server.jitter_ms =
            (self.options.server.jitter_ms + delta).clamp(MIN_JITTER_MS, MAX_JITTER_MS);
    }

    /// Steps through the offered packet sizes. Also a next-start setting.
    pub(super) fn cycle_packet_size(&mut self) {
        let current = self.options.mic.frames_per_packet;
        let next = PACKET_SIZES
            .iter()
            .find(|&&size| size > current)
            .copied()
            .unwrap_or(PACKET_SIZES[0]);
        self.options.mic.frames_per_packet = next.max(MIN_FRAMES_PER_PACKET);
    }

    pub(super) fn choose_device(&mut self, direction: Direction, name: String) {
        match direction {
            Direction::Output => self.options.server.device = Some(name),
            Direction::Input => self.options.mic.device = Some(name),
        }
    }

    /// Routes a drag to whichever slider owns it. `Down` claims the slider,
    /// `Move` only moves the claimed one, `Up` releases it.
    pub(super) fn slider_event(&mut self, id: SliderId, phase: DragPhase, at: f32) {
        match phase {
            DragPhase::Down => {
                self.dragging = Some(id);
                self.apply_slider(id, at);
            }
            DragPhase::Move => {
                if self.dragging == Some(id) {
                    self.apply_slider(id, at);
                }
            }
            DragPhase::Up => {
                if self.dragging == Some(id) {
                    self.dragging = None;
                }
            }
        }
    }

    fn apply_slider(&mut self, id: SliderId, at: f32) {
        match id {
            SliderId::MasterGain => self.set_master_gain(at * MAX_GAIN),
            SliderId::FeedbackShift => self.set_feedback_hz(at * MAX_FEEDBACK_SHIFT_HZ),
            SliderId::MicGain => self.set_mic_gain(at * MAX_GAIN),
            SliderId::Source(ssrc) => self.set_source_gain(ssrc, at * MAX_GAIN),
        }
    }

    // -- keyboard ---------------------------------------------------------

    /// Takes no `Window` because it needs none - which is also what lets a test
    /// drive it directly.
    fn on_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let Some(which) = self.focused else {
            return;
        };
        if event.keystroke.key == "escape" {
            self.focused = None;
            cx.notify();
            return;
        }
        match self.field_mut(which).key(&event.keystroke) {
            FieldKey::Edited => {}
            // Enter on an address or a port is "go", which is what someone who
            // just typed one means by it.
            FieldKey::Submitted => {
                self.focused = None;
                match self.panel {
                    Panel::Mixer if self.server.is_none() => self.toggle_server(),
                    Panel::Microphone if self.mic.is_none() => self.toggle_mic(),
                    _ => {}
                }
            }
            FieldKey::Ignored => return,
        }
        cx.notify();
    }

    // -- chrome -----------------------------------------------------------

    fn tab(&self, panel: Panel, text: &'static str, cx: &mut Context<Self>) -> impl IntoElement {
        button(text, text, ButtonKind::Toggle(self.panel == panel)).on_click(cx.listener(
            move |this, _, _, cx| {
                this.show(panel);
                cx.notify();
            },
        ))
    }

    /// The top row, which is also the titlebar. The name and the strapline are
    /// one drag region that stretches to meet the tabs, so there is a generous
    /// area to move the window by that never overlaps a control.
    fn header(
        &mut self,
        decorations: Decorations,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let live = self.is_live();
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .child(frame::draggable(
                div()
                    .id("titlebar-drag")
                    .flex()
                    .flex_row()
                    .flex_1()
                    .items_baseline()
                    .gap_2()
                    .child(div().text_lg().text_color(rgb(TEXT)).child("LAN Mic"))
                    .child(label("48 kHz mono, no codec")),
                decorations,
            ))
            .child(self.tab(Panel::Mixer, "Mixer", cx))
            .child(self.tab(Panel::Microphone, "Microphone", cx))
            .child(
                div()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .text_xs()
                    .bg(rgb(if live { LIVE } else { PANEL_ALT }))
                    .text_color(rgb(if live { 0x0d1f14 } else { MUTED }))
                    .child(if live { "LIVE" } else { "idle" }),
            )
            .children(frame::controls(decorations, window))
    }

    fn error_bar(&self) -> Option<impl IntoElement> {
        self.error.clone().map(|message| {
            div()
                .px_3()
                .py_2()
                .rounded_md()
                .bg(rgb(0x2a1a1c))
                .border_1()
                .border_color(rgb(DANGER))
                .text_xs()
                .text_color(rgb(DANGER))
                .child(message)
        })
    }
}

impl Render for LanMic {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Everything drawn below comes from state the refresh tick collected in
        // `poll`, so one frame cannot disagree with itself about who is on the
        // desk - and so a test can put strips on the screen without a device.
        let decorations = window.window_decorations();
        let header = self.header(decorations, window, cx);
        let panel = match self.panel {
            Panel::Mixer => self.render_mixer(cx).into_any_element(),
            Panel::Microphone => self.render_mic(cx).into_any_element(),
        };

        // The shell is the window's own border and resize grip, and draws
        // nothing at all where the platform provides them.
        frame::shell(decorations).child(
            div()
                .id("root")
                .track_focus(&self.focus)
                .on_key_down(cx.listener(|this, event, _, cx| this.on_key(event, cx)))
                .size_full()
                .flex()
                .flex_col()
                .gap_3()
                .p_4()
                .bg(rgb(BG))
                .text_color(rgb(TEXT))
                .font_family("sans-serif")
                .text_sm()
                .child(header)
                .children(self.error_bar())
                .child(panel),
        )
    }
}

/// Frames to milliseconds, for every buffer depth on the screen.
pub(super) fn ms(frames: u32) -> f32 {
    frames as f32 * 1000.0 / SAMPLE_RATE as f32
}

pub fn run(options: Options) {
    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1040.), px(720.)), cx);
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let opened = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("LAN Mic".into()),
                    ..Default::default()
                }),
                window_min_size: Some(size(px(880.), px(560.))),
                app_id: Some("com.lanmic.audio".into()),
                // Linux only, and ignored elsewhere. Asking for client-side
                // decorations is what makes GNOME's Wayland session - which
                // does not implement the server-side protocol at all - report
                // `Decorations::Client` so `frame` knows to draw a titlebar
                // instead of leaving a window with no way to move or close it.
                // X11 without a compositor refuses and falls back to the
                // window manager's own, which is the right answer there.
                window_decorations: Some(WindowDecorations::Client),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| LanMic::new(options, window, cx)),
        );
        if let Err(e) = opened {
            // No display, no GPU, no Wayland or X11 socket: say which way out
            // there is rather than printing a backtrace at somebody.
            eprintln!("lanmic: could not open a window: {e}");
            eprintln!("        try `lanmic --headless` for the mixer on this terminal.");
            cx.quit();
            return;
        }
        cx.activate(true);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_field_has_its_own_slot() {
        let mut seen: Vec<usize> = Field::ALL.iter().map(|f| f.index()).collect();
        seen.sort_unstable();
        assert_eq!(seen, [0, 1, 2, 3]);
    }

    #[test]
    fn frames_become_the_milliseconds_the_ui_shows() {
        assert_eq!(ms(240), 5.0);
        assert_eq!(ms(720), 15.0);
        assert_eq!(ms(0), 0.0);
    }

    #[test]
    fn the_packet_sizes_offered_are_all_ones_the_engine_accepts() {
        for size in PACKET_SIZES {
            assert!(size >= MIN_FRAMES_PER_PACKET, "{size} is below the floor");
            assert!(
                size <= lanmic::protocol::MAX_FRAMES_PER_PACKET,
                "{size} is above the ceiling"
            );
        }
    }
}

/// Render tests.
///
/// GPUI ships a test platform: a real window, a real layout pass, a real paint
/// pass, and no display behind any of it. So the whole render path runs under
/// `cargo test`, on every panel and on a full desk of strips, on a machine
/// with no GPU - which is what catches the thing the type checker cannot,
/// namely anything in `render` that panics or fails to lay out.
///
/// What it does not do is look at pixels. A control drawn the wrong colour, or
/// off the bottom of the window, still needs eyes.
#[cfg(test)]
mod render_tests {
    use super::*;
    use gpui::{Entity, Keystroke, Modifiers, TestAppContext, VisualTestContext};
    use lanmic::mixer::MAX_SOURCES;

    fn open(cx: &mut TestAppContext) -> (Entity<LanMic>, &mut VisualTestContext) {
        let (view, cx) =
            cx.add_window_view(|window, cx| LanMic::new(Options::default(), window, cx));
        cx.run_until_parked();
        (view, cx)
    }

    fn typed(key: &str) -> Keystroke {
        Keystroke {
            modifiers: Modifiers::default(),
            key: key.to_string(),
            key_char: Some(key.to_string()),
        }
    }

    #[gpui::test]
    fn both_panels_lay_out(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);

        // The idle mixer is the first thing anybody sees: no strips, no
        // session, every control drawn but inert.
        view.update(cx, |this, _| assert_eq!(this.panel, Panel::Mixer));

        view.update(cx, |this, cx| {
            this.show(Panel::Microphone);
            cx.notify();
        });
        cx.run_until_parked();
        view.update(cx, |this, _| assert_eq!(this.panel, Panel::Microphone));
    }

    #[gpui::test]
    fn a_full_deskful_of_strips_lays_out(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);

        // Eight strips - a full table - each with its own meter, fader, mute
        // button, drag-tracking canvas and counters. This is the densest thing
        // the window ever draws and the only part of it that is built in a
        // loop, so it is where a layout bug would land.
        view.update(cx, |this, cx| {
            this.sources = (0..MAX_SOURCES)
                .map(|i| SourceSnapshot {
                    ssrc: 0x1000 + i as u32,
                    peak_milli: 250 * i as u32,
                    buffer_frames: 720,
                    packets: 100,
                    lost: i as u32,
                    underruns: 0,
                    age_ms: if i == 0 { 400 } else { 10 },
                    muted: i % 3 == 0,
                    gain_milli: 1000,
                })
                .collect();
            cx.notify();
        });
        cx.run_until_parked();
        view.update(cx, |this, _| assert_eq!(this.sources.len(), MAX_SOURCES));

        // And the tick takes them away again when there is no session behind
        // them, which is the other state the strip list has to survive.
        view.update(cx, |this, cx| {
            this.poll();
            cx.notify();
        });
        cx.run_until_parked();
        view.update(cx, |this, _| assert!(this.sources.is_empty()));
    }

    #[gpui::test]
    fn the_error_banner_lays_out(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        view.update(cx, |this, cx| {
            this.error = Some("no mixer answered; type the address in".into());
            cx.notify();
        });
        cx.run_until_parked();
        view.update(cx, |this, _| assert!(this.error.is_some()));
    }

    #[gpui::test]
    fn typing_reaches_the_focused_field(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);

        view.update(cx, |this, cx| {
            this.show(Panel::Microphone);
            this.focused = Some(Field::MicHost);
            this.field_mut(Field::MicHost).value.clear();
            cx.notify();
        });
        cx.run_until_parked();

        view.update(cx, |this, cx| {
            for key in ["1", "0", ".", "1"] {
                this.on_key(
                    &KeyDownEvent {
                        keystroke: typed(key),
                        is_held: false,
                    },
                    cx,
                );
            }
        });
        cx.run_until_parked();
        view.update(cx, |this, _| {
            assert_eq!(this.field(Field::MicHost).value, "10.1");
        });
    }

    #[gpui::test]
    fn escape_gives_up_the_field_and_enter_starts_the_session(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);

        view.update(cx, |this, cx| {
            this.show(Panel::Microphone);
            this.focused = Some(Field::MicHost);
            this.on_key(
                &KeyDownEvent {
                    keystroke: Keystroke {
                        modifiers: Modifiers::default(),
                        key: "escape".into(),
                        key_char: None,
                    },
                    is_held: false,
                },
                cx,
            );
        });
        view.update(cx, |this, _| assert_eq!(this.focused, None));

        // Enter on an empty address is a start attempt, and a start attempt
        // with no address is the error banner rather than a live session.
        view.update(cx, |this, cx| {
            this.focused = Some(Field::MicHost);
            this.field_mut(Field::MicHost).value.clear();
            this.on_key(
                &KeyDownEvent {
                    keystroke: Keystroke {
                        modifiers: Modifiers::default(),
                        key: "enter".into(),
                        key_char: None,
                    },
                    is_held: false,
                },
                cx,
            );
        });
        cx.run_until_parked();
        view.update(cx, |this, _| {
            assert_eq!(this.focused, None);
            assert!(this.mic.is_none());
            assert!(this.error.is_some(), "an empty address should say so");
        });
    }
}
