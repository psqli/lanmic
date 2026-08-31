//! Microphone to loudspeaker, over a real socket, with no phone involved.
//!
//! Everything here is the code the app runs, minus the two Oboe streams: the
//! capture encoder stands in for the input callback and `Mixer::render` for the
//! output one. That leaves very little of the engine that only a device can
//! exercise.

use std::sync::Arc;

use lanmic::mixer::MAX_SOURCES;
use lanmic::protocol::{Header, PacketType, MAX_PACKET_BYTES};
use lanmic::receiver::{self, PacketRouter, Polled, RxShared};
use lanmic::transmitter::{self, CaptureEncoder, Packetiser, TxShared};

const JITTER_MS: i32 = 25;
const TARGET_FRAMES: usize = 48_000 * JITTER_MS as usize / 1000; // 1200
const BLOCK: usize = 240;

struct Rig {
    router: PacketRouter,
    mixer: lanmic::mixer::Mixer,
    table: Arc<lanmic::mixer::Table>,
    rx_shared: Arc<RxShared>,
    port: u16,
}

fn rig() -> Rig {
    let rx_shared = Arc::new(RxShared::default());
    let (router, mixer, table) = receiver::build(0, JITTER_MS, rx_shared.clone()).unwrap();
    // These tests are about what the transport does to the samples, so the
    // feedback shift comes out of the path - it deliberately rewrites every one
    // of them. That it is on by default, and reaches the bus, is covered by
    // `feedback_suppression_is_on_by_default_and_reaches_the_bus` and by
    // `the_default_suppression_survives_the_real_receiver_path` below.
    table.set_feedback_shift_hz(0.0);
    let port = router.local_port();
    Rig {
        router,
        mixer,
        table,
        rx_shared,
        port,
    }
}

fn microphone(port: u16, frames_per_packet: usize) -> (CaptureEncoder, Packetiser, Arc<TxShared>) {
    let shared = Arc::new(TxShared::default());
    shared.reset_for_session();
    let socket = lanmic::net::open_sender("127.0.0.1", port).unwrap();
    let (enc, tx) = transmitter::build(socket, frames_per_packet, shared.clone());
    (enc, tx, shared)
}

/// Drains whatever is waiting on the router, returning what it saw.
fn drain(router: &mut PacketRouter, now_ms: i64, budget: usize) -> Vec<Polled> {
    let mut seen = Vec::new();
    for _ in 0..budget {
        match router.poll(now_ms) {
            Polled::Idle => break,
            other => seen.push(other),
        }
    }
    seen
}

#[test]
fn a_ramp_survives_capture_wire_and_mix_sample_accurate() {
    let mut r = rig();
    let (mut enc, mut tx, _shared) = microphone(r.port, 120);

    // 20 packets of a ramp, pushed through the capture path a burst at a time.
    const PACKETS: usize = 20;
    const FRAMES: usize = 120;
    let ramp: Vec<i16> = (0..PACKETS * FRAMES).map(|i| (i % 20_000) as i16).collect();
    for burst in ramp.chunks(FRAMES) {
        assert_eq!(enc.push(burst, 1), burst.len());
    }
    assert_eq!(tx.pump(), PACKETS);

    let audio = drain(&mut r.router, 0, PACKETS * 2);
    assert_eq!(audio.len(), PACKETS, "every packet arrived: {audio:?}");
    assert!(audio
        .iter()
        .all(|p| matches!(p, Polled::Audio { frames: 120, .. })));

    // The priming silence comes out first, then the ramp, in order.
    for _ in 0..TARGET_FRAMES / BLOCK {
        assert!(r.mixer.render(BLOCK).iter().all(|s| s.abs() < 1e-6));
    }

    let mut index = 0usize;
    for _ in 0..PACKETS * FRAMES / BLOCK {
        for &s in r.mixer.render(BLOCK) {
            let want = (index % 20_000) as f32 / 32768.0;
            assert!(
                (s - want).abs() < 1e-4,
                "sample {index}: got {s}, want {want}"
            );
            index += 1;
        }
    }
    assert_eq!(index, PACKETS * FRAMES);
    assert_eq!(r.table.active_sources(), 1);
}

