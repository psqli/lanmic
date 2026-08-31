# LAU1 — LAN Audio, version 1

A deliberately dumb, low-latency transport. No handshake, no retransmission, no
congestion control: on a dedicated LAN a lost packet is cheaper to conceal than
to recover.

## Constants

| Parameter        | Value                                   |
|------------------|-----------------------------------------|
| Sample rate      | 48000 Hz (fixed, both ends)             |
| Sample format    | signed 16-bit little-endian             |
| Channels         | 1 (mono) on the wire                    |
| Frames / packet  | 120 / 240 / 480 (2.5 / 5 / 10 ms)       |
| Default port     | 45678/udp (audio)                       |
| Discovery port   | 45679/udp (broadcast)                   |
| Max payload      | 960 samples = 1920 B (+20 B header)     |

Everything fits in one Ethernet/Wi-Fi frame; no IP fragmentation.

## Audio packet

All multi-byte fields little-endian.

```
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|      'L'      |      'A'      |      'U'      |      '1'      |
+---------------+---------------+---------------+---------------+
|     type      |   channels    |     flags     |   reserved    |
+---------------+---------------+---------------+---------------+
|                             ssrc                              |
+---------------------------------------------------------------+
|                              seq                              |
+---------------------------------------------------------------+
|                   timestamp (frame counter)                   |
+---------------------------------------------------------------+
|                        payload (int16 LE) ...                 |
```

* **type** — `0` AUDIO, `1` HELLO, `2` BYE, `3` DISCOVER, `4` ANNOUNCE.
* **channels** — always 1 in v1.
* **flags** — bit 0 `MUTED` (payload is silence, still sent to keep the
  jitter buffer primed and the NAT/ARP path warm).
* **ssrc** — random 32-bit id chosen once per capture session. Identifies a
  microphone; survives IP changes (roaming between APs).
* **seq** — increments by 1 per packet. Used only for loss statistics.
* **timestamp** — index of the first frame in the packet within the sender's
  capture stream. This, not arrival time, is what the jitter buffer aligns on.

`HELLO` is sent 3x at start and `BYE` 3x at stop, both with an empty payload —
purely so the server can light up / drop a channel promptly. Neither is
required for correctness; a source appears on its first AUDIO packet and is
retired after 2 s of silence.

## Discovery

Transmitter broadcasts a `DISCOVER` packet (empty payload, header only) to
`255.255.255.255:45679` and listens on the ephemeral port it sent from. Any
server replies with `ANNOUNCE` **from the discovery port**, addressed back to
the probe's source port; the payload is the UTF-8 display name.

The transmitter takes the source address of the reply as the server address and
assumes audio goes to the default audio port. Manual entry always overrides
discovery, and is the fallback on networks that drop broadcast traffic.

Discovery is entirely optional: it never carries audio, and a transmitter given
an address by hand never sends a `DISCOVER` packet at all.

## Jitter buffer (receiver side, per ssrc)

Each source owns an SPSC ring of int16 frames. The network thread writes, the
audio callback reads.

* First packet primes the ring with `targetFrames` of silence (default 15 ms).
* `timestamp` gaps insert silence (concealment) up to 4x target; a larger gap
  is treated as a restart and re-primes.
* `timestamp` older than the write cursor = late or duplicate, dropped.
* Reader-side trimming: if fill exceeds `maxFill` (target x 4) the audio
  thread discards down to target. This is where accumulated latency from
  clock drift is shed — always by dropping the *oldest* audio.
* Underrun sets a resync flag; the writer then re-primes with `targetFrames`
  of silence so the buffer returns to its target depth instead of hovering at
  zero and clicking on every callback.
* A packet that will not fit is dropped whole and the write cursor stays put,
  so the next packet arrives as a `timestamp` gap and is concealed. This only
  happens when the reader has stalled, at which point the stale audio in the
  ring is worthless anyway.

Sender and receiver clocks are independent and will drift (tens of ppm). Over
a one-hour talk that is a few hundred milliseconds — the trim/re-prime pair
above absorbs it with one inaudible glitch every several minutes rather than
with resampling.

## Latency budget (typical, 48 kHz, 5 ms packets)

| Stage                                | ms        |
|--------------------------------------|-----------|
| Capture (AAudio exclusive, 2 bursts) | 6-10      |
| Packetisation                        | 5         |
| Wi-Fi 5 GHz LAN                      | 1-5       |
| Jitter buffer                        | 15        |
| Mix + playback (AAudio exclusive)    | 6-12      |
| **Total**                            | **33-47** |

Under ~50 ms is the usable range for live reinforcement; below ~25 ms it stops
being perceptible as an echo to the person speaking. Wi-Fi is the variable that
dominates — see README for how to make it behave.
