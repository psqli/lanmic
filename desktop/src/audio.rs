//! cpal in the place Oboe holds on the phone.
//!
//! The engine asks for two things and no more: interleaved `i16` handed to
//! [`CaptureEncoder::push`](lanmic::transmitter::CaptureEncoder::push), and a
//! mono `f32` bus out of [`Mixer::render`](lanmic::mixer::Mixer::render) written
//! into whatever the device wants. This module is the adapter for both, plus
//! the device and config picking that Oboe does for itself.
//!
//! Nothing here allocates once a stream is running: the format conversions work
//! through scratch buffers sized at open time.

use std::io;

use cpal::traits::{DeviceTrait, HostTrait};
use cpal::{
    BufferSize, Device, SampleFormat, StreamConfig, SupportedBufferSize, SupportedStreamConfigRange,
};

use lanmic::protocol::SAMPLE_RATE;

/// The engine is 48 kHz throughout - the wire format says so - and there is no
/// resampler anywhere in it. A device that will not run at 48 kHz is refused
/// rather than quietly rate-converted.
pub const REQUIRED_RATE: u32 = SAMPLE_RATE;

/// One device as the UI and `--list-devices` show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    pub name: String,
    /// The host's default for this direction, used when nothing was chosen.
    pub is_default: bool,
    /// False when the device cannot do 48 kHz in a format we can convert.
    pub usable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Input,
    Output,
}

/// The config an open will use: 48 kHz, a channel count the device offers, and
/// the sample format we will convert through.
#[derive(Debug, Clone, PartialEq)]
pub struct Chosen {
    pub config: StreamConfig,
    pub format: SampleFormat,
}

impl Chosen {
    pub fn channels(&self) -> usize {
        self.config.channels as usize
    }
}

/// Formats worth converting, best first. `I16` is what the wire and the capture
/// ring already speak; `F32` is what most modern backends hand out. Anything
/// else is refused rather than converted through a third representation.
const USABLE_FORMATS: [SampleFormat; 2] = [SampleFormat::I16, SampleFormat::F32];

fn format_rank(f: SampleFormat) -> Option<usize> {
    USABLE_FORMATS.iter().position(|&u| u == f)
}

/// Picks a 48 kHz config out of what a device offers.
///
/// `prefer_channels` is what the direction would rather have - 1 for capture,
/// since the wire is mono and a downmix of a stereo capture is one more thing
/// to be wrong; 2 for playback, because a mono-only output device is rare and
/// a phone's mixer plays stereo. It is a preference, not a requirement: the
/// closest count wins, ties going to the narrower stream.
///
/// `block_frames` is the buffer size to ask for. Requesting one is the single
/// biggest latency lever cpal exposes, so it is clamped into the device's range
/// rather than dropped when it does not fit.
pub fn choose_config(
    ranges: impl IntoIterator<Item = SupportedStreamConfigRange>,
    prefer_channels: u16,
    block_frames: u32,
) -> Option<Chosen> {
    let mut best: Option<(usize, u16, SupportedStreamConfigRange)> = None;
    for range in ranges {
        if !range.contains_rate(REQUIRED_RATE) || range.channels() == 0 {
            continue;
        }
        let Some(rank) = format_rank(range.sample_format()) else {
            continue;
        };
        let distance = range.channels().abs_diff(prefer_channels);
        // Sort by channel fit first: a device that offers f32 stereo and i16
        // 8-channel should give the UI two channels, not eight.
        let key = (distance, rank, range.channels());
        let better = match &best {
            None => true,
            Some((r, _, b)) => key < (b.channels().abs_diff(prefer_channels), *r, b.channels()),
        };
        if better {
            best = Some((rank, range.channels(), range));
        }
    }

    let (_, _, range) = best?;
    let buffer_size = match *range.buffer_size() {
        SupportedBufferSize::Range { min, max } if min <= max && max > 0 => {
            BufferSize::Fixed(block_frames.clamp(min.max(1), max))
        }
        // The backend will not say, so let it choose. PulseAudio and PipeWire
        // land here and pick something reasonable on their own.
        _ => BufferSize::Default,
    };
    Some(Chosen {
        config: StreamConfig {
            channels: range.channels(),
            sample_rate: REQUIRED_RATE,
            buffer_size,
        },
        format: range.sample_format(),
    })
}

fn supported(device: &Device, direction: Direction) -> Vec<SupportedStreamConfigRange> {
    let configs = match direction {
        Direction::Input => device.supported_input_configs().map(|c| c.collect()),
        Direction::Output => device.supported_output_configs().map(|c| c.collect()),
    };
    configs.unwrap_or_default()
}

pub fn device_name(device: &Device) -> String {
    device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| "(unnamed device)".into())
}

