#!/usr/bin/env python3
"""
LAU1 mixing server - the desktop twin of the Android app's server mode.

Receives 48 kHz mono PCM from one or more phones over UDP, runs one jitter
buffer per source, sums them and plays the mix out of the selected audio
device with as little added delay as the OS allows.

    pip install sounddevice numpy
    python3 lan_audio_server.py --list-devices
    python3 lan_audio_server.py --jitter 15 --device 3

Protocol is documented in ../PROTOCOL.md.
"""

from __future__ import annotations

import argparse
import socket
import struct
import sys
import threading
import time
from dataclasses import dataclass, field

import numpy as np

SAMPLE_RATE = 48000
HEADER = 20
MAGIC = b"LAU1"
MAX_FRAMES = 960
MAX_PACKET = HEADER + MAX_FRAMES * 2

T_AUDIO, T_HELLO, T_BYE, T_DISCOVER, T_ANNOUNCE = 0, 1, 2, 3, 4
F_MUTED = 1

SOURCE_TIMEOUT = 2.0

_hdr = struct.Struct("<4sBBBBIII")


def parse_header(data: bytes):
    if len(data) < HEADER:
        return None
    magic, typ, ch, flags, _rsv, ssrc, seq, ts = _hdr.unpack_from(data, 0)
    if magic != MAGIC:
        return None
    return typ, ch, flags, ssrc, seq, ts


def build_header(typ: int, ssrc: int = 0, seq: int = 0, ts: int = 0) -> bytes:
    return _hdr.pack(MAGIC, typ, 1, 0, 0, ssrc, seq, ts)


@dataclass
class Stats:
    packets: int = 0
    lost: int = 0
    late: int = 0
    concealed: int = 0
    underruns: int = 0
    trims: int = 0


class Source:
    """One microphone: a timestamp-aligned ring with a target depth."""

    def __init__(self, ssrc: int, target_frames: int):
        self.ssrc = ssrc
        self.target = target_frames
        self.max_fill = target_frames * 4
        self.max_gap = target_frames * 4
        self.cap = max(target_frames * 8, 8192)
        self.buf = np.zeros(self.cap, dtype=np.int16)
        self.w = 0
        self.r = 0
        self.lock = threading.Lock()
        self.primed = False
        self.write_ts = 0
        self.expected_seq: int | None = None
        self.resync = False
        self.last_seen = time.monotonic()
        self.peak = 0.0
        self.gain = 1.0
        self.muted = False
        self.stats = Stats()

    # ---- writer (network thread), lock held ----

    def _write(self, arr: np.ndarray) -> bool:
        """Append `arr`, or drop it whole. Returns False when it was dropped."""
        n = len(arr)
        if n == 0:
            return True
        free = self.cap - (self.w - self.r)
        if n > free:
            return False  # reader stalled; dropping is better than corrupting
        i = self.w % self.cap
        first = min(n, self.cap - i)
        self.buf[i:i + first] = arr[:first]
        if n > first:
            self.buf[0:n - first] = arr[first:]
        self.w += n
        return True

    def _prime(self, ts: int) -> None:
        self._write(np.zeros(self.target, dtype=np.int16))
        self.write_ts = ts
        self.primed = True

    def on_packet(self, seq: int, ts: int, pcm: np.ndarray | None, frames: int,
                  muted: bool) -> None:
        with self.lock:
            self.stats.packets += 1
            self.last_seen = time.monotonic()

            if self.resync:
                self.resync = False
                self.primed = False

            if self.expected_seq is not None:
                d = (seq - self.expected_seq) & 0xFFFFFFFF
                if 0 < d < 0x80000000:
                    self.stats.lost += d
            self.expected_seq = (seq + 1) & 0xFFFFFFFF

            if not self.primed:
                self._prime(ts)
            else:
                delta = (ts - self.write_ts) & 0xFFFFFFFF
                if delta >= 0x80000000:          # negative: late or duplicate
                    self.stats.late += 1
                    return
                if delta > self.max_gap:
                    self._prime(ts)
                elif delta > 0:
                    self._write(np.zeros(delta, dtype=np.int16))
                    self.stats.concealed += delta

            if frames:
                if muted or pcm is None:
                    written = self._write(np.zeros(frames, dtype=np.int16))
                else:
                    written = self._write(pcm)
                # Only advance the write cursor for audio we actually stored.
                # Leaving it put makes the next packet arrive as a timestamp
                # gap, so the drop is concealed instead of silently shortening
                # the stream - this is what the C++ receiver does.
                if written:
                    self.write_ts = (ts + frames) & 0xFFFFFFFF

    # ---- reader (audio callback) ----

    def read(self, n: int) -> np.ndarray:
        out = np.zeros(n, dtype=np.int16)
        with self.lock:
            avail = self.w - self.r
            if avail > self.max_fill:
                self.r += avail - self.target      # shed accumulated latency
                self.stats.trims += 1
                avail = self.target
            take = min(n, avail)
            if take:
                i = self.r % self.cap
                first = min(take, self.cap - i)
                out[:first] = self.buf[i:i + first]
                if take > first:
                    out[first:take] = self.buf[0:take - first]
                self.r += take
            if take < n:
                self.stats.underruns += 1
                self.resync = True
        return out

    @property
    def fill_ms(self) -> float:
        return (self.w - self.r) * 1000.0 / SAMPLE_RATE


