//! One thread owns one audio stream, from open to close.
//!
//! Every backend this engine has been wired to - Oboe on a phone, cpal on a
//! desktop - hands its callback a slice and expects the answer immediately, and
//! every one of them can drop a stream underneath you when the route changes:
//! a headset is unplugged, a USB interface is pulled, the phone switches to
//! Bluetooth. Recovering from that means opening a replacement, and opening a
//! replacement while the old one is still live would put two producers on a
//! single-producer ring.
//!
//! So the rule is the one the Android engine grew up with: **exactly one thread
//! ever creates, starts or destroys an audio stream.** [`supervise`] is that
//! thread's body. Nothing else can install a stream, which makes two live
//! streams not a race to be guarded against but a state that cannot be
//! constructed.

use std::io;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// How often the supervisor wakes to refresh stats and notice a dead stream.
pub const SUPERVISOR_TICK: Duration = Duration::from_millis(200);
/// Backoff ceiling for reopening a stream that will not come back.
pub const MAX_REOPEN_BACKOFF: Duration = Duration::from_millis(3200);

/// Opens once, reports whether that worked, then keeps the stream alive until
/// asked to stop.
///
/// `open` is called on this thread and nowhere else, which is the whole point.
/// `publish` runs on every tick with the live stream, for whatever counters the
/// backend exposes. The first open's result goes to `ready` so the caller can
/// fail a `start()` synchronously instead of returning a session that has no
/// audio; later reopens are retried with backoff and only logged.
pub fn supervise<S, O, P>(
    running: impl Fn() -> bool,
    restart_requested: impl Fn() -> bool,
    mut open: O,
    mut publish: P,
    ready: mpsc::Sender<io::Result<()>>,
) where
    O: FnMut() -> io::Result<S>,
    P: FnMut(&mut S),
{
    let mut stream = match open() {
        Ok(s) => {
            let _ = ready.send(Ok(()));
            Some(s)
        }
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };

    let mut backoff = SUPERVISOR_TICK;
    while running() {
        thread::sleep(SUPERVISOR_TICK);
        if !running() {
            break;
        }

        if let Some(s) = stream.as_mut() {
            publish(s);
        }

        if restart_requested() {
            // The backend has already stopped and closed it; letting our handle
            // go is all that is left before reopening.
            stream = None;
        }
        if stream.is_none() {
            match open() {
                Ok(s) => {
                    stream = Some(s);
                    backoff = SUPERVISOR_TICK;
                }
                Err(e) => {
                    // The device that took the route away is often still busy
                    // on the first try, and giving up once would leave a live
                    // session with dead audio.
                    log::warn!("stream did not come back: {e}; retrying");
                    thread::sleep(backoff);
                    backoff = (backoff * 2).min(MAX_REOPEN_BACKOFF);
                }
            }
        }
    }
    // Explicit, so the stream is closed - and its callbacks quiesced - before
    // the state they reach through drops.
    drop(stream);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::Arc;

    /// Stands in for a stream: says when it was closed.
    struct FakeStream(Arc<AtomicU32>);
    impl Drop for FakeStream {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn a_failed_first_open_is_reported_and_gives_up() {
        let (tx, rx) = mpsc::channel();
        let opens = Arc::new(AtomicU32::new(0));
        let n = opens.clone();
        supervise(
            || true,
            || false,
            move || -> io::Result<FakeStream> {
                n.fetch_add(1, Ordering::Relaxed);
                Err(io::Error::other("no device"))
            },
            |_: &mut FakeStream| unreachable!("nothing was opened"),
            tx,
        );
        assert!(rx.recv().unwrap().is_err());
        // One attempt: a start that cannot open is the caller's to report, not
        // something to retry behind their back.
        assert_eq!(opens.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn the_stream_is_closed_when_running_goes_false() {
        let (tx, rx) = mpsc::channel();
        let running = Arc::new(AtomicBool::new(true));
        let closes = Arc::new(AtomicU32::new(0));

        let handle = {
            let running = running.clone();
            let closes = closes.clone();
            thread::spawn(move || {
                supervise(
                    move || running.load(Ordering::Acquire),
                    || false,
                    move || Ok(FakeStream(closes.clone())),
                    |_: &mut FakeStream| {},
                    tx,
                )
            })
        };

        assert!(rx.recv().unwrap().is_ok());
        running.store(false, Ordering::Release);
        handle.join().unwrap();
        assert_eq!(closes.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn a_restart_request_closes_the_old_stream_before_opening_the_new_one() {
        let (tx, rx) = mpsc::channel();
        let running = Arc::new(AtomicBool::new(true));
        let opens = Arc::new(AtomicU32::new(0));
        let closes = Arc::new(AtomicU32::new(0));
        // One restart, then never again.
        let restart = Arc::new(AtomicBool::new(true));

        let handle = {
            let (running, opens, closes, restart) = (
                running.clone(),
                opens.clone(),
                closes.clone(),
                restart.clone(),
            );
            thread::spawn(move || {
                supervise(
                    move || running.load(Ordering::Acquire),
                    move || restart.swap(false, Ordering::AcqRel),
                    move || {
                        opens.fetch_add(1, Ordering::Relaxed);
                        Ok(FakeStream(closes.clone()))
                    },
                    |_: &mut FakeStream| {},
                    tx,
                )
            })
        };

        assert!(rx.recv().unwrap().is_ok());
        // Two ticks is enough for the restart to be seen and acted on.
        thread::sleep(SUPERVISOR_TICK * 3);
        running.store(false, Ordering::Release);
        handle.join().unwrap();

        assert_eq!(opens.load(Ordering::Relaxed), 2, "did not reopen");
        assert_eq!(closes.load(Ordering::Relaxed), 2, "leaked a stream");
    }
}
