//! The command line.
//!
//! The flags deliberately match `server/lan_audio_server.py` where the two
//! overlap, because they are alternatives for the same job and nobody should
//! have to remember which one spells it `--blocksize`.
//!
//! [`Cli`] is the flags as written; [`Options`] is what the rest of the program
//! wants - two config structs and a mode. They are separate because the
//! mapping is not one to one: `--port` sets a port at both ends, `--headless`
//! and `--mic` between them decide the mode, and an empty `--name` falls back
//! to the hostname.

use clap::Parser;

use lanmic::protocol::{DEFAULT_AUDIO_PORT, DISCOVERY_PORT};
use lanmic::receiver::{MAX_JITTER_MS, MIN_JITTER_MS};

use crate::engine::{default_server_name, MicConfig, ServerConfig, DEFAULT_BLOCK_FRAMES};

/// Ranges are declared here rather than checked afterwards, so a bad value is
/// refused by the parser with the range in the message - and so the help text
/// and the validation cannot disagree.
#[derive(Parser, Debug)]
#[command(
    name = "lanmic",
    version,
    about = "LAN Mic - wireless microphones over your own Wi-Fi",
    after_help = "\
Examples:
  lanmic                          open the window (mixer and microphone in one)
  lanmic --headless               run the mixer on this terminal
  lanmic --headless --mic HOST    run this machine as a microphone
  lanmic --list-devices           show the audio devices and exit

Device names are matched by substring, so --output usb is enough. Everything
runs at 48 kHz with no resampler, so a device that will not do 48 kHz is
refused rather than rate-converted."
)]
pub struct Cli {
    /// Run on this terminal instead of opening a window
    #[arg(long)]
    pub headless: bool,

    /// List the audio devices and exit
    #[arg(long)]
    pub list_devices: bool,

