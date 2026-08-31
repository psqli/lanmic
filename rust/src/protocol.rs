//! LAU1 wire format. No allocation, no platform calls, no unsafe.
//!
//! The format itself is documented in `PROTOCOL.md`; this module is only the
//! encoder and decoder for it.

pub const SAMPLE_RATE: u32 = 48_000;
pub const WIRE_CHANNELS: u8 = 1;
pub const HEADER_BYTES: usize = 20;
/// 20 ms at 48 kHz. Keeps a full packet inside one Ethernet/Wi-Fi frame.
pub const MAX_FRAMES_PER_PACKET: usize = 960;
pub const MAX_PACKET_BYTES: usize = HEADER_BYTES + MAX_FRAMES_PER_PACKET * 2;
pub const DEFAULT_AUDIO_PORT: u16 = 45678;
pub const DISCOVERY_PORT: u16 = 45679;

const MAGIC: [u8; 4] = *b"LAU1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PacketType {
    Audio = 0,
    Hello = 1,
    Bye = 2,
    Discover = 3,
    Announce = 4,
}

impl PacketType {
    /// Unknown types decode to `None` rather than erroring: a v2 sender adding
    /// a type must not make its audio undecodable to a v1 receiver.
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => Self::Audio,
            1 => Self::Hello,
            2 => Self::Bye,
            3 => Self::Discover,
            4 => Self::Announce,
            _ => return None,
        })
    }
}

/// Bit 0: the payload is silence, still sent to keep the jitter buffer primed
/// and the ARP path warm.
pub const FLAG_MUTED: u8 = 1 << 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub packet_type: u8,
    pub channels: u8,
    pub flags: u8,
    pub ssrc: u32,
    pub seq: u32,
    /// Index of this packet's first frame within the sender's capture stream.
    /// This, not arrival time, is what the jitter buffer aligns on.
    pub timestamp: u32,
}

impl Header {
    pub fn new(packet_type: PacketType) -> Self {
        Self {
            packet_type: packet_type as u8,
            channels: WIRE_CHANNELS,
            flags: 0,
            ssrc: 0,
            seq: 0,
            timestamp: 0,
        }
    }

    pub fn kind(&self) -> Option<PacketType> {
        PacketType::from_u8(self.packet_type)
    }

    pub fn muted(&self) -> bool {
        self.flags & FLAG_MUTED != 0
    }

    pub fn encode(&self, buf: &mut [u8; HEADER_BYTES]) {
        buf[0..4].copy_from_slice(&MAGIC);
        buf[4] = self.packet_type;
        buf[5] = self.channels;
        buf[6] = self.flags;
        buf[7] = 0; // reserved
        buf[8..12].copy_from_slice(&self.ssrc.to_le_bytes());
        buf[12..16].copy_from_slice(&self.seq.to_le_bytes());
        buf[16..20].copy_from_slice(&self.timestamp.to_le_bytes());
    }

    pub fn to_bytes(&self) -> [u8; HEADER_BYTES] {
        let mut buf = [0u8; HEADER_BYTES];
        self.encode(&mut buf);
        buf
    }

    /// `None` for anything too short or not carrying the magic.
    pub fn decode(buf: &[u8]) -> Option<Self> {
        if buf.len() < HEADER_BYTES || buf[0..4] != MAGIC {
            return None;
        }
        Some(Self {
            packet_type: buf[4],
            channels: buf[5],
            flags: buf[6],
            ssrc: u32::from_le_bytes(buf[8..12].try_into().ok()?),
            seq: u32::from_le_bytes(buf[12..16].try_into().ok()?),
            timestamp: u32::from_le_bytes(buf[16..20].try_into().ok()?),
        })
    }
}

/// Little-endian int16 samples to bytes. `dst` must hold `2 * src.len()`.
pub fn pcm_to_wire(dst: &mut [u8], src: &[i16]) {
    debug_assert!(dst.len() >= src.len() * 2);
    for (chunk, &s) in dst.chunks_exact_mut(2).zip(src) {
        chunk.copy_from_slice(&s.to_le_bytes());
    }
}

/// Bytes to little-endian int16 samples. Decodes `min(dst.len(), src.len()/2)`
/// frames and returns how many.
pub fn wire_to_pcm(dst: &mut [i16], src: &[u8]) -> usize {
    let n = dst.len().min(src.len() / 2);
    for (d, chunk) in dst[..n].iter_mut().zip(src.chunks_exact(2)) {
        *d = i16::from_le_bytes([chunk[0], chunk[1]]);
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trip() {
        let h = Header {
            packet_type: PacketType::Audio as u8,
            channels: 1,
            flags: FLAG_MUTED,
            ssrc: 0xDEAD_BEEF,
            seq: 123_456,
            timestamp: 0xFFFF_FF00,
        };
        let bytes = h.to_bytes();
        assert_eq!(bytes.len(), HEADER_BYTES);
        assert_eq!(Header::decode(&bytes), Some(h));
        assert!(Header::decode(&bytes).unwrap().muted());
    }

    #[test]
    fn bad_magic_and_short_packets_are_rejected() {
        let mut bytes = Header::new(PacketType::Audio).to_bytes();
        bytes[1] = b'X';
        assert_eq!(Header::decode(&bytes), None);
        bytes[1] = b'A';
        assert!(Header::decode(&bytes).is_some());
        assert_eq!(Header::decode(&bytes[..HEADER_BYTES - 1]), None);
        assert_eq!(Header::decode(&[]), None);
    }

    #[test]
    fn unknown_types_decode_but_do_not_classify() {
        let mut h = Header::new(PacketType::Audio);
        h.packet_type = 99;
        let back = Header::decode(&h.to_bytes()).unwrap();
        assert_eq!(back.packet_type, 99);
        assert_eq!(back.kind(), None);
    }

    #[test]
    fn pcm_survives_the_wire_including_the_rails() {
        let pcm = [0i16, 32767, -32768, -1];
        let mut wire = [0u8; 8];
        pcm_to_wire(&mut wire, &pcm);
        assert_eq!(wire[2], 0xFF);
        assert_eq!(wire[3], 0x7F);
        let mut back = [0i16; 4];
        assert_eq!(wire_to_pcm(&mut back, &wire), 4);
        assert_eq!(back, pcm);
    }

    #[test]
    fn wire_to_pcm_clamps_to_the_shorter_side() {
        let mut back = [7i16; 4];
        assert_eq!(wire_to_pcm(&mut back, &[1, 0, 2, 0]), 2);
        assert_eq!(back, [1, 2, 7, 7]);
    }
}
