// LAU1 wire format. Header-only, no platform dependencies, no allocation.
#pragma once

#include <cstddef>
#include <cstdint>
#include <cstring>

namespace lau {

inline constexpr int      kSampleRate         = 48000;
inline constexpr int      kWireChannels       = 1;
inline constexpr size_t   kHeaderBytes        = 20;
inline constexpr int      kMaxFramesPerPacket = 960;   // 20 ms
inline constexpr size_t   kMaxPacketBytes     = kHeaderBytes + kMaxFramesPerPacket * 2;
inline constexpr uint16_t kDefaultAudioPort   = 45678;
inline constexpr uint16_t kDiscoveryPort      = 45679;

enum PacketType : uint8_t {
    kTypeAudio    = 0,
    kTypeHello    = 1,
    kTypeBye      = 2,
    kTypeDiscover = 3,
    kTypeAnnounce = 4,
};

enum PacketFlags : uint8_t {
    kFlagMuted = 1u << 0,
};

struct Header {
    uint8_t  type      = kTypeAudio;
    uint8_t  channels  = 1;
    uint8_t  flags     = 0;
    uint32_t ssrc      = 0;
    uint32_t seq       = 0;
    uint32_t timestamp = 0;
};

inline void put_u32le(uint8_t* p, uint32_t v) {
    p[0] = static_cast<uint8_t>(v);
    p[1] = static_cast<uint8_t>(v >> 8);
    p[2] = static_cast<uint8_t>(v >> 16);
    p[3] = static_cast<uint8_t>(v >> 24);
}

inline uint32_t get_u32le(const uint8_t* p) {
    return static_cast<uint32_t>(p[0]) | (static_cast<uint32_t>(p[1]) << 8) |
           (static_cast<uint32_t>(p[2]) << 16) | (static_cast<uint32_t>(p[3]) << 24);
}

// Returns the number of bytes written (always kHeaderBytes).
inline size_t write_header(uint8_t* buf, const Header& h) {
    buf[0] = 'L'; buf[1] = 'A'; buf[2] = 'U'; buf[3] = '1';
    buf[4] = h.type;
    buf[5] = h.channels;
    buf[6] = h.flags;
    buf[7] = 0;
    put_u32le(buf + 8,  h.ssrc);
    put_u32le(buf + 12, h.seq);
    put_u32le(buf + 16, h.timestamp);
    return kHeaderBytes;
}

inline bool parse_header(const uint8_t* buf, size_t len, Header* out) {
    if (len < kHeaderBytes) return false;
    if (buf[0] != 'L' || buf[1] != 'A' || buf[2] != 'U' || buf[3] != '1') return false;
    out->type      = buf[4];
    out->channels  = buf[5];
    out->flags     = buf[6];
    out->ssrc      = get_u32le(buf + 8);
    out->seq       = get_u32le(buf + 12);
    out->timestamp = get_u32le(buf + 16);
    return true;
}

#if defined(__BYTE_ORDER__) && __BYTE_ORDER__ == __ORDER_LITTLE_ENDIAN__
#define LAU_LITTLE_ENDIAN 1
#endif

inline void pcm_to_wire(uint8_t* dst, const int16_t* src, size_t frames) {
#ifdef LAU_LITTLE_ENDIAN
    std::memcpy(dst, src, frames * 2);
#else
    for (size_t i = 0; i < frames; ++i) {
        uint16_t v = static_cast<uint16_t>(src[i]);
        dst[i * 2]     = static_cast<uint8_t>(v);
        dst[i * 2 + 1] = static_cast<uint8_t>(v >> 8);
    }
#endif
}

inline void wire_to_pcm(int16_t* dst, const uint8_t* src, size_t frames) {
#ifdef LAU_LITTLE_ENDIAN
    std::memcpy(dst, src, frames * 2);
#else
    for (size_t i = 0; i < frames; ++i) {
        dst[i] = static_cast<int16_t>(static_cast<uint16_t>(src[i * 2]) |
                                      (static_cast<uint16_t>(src[i * 2 + 1]) << 8));
    }
#endif
}

}  // namespace lau
