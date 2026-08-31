// Host-side test for the platform-independent half of the audio core.
//   g++ -std=c++17 -O2 -Wall -Wextra -I../app/src/main/cpp
//       host_test.cpp ../app/src/main/cpp/udp_socket.cpp -o host_test -lpthread
#include <cassert>
#include <cmath>
#include <cstdio>
#include <cstring>
#include <thread>
#include <vector>

#include "jitter_buffer.h"
#include "mixer.h"
#include "protocol.h"
#include "spsc_ring.h"
#include "udp_socket.h"

using namespace lau;

static int gFailures = 0;
#define CHECK(cond)                                                            \
    do {                                                                       \
        if (!(cond)) {                                                         \
            printf("  FAIL %s:%d  %s\n", __FILE__, __LINE__, #cond);           \
            ++gFailures;                                                       \
        }                                                                      \
    } while (0)

static void testHeader() {
    printf("header round-trip\n");
    uint8_t buf[kMaxPacketBytes];
    Header h;
    h.type = kTypeAudio;
    h.channels = 1;
    h.flags = kFlagMuted;
    h.ssrc = 0xDEADBEEF;
    h.seq = 123456;
    h.timestamp = 0xFFFFFF00;
    CHECK(write_header(buf, h) == kHeaderBytes);

    Header out;
    CHECK(parse_header(buf, kHeaderBytes, &out));
    CHECK(out.type == h.type && out.channels == 1 && out.flags == kFlagMuted);
    CHECK(out.ssrc == h.ssrc && out.seq == h.seq && out.timestamp == h.timestamp);

    buf[1] = 'X';
    CHECK(!parse_header(buf, kHeaderBytes, &out));
    buf[1] = 'A';
    CHECK(!parse_header(buf, kHeaderBytes - 1, &out));

    int16_t pcm[4] = {0, 32767, -32768, -1};
    int16_t back[4] = {};
    pcm_to_wire(buf + kHeaderBytes, pcm, 4);
    CHECK(buf[kHeaderBytes + 2] == 0xFF && buf[kHeaderBytes + 3] == 0x7F);
    wire_to_pcm(back, buf + kHeaderBytes, 4);
    CHECK(memcmp(pcm, back, sizeof(pcm)) == 0);
}

static void testRing() {
    printf("spsc ring\n");
    SpscRing<int16_t> r;
    r.init(1000);  // rounds up to 1024
    CHECK(r.capacity() == 1024);
    CHECK(r.size() == 0 && r.space() == 1024);

    std::vector<int16_t> in(700), out(700);
    for (size_t i = 0; i < in.size(); ++i) in[i] = static_cast<int16_t>(i);

    CHECK(r.write(in.data(), 700) == 700);
    CHECK(r.size() == 700);
    CHECK(r.read(out.data(), 700) == 700);
    CHECK(memcmp(in.data(), out.data(), 700 * 2) == 0);

    // force a wrap
    CHECK(r.write(in.data(), 700) == 700);
    CHECK(r.read(out.data(), 700) == 700);
    CHECK(memcmp(in.data(), out.data(), 700 * 2) == 0);

    CHECK(r.write(in.data(), 700) == 700);
    CHECK(r.write(in.data(), 700) == 324);   // clipped to free space
    CHECK(r.size() == 1024);
    CHECK(r.skip(24) == 24);
    CHECK(r.size() == 1000);
    CHECK(r.read(out.data(), 700) == 700);
    CHECK(out[0] == 24);

    // producer/consumer under contention
    SpscRing<int16_t> c;
    c.init(4096);
    constexpr size_t kTotal = 512000;  // exact multiple of the 128-frame block
    std::thread prod([&] {
        size_t sent = 0;
        int16_t blk[128];
        while (sent < kTotal) {
            for (int i = 0; i < 128; ++i) blk[i] = static_cast<int16_t>((sent + i) & 0x7FFF);
            size_t n = c.write(blk, 128);
            sent += n;
            if (n == 0) std::this_thread::yield();
        }
    });
    size_t got = 0;
    bool ordered = true;
    int16_t rb[128];
    while (got < kTotal) {
        size_t n = c.read(rb, 128);
        for (size_t i = 0; i < n; ++i) {
            if (rb[i] != static_cast<int16_t>((got + i) & 0x7FFF)) ordered = false;
        }
        got += n;
        if (n == 0) std::this_thread::yield();
    }
    prod.join();
    CHECK(ordered);
    CHECK(got == kTotal);
}

static void feed(JitterBuffer& jb, uint32_t seq, uint32_t ts, int16_t value, size_t n) {
    std::vector<int16_t> pcm(n, value);
    jb.onPacket(seq, ts, pcm.data(), n, false);
}

static void testJitter() {
    printf("jitter buffer\n");
    const size_t kTarget = 240;  // 5 ms
    JitterBuffer jb;
    jb.configure(static_cast<int>(kTarget));

    // priming: first packet leaves exactly target frames of silence in front
    feed(jb, 0, 0, 1000, 120);
    CHECK(jb.fill() == kTarget + 120);

    std::vector<int16_t> out(120);
    jb.read(out.data(), 120);
    CHECK(out[0] == 0 && out[119] == 0);          // the priming silence
    jb.read(out.data(), 120);
    CHECK(out[0] == 0);
    CHECK(jb.fill() == 120);
    jb.read(out.data(), 120);
    CHECK(out[0] == 1000 && out[119] == 1000);    // the audio, in order

    // in-order stream keeps its depth
    JitterBuffer j2;
    j2.configure(static_cast<int>(kTarget));
    for (uint32_t i = 0; i < 20; ++i) feed(j2, i, i * 120, static_cast<int16_t>(i + 1), 120);
    CHECK(j2.fill() == kTarget + 20 * 120);

    // gap: a missing packet becomes exactly one packet of silence
    JitterBuffer j3;
    j3.configure(static_cast<int>(kTarget));
    feed(j3, 0, 0, 5, 120);
    feed(j3, 2, 240, 7, 120);   // packet 1 (ts=120) never arrived
    j3.read(out.data(), 120);   // priming silence, first half
    j3.read(out.data(), 120);   // priming silence, second half
    CHECK(out[0] == 0);
    j3.read(out.data(), 120);
    CHECK(out[0] == 5);
    j3.read(out.data(), 120);
    CHECK(out[0] == 0);         // concealed
    j3.read(out.data(), 120);
    CHECK(out[0] == 7);
    JitterStats s3 = j3.stats();
    CHECK(s3.lost == 1);
    CHECK(s3.concealed == 120);

    // a late duplicate is dropped, not played
    JitterBuffer j4;
    j4.configure(static_cast<int>(kTarget));
    feed(j4, 0, 0, 5, 120);
    feed(j4, 1, 120, 6, 120);
    const size_t before = j4.fill();
    feed(j4, 2, 0, 9, 120);     // arrives after its slot was written
    CHECK(j4.fill() == before);
    CHECK(j4.stats().late == 1);

    // runaway buffer gets trimmed back to target, oldest audio first
    JitterBuffer j5;
    j5.configure(static_cast<int>(kTarget));
    for (uint32_t i = 0; i < 40; ++i)
        feed(j5, i, i * 120, static_cast<int16_t>(100 + i), 120);
    CHECK(j5.fill() > kTarget * 4);
    j5.read(out.data(), 120);
    CHECK(j5.fill() <= kTarget);
    CHECK(j5.stats().trims == 1);

    // underrun conceals with silence and re-primes on the next packet
    JitterBuffer j6;
    j6.configure(static_cast<int>(kTarget));
    feed(j6, 0, 0, 3, 120);
    for (int i = 0; i < 10; ++i) j6.read(out.data(), 120);
    CHECK(j6.stats().underruns > 0);
    CHECK(out[0] == 0);
    feed(j6, 1, 120, 4, 120);
    CHECK(j6.fill() == kTarget + 120);   // re-primed
}

static void testMixer() {
    printf("mixer + limiter\n");
    SourceTable t;
    t.configure(480);   // 10 ms target -> 40 ms ceiling, room for this test

    JitterBuffer* a = t.acquire(0xAAAA, 0);
    JitterBuffer* b = t.acquire(0xBBBB, 0);
    CHECK(a != nullptr && b != nullptr);
    CHECK(t.acquire(0xAAAA, 0) == a);   // same ssrc -> same buffer

    // drain each buffer's priming silence
    std::vector<int16_t> pcm(240, 8000);
    for (uint32_t i = 0; i < 5; ++i) {
        a->onPacket(i, i * 240, pcm.data(), 240, false);
        b->onPacket(i, i * 240, pcm.data(), 240, false);
    }
    std::vector<float>   mix(240);
    std::vector<int16_t> scratch(240);
    CHECK(t.mix(mix.data(), 240, scratch.data()) == 2);
    CHECK(std::fabs(mix[0]) < 1e-6f);   // priming silence, first half
    t.mix(mix.data(), 240, scratch.data());
    CHECK(std::fabs(mix[0]) < 1e-6f);   // priming silence, second half

    t.mix(mix.data(), 240, scratch.data());
    const float expect = 2.0f * 8000.0f / 32768.0f;
    CHECK(std::fabs(mix[0] - expect) < 1e-3f);

    t.setMuted(0xBBBB, true);
    t.mix(mix.data(), 240, scratch.data());
    CHECK(std::fabs(mix[0] - expect / 2.0f) < 1e-3f);

    t.setMuted(0xBBBB, false);
    t.setGain(0xAAAA, 500);   // 0.5x
    t.mix(mix.data(), 240, scratch.data());
    CHECK(std::fabs(mix[0] - expect * 0.75f) < 1e-3f);

    t.retire(0xBBBB);
    CHECK(t.mix(mix.data(), 240, scratch.data()) == 1);

    SourceSnapshot snaps[kMaxSources];
    CHECK(t.snapshot(snaps, kMaxSources, 10) == 1);
    CHECK(snaps[0].ssrc == 0xAAAA);
    CHECK(snaps[0].ageMs == 10);

    t.reapStale(5000, 2000);
    CHECK(t.snapshot(snaps, kMaxSources, 5000) == 0);

    // table full -> refuse rather than corrupt
    SourceTable full;
    full.configure(240);
    for (int i = 0; i < kMaxSources; ++i)
        CHECK(full.acquire(static_cast<uint32_t>(1000 + i), 0) != nullptr);
    CHECK(full.acquire(9999, 0) == nullptr);

    // limiter never lets the bus past the ceiling
    Limiter lim;
    lim.configure(0.98f, kSampleRate, 150.0f);
    std::vector<float> hot(4800, 3.0f);
    lim.process(hot.data(), hot.size());
    float peak = 0.0f;
    for (float v : hot) peak = std::fmax(peak, std::fabs(v));
    CHECK(peak <= 0.981f);
    CHECK(lim.gain() < 0.4f);

    std::vector<float> quiet(48000, 0.05f);
    lim.process(quiet.data(), quiet.size());
    CHECK(lim.gain() > 0.9f);   // released back open
}

static void testUdpLoopback() {
    printf("udp loopback\n");
    UdpSocket rx;
    CHECK(rx.openReceiver(46789, 500));
    UdpSocket tx;
    CHECK(tx.openSender("127.0.0.1", 46789));

    uint8_t pkt[kHeaderBytes + 480];
    Header  h;
    h.ssrc = 0x1234;
    h.seq = 7;
    h.timestamp = 240;
    write_header(pkt, h);
    std::vector<int16_t> pcm(240, -1234);
    pcm_to_wire(pkt + kHeaderBytes, pcm.data(), 240);
    CHECK(tx.send(pkt, sizeof(pkt)) == static_cast<int>(sizeof(pkt)));

    uint8_t got[kMaxPacketBytes];
    uint32_t from = 0;
    const int n = rx.receive(got, sizeof(got), &from);
    CHECK(n == static_cast<int>(sizeof(pkt)));
    CHECK(from == 0x7F000001u);

    Header ph;
    CHECK(parse_header(got, static_cast<size_t>(n), &ph));
    CHECK(ph.ssrc == 0x1234 && ph.seq == 7 && ph.timestamp == 240);
    std::vector<int16_t> back(240);
    wire_to_pcm(back.data(), got + kHeaderBytes, 240);
    CHECK(back[0] == -1234 && back[239] == -1234);
}

// End-to-end: packetise a ramp, ship it over the loopback, mix it, and check
// the samples come out in order and undamaged.
static void testEndToEnd() {
    printf("end-to-end over the wire\n");
    UdpSocket rx;
    CHECK(rx.openReceiver(46790, 500));
    UdpSocket tx;
    CHECK(tx.openSender("127.0.0.1", 46790));

    SourceTable table;
    table.configure(1200);   // 25 ms target -> 100 ms ceiling

    constexpr int kPackets = 20;
    constexpr int kFrames  = 120;
    for (int p = 0; p < kPackets; ++p) {
        uint8_t buf[kHeaderBytes + kFrames * 2];
        Header  h;
        h.ssrc = 0x5150;
        h.seq = static_cast<uint32_t>(p);
        h.timestamp = static_cast<uint32_t>(p * kFrames);
        write_header(buf, h);
        int16_t pcm[kFrames];
        for (int i = 0; i < kFrames; ++i)
            pcm[i] = static_cast<int16_t>((p * kFrames + i) % 20000);
        pcm_to_wire(buf + kHeaderBytes, pcm, kFrames);
        CHECK(tx.send(buf, sizeof(buf)) > 0);
    }

    uint8_t got[kMaxPacketBytes];
    int received = 0;
    for (int i = 0; i < kPackets; ++i) {
        const int n = rx.receive(got, sizeof(got));
        if (n <= 0) break;
        Header h;
        CHECK(parse_header(got, static_cast<size_t>(n), &h));
        const size_t frames = (static_cast<size_t>(n) - kHeaderBytes) / 2;
        int16_t pcm[kMaxFramesPerPacket];
        wire_to_pcm(pcm, got + kHeaderBytes, frames);
        JitterBuffer* jb = table.acquire(h.ssrc, 0);
        CHECK(jb != nullptr);
        jb->onPacket(h.seq, h.timestamp, pcm, frames, false);
        ++received;
    }
    CHECK(received == kPackets);

    std::vector<float>   mix(240);
    std::vector<int16_t> scratch(240);
    for (int i = 0; i < 5; ++i) {   // 1200 frames of priming silence
        table.mix(mix.data(), 240, scratch.data());
        CHECK(std::fabs(mix[0]) < 1e-6f);
    }

    int mismatches = 0;
    int index = 0;
    for (int blk = 0; blk < 10; ++blk) {
        table.mix(mix.data(), 240, scratch.data());
        for (int i = 0; i < 240; ++i, ++index) {
            const float want = static_cast<float>(index % 20000) / 32768.0f;
            if (std::fabs(mix[i] - want) > 1e-4f) ++mismatches;
        }
    }
    CHECK(mismatches == 0);
    printf("  %d samples verified sample-accurate\n", index);
}

int main() {
    testHeader();
    testRing();
    testJitter();
    testMixer();
    testUdpLoopback();
    testEndToEnd();
    if (gFailures == 0) {
        printf("\nall tests passed\n");
        return 0;
    }
    printf("\n%d checks failed\n", gFailures);
    return 1;
}
