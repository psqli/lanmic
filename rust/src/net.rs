//! UDP plumbing. Same code on Android and on any desktop the portable half is
//! tested on.
//!
//! The only thing here that is not `std::net` is the socket options, which is
//! why [`socket2`] is in the dependency list: marking the flow as voice is what
//! keeps our packets ahead of everybody else's video streaming on a shared
//! access point, and `std` has no way to say it.

use std::io;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::time::Duration;

use socket2::{Domain, Protocol, Socket, Type};

/// DSCP EF (46) shifted into the TOS byte. Android and Linux map this to WMM
/// Voice on Wi-Fi.
const TOS_VOICE: u32 = 0xB8;
/// IPv6 traffic class carries the same DSCP value in its own field.
const TCLASS_VOICE: u32 = 0xB8;

const SEND_BUFFER_BYTES: usize = 256 * 1024;
const RECV_BUFFER_BYTES: usize = 512 * 1024;

fn mark_as_voice(socket: &Socket, addr_is_v6: bool) {
    // Best effort throughout: a network that ignores DSCP still carries audio,
    // it just carries it behind the video.
    if addr_is_v6 {
        let _ = socket.set_tclass_v6(TCLASS_VOICE);
    } else {
        let _ = socket.set_tos_v4(TOS_VOICE);
    }
}

/// Connected sender. Resolves `host` (v4 or v6), marks the flow as voice, and
/// is non-blocking so a send can never stall the sender thread behind a busy
/// radio - a late packet is worth less than the one behind it.
pub fn open_sender(host: &str, port: u16) -> io::Result<UdpSocket> {
    if host.trim().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "no server address",
        ));
    }
    let addrs: Vec<SocketAddr> = (host, port).to_socket_addrs()?.collect();
    let mut last = io::Error::new(io::ErrorKind::AddrNotAvailable, "host resolved to nothing");
    for addr in addrs {
        let domain = Domain::for_address(addr);
        let socket = match Socket::new(domain, Type::DGRAM, Some(Protocol::UDP)) {
            Ok(s) => s,
            Err(e) => {
                last = e;
                continue;
            }
        };
        if let Err(e) = socket.connect(&addr.into()) {
            last = e;
            continue;
        }
        mark_as_voice(&socket, addr.is_ipv6());
        let _ = socket.set_send_buffer_size(SEND_BUFFER_BYTES);
        socket.set_nonblocking(true)?;
        return Ok(socket.into());
    }
    Err(last)
}

/// Bound receiver with a read timeout, so the reader thread can poll for
/// shutdown instead of needing to be woken.
pub fn open_receiver(port: u16, read_timeout: Duration) -> io::Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    let _ = socket.set_reuse_address(true);
    mark_as_voice(&socket, false);
    let _ = socket.set_recv_buffer_size(RECV_BUFFER_BYTES);
    let addr: SocketAddr = (std::net::Ipv4Addr::UNSPECIFIED, port).into();
    socket.bind(&addr.into())?;
    socket.set_read_timeout(Some(read_timeout))?;
    Ok(socket.into())
}

/// Validates a port that came from outside - the UI, across a language
/// boundary. Anything outside 1..=65535 must not reach the socket layer, where
/// the cast to 16 bits would silently turn 70000 into 4464.
pub fn validate_port(port: i32) -> io::Result<u16> {
    u16::try_from(port).ok().filter(|&p| p != 0).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("port {port} is outside 1..=65535"),
        )
    })
}

/// True for the errors that mean "nothing to read right now" rather than
/// "this socket is finished".
pub fn is_would_block(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Header, PacketType, HEADER_BYTES, MAX_PACKET_BYTES};

    #[test]
    fn a_packet_survives_the_loopback_intact() {
        let rx = open_receiver(0, Duration::from_millis(500)).unwrap();
        let port = rx.local_addr().unwrap().port();
        let tx = open_sender("127.0.0.1", port).unwrap();

        let mut h = Header::new(PacketType::Audio);
        h.ssrc = 0x1234;
        h.seq = 7;
        h.timestamp = 240;
        let mut pkt = vec![0u8; HEADER_BYTES + 480];
        pkt[..HEADER_BYTES].copy_from_slice(&h.to_bytes());
        crate::protocol::pcm_to_wire(&mut pkt[HEADER_BYTES..], &[-1234i16; 240]);
        assert_eq!(tx.send(&pkt).unwrap(), pkt.len());

        let mut got = [0u8; MAX_PACKET_BYTES];
        let (n, from) = rx.recv_from(&mut got).unwrap();
        assert_eq!(n, pkt.len());
        assert!(from.ip().is_loopback());

        let back = Header::decode(&got[..n]).unwrap();
        assert_eq!((back.ssrc, back.seq, back.timestamp), (0x1234, 7, 240));
        let mut pcm = [0i16; 240];
        assert_eq!(
            crate::protocol::wire_to_pcm(&mut pcm, &got[HEADER_BYTES..n]),
            240
        );
        assert!(pcm.iter().all(|&s| s == -1234));
    }

    #[test]
    fn a_receiver_with_nothing_to_read_times_out_rather_than_erroring() {
        let rx = open_receiver(0, Duration::from_millis(30)).unwrap();
        let mut buf = [0u8; MAX_PACKET_BYTES];
        let err = rx.recv_from(&mut buf).unwrap_err();
        assert!(is_would_block(&err), "unexpected error: {err:?}");
    }

    #[test]
    fn ports_from_outside_are_range_checked() {
        assert_eq!(validate_port(45678).unwrap(), 45678);
        assert_eq!(validate_port(65535).unwrap(), 65535);
        for bad in [0, -1, 65536, 70000, i32::MAX, i32::MIN] {
            assert!(validate_port(bad).is_err(), "{bad} should be rejected");
        }
    }

    #[test]
    fn an_empty_or_unresolvable_host_is_an_error_not_a_panic() {
        assert!(open_sender("", 45678).is_err());
        assert!(open_sender("   ", 45678).is_err());
        assert!(open_sender("no.such.host.invalid", 45678).is_err());
    }
}
