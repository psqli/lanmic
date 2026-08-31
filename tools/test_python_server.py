#!/usr/bin/env python3
"""Exercises the Python server's jitter buffer, mixer and UDP path without
needing an audio device (PortAudio is only touched inside Server.run)."""

import os
import socket
import sys
import threading
import time
import types

import numpy as np

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "server"))
import lan_audio_server as S  # noqa: E402

FAIL = 0


def check(cond, msg):
    global FAIL
    if not cond:
        print(f"  FAIL {msg}")
        FAIL += 1


def args(**kw):
    base = dict(port=46999, discovery_port=46998, jitter=25, blocksize=240,
                device=None, channels=2, gain=1.0, name="test", no_discovery=True,
                list_devices=False)
    base.update(kw)
    return types.SimpleNamespace(**base)


def test_header():
    print("header round-trip")
    h = S.build_header(S.T_AUDIO, 0xDEADBEEF, 42, 4800)
    check(len(h) == S.HEADER, "header is 20 bytes")
    p = S.parse_header(h)
    check(p is not None and p[3] == 0xDEADBEEF and p[4] == 42 and p[5] == 4800,
          "fields survive")
    check(S.parse_header(b"XXXX" + h[4:]) is None, "bad magic rejected")
    check(S.parse_header(b"LAU1") is None, "short packet rejected")


def test_source():
    print("jitter buffer")
    src = S.Source(1, 240)
    src.on_packet(0, 0, np.full(120, 1000, dtype=np.int16), 120, False)
    check(src.w - src.r == 360, "primed with target + payload")
    check(np.all(src.read(120) == 0), "priming silence 1/2")
    check(np.all(src.read(120) == 0), "priming silence 2/2")
    check(np.all(src.read(120) == 1000), "audio in order")

    # gap concealment
    g = S.Source(2, 240)
    g.on_packet(0, 0, np.full(120, 5, dtype=np.int16), 120, False)
    g.on_packet(2, 240, np.full(120, 7, dtype=np.int16), 120, False)
    g.read(120); g.read(120)
    check(np.all(g.read(120) == 5), "packet before the gap")
    check(np.all(g.read(120) == 0), "gap concealed with silence")
    check(np.all(g.read(120) == 7), "packet after the gap")
    check(g.stats.lost == 1, f"one lost packet counted (got {g.stats.lost})")
    check(g.stats.concealed == 120, "120 frames concealed")

    # late duplicate is dropped
    l = S.Source(3, 240)
    l.on_packet(0, 0, np.full(120, 5, dtype=np.int16), 120, False)
    l.on_packet(1, 120, np.full(120, 6, dtype=np.int16), 120, False)
    before = l.w
    l.on_packet(2, 0, np.full(120, 9, dtype=np.int16), 120, False)
    check(l.w == before, "late packet not written")
    check(l.stats.late == 1, "late counted")

    # runaway depth gets trimmed
    t = S.Source(4, 240)
    for i in range(40):
        t.on_packet(i, i * 120, np.full(120, 100 + i, dtype=np.int16), 120, False)
    check(t.w - t.r > 960, "buffer overfilled")
    t.read(120)
    check(t.w - t.r <= 240, "trimmed back to target")
    check(t.stats.trims == 1, "trim counted")

    # underrun re-primes
    u = S.Source(5, 240)
    u.on_packet(0, 0, np.full(120, 3, dtype=np.int16), 120, False)
    for _ in range(10):
        out = u.read(120)
    check(u.stats.underruns > 0, "underrun counted")
    check(np.all(out == 0), "underrun outputs silence")
    u.on_packet(1, 120, np.full(120, 4, dtype=np.int16), 120, False)
    check(u.w - u.r == 360, "re-primed to target")


def test_reprime_depth():
    print("re-prime depth")
    src = S.Source(7, 240)
    for i in range(8):
        src.on_packet(i, i * 120, np.full(120, 3, dtype=np.int16), 120, False)
    check(src.w - src.r == 240 + 8 * 120, "buffer is deep")
    # A jump far beyond max_gap is a sender restart: prime back up to the
    # target, do not stack another target of silence on what is already there.
    src.on_packet(8, 5_000_000, np.full(120, 4, dtype=np.int16), 120, False)
    check(src.w - src.r == 240 + 9 * 120, "re-prime tops up rather than stacks")


def test_full_buffer():
    print("full buffer")
    src = S.Source(9, 240)
    src.on_packet(0, 0, np.full(120, 1, dtype=np.int16), 120, False)
    check(src.write_ts == 120, "cursor advances for a stored packet")

    # Fill the ring without ever reading, so the next write has nowhere to go.
    seq, ts = 1, 120
    while src.cap - (src.w - src.r) >= 120:
        src.on_packet(seq, ts, np.full(120, 2, dtype=np.int16), 120, False)
        seq, ts = seq + 1, ts + 120

    before_w, before_ts = src.w, src.write_ts
    src.on_packet(seq, ts, np.full(120, 3, dtype=np.int16), 120, False)
    check(src.w == before_w, "full ring drops the payload")
    # Leaving write_ts put turns the drop into a timestamp gap that the next
    # packet conceals, which is what the C++ receiver does.
    check(src.write_ts == before_ts, "write cursor stays put after a drop")


