//! Command line, parsed by hand.
//!
//! The flags deliberately match `server/lan_audio_server.py`, because the two
//! are alternatives for the same job and nobody should have to remember which
//! one spells it `--blocksize`.

use lanmic::receiver::{MAX_JITTER_MS, MIN_JITTER_MS};

use crate::engine::{default_server_name, MicConfig, ServerConfig};

pub const USAGE: &str = "\
LAN Mic - wireless microphones over your own Wi-Fi

    lanmic                          open the window (mixer and microphone in one)
    lanmic --headless               run the mixer on this terminal
    lanmic --headless --mic HOST    run this machine as a microphone
    lanmic --list-devices           show the audio devices and exit

Mixer:
    --port N          audio port to bind (default 45678)
    --jitter MS       jitter buffer target, 5..200 (default 15)
    --output NAME     output device; a substring is enough (default: system)
    --blocksize N     output block in frames; 240 = 5 ms (default 240)
    --name NAME       what to answer discovery probes with (default: hostname)
    --discovery-port N                                     (default 45679)
    --no-discovery    do not answer discovery probes

Microphone:
    --mic HOST        server address; the mixer's IP
    --input NAME      input device; a substring is enough (default: system)
    --packet N        frames per packet; 240 = 5 ms (default 240)

    -h, --help        this text
";

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Mode {
    /// The GPUI window, which does both jobs.
    Window,
    /// The mixer, on a terminal, as the Python server does it.
    HeadlessServer,
    /// This machine as a microphone, on a terminal.
    HeadlessMic,
    ListDevices,
    Help,
}

#[derive(Debug, Clone)]
pub struct Options {
    pub mode: Mode,
    pub server: ServerConfig,
    pub mic: MicConfig,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            mode: Mode::Window,
            server: ServerConfig::default(),
            mic: MicConfig::default(),
        }
    }
}

fn number<T: std::str::FromStr>(flag: &str, value: Option<String>) -> Result<T, String> {
    let raw = value.ok_or_else(|| format!("{flag} needs a value"))?;
    raw.parse()
        .map_err(|_| format!("{flag}: {raw:?} is not a number"))
}

fn text(flag: &str, value: Option<String>) -> Result<String, String> {
    value.ok_or_else(|| format!("{flag} needs a value"))
}

