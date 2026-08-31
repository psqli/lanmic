#include "udp_socket.h"

#include <errno.h>
#include <fcntl.h>
#include <netdb.h>
#include <netinet/in.h>
#include <netinet/ip.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <unistd.h>

#include "log.h"

namespace lau {

namespace {
// DSCP EF (46) << 2. Android/Linux map this to WMM Voice on Wi-Fi, which is
// what keeps our packets ahead of everybody's video streaming.
constexpr int kTosVoice = 0xB8;

void applyCommonOptions(int fd, bool sender) {
    int tos = kTosVoice;
    setsockopt(fd, IPPROTO_IP, IP_TOS, &tos, sizeof(tos));
#ifdef SO_PRIORITY
    int prio = 6;  // TC_PRIO_INTERACTIVE
    setsockopt(fd, SOL_SOCKET, SO_PRIORITY, &prio, sizeof(prio));
#endif
    int buf = sender ? 256 * 1024 : 512 * 1024;
    setsockopt(fd, SOL_SOCKET, sender ? SO_SNDBUF : SO_RCVBUF, &buf, sizeof(buf));
}
}  // namespace

bool UdpSocket::openSender(const std::string& host, uint16_t port) {
    close();
    char portStr[16];
    snprintf(portStr, sizeof(portStr), "%u", static_cast<unsigned>(port));

    addrinfo hints{};
    hints.ai_family   = AF_UNSPEC;
    hints.ai_socktype = SOCK_DGRAM;
    hints.ai_protocol = IPPROTO_UDP;

    addrinfo* res = nullptr;
    const int rc  = getaddrinfo(host.c_str(), portStr, &hints, &res);
    if (rc != 0 || res == nullptr) {
        LAU_LOGE("getaddrinfo(%s) failed: %s", host.c_str(), gai_strerror(rc));
        return false;
    }

    int fd = -1;
    for (addrinfo* ai = res; ai != nullptr; ai = ai->ai_next) {
        fd = ::socket(ai->ai_family, ai->ai_socktype, ai->ai_protocol);
        if (fd < 0) continue;
        if (::connect(fd, ai->ai_addr, ai->ai_addrlen) == 0) break;
        ::close(fd);
        fd = -1;
    }
    freeaddrinfo(res);
    if (fd < 0) {
        LAU_LOGE("connect to %s:%u failed: %s", host.c_str(), port, strerror(errno));
        return false;
    }

    applyCommonOptions(fd, true);
    const int flags = fcntl(fd, F_GETFL, 0);
    fcntl(fd, F_SETFL, flags | O_NONBLOCK);
    fd_ = fd;
    return true;
}

bool UdpSocket::openReceiver(uint16_t port, int recvTimeoutMs) {
    close();
    int fd = ::socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP);
    if (fd < 0) {
        LAU_LOGE("socket() failed: %s", strerror(errno));
        return false;
    }
    int one = 1;
    setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof(one));
    applyCommonOptions(fd, false);

    sockaddr_in addr{};
    addr.sin_family      = AF_INET;
    addr.sin_addr.s_addr = htonl(INADDR_ANY);
    addr.sin_port        = htons(port);
    if (::bind(fd, reinterpret_cast<sockaddr*>(&addr), sizeof(addr)) != 0) {
        LAU_LOGE("bind(%u) failed: %s", port, strerror(errno));
        ::close(fd);
        return false;
    }

    timeval tv{};
    tv.tv_sec  = recvTimeoutMs / 1000;
    tv.tv_usec = (recvTimeoutMs % 1000) * 1000;
    setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv));

    fd_ = fd;
    return true;
}

int UdpSocket::send(const void* data, size_t len) {
    if (fd_ < 0) return -1;
    ssize_t n = ::send(fd_, data, len, 0);
    if (n < 0 && (errno == EAGAIN || errno == EWOULDBLOCK)) return 0;
    return static_cast<int>(n);
}

int UdpSocket::receive(void* data, size_t len, uint32_t* fromIpv4) {
    if (fd_ < 0) return -1;
    sockaddr_in from{};
    socklen_t   fromLen = sizeof(from);
    ssize_t     n = ::recvfrom(fd_, data, len, 0, reinterpret_cast<sockaddr*>(&from), &fromLen);
    if (n < 0) {
        if (errno == EAGAIN || errno == EWOULDBLOCK || errno == EINTR) return 0;
        return -1;
    }
    if (fromIpv4 != nullptr) *fromIpv4 = ntohl(from.sin_addr.s_addr);
    return static_cast<int>(n);
}

void UdpSocket::close() {
    if (fd_ >= 0) {
        ::close(fd_);
        fd_ = -1;
    }
}

}  // namespace lau