    // -- mixer ------------------------------------------------------------
    /// Audio port, at both ends
    #[arg(long, default_value_t = DEFAULT_AUDIO_PORT,
          value_parser = clap::value_parser!(u16).range(1..),
          help_heading = "Mixer")]
    pub port: u16,

    /// Jitter buffer target
    #[arg(long, value_name = "MS", default_value_t = 15,
          value_parser = clap::value_parser!(i32).range(MIN_JITTER_MS as i64..=MAX_JITTER_MS as i64),
          help_heading = "Mixer")]
    pub jitter: i32,

    /// Output device; a substring of its name is enough
    #[arg(long, value_name = "NAME", help_heading = "Mixer")]
    pub output: Option<String>,

    /// Output block; 240 frames is 5 ms
    #[arg(long, value_name = "FRAMES", default_value_t = DEFAULT_BLOCK_FRAMES,
          value_parser = clap::value_parser!(u32).range(1..),
          help_heading = "Mixer")]
    pub blocksize: u32,

    /// What to answer discovery probes with [default: this machine's hostname]
    #[arg(long, value_name = "NAME", help_heading = "Mixer")]
    pub name: Option<String>,

    /// Port discovery probes are answered on
    #[arg(long, value_name = "PORT", default_value_t = DISCOVERY_PORT,
          value_parser = clap::value_parser!(u16).range(1..),
          help_heading = "Mixer")]
    pub discovery_port: u16,

    /// Do not answer discovery probes
    #[arg(long, help_heading = "Mixer")]
    pub no_discovery: bool,

    // -- microphone -------------------------------------------------------
    /// Server address: the mixer's IP
    ///
    /// With --headless this machine becomes a microphone sending there.
    /// Without it, the window opens on the microphone panel with the address
    /// already filled in.
    #[arg(long, value_name = "HOST", help_heading = "Microphone")]
    pub mic: Option<String>,

    /// Input device; a substring of its name is enough
    #[arg(long, value_name = "NAME", help_heading = "Microphone")]
    pub input: Option<String>,

    /// Packet payload; 240 frames is 5 ms
    #[arg(long, value_name = "FRAMES", default_value_t = DEFAULT_BLOCK_FRAMES,
          value_parser = clap::value_parser!(u32).range(1..),
          help_heading = "Microphone")]
    pub packet: u32,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Mode {
    /// The GPUI window, which does both jobs.
    Window,
    /// The mixer, on a terminal, as the Python server does it.
    HeadlessServer,
    /// This machine as a microphone, on a terminal.
    HeadlessMic,
    ListDevices,
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

impl From<Cli> for Options {
    fn from(cli: Cli) -> Self {
        let host = cli.mic.unwrap_or_default().trim().to_string();
        let mode = match (cli.list_devices, cli.headless, host.is_empty()) {
            (true, _, _) => Mode::ListDevices,
            (_, true, false) => Mode::HeadlessMic,
            (_, true, true) => Mode::HeadlessServer,
            // `--mic` on its own opens the window with the address filled in;
            // it takes a terminal only when asked for one.
            _ => Mode::Window,
        };

        Options {
            mode,
            server: ServerConfig {
                port: cli.port,
                discovery_port: cli.discovery_port,
                jitter_ms: cli.jitter,
                // An empty `--name` would otherwise advertise nothing at all.
                name: cli
                    .name
                    .map(|n| n.trim().to_string())
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(default_server_name),
                device: cli.output,
                block_frames: cli.blocksize,
                discovery: !cli.no_discovery,
            },
            mic: MicConfig {
                host,
                // One flag for both ends: a microphone and a mixer on one
                // machine talking to each other on different ports is never
                // what was meant.
                port: cli.port,
                frames_per_packet: cli.packet as usize,
                device: cli.input,
            },
        }
    }
}

/// Parses a whole `argv`, program name included.
///
/// The error is clap's, not a string: `--help` and `--version` come back
/// through it too, and only clap knows they belong on stdout with a zero exit.
pub fn parse_from<I, T>(argv: I) -> Result<Options, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    Cli::try_parse_from(argv).map(Options::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_str(s: &str) -> Result<Options, clap::Error> {
        parse_from(std::iter::once("lanmic").chain(s.split_whitespace()))
    }

    #[test]
    fn the_declared_command_is_internally_consistent() {
        // clap's own audit: duplicate flags, a default that its value_parser
        // would reject, a mistyped help heading. Cheap, and it fails here
        // rather than on someone's terminal.
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }

    #[test]
    fn no_arguments_opens_the_window_with_the_documented_defaults() {
        let o = parse_str("").unwrap();
        assert_eq!(o.mode, Mode::Window);
        assert_eq!(o.server.port, DEFAULT_AUDIO_PORT);
        assert_eq!(o.server.discovery_port, DISCOVERY_PORT);
        assert_eq!(o.server.jitter_ms, 15);
        assert_eq!(o.server.block_frames, DEFAULT_BLOCK_FRAMES);
        assert_eq!(o.mic.frames_per_packet, DEFAULT_BLOCK_FRAMES as usize);
        assert!(o.server.discovery);
        assert!(o.server.device.is_none());
        assert!(o.mic.device.is_none());
        assert!(!o.server.name.trim().is_empty());
    }

    #[test]
    fn the_audio_port_moves_both_ends_together() {
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
    fn an_all_whitespace_address_is_no_address() {
        // Quoted, so it survives as one argument.
        let o = parse_from(["lanmic", "--headless", "--mic", "   "]).unwrap();
        assert_eq!(o.mode, Mode::HeadlessServer);
        assert!(o.mic.host.is_empty());
    }

    #[test]
    fn list_devices_beats_everything_else() {
        assert_eq!(
            parse_str("--headless --list-devices").unwrap().mode,
            Mode::ListDevices
        );
        assert_eq!(
            parse_str("--list-devices --mic 10.0.0.4").unwrap().mode,
            Mode::ListDevices
        );
    }

    #[test]
    fn help_and_version_come_back_as_clap_errors_not_as_options() {
        for flag in ["--help", "-h", "--version", "-V"] {
            let err = parse_str(flag).unwrap_err();
            assert!(
                !err.use_stderr(),
                "{flag} should print to stdout, not stderr"
            );
        }
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
        assert!(parse_str("--discovery-port 0").is_err());
        assert!(parse_str("--blocksize 0").is_err());
        assert!(parse_str("--packet 0").is_err());
        assert!(parse_str("--wat").is_err());
        // A positional argument is nothing this program takes.
        assert!(parse_str("somefile").is_err());
        // A flag whose value was forgotten must not eat the next flag.
        assert!(parse_str("--name --no-discovery").is_err());
    }

    #[test]
    fn device_names_survive_being_read_off_a_command_line() {
        let o = parse_from([
            "lanmic",
            "--output",
            "USB Audio CODEC",
            "--input",
            "Scarlett 2i2",
        ])
        .unwrap();
        assert_eq!(o.server.device.as_deref(), Some("USB Audio CODEC"));
        assert_eq!(o.mic.device.as_deref(), Some("Scarlett 2i2"));
    }

    #[test]
    fn an_empty_name_falls_back_rather_than_advertising_nothing() {
        let o = parse_from(["lanmic", "--name", "   "]).unwrap();
        assert!(!o.server.name.trim().is_empty());
        let o = parse_from(["lanmic", "--name", "Front of house"]).unwrap();
        assert_eq!(o.server.name, "Front of house");
    }

    #[test]
    fn discovery_is_on_until_it_is_turned_off() {
        assert!(parse_str("").unwrap().server.discovery);
        assert!(!parse_str("--no-discovery").unwrap().server.discovery);
    }
}
