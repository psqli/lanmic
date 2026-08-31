//! The whole JNI surface. Two engines behind two mutexes, and nothing else.
//!
//! Everything here is a boundary: values arriving from Kotlin are validated
//! before they reach the engine, and nothing that can fail is allowed to
//! unwind back across it.

use std::borrow::Cow;
use std::sync::{Mutex, MutexGuard, TryLockError};

use jni::objects::{JObject, JString};
use jni::sys::{jboolean, jdoubleArray, jfloat, jint, jlong, jsize, JNI_FALSE, JNI_TRUE};
use jni::JNIEnv;

use crate::android::{Receiver, Transmitter};
use crate::mixer::SourceSnapshot;
use crate::net::validate_port;
use crate::protocol::DEFAULT_AUDIO_PORT;
use crate::util::now_ms;

static TX: Mutex<Option<Transmitter>> = Mutex::new(None);
static RX: Mutex<Option<Receiver>> = Mutex::new(None);

/// A poisoned engine mutex means some other call panicked while holding it.
/// The engine behind it is still a valid object, and refusing to ever start
/// audio again would be a worse outcome than carrying on.
fn lock<T>(m: &'static Mutex<T>) -> MutexGuard<'static, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Stats are polled about twelve times a second while a start or stop may be
/// holding the lock through a stream open. Blocking there would jank the UI,
/// so a poll that cannot get in reports the zero value for that frame.
fn try_lock<T>(m: &'static Mutex<T>) -> Option<MutexGuard<'static, T>> {
    match m.try_lock() {
        Ok(g) => Some(g),
        Err(TryLockError::Poisoned(e)) => Some(e.into_inner()),
        Err(TryLockError::WouldBlock) => None,
    }
}

fn init_logging() {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("lanmic"),
    );
}

fn rust_string(env: &mut JNIEnv, s: &JString) -> String {
    match env.get_string(s) {
        Ok(js) => Cow::from(&*js).into_owned(),
        Err(_) => String::new(),
    }
}

/// Kotlin sees these as `DoubleArray?`; null is only reachable if the VM
/// cannot allocate, at which point there is nothing useful to report anyway.
fn doubles(env: &mut JNIEnv, values: &[f64]) -> jdoubleArray {
    let Ok(array) = env.new_double_array(values.len() as jsize) else {
        return std::ptr::null_mut();
    };
    if env.set_double_array_region(&array, 0, values).is_err() {
        return std::ptr::null_mut();
    }
    array.into_raw()
}

fn jbool(v: bool) -> jboolean {
    if v {
        JNI_TRUE
    } else {
        JNI_FALSE
    }
}

// ---------------------------------------------------------------------------
// Transmitter
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "system" fn Java_com_lanmic_audio_NativeAudio_nativeStartTransmitter<'local>(
    mut env: JNIEnv<'local>,
    _this: JObject<'local>,
    host: JString<'local>,
    port: jint,
    frames_per_packet: jint,
    input_preset: jint,
) -> jboolean {
    init_logging();
    let host = rust_string(&mut env, &host);
    let port = match validate_port(port) {
        Ok(p) => p,
        Err(e) => {
            log::error!("refusing to transmit: {e}");
            return JNI_FALSE;
        }
    };

    let mut engine = lock(&TX);
    if engine.as_ref().is_some_and(|t| t.is_running()) {
        return JNI_TRUE;
    }
    // Retire any previous session first: dropping joins its threads, and two
    // engines must never hold the microphone at once.
    *engine = None;

    match Transmitter::start(&host, port, frames_per_packet.max(0) as usize, input_preset) {
        Ok(t) => {
            *engine = Some(t);
            JNI_TRUE
        }
        Err(e) => {
            log::error!("transmitter failed to start: {e}");
            JNI_FALSE
        }
    }
}

#[no_mangle]
pub extern "system" fn Java_com_lanmic_audio_NativeAudio_nativeStopTransmitter(
    _env: JNIEnv,
    _this: JObject,
) {
    *lock(&TX) = None;
}

#[no_mangle]
pub extern "system" fn Java_com_lanmic_audio_NativeAudio_nativeIsTransmitting(
    _env: JNIEnv,
    _this: JObject,
) -> jboolean {
    jbool(try_lock(&TX).is_some_and(|e| e.as_ref().is_some_and(|t| t.is_running())))
}

#[no_mangle]
pub extern "system" fn Java_com_lanmic_audio_NativeAudio_nativeSetTxGain(
    _env: JNIEnv,
    _this: JObject,
    gain: jfloat,
) {
    if let Some(engine) = try_lock(&TX) {
        if let Some(t) = engine.as_ref() {
            t.shared().set_gain(gain);
        }
    }
}

#[no_mangle]
pub extern "system" fn Java_com_lanmic_audio_NativeAudio_nativeSetTxMuted(
    _env: JNIEnv,
    _this: JObject,
    muted: jboolean,
) {
    if let Some(engine) = try_lock(&TX) {
        if let Some(t) = engine.as_ref() {
            t.shared().set_muted(muted != JNI_FALSE);
        }
    }
}