/// Parses everything after the program name. Errors are for a human on a
/// terminal, so they name the flag and what was wrong with it.
pub fn parse<I: IntoIterator<Item = String>>(argv: I) -> Result<Options, String> {
    let mut options = Options::default();
    let mut headless = false;
    let mut args = argv.into_iter().peekable();

    while let Some(arg) = args.next() {
        // Only flags take values, so a value that looks like a flag is a
        // missing argument rather than a filename.
        let mut value = || args.next_if(|a| !a.starts_with("--"));
        match arg.as_str() {
            "-h" | "--help" => {
                return Ok(Options {
                    mode: Mode::Help,
                    ..options
                })
            }
            "--list-devices" => options.mode = Mode::ListDevices,
            "--headless" => headless = true,
            "--port" => {
                let port = number::<u16>("--port", value())?;
                options.server.port = port;
                options.mic.port = port;
            }
            "--discovery-port" => {
                options.server.discovery_port = number("--discovery-port", value())?
            }
            "--jitter" => {
                let ms: i32 = number("--jitter", value())?;
                if !(MIN_JITTER_MS..=MAX_JITTER_MS).contains(&ms) {
                    return Err(format!(
                        "--jitter: {ms} ms is outside {MIN_JITTER_MS}..={MAX_JITTER_MS}"
                    ));
                }
                options.server.jitter_ms = ms;
            }
            "--name" => options.server.name = text("--name", value())?,
            "--no-discovery" => options.server.discovery = false,
            "--output" => options.server.device = Some(text("--output", value())?),
            "--input" => options.mic.device = Some(text("--input", value())?),
            "--blocksize" => options.server.block_frames = positive("--blocksize", value())?,
            "--packet" => {
                options.mic.frames_per_packet = positive::<u32>("--packet", value())? as usize
            }
            "--mic" => {
                options.mic.host = text("--mic", value())?;
                if options.mode == Mode::Window {
                    // Only meaningful with --headless, which may not have been
                    // seen yet; the pass below settles it.
                    options.mode = Mode::HeadlessMic;
                }
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }

    if options.server.port == 0 {
        return Err("--port: 0 is not a port".into());
    }
    options.mode = match (options.mode, headless, options.mic.host.is_empty()) {
        (Mode::ListDevices, _, _) => Mode::ListDevices,
        (Mode::Help, _, _) => Mode::Help,
        // `--mic` on its own opens the window with the address filled in; it
        // takes a terminal only when asked for one.
        (_, true, false) => Mode::HeadlessMic,
        (_, true, true) => Mode::HeadlessServer,
        _ => Mode::Window,
    };
    if options.server.name.trim().is_empty() {
        options.server.name = default_server_name();
    }
    Ok(options)
}

fn positive<T>(flag: &str, value: Option<String>) -> Result<T, String>
where
    T: std::str::FromStr + PartialOrd + Default + std::fmt::Display,
{
    let n: T = number(flag, value)?;
    if n <= T::default() {
        return Err(format!("{flag}: {n} is not a frame count"));
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::DEFAULT_BLOCK_FRAMES;
    use lanmic::protocol::{DEFAULT_AUDIO_PORT, DISCOVERY_PORT};

    fn parse_str(s: &str) -> Result<Options, String> {
        parse(s.split_whitespace().map(str::to_string))
    }

    #[test]
    fn no_arguments_opens_the_window_with_the_documented_defaults() {
        let o = parse_str("").unwrap();
        assert_eq!(o.mode, Mode::Window);
        assert_eq!(o.server.port, DEFAULT_AUDIO_PORT);
        assert_eq!(o.server.discovery_port, DISCOVERY_PORT);
        assert_eq!(o.server.jitter_ms, 15);
        assert_eq!(o.server.block_frames, DEFAULT_BLOCK_FRAMES);
        assert!(o.server.discovery);
        assert!(!o.server.name.trim().is_empty());
    }

    #[test]
    fn the_audio_port_moves_both_ends_together() {
        // One flag, because a microphone and a mixer on one machine talking to
        // each other on different ports is never what was meant.
        let o = parse_str("--port 40000").unwrap();
        assert_eq!(o.server.port, 40000);
        assert_eq!(o.mic.port, 40000);
    }

    #[test]
    fn headless_picks_the_mixer_unless_a_server_address_was_given() {
        assert_eq!(parse_str("--headless").unwrap().mode, Mode::HeadlessServer);
        let o = parse_str("--headless --mic 192.168.1.50").unwrap();
        assert_eq!(o.mode, Mode::HeadlessMic);
        assert_eq!(o.mic.host, "192.168.1.50");
    }

    #[test]
    fn mic_without_headless_opens_the_window_with_the_address_filled_in() {
        let o = parse_str("--mic 10.0.0.4").unwrap();
        assert_eq!(o.mode, Mode::Window);
        assert_eq!(o.mic.host, "10.0.0.4");
    }

    #[test]
    fn list_devices_and_help_beat_everything_else() {
        assert_eq!(
            parse_str("--headless --list-devices").unwrap().mode,
            Mode::ListDevices
        );
        assert_eq!(parse_str("--headless --help").unwrap().mode, Mode::Help);
        assert_eq!(parse_str("-h").unwrap().mode, Mode::Help);
    }

    #[test]
    fn the_jitter_target_is_range_checked_where_it_is_typed() {
        assert_eq!(parse_str("--jitter 5").unwrap().server.jitter_ms, 5);
        assert_eq!(parse_str("--jitter 200").unwrap().server.jitter_ms, 200);
        for bad in ["--jitter 0", "--jitter 4", "--jitter 201", "--jitter -1"] {
            assert!(parse_str(bad).is_err(), "{bad} should be rejected");
        }
    }

    #[test]
    fn nonsense_is_reported_rather_than_defaulted() {
        assert!(parse_str("--port").is_err());
        assert!(parse_str("--port 0").is_err());
        assert!(parse_str("--port 70000").is_err());
        assert!(parse_str("--port abc").is_err());
        assert!(parse_str("--blocksize 0").is_err());
        assert!(parse_str("--packet 0").is_err());
        assert!(parse_str("--wat").is_err());
        // A flag whose value was forgotten must not eat the next flag.
        assert!(parse_str("--name --no-discovery").is_err());
    }

    #[test]
    fn device_names_survive_being_read_off_a_command_line() {
        let o = parse(
            ["--output", "USB Audio CODEC", "--input", "Scarlett 2i2"]
                .iter()
                .map(|s| s.to_string()),
        )
        .unwrap();
        assert_eq!(o.server.device.as_deref(), Some("USB Audio CODEC"));
        assert_eq!(o.mic.device.as_deref(), Some("Scarlett 2i2"));
    }

    #[test]
    fn an_empty_name_falls_back_rather_than_advertising_nothing() {
        let o = parse(["--name", "   "].iter().map(|s| s.to_string())).unwrap();
        assert!(!o.server.name.trim().is_empty());
    }
}