class Limiter:
    """Zero-latency peak limiter with a per-block gain ramp."""

    def __init__(self, ceiling: float = 0.98, release_ms: float = 150.0):
        self.ceiling = ceiling
        self.release_ms = release_ms
        self.gain = 1.0

    def process(self, x: np.ndarray) -> np.ndarray:
        peak = float(np.max(np.abs(x))) if x.size else 0.0
        wanted = self.ceiling / peak if peak > self.ceiling else 1.0
        if wanted < self.gain:
            new_gain = wanted                      # instant attack
        else:
            block_ms = x.size * 1000.0 / SAMPLE_RATE
            a = min(1.0, block_ms / self.release_ms)
            new_gain = self.gain + (wanted - self.gain) * a
        ramp = np.linspace(self.gain, new_gain, x.size, dtype=np.float32)
        self.gain = new_gain
        return x * ramp


class Server:
    def __init__(self, args):
        self.args = args
        self.target = SAMPLE_RATE * args.jitter // 1000
        self.sources: dict[int, Source] = {}
        self.slock = threading.Lock()
        self.limiter = Limiter()
        self.master = args.gain
        self.running = False
        self.packets = 0
        self.bad = 0
        self.underruns = 0
        self.peak = 0.0

    # ---- network ----

    def net_loop(self):
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 1 << 19)
        try:
            sock.setsockopt(socket.IPPROTO_IP, socket.IP_TOS, 0xB8)
        except OSError:
            pass
        sock.bind(("", self.args.port))
        sock.settimeout(0.2)
        last_reap = time.monotonic()

        while self.running:
            try:
                data, _addr = sock.recvfrom(MAX_PACKET)
            except socket.timeout:
                data = None
            except OSError:
                break

            now = time.monotonic()
            if now - last_reap > 0.5:
                self.reap(now)
                last_reap = now
            if not data:
                continue

            h = parse_header(data)
            if h is None:
                self.bad += 1
                continue
            typ, ch, flags, ssrc, seq, ts = h
            self.packets += 1

            if typ == T_BYE:
                with self.slock:
                    if self.sources.pop(ssrc, None) is not None:
                        print(f"\n[-] microphone MIC-{ssrc & 0xFFFF:04X} said goodbye")
                continue
            if typ == T_HELLO:
                self.get_source(ssrc)
                continue
            if typ != T_AUDIO or ch != 1:
                continue

            frames = (len(data) - HEADER) // 2
            if frames <= 0 or frames > MAX_FRAMES:
                self.bad += 1
                continue

            muted = bool(flags & F_MUTED)
            pcm = None if muted else np.frombuffer(
                data, dtype="<i2", count=frames, offset=HEADER
            )
            self.get_source(ssrc).on_packet(seq, ts, pcm, frames, muted)

        sock.close()

    def get_source(self, ssrc: int) -> Source:
        with self.slock:
            s = self.sources.get(ssrc)
            if s is None:
                s = Source(ssrc, self.target)
                self.sources[ssrc] = s
                print(f"\n[+] microphone MIC-{ssrc & 0xFFFF:04X} joined")
            return s

    def reap(self, now: float):
        with self.slock:
            dead = [k for k, v in self.sources.items()
                    if now - v.last_seen > SOURCE_TIMEOUT]
            for k in dead:
                del self.sources[k]
                print(f"\n[-] microphone MIC-{k & 0xFFFF:04X} left")

    # ---- audio ----

    def callback(self, outdata, frames, time_info, status):
        if status:
            self.underruns += 1
        with self.slock:
            srcs = list(self.sources.values())

        mix = np.zeros(frames, dtype=np.float32)
        for s in srcs:
            pcm = s.read(frames).astype(np.float32) * (1.0 / 32768.0)
            if s.muted:
                pcm *= 0.0
            elif s.gain != 1.0:
                pcm *= s.gain
            p = float(np.max(np.abs(pcm))) if pcm.size else 0.0
            s.peak = max(p, s.peak * 0.85)
            mix += pcm

        if self.master != 1.0:
            mix *= self.master
        mix = self.limiter.process(mix)
        self.peak = max(float(np.max(np.abs(mix))) if mix.size else 0.0,
                        self.peak * 0.85)

        if outdata.shape[1] == 1:
            outdata[:, 0] = mix
        else:
            for c in range(outdata.shape[1]):
                outdata[:, c] = mix

    # ---- discovery ----

    def discovery_loop(self):
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
        try:
            sock.bind(("", self.args.discovery_port))
        except OSError as e:
            print(f"discovery disabled: {e}")
            return
        sock.settimeout(0.5)
        reply = build_header(T_ANNOUNCE) + self.args.name.encode()
        while self.running:
            try:
                data, addr = sock.recvfrom(256)
            except socket.timeout:
                continue
            except OSError:
                break
            h = parse_header(data)
            if h and h[0] == T_DISCOVER:
                try:
                    sock.sendto(reply, addr)
                except OSError:
                    pass
        sock.close()

    # ---- console ----

    def status_loop(self):
        bar_w = 20
        while self.running:
            with self.slock:
                srcs = sorted(self.sources.values(), key=lambda s: s.ssrc)
            parts = []
            for s in srcs:
                lvl = int(min(1.0, s.peak) * bar_w)
                parts.append(
                    f"MIC-{s.ssrc & 0xFFFF:04X} [{'#' * lvl}{'.' * (bar_w - lvl)}] "
                    f"{s.fill_ms:4.0f}ms lost:{s.stats.lost} und:{s.stats.underruns}"
                )
            head = (f"sources:{len(srcs)} pkts:{self.packets} "
                    f"lim:{self.limiter.gain:.2f} out-xrun:{self.underruns}")
            sys.stdout.write("\r\033[K" + head + ("  |  " + "  |  ".join(parts)
                                                  if parts else ""))
            sys.stdout.flush()
            time.sleep(0.25)

    def run(self):
        import sounddevice as sd

        self.running = True
        threading.Thread(target=self.net_loop, daemon=True).start()
        if not self.args.no_discovery:
            threading.Thread(target=self.discovery_loop, daemon=True).start()
        threading.Thread(target=self.status_loop, daemon=True).start()

        print(f"LAU1 server '{self.args.name}' on udp/{self.args.port}, "
              f"jitter {self.args.jitter} ms, blocksize {self.args.blocksize}")
        print("local addresses:", ", ".join(local_addresses()))

        try:
            with sd.OutputStream(
                samplerate=SAMPLE_RATE,
                blocksize=self.args.blocksize,
                device=self.args.device,
                channels=self.args.channels,
                dtype="float32",
                latency="low",
                callback=self.callback,
            ) as stream:
                print(f"output latency: {stream.latency * 1000:.1f} ms  "
                      f"(Ctrl-C to stop)")
                while True:
                    time.sleep(0.5)
        except KeyboardInterrupt:
            pass
        finally:
            self.running = False
            print("\nstopped")