def test_limiter():
    print("limiter")
    lim = S.Limiter()
    hot = np.full(4800, 3.0, dtype=np.float32)
    out = lim.process(hot)
    # Instant attack means the very first block is already under the ceiling.
    check(float(np.max(np.abs(out))) <= 0.9801, "first block already limited")
    out2 = lim.process(np.full(4800, 3.0, dtype=np.float32))
    check(float(np.max(np.abs(out2))) <= 0.9801, "stays below ceiling")
    for _ in range(20):
        lim.process(np.full(4800, 0.05, dtype=np.float32))
    check(lim.gain > 0.9, "released back open")
    # ... and a transient arriving into an open limiter is caught on arrival,
    # not one block later.
    spike = lim.process(np.full(240, 5.0, dtype=np.float32))
    check(float(np.max(np.abs(spike))) <= 0.9801, "transient caught on arrival")


def test_end_to_end():
    print("end-to-end over the wire")
    srv = S.Server(args())
    srv.running = True
    net = threading.Thread(target=srv.net_loop, daemon=True)
    net.start()
    time.sleep(0.2)

    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.connect(("127.0.0.1", 46999))

    ssrc_a, ssrc_b = 0xAAAA0001, 0xBBBB0002
    frames = 120
    packets = 20
    for p in range(packets):
        for ssrc, val in ((ssrc_a, 4000), (ssrc_b, 2000)):
            pcm = np.full(frames, val, dtype="<i2").tobytes()
            sock.send(S.build_header(S.T_AUDIO, ssrc, p, p * frames) + pcm)
        time.sleep(0.002)
    time.sleep(0.4)

    check(len(srv.sources) == 2, f"two sources registered (got {len(srv.sources)})")
    check(srv.packets == packets * 2, f"all packets parsed (got {srv.packets})")
    check(srv.bad == 0, "no malformed packets")

    out = np.zeros((240, 2), dtype=np.float32)
    # 25 ms target = 1200 frames = 5 blocks of priming silence
    for _ in range(5):
        srv.callback(out, 240, None, None)
        check(float(np.max(np.abs(out))) < 1e-6, "priming silence")
    srv.callback(out, 240, None, None)
    want = (4000 + 2000) / 32768.0
    check(abs(float(out[0, 0]) - want) < 1e-3,
          f"sources summed (got {float(out[0, 0]):.4f}, want {want:.4f})")
    check(abs(float(out[0, 0]) - float(out[0, 1])) < 1e-9, "mono duplicated to both channels")

    # per-source mute and gain
    srv.sources[ssrc_b].muted = True
    srv.callback(out, 240, None, None)
    check(abs(float(out[0, 0]) - 4000 / 32768.0) < 1e-3, "mute removes a source")
    srv.sources[ssrc_b].muted = False
    srv.sources[ssrc_a].gain = 0.5
    srv.callback(out, 240, None, None)
    check(abs(float(out[0, 0]) - (2000 + 2000) / 32768.0) < 1e-3, "per-source gain")

    # BYE retires a source
    sock.send(S.build_header(S.T_BYE, ssrc_a, 0, 0))
    time.sleep(0.3)
    check(ssrc_a not in srv.sources, "BYE retired the source")

    # timeout retires the other one
    srv.sources[ssrc_b].last_seen = time.monotonic() - 5
    srv.reap(time.monotonic())
    check(len(srv.sources) == 0, "stale source reaped")

    # garbage in, no crash out
    sock.send(b"not-a-lau1-packet")
    sock.send(b"LAU1" + b"\x00" * 8)
    time.sleep(0.3)
    check(srv.bad >= 1, "malformed packets counted")

    srv.running = False
    net.join(timeout=2)


def test_discovery():
    print("discovery")
    srv = S.Server(args(no_discovery=False))
    srv.running = True
    t = threading.Thread(target=srv.discovery_loop, daemon=True)
    t.start()
    time.sleep(0.2)

    c = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    c.settimeout(1.0)
    c.sendto(S.build_header(S.T_DISCOVER), ("127.0.0.1", 46998))
    try:
        data, addr = c.recvfrom(256)
        h = S.parse_header(data)
        check(h is not None and h[0] == S.T_ANNOUNCE, "ANNOUNCE returned")
        check(data[S.HEADER:].decode() == "test", "server name in payload")
    except socket.timeout:
        check(False, "no discovery reply")
    srv.running = False
    t.join(timeout=2)


test_header()
test_source()
test_reprime_depth()
test_full_buffer()
test_limiter()
test_end_to_end()
test_discovery()

if FAIL:
    print(f"\n{FAIL} checks failed")
    sys.exit(1)
print("\nall tests passed")
