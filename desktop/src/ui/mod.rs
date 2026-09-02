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
//!   widgets.rs    the peak meter and the fixed-width readouts around it
//!   theme.rs      colours, which are also the component library's colours
//! ```
//!
//! # What is drawn here and what is not
//!
//! Buttons, sliders, text inputs, the titlebar and the Linux window border are
//! [`gpui_component`]'s. That is most of the chrome, including the three things
//! a window cannot do without - moving, resizing and closing - which
//! [`Root`] and [`TitleBar`] between them provide. What is left in this crate
//! is the part that is about audio: the meter, the readouts, the panels, and
//! the wiring from a control to an atomic.
//!
//! The state that used to back a hand-rolled slider and text field is gone with
//! them. A slider is an [`Entity<SliderState>`] that emits [`SliderEvent`]; a
//! field is an [`Entity<InputState>`] that emits [`InputEvent`]. This view
//! subscribes and applies the result, and owns no drag state or key routing of
//! its own.
//!
//! The refresh tick is the only thing that redraws: [`REFRESH`] wakes the view,
//! [`LanMic::poll`] re-reads the counters, and GPUI diffs the result. Nothing
//! here blocks on audio, and nothing on an audio thread knows this file exists.

mod mic;
mod mixer;
mod theme;
mod widgets;

use std::collections::HashMap;
use std::time::Duration;

use gpui::{
    prelude::*, px, size, App, Application, Bounds, Context, Entity, SharedString, Subscription,
    Task, TitlebarOptions, Window, WindowBackgroundAppearance, WindowBounds, WindowDecorations,
    WindowOptions,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::slider::{SliderEvent, SliderState, SliderValue};
use gpui_component::theme::{Theme, ThemeMode};
use gpui_component::{h_flex, v_flex, Root, Selectable, Sizable, TitleBar};

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
/// Gains run 0..=2 (-inf..+6 dB); the sliders carry that range themselves.
const MAX_GAIN: f32 = 2.0;
/// Fine enough that a fader feels continuous, coarse enough that a drag does
/// not emit an event per pixel.
const GAIN_STEP: f32 = 0.01;
/// The packet sizes worth offering, in frames at 48 kHz: 2.5, 5, 10, 20 ms.
const PACKET_SIZES: [usize; 4] = [120, 240, 480, 960];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    Mixer,
    Microphone,
}

/// One microphone's fader. The subscription is held beside the state because
/// dropping the pair is how a strip that has left the desk stops being wired
/// to anything.
struct SourceFader {
    state: Entity<SliderState>,
    _subscription: Subscription,
}

/// A slider reports a single value or a range; every slider here is single, and
/// this is the one place that has to say so.
fn single(value: SliderValue) -> f32 {
    match value {
        SliderValue::Single(v) => v,
        SliderValue::Range(_, high) => high,
    }
}

fn gain_slider(gain: f32, cx: &mut App) -> Entity<SliderState> {
    cx.new(|_| {
        SliderState::new()
            .min(0.0)
            .max(MAX_GAIN)
            .step(GAIN_STEP)
            .default_value(gain)
    })
}

pub struct LanMic {
    pub(super) options: Options,
    pub(super) panel: Panel,
    pub(super) server: Option<Server>,
    pub(super) mic: Option<Microphone>,
    /// The last thing that went wrong, shown until the next attempt.
    pub(super) error: Option<SharedString>,

    pub(super) server_port: Entity<InputState>,
    pub(super) server_name: Entity<InputState>,
    pub(super) mic_host: Entity<InputState>,
    pub(super) mic_port: Entity<InputState>,

    pub(super) master_fader: Entity<SliderState>,
    pub(super) feedback_fader: Entity<SliderState>,
    pub(super) mic_fader: Entity<SliderState>,
    /// One per live microphone, made and dropped as strips come and go.
    source_faders: HashMap<u32, SourceFader>,

