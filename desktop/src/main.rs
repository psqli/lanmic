//! LAN Mic for the desktop.
//!
//! One binary, two jobs, the same two the phone does: be the **mixer** that
//! every microphone sends to, or be a **microphone** sending to one. The
//! engine underneath is `lanmic`, the crate the Android app ships - protocol,
//! jitter buffers, mixer, limiter, feedback shifter and sockets, unchanged -
//! with cpal streams where the phone has Oboe and a GPUI window where it has
//! Compose.
//!
//! ```text
//!   src/args.rs       the command line
//!   src/audio.rs      cpal: devices, 48 kHz configs, format conversion
//!   src/engine.rs     the two sessions and the threads around them
//!   src/discovery.rs  DISCOVER / ANNOUNCE, both halves
//!   src/console.rs    --headless, for a machine with no screen
//!   src/ui/           the GPUI window
//! ```

mod args;
mod audio;
mod console;
mod discovery;
mod engine;
mod ui;

use std::process::ExitCode;

use args::Mode;

fn main() -> ExitCode {
    // Quiet by default; `RUST_LOG=lanmic_desktop=debug` when something is wrong.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    // clap prints its own help, version and errors, on the right stream and
    // with the right exit code, and `exit` does not come back.
    let options = match args::parse_from(std::env::args()) {
        Ok(options) => options,
        Err(e) => e.exit(),
    };

    let result = match options.mode {
        Mode::ListDevices => {
            console::list_devices();
            return ExitCode::SUCCESS;
        }
        Mode::HeadlessServer => console::run_server(&options),
        Mode::HeadlessMic => console::run_mic(&options),
        Mode::Window => {
            ui::run(options);
            return ExitCode::SUCCESS;
        }
    };

    if let Err(e) = result {
        eprintln!("lanmic: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