/// `[packetsSent, framesDropped, sendErrors, xruns, peak, latencyMs, running]`
#[no_mangle]
pub extern "system" fn Java_com_lanmic_audio_NativeAudio_nativeTxStats<'local>(
    mut env: JNIEnv<'local>,
    _this: JObject<'local>,
) -> jdoubleArray {
    let s = try_lock(&TX)
        .and_then(|e| e.as_ref().map(|t| t.stats()))
        .unwrap_or_default();
    doubles(
        &mut env,
        &[
            s.packets_sent as f64,
            s.frames_dropped as f64,
            s.send_errors as f64,
            s.xruns as f64,
            s.peak as f64,
            s.latency_ms as f64,
            s.running as u8 as f64,
        ],
    )
}

// ---------------------------------------------------------------------------
// Receiver
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "system" fn Java_com_lanmic_audio_NativeAudio_nativeStartServer(
    _env: JNIEnv,
    _this: JObject,
    port: jint,
    jitter_ms: jint,
) -> jboolean {
    init_logging();
    let port = match validate_port(port) {
        Ok(p) => p,
        Err(e) => {
            log::error!("refusing to listen: {e}");
            return JNI_FALSE;
        }
    };

    let mut engine = lock(&RX);
    if engine.as_ref().is_some_and(|r| r.is_running()) {
        return JNI_TRUE;
    }
    // Drop the old one before binding: it still holds the port.
    *engine = None;

    match Receiver::start(port, jitter_ms) {
        Ok(r) => {
            *engine = Some(r);
            JNI_TRUE
        }
        Err(e) => {
            log::error!("server failed to start on port {port}: {e}");
            JNI_FALSE
        }
    }
}

#[no_mangle]
pub extern "system" fn Java_com_lanmic_audio_NativeAudio_nativeStopServer(
    _env: JNIEnv,
    _this: JObject,
) {
    *lock(&RX) = None;
}

#[no_mangle]
pub extern "system" fn Java_com_lanmic_audio_NativeAudio_nativeIsServing(
    _env: JNIEnv,
    _this: JObject,
) -> jboolean {
    jbool(try_lock(&RX).is_some_and(|e| e.as_ref().is_some_and(|r| r.is_running())))
}

#[no_mangle]
pub extern "system" fn Java_com_lanmic_audio_NativeAudio_nativeSetMasterGain(
    _env: JNIEnv,
    _this: JObject,
    gain: jfloat,
) {
    if let Some(engine) = try_lock(&RX) {
        if let Some(r) = engine.as_ref() {
            r.table().set_master_gain(gain);
        }
    }
}

#[no_mangle]
pub extern "system" fn Java_com_lanmic_audio_NativeAudio_nativeSetSourceGain(
    _env: JNIEnv,
    _this: JObject,
    ssrc: jlong,
    gain: jfloat,
) {
    if let Some(engine) = try_lock(&RX) {
        if let Some(r) = engine.as_ref() {
            r.table().set_source_gain(ssrc as u32, gain);
        }
    }
}

#[no_mangle]
pub extern "system" fn Java_com_lanmic_audio_NativeAudio_nativeSetSourceMuted(
    _env: JNIEnv,
    _this: JObject,
    ssrc: jlong,
    muted: jboolean,
) {
    if let Some(engine) = try_lock(&RX) {
        if let Some(r) = engine.as_ref() {
            r.table().set_source_muted(ssrc as u32, muted != JNI_FALSE);
        }
    }
}

/// `[packets, badPackets, activeSources, xruns, masterPeak, limiterGain,
/// latencyMs, running]`
#[no_mangle]
pub extern "system" fn Java_com_lanmic_audio_NativeAudio_nativeServerStats<'local>(
    mut env: JNIEnv<'local>,
    _this: JObject<'local>,
) -> jdoubleArray {
    let s = try_lock(&RX)
        .and_then(|e| e.as_ref().map(|r| r.stats()))
        .unwrap_or_default();
    doubles(
        &mut env,
        &[
            s.packets as f64,
            s.bad_packets as f64,
            s.active_sources as f64,
            s.xruns as f64,
            s.master_peak as f64,
            s.limiter_gain as f64,
            s.latency_ms as f64,
            s.running as u8 as f64,
        ],
    )
}

/// Nine doubles per source:
/// `[ssrc, peak, bufferFrames, packets, lost, underruns, ageMs, muted, gain]`
#[no_mangle]
pub extern "system" fn Java_com_lanmic_audio_NativeAudio_nativeServerSources<'local>(
    mut env: JNIEnv<'local>,
    _this: JObject<'local>,
) -> jdoubleArray {
    let mut snaps: Vec<SourceSnapshot> = Vec::new();
    if let Some(engine) = try_lock(&RX) {
        if let Some(r) = engine.as_ref() {
            r.table().snapshot(now_ms(), &mut snaps);
        }
    }
    let mut flat = Vec::with_capacity(snaps.len() * 9);
    for s in &snaps {
        flat.extend_from_slice(&[
            s.ssrc as f64,
            s.peak_milli as f64 / 1000.0,
            s.buffer_frames as f64,
            s.packets as f64,
            s.lost as f64,
            s.underruns as f64,
            s.age_ms as f64,
            s.muted as u8 as f64,
            s.gain_milli as f64 / 1000.0,
        ]);
    }
    doubles(&mut env, &flat)
}

/// Kept in sync with `NativeAudio.DEFAULT_PORT` on the Kotlin side; a mismatch
/// would be silent, so it is asserted rather than trusted.
#[allow(dead_code)]
const _: () = assert!(DEFAULT_AUDIO_PORT == 45678);