/// Every device in this direction, in host order, annotated with whether it can
/// actually carry the engine. Unusable devices are listed rather than hidden:
/// "my interface is not in the menu" is a worse bug report than "my interface
/// says 48 kHz unsupported".
pub fn devices(direction: Direction) -> Vec<DeviceInfo> {
    let host = cpal::default_host();
    let default = match direction {
        Direction::Input => host.default_input_device(),
        Direction::Output => host.default_output_device(),
    }
    .map(|d| device_name(&d));

    let found = match direction {
        Direction::Input => host.input_devices().map(|d| d.collect::<Vec<_>>()),
        Direction::Output => host.output_devices().map(|d| d.collect::<Vec<_>>()),
    };
    let Ok(found) = found else {
        return Vec::new();
    };

    found
        .into_iter()
        .map(|device| {
            let name = device_name(&device);
            DeviceInfo {
                is_default: Some(&name) == default.as_ref(),
                usable: choose_config(supported(&device, direction), 1, 240).is_some(),
                name,
            }
        })
        .collect()
}

/// Resolves a device by name, or the host default when `name` is `None`.
/// Matching is exact first, then a case-insensitive substring, so a UI can pass
/// the full name and a command line can pass "usb".
pub fn open_device(direction: Direction, name: Option<&str>) -> io::Result<Device> {
    let host = cpal::default_host();
    let Some(wanted) = name.map(str::trim).filter(|n| !n.is_empty()) else {
        return match direction {
            Direction::Input => host.default_input_device(),
            Direction::Output => host.default_output_device(),
        }
        .ok_or_else(|| io::Error::other(format!("no default {direction:?} device")));
    };

    let candidates = match direction {
        Direction::Input => host.input_devices().map(|d| d.collect::<Vec<_>>()),
        Direction::Output => host.output_devices().map(|d| d.collect::<Vec<_>>()),
    }
    .map_err(io::Error::other)?;

    let lowered = wanted.to_lowercase();
    let mut fuzzy = None;
    for device in candidates {
        let name = device_name(&device);
        if name == wanted {
            return Ok(device);
        }
        if fuzzy.is_none() && name.to_lowercase().contains(&lowered) {
            fuzzy = Some(device);
        }
    }
    fuzzy.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("no {direction:?} device matching {wanted:?}"),
        )
    })
}

/// The config this device will be opened with, or an error naming why it will
/// not do. Called on every reopen, so a device swapped for another model is
/// re-examined rather than reopened on stale assumptions.
pub fn config_for(device: &Device, direction: Direction, block_frames: u32) -> io::Result<Chosen> {
    let prefer = match direction {
        Direction::Input => 1,
        Direction::Output => 2,
    };
    choose_config(supported(device, direction), prefer, block_frames).ok_or_else(|| {
        io::Error::other(format!(
            "{} cannot do {} Hz in 16-bit or float",
            device_name(device),
            REQUIRED_RATE
        ))
    })
}

// ---------------------------------------------------------------------------
// Format conversion. Both directions, both formats, no allocation.
// ---------------------------------------------------------------------------

/// Spreads the mono mix bus across an interleaved float buffer, padding with
/// silence if the mixer produced less than was asked for. The same shape as
/// [`lanmic::receiver::write_stereo`], for backends that want floats and for
/// output devices that are not stereo.
pub fn write_mix_f32(mix: &[f32], out: &mut [f32], channels: usize) {
    let channels = channels.max(1);
    for (frame, &s) in out.chunks_mut(channels).zip(mix) {
        frame.fill(s);
    }
    for frame in out.chunks_mut(channels).skip(mix.len()) {
        frame.fill(0.0);
    }
}

/// As [`write_mix_f32`], for backends that want 16-bit. Clamped, not wrapped:
/// the limiter holds the bus under 1.0, but master gain is applied after it on
/// nobody's authority but the user's.
pub fn write_mix_i16(mix: &[f32], out: &mut [i16], channels: usize) {
    let channels = channels.max(1);
    for (frame, &s) in out.chunks_mut(channels).zip(mix) {
        let v = (s * 32767.0).clamp(-32768.0, 32767.0) as i16;
        frame.fill(v);
    }
    for frame in out.chunks_mut(channels).skip(mix.len()) {
        frame.fill(0);
    }
}