#[test]
fn the_default_suppression_survives_the_real_receiver_path() {
    // `rig()` switches the shift off; this checks what an operator actually
    // gets from `receiver::build`, which is where the table comes from in the
    // app: suppression on, at a working depth, colouring the bus.
    let rx_shared = Arc::new(RxShared::default());
    let (mut router, mut mixer, table) = receiver::build(0, JITTER_MS, rx_shared).unwrap();
    assert_eq!(
        table.feedback_shift_hz(),
        lanmic::mixer::DEFAULT_FEEDBACK_SHIFT_HZ
    );

    let (mut enc, mut tx, _shared) = microphone(router.local_port(), 240);
    let pcm: Vec<i16> = (0..240)
        .map(|i| ((i as f32 * 0.13).sin() * 16000.0) as i16)
        .collect();
    for _ in 0..2 {
        enc.push(&pcm, 1);
    }
    tx.pump();
    drain(&mut router, 0, 8);

    for _ in 0..TARGET_FRAMES / BLOCK {
        mixer.render(BLOCK);
    }
    let out = mixer.render(BLOCK);
    assert!(out.iter().any(|s| s.abs() > 0.01), "the bus is silent");
    // Shifted, so not a sample-for-sample copy of what went in.
    let same = out
        .iter()
        .zip(&pcm)
        .filter(|(o, &p)| (**o - p as f32 / 32768.0).abs() < 1e-4)
        .count();
    assert!(same < out.len() / 2, "the shift did not reach the bus");
}

#[test]
fn stereo_capture_is_downmixed_to_the_mono_wire_format() {
    let mut r = rig();
    let (mut enc, mut tx, _shared) = microphone(r.port, 120);

    // Left and right average to a known value, and the two do not simply sum
    // into clipping.
    let interleaved: Vec<i16> = (0..120).flat_map(|_| [30_000i16, 10_000i16]).collect();
    enc.push(&interleaved, 2);
    assert_eq!(tx.pump(), 1);
    drain(&mut r.router, 0, 4);

    for _ in 0..TARGET_FRAMES / BLOCK {
        r.mixer.render(BLOCK);
    }
    let want = 20_000.0 / 32768.0;
    for &s in &r.mixer.render(120)[..120] {
        assert!((s - want).abs() < 1e-3, "got {s}, want {want}");
    }
}

#[test]
fn two_microphones_land_on_separate_strips_and_sum() {
    let mut r = rig();
    let (mut a_enc, mut a_tx, _a) = microphone(r.port, 240);
    let (mut b_enc, mut b_tx, _b) = microphone(r.port, 240);
    assert_ne!(a_tx.ssrc(), b_tx.ssrc(), "each session picks its own ssrc");

    for _ in 0..6 {
        a_enc.push(&[8000i16; 240], 1);
        b_enc.push(&[8000i16; 240], 1);
    }
    a_tx.pump();
    b_tx.pump();
    drain(&mut r.router, 0, 32);

    for _ in 0..TARGET_FRAMES / BLOCK {
        r.mixer.render(BLOCK);
    }
    assert_eq!(r.table.active_sources(), 2);
    let want = 2.0 * 8000.0 / 32768.0;
    assert!((r.mixer.render(BLOCK)[0] - want).abs() < 1e-3);

    // Muting one strip halves the bus without disturbing the other.
    r.table.set_source_muted(a_tx.ssrc(), true);
    assert!((r.mixer.render(BLOCK)[0] - want / 2.0).abs() < 1e-3);
}

#[test]
fn hello_lights_a_strip_up_and_bye_drops_it() {
    let mut r = rig();
    let (_enc, tx, _shared) = microphone(r.port, 240);

    tx.send_control(PacketType::Hello, 3);
    let seen = drain(&mut r.router, 0, 8);
    assert!(seen.iter().any(|p| *p == Polled::Hello(tx.ssrc())));
    r.mixer.render(BLOCK);
    assert_eq!(r.table.active_sources(), 1);

    tx.send_control(PacketType::Bye, 3);
    let seen = drain(&mut r.router, 0, 8);
    assert!(seen.iter().any(|p| *p == Polled::Bye(tx.ssrc())));
    r.mixer.render(BLOCK);
    assert_eq!(r.table.active_sources(), 0);
}

#[test]
fn a_source_that_stops_talking_is_reaped() {
    let mut r = rig();
    let (_enc, tx, _shared) = microphone(r.port, 240);
    tx.send_control(PacketType::Hello, 1);
    drain(&mut r.router, 1_000, 4);
    r.mixer.render(BLOCK);
    assert_eq!(r.table.active_sources(), 1);

    // Nothing arrives, and the clock moves past the timeout.
    r.router.poll(1_000 + receiver::SOURCE_TIMEOUT_MS + 600);
    r.mixer.render(BLOCK);
    assert_eq!(r.table.active_sources(), 0);
}

#[test]
fn a_muted_microphone_holds_its_strip_but_sends_silence() {
    let mut r = rig();
    let (mut enc, mut tx, shared) = microphone(r.port, 240);
    shared.set_muted(true);

    for _ in 0..6 {
        enc.push(&[20_000i16; 240], 1);
    }
    tx.pump();
    drain(&mut r.router, 0, 16);

    for _ in 0..TARGET_FRAMES / BLOCK + 4 {
        assert!(r.mixer.render(BLOCK).iter().all(|s| s.abs() < 1e-6));
    }
    assert_eq!(r.table.active_sources(), 1, "the strip stays lit");
}