    pub(super) output_devices: Vec<DeviceInfo>,
    pub(super) input_devices: Vec<DeviceInfo>,
    /// Cached: `getifaddrs` on every one of twelve frames a second, to draw a
    /// line that changes when the Wi-Fi does, would be a syscall for nothing.
    pub(super) addresses: Vec<String>,

    pub(super) found: Vec<Found>,
    pub(super) searching: bool,

    /// Held here as well as in the engine so a stop and a start do not lose
    /// the desk settings.
    pub(super) master_gain: f32,
    pub(super) feedback_hz: f32,
    pub(super) mic_gain: f32,
    pub(super) mic_muted: bool,

    /// Refilled from the source table on every tick.
    pub(super) sources: Vec<SourceSnapshot>,

    _subscriptions: Vec<Subscription>,
    _refresh: Task<()>,
}

impl LanMic {
    fn new(options: Options, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let port = options.server.port.to_string();
        let name = options.server.name.clone();
        let host = options.mic.host.clone();
        let mic_port = options.mic.port.to_string();

        let server_port = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("45678")
                .default_value(port)
        });
        let server_name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("this machine")
                .default_value(name)
        });
        let mic_host = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("192.168.1.50")
                .default_value(host)
        });
        let mic_port_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("45678")
                .default_value(mic_port)
        });

        let master_fader = gain_slider(1.0, cx);
        let feedback_fader = cx.new(|_| {
            SliderState::new()
                .min(0.0)
                .max(MAX_FEEDBACK_SHIFT_HZ)
                .step(0.1)
                .default_value(DEFAULT_FEEDBACK_SHIFT_HZ)
        });
        let mic_fader = gain_slider(1.0, cx);

        // Enter in an address or a port is "go", which is what someone who has
        // just typed one means by it.
        let subscriptions = vec![
            cx.subscribe(&master_fader, |this, _, event: &SliderEvent, _| {
                let SliderEvent::Change(value) = event;
                this.set_master_gain(single(*value));
            }),
            cx.subscribe(&feedback_fader, |this, _, event: &SliderEvent, _| {
                let SliderEvent::Change(value) = event;
                this.set_feedback_hz(single(*value));
            }),
            cx.subscribe(&mic_fader, |this, _, event: &SliderEvent, _| {
                let SliderEvent::Change(value) = event;
                this.set_mic_gain(single(*value));
            }),
            cx.subscribe(&mic_host, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) && this.mic.is_none() {
                    this.toggle_mic(cx);
                }
            }),
            cx.subscribe(&mic_port_input, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) && this.mic.is_none() {
                    this.toggle_mic(cx);
                }
            }),
            cx.subscribe(&server_port, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) && this.server.is_none() {
                    this.toggle_server(cx);
                }
            }),
            cx.subscribe(&server_name, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) && this.server.is_none() {
                    this.toggle_server(cx);
                }
            }),
        ];

        let refresh = cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(REFRESH).await;
            // The view is gone: so is the window, and so is this loop.
            if this
                .update(cx, |this, cx| {
                    this.poll(cx);
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
            server_port,
            server_name,
            mic_host,
            mic_port: mic_port_input,
            master_fader,
            feedback_fader,
            mic_fader,
            source_faders: HashMap::new(),
            output_devices: Vec::new(),
            input_devices: Vec::new(),
            addresses: Vec::new(),
            found: Vec::new(),
            searching: false,
            master_gain: 1.0,
            feedback_hz: DEFAULT_FEEDBACK_SHIFT_HZ,
            mic_gain: 1.0,
            mic_muted: false,
            sources: Vec::new(),
            _subscriptions: subscriptions,
            _refresh: refresh,
        };
        this.rescan_devices();
        this
    }

    // -- state ------------------------------------------------------------

    /// The trimmed contents of a field.
    fn text(input: &Entity<InputState>, cx: &App) -> String {
        input.read(cx).value().trim().to_string()
    }

    pub(super) fn is_live(&self) -> bool {
        self.server.is_some() || self.mic.is_some()
    }

    pub(super) fn fader_for(&self, ssrc: u32) -> Option<&Entity<SliderState>> {
        self.source_faders.get(&ssrc).map(|fader| &fader.state)
    }

    /// Re-reads what this machine has: the device lists and its own addresses.
    /// Called at startup, on the rescan buttons, and whenever a session starts
    /// - the three moments any of it can have changed under us.
    pub(super) fn rescan_devices(&mut self) {
        self.output_devices = audio::devices(Direction::Output);
        self.input_devices = audio::devices(Direction::Input);
        self.addresses = discovery::local_addresses();
    }

    /// Reads what the audio threads have published since the last tick. This
    /// is the only thing that writes `sources`, which is what keeps `render` a
    /// function of state rather than a second place state is decided.
    pub(super) fn poll(&mut self, cx: &mut Context<Self>) {
        match &self.server {
            Some(server) => {
                server.table().snapshot(now_ms(), &mut self.sources);
                // One strip per source, in a stable order: without this the
                // strips would reshuffle whenever a slot was reused.
                self.sources.sort_by_key(|s| s.ssrc);
            }
            None => self.sources.clear(),
        }
        self.sync_source_faders(cx);
    }

    /// Gives every live microphone a fader and takes it back when it leaves.
    /// A fader outliving its strip would keep writing gain to a slot that has
    /// been handed to somebody else.
    fn sync_source_faders(&mut self, cx: &mut Context<Self>) {
        let live: Vec<(u32, f32)> = self
            .sources
            .iter()
            .map(|s| (s.ssrc, s.gain_milli as f32 / 1000.0))
            .collect();
        self.source_faders
            .retain(|ssrc, _| live.iter().any(|(live, _)| live == ssrc));

        for (ssrc, gain) in live {
            if self.source_faders.contains_key(&ssrc) {
                continue;
            }
            let state = gain_slider(gain, cx);
            let subscription = cx.subscribe(&state, move |this, _, event: &SliderEvent, _| {
                let SliderEvent::Change(value) = event;
                this.set_source_gain(ssrc, single(*value));
            });
            self.source_faders.insert(
                ssrc,
                SourceFader {
                    state,
                    _subscription: subscription,
                },
            );
        }
    }

    /// Applies the desk settings to a session that has just started. A new one
    /// begins at unity by construction, which would silently undo a master
    /// fader someone left down.
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

    fn port_from(input: &Entity<InputState>, cx: &App) -> Result<u16, String> {
        let raw = Self::text(input, cx);
        raw.parse::<i32>()
            .ok()
            .and_then(|p| lanmic::net::validate_port(p).ok())
            .ok_or_else(|| format!("{raw:?} is not a port"))
    }

    // -- starting and stopping --------------------------------------------

    pub(super) fn toggle_server(&mut self, cx: &mut Context<Self>) {
        if self.server.take().is_some() {
            return;
        }
        // One at a time, as on the phone: a machine is a microphone or a
        // mixer, and being both would mix itself into its own speakers.
        self.mic = None;
        self.error = None;

        let mut config = self.options.server.clone();
        match Self::port_from(&self.server_port, cx) {
            Ok(port) => config.port = port,
            Err(message) => {
                self.error = Some(format!("port: {message}").into());
                return;
            }
        }
        let name = Self::text(&self.server_name, cx);
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

    pub(super) fn toggle_mic(&mut self, cx: &mut Context<Self>) {
        if self.mic.take().is_some() {
            return;
        }
        self.server = None;
        self.error = None;

        let mut config = self.options.mic.clone();
        config.host = Self::text(&self.mic_host, cx);
        if config.host.is_empty() {
            self.error = Some("no server address: find one, or type it in".into());
            return;
        }
        match Self::port_from(&self.mic_port, cx) {
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
        // Whatever went wrong was about the panel being left.
        self.error = None;
    }

    pub(super) fn find_servers(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.searching {
            return;
        }
        self.searching = true;
        self.error = None;
        self.found.clear();

        let port = self.options.server.discovery_port;
        // `spawn_in` rather than `spawn`: setting the address field on the way
        // out goes through the input's own setter, which wants a window.
        cx.spawn_in(window, async move |this, cx| {
            // A broadcast and a second of listening: off the UI thread, where
            // a second of anything is a second of a frozen window.
            let result = cx
                .background_executor()
                .spawn(async move { discovery::probe(port, DISCOVERY_WAIT) })
                .await;
            this.update_in(cx, |this, window, cx| {
                this.searching = false;
                match result {
                    Ok(found) => {
                        if found.is_empty() {
                            this.error = Some("no mixer answered; type the address in".into());
                        }
                        // One answer is not a choice: take it.
                        if let [only] = found.as_slice() {
                            this.set_host(only.host(), window, cx);
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

    /// Puts an address into the field, as picking one off the discovery list
    /// does. Goes through the input's own setter so its cursor and undo
    /// history stay consistent.
    pub(super) fn set_host(&mut self, host: String, window: &mut Window, cx: &mut Context<Self>) {
        self.mic_host
            .update(cx, |state, cx| state.set_value(host, window, cx));
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

    // -- chrome -----------------------------------------------------------

    fn tab(&self, panel: Panel, text: &'static str, cx: &mut Context<Self>) -> impl IntoElement {
        Button::new(text)
            .label(text)
            .ghost()
            .small()
            .selected(self.panel == panel)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.show(panel);
                cx.notify();
            }))
    }

    /// The titlebar, which is also where the mode lives. `TitleBar` supplies
    /// what a window needs and this platform may not: drag to move, double
    /// click to maximise, right click for the window menu, and the
    /// minimise/maximise/close buttons - drawn only where the desktop is not
    /// drawing its own.
    fn title_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let live = self.is_live();
        TitleBar::new().child(
            h_flex()
                .w_full()
                .items_center()
                .gap_2()
                .child(
                    h_flex()
                        .flex_1()
                        .items_baseline()
                        .gap_2()
                        .child(div_text("LAN Mic"))
                        .child(label("48 kHz mono, no codec")),
                )
                .child(self.tab(Panel::Mixer, "Mixer", cx))
                .child(self.tab(Panel::Microphone, "Microphone", cx))
                .child(
                    gpui::div()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .text_xs()
                        .bg(gpui::rgb(if live { LIVE } else { PANEL_ALT }))
                        .text_color(gpui::rgb(if live { 0x0d1f14 } else { MUTED }))
                        .child(if live { "LIVE" } else { "idle" }),
                ),
        )
    }

    fn error_bar(&self) -> Option<impl IntoElement> {
        self.error.clone().map(|message| {
            gpui::div()
                .px_3()
                .py_2()
                .rounded_md()
                .bg(gpui::rgb(0x2a1a1c))
                .border_1()
                .border_color(gpui::rgb(DANGER))
                .text_xs()
                .text_color(gpui::rgb(DANGER))
                .child(message)
        })
    }
}

/// The app name, which is the one piece of text with a size of its own.
fn div_text(text: &'static str) -> impl IntoElement {
    gpui::div()
        .text_lg()
        .text_color(gpui::rgb(TEXT))
        .child(text)
}

impl Render for LanMic {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Everything drawn below comes from state the refresh tick collected in
        // `poll`, so one frame cannot disagree with itself about who is on the
        // desk - and so a test can put strips on the screen without a device.
        let title_bar = self.title_bar(cx);
        let panel = match self.panel {
            Panel::Mixer => self.render_mixer(cx).into_any_element(),
            Panel::Microphone => self.render_mic(cx).into_any_element(),
        };

        v_flex().size_full().text_sm().child(title_bar).child(
            v_flex()
                .flex_1()
                .min_h(px(0.))
                .gap_3()
                .p_4()
                .children(self.error_bar())
                .child(panel),
        )
    }
}

/// Frames to milliseconds, for every buffer depth on the screen.
pub(super) fn ms(frames: u32) -> f32 {
    frames as f32 * 1000.0 / SAMPLE_RATE as f32
}

/// Paints the component library with this app's palette, so a button and a
/// channel strip belong to the same screen.
fn apply_palette(cx: &mut App) {
    Theme::change(ThemeMode::Dark, None, cx);
    let theme = cx.global_mut::<Theme>();
    theme.background = gpui::rgb(BG).into();
    theme.foreground = gpui::rgb(TEXT).into();
    theme.border = gpui::rgb(BORDER).into();
    theme.muted_foreground = gpui::rgb(MUTED).into();
    theme.primary = gpui::rgb(ACCENT).into();
    theme.primary_hover = gpui::rgb(ACCENT).into();
    theme.primary_active = gpui::rgb(ACCENT).into();
    theme.primary_foreground = gpui::rgb(0xffffff).into();
    theme.danger = gpui::rgb(DANGER).into();
    theme.accent = gpui::rgb(PANEL_ALT).into();
    theme.accent_foreground = gpui::rgb(TEXT).into();
    theme.input = gpui::rgb(TRACK).into();
    theme.title_bar = gpui::rgb(PANEL).into();
    theme.title_bar_border = gpui::rgb(BORDER_STRONG).into();
}

pub fn run(options: Options) {
    Application::new().run(move |cx: &mut App| {
        gpui_component::init(cx);
        apply_palette(cx);
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let bounds = Bounds::centered(None, size(px(1040.), px(720.)), cx);
        let opened = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("LAN Mic".into()),
                    appears_transparent: true,
                    ..Default::default()
                }),
                window_min_size: Some(size(px(880.), px(560.))),
                app_id: Some("com.lanmic.audio".into()),
                // Transparent, because the frame `Root` draws has a shadow
                // outside the window's own edge to drop it onto.
                window_background: WindowBackgroundAppearance::Transparent,
                // Linux only, and ignored elsewhere. Asking for client-side
                // decorations is what makes GNOME's Wayland session - which
                // does not implement the server-side protocol at all - report
                // `Decorations::Client`, so the frame is drawn rather than
                // leaving a window with no way to move or close it. X11
                // without a compositor refuses and falls back to the window
                // manager's own, which is the right answer there.
                window_decorations: Some(WindowDecorations::Client),
                ..Default::default()
            },
            |window, cx| {
                let view: gpui::AnyView = cx.new(|cx| LanMic::new(options, window, cx)).into();
                // `Root` is the component library's window shell: it draws the
                // border, the shadow and the resize edges on the platforms
                // that leave them to the application, and hosts anything that
                // has to float above the view.
                cx.new(|cx| Root::new(view, window, cx))
            },
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

    #[test]
    fn a_slider_reports_one_value_however_it_is_shaped() {
        assert_eq!(single(SliderValue::Single(0.75)), 0.75);
        assert_eq!(single(SliderValue::Range(0.1, 0.9)), 0.9);
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
/// What it does not do is look at pixels. A control drawn the wrong colour
/// still needs eyes. It does measure them, though: see
/// [`a_reading_that_grows_does_not_move_the_row`], which is the test for the
/// fixed-width readouts rather than for any of the code that draws them.
#[cfg(test)]
mod render_tests {
    use super::*;
    use gpui::{Entity, TestAppContext, VisualTestContext};
    use gpui_component::input::InputEvent;
    use lanmic::mixer::MAX_SOURCES;

    fn open(cx: &mut TestAppContext) -> (Entity<LanMic>, &mut VisualTestContext) {
        cx.update(|cx| {
            // The component library keeps its theme in a global, and every one
            // of its components reads it while rendering.
            gpui_component::init(cx);
            apply_palette(cx);
        });
        let (view, cx) =
            cx.add_window_view(|window, cx| LanMic::new(Options::default(), window, cx));
        cx.run_until_parked();
        (view, cx)
    }

    fn source(ssrc: u32, buffer_frames: u32, packets: u32) -> SourceSnapshot {
        SourceSnapshot {
            ssrc,
            peak_milli: 500,
            buffer_frames,
            packets,
            lost: 0,
            underruns: 0,
            age_ms: 10,
            muted: false,
            gain_milli: 1000,
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
        // button and counters. This is the densest thing the window ever
        // draws and the only part built in a loop, so it is where a layout
        // bug would land.
        view.update(cx, |this, cx| {
            this.sources = (0..MAX_SOURCES)
                .map(|i| source(0x1000 + i as u32, 720, 100))
                .collect();
            cx.notify();
        });
        cx.run_until_parked();
        view.update(cx, |this, _| assert_eq!(this.sources.len(), MAX_SOURCES));

        // And the tick takes them away again when there is no session behind
        // them, which is the other state the strip list has to survive.
        view.update(cx, |this, cx| {
            this.poll(cx);
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

    /// The reason the readouts have fixed widths.
    ///
    /// A buffer depth crossing from `9 ms` to `10 ms` is one character wider.
    /// In a plain flex row that pushes every reading after it - and the mute
    /// button beyond them - a few pixels sideways, several times a second,
    /// for as long as the mixer is running. This renders the same strip eight
    /// ways and requires the end of the row to land on the same pixel every
    /// time.
    #[gpui::test]
    fn a_reading_that_grows_does_not_move_the_row(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);

        let mut positions = Vec::new();
        // 9 ms and 10 ms; one packet and a million.
        for (buffer_frames, packets) in [
            (432, 1),
            (480, 1),
            (432, 1_000_000),
            (480, 1_000_000),
            (9600, 999),
            (240, 12),
            (720, 123_456),
            (48, 7),
        ] {
            view.update(cx, |this, cx| {
                this.sources = vec![source(0x1234, buffer_frames, packets)];
                cx.notify();
            });
            cx.run_until_parked();
            positions.push((
                buffer_frames,
                packets,
                cx.debug_bounds(mixer::STRIP_TAIL)
                    .expect("the strip's last reading should have been drawn"),
            ));
        }

        let (_, _, first) = positions[0];
        for (buffer_frames, packets, bounds) in &positions {
            assert_eq!(
                bounds.origin, first.origin,
                "the row moved at buf={buffer_frames} pkts={packets}"
            );
            assert_eq!(
                bounds.size, first.size,
                "the row changed width at buf={buffer_frames} pkts={packets}"
            );
        }
    }

    #[gpui::test]
    fn enter_in_the_address_field_tries_to_start(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        view.update(cx, |this, cx| {
            this.show(Panel::Microphone);
            cx.notify();
        });
        cx.run_until_parked();

        // The field is empty, so the attempt has to fail loudly rather than
        // quietly starting a microphone pointed at nowhere.
        let host = view.read_with(cx, |this, _| this.mic_host.clone());
        host.update(cx, |_, cx| {
            cx.emit(InputEvent::PressEnter { secondary: false });
        });
        cx.run_until_parked();

        view.update(cx, |this, _| {
            assert!(this.mic.is_none());
            assert!(this.error.is_some(), "an empty address should say so");
        });
    }

    #[gpui::test]
    fn an_address_typed_into_the_field_is_what_gets_dialled(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        let host = view.read_with(cx, |this, _| this.mic_host.clone());
        cx.update_window_entity(&host, |state, window, cx| {
            state.set_value("192.168.1.50", window, cx);
        });
        cx.run_until_parked();

        view.update(cx, |this, cx| {
            assert_eq!(LanMic::text(&this.mic_host, cx), "192.168.1.50");
        });
    }
}
