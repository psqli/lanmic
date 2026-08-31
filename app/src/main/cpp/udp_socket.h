#pragma once

#include <cstdint>
#include <cstddef>
#include <string>

namespace lau {

// Thin POSIX UDP wrapper. Same code compiles on Android and desktop Linux/macOS.
class UdpSocket {
public:
    UdpSocket() = default;
    ~UdpSocket() { close(); }

    UdpSocket(const UdpSocket&)            = delete;
    UdpSocket& operator=(const UdpSocket&) = delete;

    // Connected sender. Resolves host (v4/v6), marks the flow as voice (DSCP EF
    // -> Wi-Fi WMM AC_VO) and makes the socket non-blocking so send() can never
    // stall the sender thread.
    bool openSender(const std::string& host, uint16_t port);

    // Bound receiver with a receive timeout so the reader thread can poll for
    // shutdown.
    bool openReceiver(uint16_t port, int recvTimeoutMs);

    int  send(const void* data, size_t len);
    // Returns bytes received, 0 on timeout, -1 on error.
    int  receive(void* data, size_t len, uint32_t* fromIpv4 = nullptr);

    bool isOpen() const { return fd_ >= 0; }
    void close();

private:
    int fd_ = -1;
};

}  // namespace lau