def local_addresses() -> list[str]:
    addrs = set()
    try:
        for info in socket.getaddrinfo(socket.gethostname(), None, socket.AF_INET):
            addrs.add(info[4][0])
    except OSError:
        pass
    try:
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        s.connect(("8.8.8.8", 53))
        addrs.add(s.getsockname()[0])
        s.close()
    except OSError:
        pass
    return sorted(a for a in addrs if not a.startswith("127."))


def main():
    p = argparse.ArgumentParser(description="LAU1 LAN audio mixing server")
    p.add_argument("--port", type=int, default=45678)
    p.add_argument("--discovery-port", type=int, default=45679)
    p.add_argument("--jitter", type=int, default=15,
                   help="jitter buffer target in ms (default 15)")
    p.add_argument("--blocksize", type=int, default=240,
                   help="output block in frames; 240 = 5 ms (default)")
    p.add_argument("--device", default=None,
                   help="output device index or name substring")
    p.add_argument("--channels", type=int, default=2)
    p.add_argument("--gain", type=float, default=1.0)
    p.add_argument("--name", default=socket.gethostname())
    p.add_argument("--no-discovery", action="store_true")
    p.add_argument("--list-devices", action="store_true")
    args = p.parse_args()

    if args.list_devices:
        import sounddevice as sd
        print(sd.query_devices())
        return

    if args.device is not None:
        try:
            args.device = int(args.device)
        except ValueError:
            pass

    Server(args).run()


if __name__ == "__main__":
    main()
