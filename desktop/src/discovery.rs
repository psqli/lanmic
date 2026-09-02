//! The DISCOVER/ANNOUNCE exchange from `PROTOCOL.md`, both halves.
//!
//! The mixer answers probes so a phone's **Find server** button lands on this
//! machine; the microphone side sends them so the desktop does not have to be
//! told an address by hand either. Neither half ever carries audio, and a
//! network that drops broadcast simply means typing the address in.
//!
//! Header encoding is [`lanmic::protocol`] - the same encoder the audio path
//! uses, so the two cannot drift.

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use lanmic::protocol::{Header, PacketType, HEADER_BYTES};

/// Long enough that a probe is answered promptly, short enough that a stopped
/// server's thread notices and exits.
const POLL: Duration = Duration::from_millis(500);
/// Names are advertised, not trusted: a reply is a stranger on the LAN.
const MAX_NAME_BYTES: usize = 64;
const MAX_DATAGRAM: usize = HEADER_BYTES + 256;

/// A server that answered a probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub addr: IpAddr,
    pub name: String,
}

impl Found {
    /// What the microphone side puts in the host field.
    pub fn host(&self) -> String {
        self.addr.to_string()
    }
}

fn bind_broadcast(port: u16) -> io::Result<UdpSocket> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, port))?;
    socket.set_broadcast(true)?;
    socket.set_read_timeout(Some(POLL))?;
    Ok(socket)
}

/// Truncates and sanitises an advertised name. Control characters would
/// otherwise reach a terminal status line intact.
fn clean_name(bytes: &[u8]) -> String {
    let end = bytes.len().min(MAX_NAME_BYTES);
    String::from_utf8_lossy(&bytes[..end])
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .to_string()
}

/// Answers DISCOVER probes with this server's name until `running` goes false.
///
/// Failing to bind is not fatal to a session: another mixer on this machine
/// already holds the port, or the OS refused it, and audio still works for
/// anyone told the address directly. The caller logs it and carries on, which
/// is what the Python server does too.
pub fn respond(port: u16, name: String, running: Arc<AtomicBool>) -> io::Result<JoinHandle<()>> {
    let socket = bind_broadcast(port)?;
    let mut reply = Header::new(PacketType::Announce).to_bytes().to_vec();
    reply.extend_from_slice(name.as_bytes());

    thread::Builder::new()
        .name("lau-discovery".into())
        .spawn(move || {
            let mut buf = [0u8; MAX_DATAGRAM];
            while running.load(Ordering::Acquire) {
                let (n, from) = match socket.recv_from(&mut buf) {
                    Ok(v) => v,
                    Err(ref e) if lanmic::net::is_would_block(e) => continue,
                    Err(e) => {
                        log::warn!("discovery socket closed: {e}");
                        break;
                    }
                };
                let Some(header) = Header::decode(&buf[..n]) else {
                    continue;
                };
                if header.kind() == Some(PacketType::Discover) {
                    // Back to the port the probe came from, per PROTOCOL.md.
                    let _ = socket.send_to(&reply, from);
                }
            }
        })
}

/// Broadcasts one probe and collects every reply that arrives inside `timeout`.
///
/// Blocks for the whole timeout: several servers may answer, and there is no
/// way to know the last one has. A second of waiting is the price of not
/// missing the mixer that was a millisecond slower than the first.
pub fn probe(port: u16, timeout: Duration) -> io::Result<Vec<Found>> {
    probe_to(IpAddr::V4(Ipv4Addr::BROADCAST), port, timeout)
}

/// [`probe`] aimed somewhere specific - a known subnet broadcast address, or
/// loopback, which is what the tests use.
pub fn probe_to(target: IpAddr, port: u16, timeout: Duration) -> io::Result<Vec<Found>> {
    let socket = bind_broadcast(0)?;
    // A zero read timeout means "block forever" to the OS, so never pass one on.
    socket.set_read_timeout(Some(POLL.min(timeout).max(Duration::from_millis(1))))?;
    socket.send_to(
        &Header::new(PacketType::Discover).to_bytes(),
        SocketAddr::new(target, port),
    )?;

    let deadline = Instant::now() + timeout;
    let mut found: Vec<Found> = Vec::new();
    let mut buf = [0u8; MAX_DATAGRAM];
    while Instant::now() < deadline {
        let (n, from) = match socket.recv_from(&mut buf) {
            Ok(v) => v,
            Err(ref e) if lanmic::net::is_would_block(e) => continue,
            Err(e) => return Err(e),
        };
        let Some(header) = Header::decode(&buf[..n]) else {
            continue;
        };
        if header.kind() != Some(PacketType::Announce) {
            continue;
        }
        let name = clean_name(&buf[HEADER_BYTES..n]);
        let addr = from.ip();
        // One entry per host: a server with two interfaces on this subnet
        // answers twice and would otherwise fill the list with itself.
        if !found.iter().any(|f| f.addr == addr) {
            found.push(Found {
                addr,
                name: if name.is_empty() {
                    "LAN Mic server".into()
                } else {
                    name
                },
            });
        }
    }
    Ok(found)
}