#[test]
fn rubbish_on_the_audio_port_is_counted_and_ignored() {
    let mut r = rig();
    let junk = lanmic::net::open_sender("127.0.0.1", r.port).unwrap();

    junk.send(b"hello?").unwrap();
    junk.send(&[0u8; 64]).unwrap();
    // LAU1 with a payload of zero frames.
    junk.send(&Header::new(PacketType::Audio).to_bytes())
        .unwrap();
    // LAU1 claiming a channel count this version does not speak.
    let mut stereo = Header::new(PacketType::Audio);
    stereo.channels = 2;
    let mut pkt = stereo.to_bytes().to_vec();
    pkt.extend_from_slice(&[0u8; 480]);
    junk.send(&pkt).unwrap();
    // A packet type from some future version.
    let mut future = Header::new(PacketType::Audio);
    future.packet_type = 200;
    junk.send(&future.to_bytes()).unwrap();

    let seen = drain(&mut r.router, 0, 16);
    assert_eq!(seen.len(), 5);
    assert_eq!(
        seen.iter().filter(|p| **p == Polled::Malformed).count(),
        4,
        "{seen:?}"
    );
    assert_eq!(seen.iter().filter(|p| **p == Polled::Ignored).count(), 1);

    let stats = r.rx_shared.stats(&r.table);
    assert_eq!(stats.bad_packets, 4);
    assert_eq!(r.table.active_sources(), 0);
    r.mixer.render(BLOCK);
    assert_eq!(r.table.active_sources(), 0);
}

#[test]
fn a_ninth_microphone_is_refused_and_the_other_eight_carry_on() {
    let mut r = rig();
    let mics: Vec<_> = (0..MAX_SOURCES + 1)
        .map(|_| microphone(r.port, 240))
        .collect();
    for (_, tx, _) in &mics {
        tx.send_control(PacketType::Hello, 1);
    }
    let seen = drain(&mut r.router, 0, 32);
    assert_eq!(
        seen.iter()
            .filter(|p| matches!(p, Polled::Hello(_)))
            .count(),
        MAX_SOURCES
    );
    assert_eq!(
        seen.iter()
            .filter(|p| matches!(p, Polled::TableFull(_)))
            .count(),
        1
    );
    r.mixer.render(BLOCK);
    assert_eq!(r.table.active_sources(), MAX_SOURCES as u32);
}

#[test]
fn frames_the_capture_ring_could_not_take_become_a_gap_not_a_splice() {
    // A plain socket, so the packet headers are visible.
    let sink = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    sink.set_read_timeout(Some(std::time::Duration::from_millis(150)))
        .unwrap();
    let (mut enc, mut tx, shared) = microphone(sink.local_addr().unwrap().port(), 240);

    // Push far more than the half-second capture ring holds without ever
    // draining it: the only way frames are ever lost on this side.
    let burst = vec![1000i16; 4800];
    let (mut offered, mut accepted) = (0usize, 0usize);
    for _ in 0..8 {
        offered += burst.len();
        accepted += enc.push(&burst, 1);
    }
    let dropped = offered - accepted;
    assert!(dropped > 0, "the ring should have overflowed");
    assert_eq!(shared.stats().frames_dropped, dropped as u64);

    tx.pump();
    let mut buf = [0u8; MAX_PACKET_BYTES];
    let mut headers = Vec::new();
    while let Ok((n, _)) = sink.recv_from(&mut buf) {
        headers.push(Header::decode(&buf[..n]).unwrap());
    }
    assert!(headers.len() > 1, "got {} packets", headers.len());

    // The invariant that matters: the timeline the receiver reconstructs is as
    // long as the audio that was captured, hole included. If the drop were
    // simply skipped, the stream would be short by exactly `dropped` frames and
    // would run that much early for the rest of the session.
    const FPP: u32 = 240;
    let timeline = headers.last().unwrap().timestamp.wrapping_add(FPP);
    assert_eq!(timeline, headers.len() as u32 * FPP + dropped as u32);

    // Every step but the one carrying the hole is exactly one packet. (Here the
    // whole hole lands on the first packet because nothing was pumped while it
    // opened; in the running app the sender is draining continuously, so it
    // lands within a packet of where it happened.)
    let steps: Vec<u32> = headers
        .windows(2)
        .map(|w| w[1].timestamp.wrapping_sub(w[0].timestamp))
        .collect();
    assert!(steps.iter().all(|&s| s == FPP), "{steps:?}");
    assert_eq!(headers[0].timestamp, dropped as u32);
}