/// Float capture into the `i16` the capture ring takes, interleave preserved so
/// the encoder still sees frames of `channels` samples and does its own
/// downmix. `scratch` is resized once and reused.
pub fn capture_f32_to_i16(input: &[f32], scratch: &mut Vec<i16>) {
    scratch.clear();
    scratch.reserve(input.len());
    scratch.extend(
        input
            .iter()
            .map(|&s| (s * 32767.0).clamp(-32768.0, 32767.0) as i16),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(
        channels: u16,
        min: u32,
        max: u32,
        format: SampleFormat,
        buffer: SupportedBufferSize,
    ) -> SupportedStreamConfigRange {
        SupportedStreamConfigRange::new(channels, min, max, buffer, format)
    }

    fn ranged(min: u32, max: u32) -> SupportedBufferSize {
        SupportedBufferSize::Range { min, max }
    }

    #[test]
    fn a_device_that_cannot_do_48k_is_refused_rather_than_resampled() {
        let offered = vec![
            range(2, 44100, 44100, SampleFormat::F32, ranged(64, 4096)),
            range(1, 8000, 16000, SampleFormat::I16, ranged(64, 4096)),
        ];
        assert!(choose_config(offered, 1, 240).is_none());
    }

    #[test]
    fn formats_we_cannot_convert_are_skipped() {
        let offered = vec![
            range(1, 48000, 48000, SampleFormat::U16, ranged(64, 4096)),
            range(1, 48000, 48000, SampleFormat::I32, ranged(64, 4096)),
        ];
        assert!(choose_config(offered, 1, 240).is_none());

        let offered = vec![
            range(1, 48000, 48000, SampleFormat::U16, ranged(64, 4096)),
            range(1, 48000, 48000, SampleFormat::F32, ranged(64, 4096)),
        ];
        assert_eq!(
            choose_config(offered, 1, 240).unwrap().format,
            SampleFormat::F32
        );
    }

    #[test]
    fn the_channel_count_closest_to_the_preference_wins() {
        let offered = vec![
            range(8, 48000, 48000, SampleFormat::I16, ranged(64, 4096)),
            range(2, 48000, 48000, SampleFormat::F32, ranged(64, 4096)),
            range(1, 48000, 48000, SampleFormat::F32, ranged(64, 4096)),
        ];
        // Capture wants mono even though a wider stream offers the nicer format.
        assert_eq!(
            choose_config(offered.clone(), 1, 240).unwrap().channels(),
            1
        );
        assert_eq!(choose_config(offered, 2, 240).unwrap().channels(), 2);
    }

    #[test]
    fn i16_is_preferred_when_the_channel_count_ties() {
        let offered = vec![
            range(2, 48000, 48000, SampleFormat::F32, ranged(64, 4096)),
            range(2, 48000, 48000, SampleFormat::I16, ranged(64, 4096)),
        ];
        assert_eq!(
            choose_config(offered, 2, 240).unwrap().format,
            SampleFormat::I16
        );
    }

    #[test]
    fn the_block_size_is_clamped_into_the_device_range_not_dropped() {
        let tight = vec![range(2, 48000, 48000, SampleFormat::I16, ranged(512, 1024))];
        assert_eq!(
            choose_config(tight, 2, 240).unwrap().config.buffer_size,
            BufferSize::Fixed(512)
        );

        let wide = vec![range(2, 48000, 48000, SampleFormat::I16, ranged(64, 4096))];
        assert_eq!(
            choose_config(wide, 2, 240).unwrap().config.buffer_size,
            BufferSize::Fixed(240)
        );

        let unknown = vec![range(
            2,
            48000,
            48000,
            SampleFormat::I16,
            SupportedBufferSize::Unknown,
        )];
        assert_eq!(
            choose_config(unknown, 2, 240).unwrap().config.buffer_size,
            BufferSize::Default
        );
    }

    #[test]
    fn the_mix_is_copied_to_every_output_channel() {
        let mix = [0.5f32, -0.5, 1.0];
        let mut out = [0.0f32; 8]; // four stereo frames for a three-frame mix
        write_mix_f32(&mix, &mut out, 2);
        assert_eq!(out, [0.5, 0.5, -0.5, -0.5, 1.0, 1.0, 0.0, 0.0]);

        let mut out = [0i16; 8];
        write_mix_i16(&mix, &mut out, 2);
        assert_eq!(out, [16383, 16383, -16383, -16383, 32767, 32767, 0, 0]);
    }

    #[test]
    fn an_output_that_is_not_stereo_still_gets_the_whole_mix() {
        let mix = [1.0f32, -1.0];
        let mut out = [0.0f32; 6];
        write_mix_f32(&mix, &mut out, 3);
        assert_eq!(out, [1.0, 1.0, 1.0, -1.0, -1.0, -1.0]);

        let mut mono = [0.0f32; 2];
        write_mix_f32(&mix, &mut mono, 1);
        assert_eq!(mono, [1.0, -1.0]);
    }

    #[test]
    fn a_short_mix_leaves_silence_rather_than_stale_audio() {
        let mut out = [7.0f32; 6];
        write_mix_f32(&[0.25], &mut out, 2);
        assert_eq!(out, [0.25, 0.25, 0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn float_capture_is_converted_frame_for_frame_and_clamped() {
        let mut scratch = Vec::new();
        capture_f32_to_i16(&[0.0, 1.0, -1.0, 2.0, -2.0], &mut scratch);
        assert_eq!(scratch, [0, 32767, -32767, 32767, -32768]);

        // Reused, not grown: a second, shorter burst leaves nothing behind.
        capture_f32_to_i16(&[0.5], &mut scratch);
        assert_eq!(scratch, [16383]);
    }
}