/// Every non-loopback address of this machine, for the "tell the phones this"
/// line. IPv4 first: that is what someone types into a phone.
pub fn local_addresses() -> Vec<String> {
    let Ok(interfaces) = if_addrs::get_if_addrs() else {
        return Vec::new();
    };
    let mut v4 = Vec::new();
    let mut v6 = Vec::new();
    for interface in interfaces {
        if interface.is_loopback() {
            continue;
        }
        match interface.ip() {
            IpAddr::V4(a) => v4.push(a.to_string()),
            // Link-local v6 needs a scope id to be useful and is only noise in
            // a list someone is going to read off a screen. `fe80::/10`, spelt
            // out because the std predicate for it is still unstable.
            IpAddr::V6(a) if a.segments()[0] & 0xffc0 != 0xfe80 => v6.push(a.to_string()),
            IpAddr::V6(_) => {}
        }
    }
    v4.extend(v6);
    v4
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Binds an ephemeral discovery port so the suite never fights the real one
    /// or another test.
    fn responder_on_free_port(name: &str) -> (u16, Arc<AtomicBool>, JoinHandle<()>) {
        let probe_socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = probe_socket.local_addr().unwrap().port();
        drop(probe_socket);
        let running = Arc::new(AtomicBool::new(true));
        let handle = respond(port, name.to_string(), running.clone()).unwrap();
        (port, running, handle)
    }

    #[test]
    fn a_probe_finds_a_server_and_learns_its_name() {
        let (port, running, handle) = responder_on_free_port("Front of house");
        let found = probe_to(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            port,
            Duration::from_millis(600),
        )
        .unwrap();
        running.store(false, Ordering::Release);
        handle.join().unwrap();

        assert_eq!(found.len(), 1, "expected exactly one server: {found:?}");
        assert_eq!(found[0].name, "Front of house");
        assert!(found[0].addr.is_loopback());
    }

    #[test]
    fn a_probe_with_nobody_listening_returns_empty_rather_than_erroring() {
        // Bind and drop, so the port is almost certainly free.
        let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = socket.local_addr().unwrap().port();
        drop(socket);
        let found = probe_to(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            port,
            Duration::from_millis(200),
        )
        .unwrap();
        assert!(found.is_empty());
    }

    #[test]
    fn the_responder_ignores_everything_that_is_not_a_probe() {
        let (port, running, handle) = responder_on_free_port("quiet");
        let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_millis(300)))
            .unwrap();
        let to = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);

        // Audio on the discovery port, and rubbish, neither of which is ours.
        socket
            .send_to(&Header::new(PacketType::Audio).to_bytes(), to)
            .unwrap();
        socket.send_to(b"not LAU1 at all", to).unwrap();

        let mut buf = [0u8; MAX_DATAGRAM];
        let answered = socket.recv_from(&mut buf).is_ok();
        running.store(false, Ordering::Release);
        handle.join().unwrap();
        assert!(!answered, "the responder answered something it should not");
    }

    #[test]
    fn an_advertised_name_is_trimmed_and_stripped_of_control_characters() {
        assert_eq!(clean_name(b"  Stage left \n"), "Stage left");
        assert_eq!(clean_name(b"a\x1b[2Jb"), "a[2Jb");
        assert_eq!(clean_name(&[0xff, 0xfe]), "\u{fffd}\u{fffd}");
        let long = vec![b'x'; 500];
        assert_eq!(clean_name(&long).len(), MAX_NAME_BYTES);
    }

    #[test]
    fn a_reply_without_a_name_still_becomes_a_usable_entry() {
        let (port, running, handle) = responder_on_free_port("");
        let found = probe_to(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            port,
            Duration::from_millis(600),
        )
        .unwrap();
        running.store(false, Ordering::Release);
        handle.join().unwrap();

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "LAN Mic server");
        assert!(!found[0].host().is_empty());
    }
}
