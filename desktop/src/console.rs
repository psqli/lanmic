//! The terminal front-end: `--headless`, and the status line the Python server
//! prints. Useful over SSH, in a rack, and on a machine with no GPU for GPUI to
//! talk to.

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use lanmic::mixer::SourceSnapshot;
use lanmic::protocol::SAMPLE_RATE;
use lanmic::util::now_ms;

use crate::args::Options;
use crate::audio::{self, Direction};
use crate::discovery;
use crate::engine::{Microphone, Server};

const REFRESH: Duration = Duration::from_millis(250);
const BAR_WIDTH: usize = 20;

/// `[####........]` at the width the status line has room for.
fn bar(level: f32) -> String {
    let filled = ((level.clamp(0.0, 1.0) * BAR_WIDTH as f32) as usize).min(BAR_WIDTH);
    format!("{}{}", "#".repeat(filled), ".".repeat(BAR_WIDTH - filled))
}

fn strip(source: &SourceSnapshot) -> String {
    let fill_ms = source.buffer_frames as f32 * 1000.0 / SAMPLE_RATE as f32;
    format!(
        "MIC-{:04X} [{}] {:4.0}ms lost:{} und:{}",
        source.ssrc & 0xFFFF,
        bar(source.peak_milli as f32 / 1000.0),
        fill_ms,
        source.lost,
        source.underruns
    )
}

/// Ctrl-C without a signal-handling crate. A `static` rather than something
/// handed to the handler, because setting one atomic is all a signal handler
/// may safely do.
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn on_signal(_: i32) {
    INTERRUPTED.store(true, Ordering::Release);
}

/// Makes Ctrl-C (and a `kill`) end the loop rather than the process, so the
/// session's `Drop` runs and the mixer gets its BYE.
pub fn catch_interrupts() {
    #[cfg(unix)]
    // SAFETY: installing a handler that only stores to an atomic. Nothing here
    // allocates, locks, or touches memory the handler does not own.
    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
    }
}

fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::Acquire)
}

pub fn list_devices() {
    for (direction, label) in [(Direction::Input, "input"), (Direction::Output, "output")] {
        println!("{label} devices:");
        let devices = audio::devices(direction);
        if devices.is_empty() {
            println!("  (none)");
        }
        for device in devices {
            println!(
                "  {}{}{}",
                if device.is_default { "* " } else { "  " },
                device.name,
                if device.usable {
                    ""
                } else {
                    "   [no 48 kHz 16-bit or float config]"
                }
            );
        }
        println!();
    }
    println!("* = system default. Names are matched by substring, so --output usb is enough.");
}

pub fn run_server(options: &Options) -> io::Result<()> {
    let server = Server::start(&options.server)?;
    catch_interrupts();

    println!(
        "LAU1 mixer '{}' on udp/{}, jitter {} ms, out {}",
        options.server.name,
        options.server.port,
        options.server.jitter_ms,
        server.device_name()
    );
    let addresses = discovery::local_addresses();
    println!(
        "point the phones at: {}",
        if addresses.is_empty() {
            "(no non-loopback address found)".to_string()
        } else {
            addresses.join(", ")
        }
    );
    println!("Ctrl-C to stop.");

    let mut sources = Vec::new();
    while !interrupted() && server.is_running() {
        let stats = server.stats();
        server.table().snapshot(now_ms(), &mut sources);
        sources.sort_by_key(|s| s.ssrc);
        let strips: Vec<String> = sources.iter().map(strip).collect();
        print!(
            "\r\x1b[K sources:{} pkts:{} bad:{} lim:{:.2} xrun:{} out:{:.1}ms{}{}",
            stats.active_sources,
            stats.packets,
            stats.bad_packets,
            stats.limiter_gain,
            stats.xruns,
            stats.latency_ms,
            if strips.is_empty() { "" } else { "  |  " },
            strips.join("  |  ")
        );
        let _ = io::stdout().flush();
        thread::sleep(REFRESH);
    }
    println!();
    Ok(())
}

pub fn run_mic(options: &Options) -> io::Result<()> {
    let mic = Microphone::start(&options.mic)?;
    catch_interrupts();

    println!(
        "LAU1 microphone -> {}, ssrc {:08X}, in {}",
        mic.target(),
        mic.ssrc(),
        mic.device_name()
    );
    println!("Ctrl-C to stop.");

    while !interrupted() && mic.is_running() {
        let stats = mic.stats();
        print!(
            "\r\x1b[K [{}] sent:{} dropped:{} errs:{} xrun:{} in:{:.1}ms",
            bar(stats.peak),
            stats.packets_sent,
            stats.frames_dropped,
            stats.send_errors,
            stats.xruns,
            stats.latency_ms
        );
        let _ = io::stdout().flush();
        thread::sleep(REFRESH);
    }
    println!();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_meter_bar_fills_and_never_overruns_its_width() {
        assert_eq!(bar(0.0), ".".repeat(BAR_WIDTH));
        assert_eq!(bar(1.0), "#".repeat(BAR_WIDTH));
        assert_eq!(bar(0.5).chars().filter(|&c| c == '#').count(), 10);
        // Gains reach 2x, so the meter reports above full scale.
        assert_eq!(bar(2.0).len(), BAR_WIDTH);
        assert_eq!(bar(f32::NAN).len(), BAR_WIDTH);
        assert_eq!(bar(-1.0), ".".repeat(BAR_WIDTH));
    }

    #[test]
    fn a_strip_reports_the_buffer_in_milliseconds_not_frames() {
        let source = SourceSnapshot {
            ssrc: 0xDEAD_BEEF,
            peak_milli: 500,
            // 15 ms at 48 kHz.
            buffer_frames: 720,
            lost: 3,
            underruns: 1,
            ..Default::default()
        };
        let line = strip(&source);
        assert!(line.contains("MIC-BEEF"), "{line}");
        assert!(line.contains("15ms"), "{line}");
        assert!(line.contains("lost:3"), "{line}");
        assert!(line.contains("und:1"), "{line}");
    }
}
