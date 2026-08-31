#!/usr/bin/env python3
"""
A LAU1 transmitter for the desktop - useful for testing the server (either the
Python one or the Android one) without a phone in your hand.

    python3 test_mic_client.py --host 192.168.1.50 --tone 440
    python3 test_mic_client.py --host 192.168.1.50 --mic
"""

from __future__ import annotations

import argparse
import random
import socket
import struct
import time

import numpy as np

SAMPLE_RATE = 48000
HEADER = 20
_hdr = struct.Struct("<4sBBBBIII")


def header(typ, ssrc, seq, ts, flags=0):
    return _hdr.pack(b"LAU1", typ, 1, flags, 0, ssrc, seq & 0xFFFFFFFF, ts & 0xFFFFFFFF)


def discover(port=45679, timeout=1.0):
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
    s.settimeout(0.3)
    probe = header(3, 0, 0, 0)
    found = []
    for _ in range(3):
        try:
            s.sendto(probe, ("255.255.255.255", port))
        except OSError:
            pass
    end = time.time() + timeout
    while time.time() < end:
        try:
            data, addr = s.recvfrom(256)
        except socket.timeout:
            continue
        if data[:4] == b"LAU1" and data[4] == 4:
            name = data[HEADER:].decode(errors="replace") or "server"
            found.append((addr[0], name))
    s.close()
    return found


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--host", default=None)
    p.add_argument("--port", type=int, default=45678)
    p.add_argument("--frames", type=int, default=240, help="frames per packet")
    p.add_argument("--tone", type=float, default=440.0)
    p.add_argument("--amp", type=float, default=0.2)
    p.add_argument("--mic", action="store_true", help="send the local microphone")
    p.add_argument("--seconds", type=float, default=0.0, help="0 = forever")
    args = p.parse_args()

    if not args.host:
        found = discover()
        if not found:
            raise SystemExit("no server found; pass --host")
        args.host = found[0][0]
        print(f"discovered {found[0][1]} at {args.host}")

    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        sock.setsockopt(socket.IPPROTO_IP, socket.IP_TOS, 0xB8)
    except OSError:
        pass
    sock.connect((args.host, args.port))

    ssrc = random.getrandbits(32) | 1
    seq = 0
    ts = 0
    n = args.frames
    period = n / SAMPLE_RATE

    for _ in range(3):
        sock.send(header(1, ssrc, seq, ts))

    print(f"sending to {args.host}:{args.port} ssrc={ssrc:08x} "
          f"{n} frames ({period * 1000:.1f} ms)")

    stream = None
    if args.mic:
        import sounddevice as sd
        stream = sd.InputStream(samplerate=SAMPLE_RATE, channels=1, dtype="int16",
                                blocksize=n, latency="low")
        stream.start()

    phase = 0.0
    dphase = 2 * np.pi * args.tone / SAMPLE_RATE
    start = time.perf_counter()
    next_t = start
    try:
        while True:
            if stream is not None:
                pcm, _overflow = stream.read(n)
                payload = pcm[:, 0].astype("<i2").tobytes()
            else:
                idx = np.arange(n)
                wave = np.sin(phase + dphase * idx) * args.amp * 32767
                phase = (phase + dphase * n) % (2 * np.pi)
                payload = wave.astype("<i2").tobytes()

            sock.send(header(0, ssrc, seq, ts) + payload)
            seq += 1
            ts += n

            next_t += period
            sleep = next_t - time.perf_counter()
            if sleep > 0:
                time.sleep(sleep)
            if args.seconds and time.perf_counter() - start > args.seconds:
                break
    except KeyboardInterrupt:
        pass
    finally:
        for _ in range(3):
            sock.send(header(2, ssrc, seq, ts))
        if stream is not None:
            stream.stop()
        print("\nstopped")


if __name__ == "__main__":
    main()
