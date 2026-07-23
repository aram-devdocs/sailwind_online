//! `sw-net` — a poll-driven UDP host that speaks the LiteNetLib 1.x wire format.
//!
//! The server drives the host from its fixed-tick loop: [`Host::poll`] pumps the
//! socket and returns the [`Event`]s that occurred this tick, and
//! [`Host::send_unreliable`] queues an outgoing datagram. This is the honest
//! init-0 subset — ConnectRequest/ConnectAccept handshake, an Unreliable data
//! channel, Ping/Pong with RTT, Disconnect and idle timeout. There are no
//! reliable channels and no fragmentation; application-level protocols (hello,
//! econ, moorage) get their reliability from retries plus server idempotency.
//!
//! Interop note: the byte layout matches the LiteNetLib 1.x source, but true
//! interop against the real C# `LiteNetLib` client is proven by the
//! `protocol-smoke` harness (a genuine LiteNetLib client), not from this crate
//! alone. The unit tests here cover the framing in isolation.

pub mod protocol;

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Opaque, stable identifier for a connected peer, assigned by the host.
pub type PeerId = u32;

/// Largest possible UDP payload. Receiving the complete datagram lets the host
/// reject every packet above the fixed LiteNetLib MTU without truncation.
const RECV_BUFFER: usize = 65_535;

/// Hard ceiling on socket datagrams consumed by one fixed-tick poll.
const MAX_POLL_PACKETS: usize = 128;

/// Hard ceiling on socket bytes consumed by one fixed-tick poll. Four maximum
/// UDP datagrams fit, while normal MTU-sized traffic reaches the packet ceiling
/// first.
const MAX_POLL_BYTES: usize = 256 * 1024;

/// One datagram can emit at most a disconnect plus a replacement connect.
const MAX_POLL_SOCKET_EVENTS: usize = MAX_POLL_PACKETS * 2;

/// Timeout cleanup gets a reserved slice of every poll's event budget, so a
/// socket flood cannot indefinitely retain expired peers.
const MAX_POLL_TIMEOUT_EVENTS: usize = 64;

/// Hard ceiling on the event vector returned by one poll.
const MAX_POLL_EVENTS: usize = MAX_POLL_SOCKET_EVENTS + MAX_POLL_TIMEOUT_EVENTS;

/// Idle timeout: a peer that sends nothing for this long is dropped
/// (`Timeout`). Mirrors LiteNetLib's default 5 s disconnect timeout.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// How often the host sends its own Ping to each peer to measure RTT.
const PING_INTERVAL: Duration = Duration::from_secs(1);

/// Default hard ceiling on live transport peers.
pub const DEFAULT_MAX_PEERS: usize = 1_024;

/// Default hard ceiling on live transport peers sharing one source IP.
pub const DEFAULT_MAX_PEERS_PER_IP: usize = 16;

/// .NET `DateTime` ticks (100 ns units) at the Unix epoch (1970-01-01).
const UNIX_EPOCH_TICKS: i64 = 621_355_968_000_000_000;

/// Why a peer left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisconnectReason {
    /// The peer sent a Disconnect packet.
    Remote,
    /// No packets were received within the idle timeout.
    Timeout,
    /// The host is shutting the connection down locally.
    Shutdown,
}

/// Something that happened to the host during a [`Host::poll`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A peer completed the handshake and is now connected.
    Connected(PeerId),
    /// A peer delivered an application datagram (payload without the LiteNetLib
    /// header byte).
    Data(PeerId, Vec<u8>),
    /// A peer was disconnected.
    Disconnected(PeerId, DisconnectReason),
}

#[derive(Debug, Clone, Copy, Default)]
struct PollWork {
    packets: usize,
    bytes: usize,
    socket_events: usize,
    timeout_events: usize,
}

struct Peer {
    id: PeerId,
    addr: SocketAddr,
    connect_time: i64,
    connection_number: u8,
    local_peer_id: i32,
    last_recv: Instant,
    last_ping_sent: Instant,
    ping_seq: u16,
    ping_sent_at: Option<Instant>,
    rtt: Option<Duration>,
}

/// A UDP host: binds a socket, tracks peers, and exposes an event/send API.
pub struct Host {
    socket: UdpSocket,
    peers: HashMap<SocketAddr, Peer>,
    by_id: HashMap<PeerId, SocketAddr>,
    peers_per_ip: HashMap<IpAddr, usize>,
    next_id: PeerId,
    next_local_peer_id: i32,
    connect_key: String,
    timeout: Duration,
    max_peers: usize,
    max_peers_per_ip: usize,
    recv_buf: Box<[u8; RECV_BUFFER]>,
}

impl Host {
    /// Bind a non-blocking UDP host that accepts peers presenting `connect_key`.
    pub fn bind<A: ToSocketAddrs>(addr: A, connect_key: &str) -> io::Result<Host> {
        Self::bind_with_limits(
            addr,
            connect_key,
            DEFAULT_MAX_PEERS,
            DEFAULT_MAX_PEERS_PER_IP,
        )
    }

    /// Bind with hard global and per-source-IP live-peer ceilings.
    pub fn bind_with_limits<A: ToSocketAddrs>(
        addr: A,
        connect_key: &str,
        max_peers: usize,
        max_peers_per_ip: usize,
    ) -> io::Result<Host> {
        if max_peers == 0 || max_peers_per_ip == 0 || max_peers_per_ip > max_peers {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "peer limits must be nonzero and per-IP must not exceed global",
            ));
        }

        let socket = UdpSocket::bind(addr)?;
        socket.set_nonblocking(true)?;
        Ok(Host {
            socket,
            peers: HashMap::new(),
            by_id: HashMap::new(),
            peers_per_ip: HashMap::new(),
            next_id: 1,
            next_local_peer_id: 0,
            connect_key: connect_key.to_string(),
            timeout: DEFAULT_TIMEOUT,
            max_peers,
            max_peers_per_ip,
            recv_buf: Box::new([0u8; RECV_BUFFER]),
        })
    }

    /// The address the socket is actually bound to (useful when binding to port 0).
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    /// Return the remote address for a live peer.
    pub fn peer_addr(&self, peer: PeerId) -> Option<SocketAddr> {
        self.by_id.get(&peer).copied()
    }

    /// Number of currently connected peers.
    pub fn peer_count(&self) -> usize {
        self.peers.len()
    }

    #[cfg(test)]
    fn peer_count_for_ip(&self, ip: IpAddr) -> usize {
        self.peers_per_ip.get(&ip).copied().unwrap_or(0)
    }

    /// Last measured round-trip time to `peer`, if a Pong has come back.
    pub fn rtt(&self, peer: PeerId) -> Option<Duration> {
        let addr = self.by_id.get(&peer)?;
        self.peers.get(addr)?.rtt
    }

    /// Pump the socket and internal timers, returning everything that happened.
    ///
    /// `now` is the caller's tick timestamp; timeouts and ping scheduling are
    /// measured against it.
    pub fn poll(&mut self, now: Instant) -> Vec<Event> {
        self.poll_with_work(now).0
    }

    fn poll_with_work(&mut self, now: Instant) -> (Vec<Event>, PollWork) {
        let mut events = Vec::with_capacity(MAX_POLL_EVENTS);
        let mut work = PollWork::default();
        self.drain_socket(now, &mut events, &mut work);
        work.socket_events = events.len();
        work.timeout_events = self.process_timeouts(now, &mut events, MAX_POLL_TIMEOUT_EVENTS);
        self.send_keepalive_pings(now);
        debug_assert!(work.packets <= MAX_POLL_PACKETS);
        debug_assert!(work.bytes <= MAX_POLL_BYTES);
        debug_assert!(work.socket_events <= MAX_POLL_SOCKET_EVENTS);
        debug_assert!(work.timeout_events <= MAX_POLL_TIMEOUT_EVENTS);
        debug_assert!(events.len() <= MAX_POLL_EVENTS);
        (events, work)
    }

    fn drain_socket(&mut self, now: Instant, events: &mut Vec<Event>, work: &mut PollWork) {
        while work.packets < MAX_POLL_PACKETS && work.bytes < MAX_POLL_BYTES {
            let remaining_bytes = MAX_POLL_BYTES - work.bytes;
            match self.socket.peek_from(&mut self.recv_buf[..]) {
                Ok((n, _)) if n > remaining_bytes => break,
                Ok(_) => {}
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(ref e) if e.kind() == io::ErrorKind::ConnectionReset => {
                    work.packets += 1;
                    continue;
                }
                Err(_) => break,
            }

            match self.socket.recv_from(&mut self.recv_buf[..]) {
                Ok((n, addr)) => {
                    work.packets += 1;
                    work.bytes += n;
                    if n > protocol::MTU {
                        continue;
                    }
                    // Copy out of the shared buffer so packet handling can take
                    // `&mut self` freely.
                    let datagram = self.recv_buf[..n].to_vec();
                    let events_before = events.len();
                    self.handle_datagram(&datagram, addr, now, events);
                    debug_assert!(events.len().saturating_sub(events_before) <= 2);
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => break,
                // Windows raises WSAECONNRESET on the *next* recv after a send
                // to an unreachable port produced an ICMP message. It is not a
                // real error for a connectionless socket — keep draining.
                Err(ref e) if e.kind() == io::ErrorKind::ConnectionReset => {
                    work.packets += 1;
                }
                Err(_) => break,
            }
        }
    }

    fn handle_datagram(
        &mut self,
        data: &[u8],
        addr: SocketAddr,
        now: Instant,
        events: &mut Vec<Event>,
    ) {
        if data.is_empty() {
            return;
        }
        let header = protocol::Header::from_byte(data[0]);
        // No fragmentation support: drop any packet flagged fragmented.
        if header.fragmented {
            return;
        }
        match header.property {
            protocol::property::CONNECT_REQUEST => {
                self.handle_connect_request(data, addr, now, events)
            }
            protocol::property::UNRELIABLE => {
                if let Some(peer) = self.peers.get_mut(&addr) {
                    peer.last_recv = now;
                    events.push(Event::Data(peer.id, data[protocol::HEADER_SIZE..].to_vec()));
                }
            }
            protocol::property::PING => self.handle_ping(data, addr, now),
            protocol::property::PONG => self.handle_pong(data, addr, now),
            protocol::property::DISCONNECT => self.handle_disconnect(data, addr, events),
            // MtuCheck is ignored: with a fixed MTU we simply never grow the
            // negotiated size. Everything else in the init-0 subset is unused.
            _ => {}
        }
    }

    fn handle_connect_request(
        &mut self,
        data: &[u8],
        addr: SocketAddr,
        now: Instant,
        events: &mut Vec<Event>,
    ) {
        let Some(req) = protocol::parse_connect_request(data) else {
            return;
        };
        if req.protocol_id != protocol::PROTOCOL_ID {
            let _ = self.socket.send_to(
                &protocol::build_control(protocol::property::INVALID_PROTOCOL),
                addr,
            );
            return;
        }
        // Validate the application connect key. A mismatch is dropped silently.
        match protocol::read_litenet_string(&req.connect_data) {
            Some(key) if key == self.connect_key => {}
            _ => return,
        }

        if let Some(existing) = self.peers.get(&addr) {
            if existing.connect_time == req.connect_time {
                // Retransmitted request: the client missed our accept. Resend it.
                let accept = protocol::build_connect_accept(
                    existing.connect_time,
                    existing.connection_number,
                    existing.local_peer_id,
                    false,
                );
                let _ = self.socket.send_to(&accept, addr);
                return;
            }
            // A genuinely new session from the same address replaces the old one.
            let old_id = existing.id;
            self.remove_peer_at(addr);
            events.push(Event::Disconnected(old_id, DisconnectReason::Remote));
        } else if self.peers.len() >= self.max_peers
            || self.peers_per_ip.get(&addr.ip()).copied().unwrap_or(0) >= self.max_peers_per_ip
        {
            return;
        }

        let id = self.next_id;
        self.next_id += 1;
        let local_peer_id = self.next_local_peer_id;
        self.next_local_peer_id += 1;

        let accept = protocol::build_connect_accept(
            req.connect_time,
            req.connection_number,
            local_peer_id,
            false,
        );
        let _ = self.socket.send_to(&accept, addr);

        self.insert_peer(Peer {
            id,
            addr,
            connect_time: req.connect_time,
            connection_number: req.connection_number,
            local_peer_id,
            last_recv: now,
            last_ping_sent: now,
            ping_seq: 0,
            ping_sent_at: None,
            rtt: None,
        });
        events.push(Event::Connected(id));
    }

    fn insert_peer(&mut self, peer: Peer) {
        let id = peer.id;
        let addr = peer.addr;
        *self.peers_per_ip.entry(addr.ip()).or_insert(0) += 1;
        self.peers.insert(addr, peer);
        self.by_id.insert(id, addr);
    }

    fn remove_peer_at(&mut self, addr: SocketAddr) -> Option<Peer> {
        let peer_id = self.peers.get(&addr)?.id;
        let ip = addr.ip();
        let remove_counter = match self.peers_per_ip.get_mut(&ip)? {
            count if *count > 1 => {
                *count -= 1;
                false
            }
            _ => true,
        };
        if remove_counter {
            self.peers_per_ip.remove(&ip);
        }
        let peer = self.peers.remove(&addr)?;
        self.by_id.remove(&peer_id);
        Some(peer)
    }

    fn handle_ping(&mut self, data: &[u8], addr: SocketAddr, now: Instant) {
        let Some(seq) = protocol::read_sequence(data) else {
            return;
        };
        if let Some(peer) = self.peers.get_mut(&addr) {
            peer.last_recv = now;
            let pong = protocol::build_pong(seq, dotnet_ticks_now());
            let _ = self.socket.send_to(&pong, peer.addr);
        }
    }

    fn handle_pong(&mut self, data: &[u8], addr: SocketAddr, now: Instant) {
        let Some(seq) = protocol::read_sequence(data) else {
            return;
        };
        if let Some(peer) = self.peers.get_mut(&addr) {
            peer.last_recv = now;
            if seq == peer.ping_seq {
                if let Some(sent) = peer.ping_sent_at.take() {
                    peer.rtt = Some(now.saturating_duration_since(sent));
                }
            }
        }
    }

    fn handle_disconnect(&mut self, data: &[u8], addr: SocketAddr, events: &mut Vec<Event>) {
        let Some(time) = protocol::read_disconnect_time(data) else {
            return;
        };
        let Some(peer) = self.peers.get(&addr) else {
            return;
        };
        if peer.connect_time != time {
            return;
        }
        let id = peer.id;
        let _ = self.socket.send_to(
            &protocol::build_control(protocol::property::SHUTDOWN_OK),
            addr,
        );
        self.remove_peer_at(addr);
        events.push(Event::Disconnected(id, DisconnectReason::Remote));
    }

    fn process_timeouts(
        &mut self,
        now: Instant,
        events: &mut Vec<Event>,
        max_events: usize,
    ) -> usize {
        let timeout = self.timeout;
        let expired: Vec<SocketAddr> = self
            .peers
            .iter()
            .filter(|(_, p)| now.saturating_duration_since(p.last_recv) > timeout)
            .map(|(addr, _)| *addr)
            .take(max_events)
            .collect();
        let before = events.len();
        for addr in expired {
            if let Some(peer) = self.remove_peer_at(addr) {
                events.push(Event::Disconnected(peer.id, DisconnectReason::Timeout));
            }
        }
        events.len() - before
    }

    fn send_keepalive_pings(&mut self, now: Instant) {
        for peer in self.peers.values_mut() {
            if now.saturating_duration_since(peer.last_ping_sent) < PING_INTERVAL {
                continue;
            }
            peer.ping_seq = peer.ping_seq.wrapping_add(1);
            peer.last_ping_sent = now;
            peer.ping_sent_at = Some(now);
            let ping = protocol::build_ping(peer.ping_seq);
            let _ = self.socket.send_to(&ping, peer.addr);
        }
    }

    /// Send an application payload to `peer` over the Unreliable channel.
    ///
    /// Unknown peers (already disconnected) are a no-op. Payloads larger than
    /// the fixed MTU are rejected rather than fragmented.
    pub fn send_unreliable(&mut self, peer: PeerId, bytes: &[u8]) -> io::Result<()> {
        if bytes.len() > protocol::MTU - protocol::HEADER_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "payload exceeds MTU; fragmentation is not supported at init-0",
            ));
        }
        let Some(addr) = self.by_id.get(&peer).copied() else {
            return Ok(());
        };
        let packet = protocol::build_unreliable(bytes);
        self.socket.send_to(&packet, addr)?;
        Ok(())
    }

    /// Remove one peer immediately and send the LiteNetLib Disconnect packet.
    ///
    /// The caller already owns the corresponding application-session cleanup,
    /// so this does not enqueue a second [`Event::Disconnected`].
    pub fn disconnect(&mut self, peer: PeerId) -> bool {
        let Some(addr) = self.by_id.get(&peer).copied() else {
            return false;
        };
        let Some(peer) = self.remove_peer_at(addr) else {
            return false;
        };
        let _ = self
            .socket
            .send_to(&protocol::build_disconnect(peer.connect_time), peer.addr);
        true
    }

    /// Gracefully disconnect every peer (Disconnect packets + `Shutdown`
    /// events). Called on ctrl-c so clients learn immediately instead of
    /// waiting for their own timeout.
    pub fn shutdown(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        for peer in self.peers.values() {
            let _ = self
                .socket
                .send_to(&protocol::build_disconnect(peer.connect_time), peer.addr);
            events.push(Event::Disconnected(peer.id, DisconnectReason::Shutdown));
        }
        self.peers.clear();
        self.by_id.clear();
        self.peers_per_ip.clear();
        events
    }
}

/// Current time as .NET `DateTime.UtcNow.Ticks` (100 ns since year 1), used in
/// the Pong reply. Only affects the client's clock-offset estimate, never its
/// RTT measurement.
fn dotnet_ticks_now() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => UNIX_EPOCH_TICKS + (d.as_nanos() / 100) as i64,
        Err(_) => UNIX_EPOCH_TICKS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connect_datagram(connect_time: i64, key: &str) -> Vec<u8> {
        let data = protocol::write_litenet_string(key);
        protocol::build_connect_request(0, connect_time, 7, 16, &data)
    }

    fn client_from(source_ip: &str, server_addr: SocketAddr) -> UdpSocket {
        let client = UdpSocket::bind((source_ip, 0)).unwrap();
        client.set_nonblocking(true).unwrap();
        client.connect(server_addr).unwrap();
        client
    }

    fn assert_bounded_poll(work: PollWork, events: &[Event]) {
        assert!(work.packets <= MAX_POLL_PACKETS);
        assert!(work.bytes <= MAX_POLL_BYTES);
        assert!(work.socket_events <= MAX_POLL_SOCKET_EVENTS);
        assert!(work.timeout_events <= MAX_POLL_TIMEOUT_EVENTS);
        assert!(events.len() <= MAX_POLL_EVENTS);
    }

    #[test]
    fn preauth_data_flood_is_bounded_and_a_queued_payload_progresses() {
        let mut server = Host::bind("127.0.0.1:0", "sailwind-online").unwrap();
        let server_addr = server.local_addr().unwrap();
        let client = client_from("127.0.0.1", server_addr);

        client
            .send(&connect_datagram(1, "sailwind-online"))
            .unwrap();
        let (events, work) = server.poll_with_work(Instant::now());
        assert_bounded_poll(work, &events);
        assert_eq!(events, vec![Event::Connected(1)]);

        let mut accept = [0u8; protocol::CONNECT_ACCEPT_SIZE];
        client.recv(&mut accept).unwrap();

        let flood = protocol::build_unreliable(b"flood");
        for _ in 0..MAX_POLL_PACKETS + 32 {
            client.send(&flood).unwrap();
        }
        let marker = protocol::build_unreliable(b"legitimate-marker");
        client.send(&marker).unwrap();

        let mut saw_marker = false;
        for _ in 0..8 {
            for _ in 0..MAX_POLL_PACKETS / 4 {
                client.send(&flood).unwrap();
            }
            let (events, work) = server.poll_with_work(Instant::now());
            assert_bounded_poll(work, &events);
            saw_marker |= events.iter().any(
                |event| matches!(event, Event::Data(_, bytes) if bytes == b"legitimate-marker"),
            );
            if saw_marker {
                break;
            }
        }
        assert!(
            saw_marker,
            "bounded polling must leave queued traffic for a later tick instead of starving it"
        );
    }

    #[test]
    fn preauth_garbage_and_replacement_churn_are_bounded_across_ticks() {
        let mut server = Host::bind("127.0.0.1:0", "sailwind-online").unwrap();
        let server_addr = server.local_addr().unwrap();
        let client = client_from("127.0.0.1", server_addr);

        for _ in 0..MAX_POLL_PACKETS + 32 {
            client.send(&[protocol::property::UNRELIABLE]).unwrap();
        }
        client
            .send(&connect_datagram(1, "sailwind-online"))
            .unwrap();

        let mut connected = false;
        for _ in 0..8 {
            for _ in 0..MAX_POLL_PACKETS / 4 {
                client.send(&[protocol::property::UNRELIABLE]).unwrap();
            }
            let (events, work) = server.poll_with_work(Instant::now());
            assert_bounded_poll(work, &events);
            connected |= events
                .iter()
                .any(|event| matches!(event, Event::Connected(_)));
            if connected {
                break;
            }
        }
        assert!(
            connected,
            "a valid request queued behind a pre-auth flood must progress"
        );

        let mut accept = [0u8; protocol::CONNECT_ACCEPT_SIZE];
        client.recv(&mut accept).unwrap();
        let mut next_connect_time = 2;
        for _ in 0..MAX_POLL_PACKETS + 32 {
            client
                .send(&connect_datagram(next_connect_time, "sailwind-online"))
                .unwrap();
            next_connect_time += 1;
        }
        client
            .send(&protocol::build_unreliable(b"replacement-marker"))
            .unwrap();

        let mut saw_marker = false;
        for _ in 0..8 {
            for _ in 0..MAX_POLL_PACKETS / 4 {
                client
                    .send(&connect_datagram(next_connect_time, "sailwind-online"))
                    .unwrap();
                next_connect_time += 1;
            }
            let (events, work) = server.poll_with_work(Instant::now());
            assert_bounded_poll(work, &events);
            saw_marker |= events.iter().any(
                |event| matches!(event, Event::Data(_, bytes) if bytes == b"replacement-marker"),
            );
            if saw_marker {
                break;
            }
        }
        assert!(
            saw_marker,
            "replacement churn must not prevent later queued data from progressing"
        );
        assert_eq!(server.peer_count(), 1);
    }

    #[test]
    fn inbound_datagrams_over_the_fixed_mtu_are_dropped_before_payload_events() {
        let mut server = Host::bind("127.0.0.1:0", "sailwind-online").unwrap();
        let server_addr = server.local_addr().unwrap();
        let client = client_from("127.0.0.1", server_addr);

        client
            .send(&connect_datagram(1, "sailwind-online"))
            .unwrap();
        assert_eq!(server.poll(Instant::now()), vec![Event::Connected(1)]);
        let mut accept = [0u8; protocol::CONNECT_ACCEPT_SIZE];
        client.recv(&mut accept).unwrap();

        let oversized = vec![protocol::property::UNRELIABLE; protocol::MTU + 1];
        client.send(&oversized).unwrap();
        client
            .send(&protocol::build_unreliable(b"after-oversized"))
            .unwrap();

        let (events, work) = server.poll_with_work(Instant::now());
        assert_bounded_poll(work, &events);
        assert_eq!(events, vec![Event::Data(1, b"after-oversized".to_vec())]);
    }

    #[test]
    fn timeout_burst_uses_its_reserved_event_budget_and_finishes_on_later_ticks() {
        let mut server = Host::bind_with_limits("127.0.0.1:0", "sailwind-online", 80, 80).unwrap();
        let server_addr = server.local_addr().unwrap();
        let t0 = Instant::now();
        let mut clients = Vec::new();

        for connect_time in 1..=70 {
            let client = client_from("127.0.0.1", server_addr);
            client
                .send(&connect_datagram(connect_time, "sailwind-online"))
                .unwrap();
            clients.push(client);
        }
        let (events, work) = server.poll_with_work(t0);
        assert_bounded_poll(work, &events);
        assert_eq!(events.len(), 70);
        assert_eq!(server.peer_count(), 70);

        let expired_at = t0 + DEFAULT_TIMEOUT + Duration::from_secs(1);
        let (events, work) = server.poll_with_work(expired_at);
        assert_bounded_poll(work, &events);
        assert_eq!(work.timeout_events, MAX_POLL_TIMEOUT_EVENTS);
        assert_eq!(events.len(), MAX_POLL_TIMEOUT_EVENTS);
        assert_eq!(server.peer_count(), 70 - MAX_POLL_TIMEOUT_EVENTS);

        let (events, work) = server.poll_with_work(expired_at);
        assert_bounded_poll(work, &events);
        assert_eq!(events.len(), 70 - MAX_POLL_TIMEOUT_EVENTS);
        assert_eq!(server.peer_count(), 0);
        drop(clients);
    }

    #[test]
    fn per_ip_admission_uses_bounded_counter_state_instead_of_peer_scans() {
        let source = include_str!("lib.rs");
        let counter_field = ["peers_per_ip: HashMap<IpAddr", ", usize>"].concat();
        let full_peer_scan = [".fil", "ter(|connected| connected.ip() == addr.ip())"].concat();
        assert!(
            source.contains(&counter_field),
            "the transport must maintain one bounded counter per live source IP"
        );
        assert!(
            !source.contains(&full_peer_scan),
            "connect admission must not scan every live peer"
        );
    }

    #[test]
    fn peer_limits_bound_global_and_per_source_transport_state() {
        let mut server = Host::bind_with_limits("127.0.0.1:0", "sailwind-online", 3, 2).unwrap();
        let server_addr = server.local_addr().unwrap();
        let first = client_from("127.0.0.1", server_addr);
        let second = client_from("127.0.0.1", server_addr);

        first.send(&connect_datagram(1, "sailwind-online")).unwrap();
        assert_eq!(server.poll(Instant::now()), vec![Event::Connected(1)]);
        second
            .send(&connect_datagram(2, "sailwind-online"))
            .unwrap();
        assert_eq!(server.poll(Instant::now()), vec![Event::Connected(2)]);
        assert_eq!(server.peer_count(), 2);
        assert_eq!(server.peer_count_for_ip("127.0.0.1".parse().unwrap()), 2);

        let same_source_excess = client_from("127.0.0.1", server_addr);
        same_source_excess
            .send(&connect_datagram(3, "sailwind-online"))
            .unwrap();
        assert!(server.poll(Instant::now()).is_empty());
        assert_eq!(server.peer_count(), 2);

        let other_source = client_from("127.0.0.2", server_addr);
        other_source
            .send(&connect_datagram(4, "sailwind-online"))
            .unwrap();
        assert_eq!(server.poll(Instant::now()), vec![Event::Connected(3)]);
        assert_eq!(server.peer_count(), 3);
        assert_eq!(server.peer_count_for_ip("127.0.0.2".parse().unwrap()), 1);

        let global_excess = client_from("127.0.0.2", server_addr);
        global_excess
            .send(&connect_datagram(5, "sailwind-online"))
            .unwrap();
        assert!(server.poll(Instant::now()).is_empty());
        assert_eq!(server.peer_count(), 3);

        let mut accept = [0u8; protocol::CONNECT_ACCEPT_SIZE];
        first.recv(&mut accept).unwrap();
        first.send(&connect_datagram(1, "sailwind-online")).unwrap();
        assert!(server.poll(Instant::now()).is_empty());
        assert_eq!(
            first.recv(&mut accept).unwrap(),
            protocol::CONNECT_ACCEPT_SIZE
        );
        assert_eq!(server.peer_count(), 3);

        first.send(&connect_datagram(6, "sailwind-online")).unwrap();
        assert_eq!(
            server.poll(Instant::now()),
            vec![
                Event::Disconnected(1, DisconnectReason::Remote),
                Event::Connected(4)
            ]
        );
        assert_eq!(server.peer_count(), 3);
        assert_eq!(server.peer_count_for_ip("127.0.0.1".parse().unwrap()), 2);

        assert!(server.disconnect(4));
        assert_eq!(server.peer_count(), 2);
        assert_eq!(server.peer_count_for_ip("127.0.0.1".parse().unwrap()), 1);
        server.shutdown();
        assert_eq!(server.peer_count_for_ip("127.0.0.1".parse().unwrap()), 0);
        assert_eq!(server.peer_count_for_ip("127.0.0.2".parse().unwrap()), 0);
    }

    #[test]
    fn handshake_then_data_then_disconnect() {
        let mut server = Host::bind("127.0.0.1:0", "sailwind-online").unwrap();
        let server_addr = server.local_addr().unwrap();

        // A raw UDP "client" socket standing in for LiteNetLib.
        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        client.set_nonblocking(true).unwrap();
        client.connect(server_addr).unwrap();

        // 1) Connect.
        client
            .send(&connect_datagram(0x1234, "sailwind-online"))
            .unwrap();
        let events = server.poll(Instant::now());
        assert_eq!(events, vec![Event::Connected(1)]);
        assert_eq!(server.peer_count(), 1);

        // Client should have received a ConnectAccept.
        let mut buf = [0u8; 64];
        let n = client.recv(&mut buf).unwrap();
        assert_eq!(n, protocol::CONNECT_ACCEPT_SIZE);
        assert_eq!(
            protocol::Header::from_byte(buf[0]).property,
            protocol::property::CONNECT_ACCEPT
        );

        // 2) Application data over the Unreliable channel.
        let payload = protocol::build_unreliable(b"hello-server");
        client.send(&payload).unwrap();
        let events = server.poll(Instant::now());
        assert_eq!(events, vec![Event::Data(1, b"hello-server".to_vec())]);

        // 3) Server -> client unreliable send.
        server.send_unreliable(1, b"hello-client").unwrap();
        let n = client.recv(&mut buf).unwrap();
        assert_eq!(&buf[..n], &protocol::build_unreliable(b"hello-client")[..]);

        // 4) Disconnect.
        client.send(&protocol::build_disconnect(0x1234)).unwrap();
        let events = server.poll(Instant::now());
        assert_eq!(
            events,
            vec![Event::Disconnected(1, DisconnectReason::Remote)]
        );
        assert_eq!(server.peer_count(), 0);
        assert_eq!(server.peer_count_for_ip("127.0.0.1".parse().unwrap()), 0);
    }

    #[test]
    fn wrong_connect_key_is_rejected() {
        let mut server = Host::bind("127.0.0.1:0", "sailwind-online").unwrap();
        let server_addr = server.local_addr().unwrap();
        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        client.connect(server_addr).unwrap();

        client.send(&connect_datagram(1, "wrong-key")).unwrap();
        let events = server.poll(Instant::now());
        assert!(events.is_empty());
        assert_eq!(server.peer_count(), 0);
    }

    #[test]
    fn bad_protocol_id_gets_invalid_protocol_reply() {
        let mut server = Host::bind("127.0.0.1:0", "sailwind-online").unwrap();
        let server_addr = server.local_addr().unwrap();
        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        client.set_nonblocking(true).unwrap();
        client.connect(server_addr).unwrap();

        // Hand-build a connect request with a wrong protocol id.
        let data = protocol::write_litenet_string("sailwind-online");
        let mut dg = protocol::build_connect_request(0, 1, 1, 16, &data);
        dg[1..5].copy_from_slice(&999i32.to_le_bytes());
        client.send(&dg).unwrap();

        let events = server.poll(Instant::now());
        assert!(events.is_empty());
        let mut buf = [0u8; 8];
        let n = client.recv(&mut buf).unwrap();
        assert_eq!(
            protocol::Header::from_byte(buf[0]).property,
            protocol::property::INVALID_PROTOCOL
        );
        assert_eq!(n, protocol::HEADER_SIZE);
    }

    #[test]
    fn ping_is_answered_with_pong() {
        let mut server = Host::bind("127.0.0.1:0", "sailwind-online").unwrap();
        let server_addr = server.local_addr().unwrap();
        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        client.set_nonblocking(true).unwrap();
        client.connect(server_addr).unwrap();

        client
            .send(&connect_datagram(1, "sailwind-online"))
            .unwrap();
        server.poll(Instant::now());
        let mut buf = [0u8; 64];
        let _ = client.recv(&mut buf).unwrap(); // drain accept

        client.send(&protocol::build_ping(0x00AB)).unwrap();
        server.poll(Instant::now());
        let n = client.recv(&mut buf).unwrap();
        assert_eq!(n, protocol::PONG_SIZE);
        assert_eq!(
            protocol::Header::from_byte(buf[0]).property,
            protocol::property::PONG
        );
        assert_eq!(protocol::read_sequence(&buf[..n]), Some(0x00AB));
    }

    #[test]
    fn idle_peer_times_out() {
        let mut server = Host::bind("127.0.0.1:0", "sailwind-online").unwrap();
        let server_addr = server.local_addr().unwrap();
        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        client.connect(server_addr).unwrap();

        let t0 = Instant::now();
        client
            .send(&connect_datagram(1, "sailwind-online"))
            .unwrap();
        assert_eq!(server.poll(t0), vec![Event::Connected(1)]);

        // Advance the logical clock well past the timeout without any traffic.
        let later = t0 + DEFAULT_TIMEOUT + Duration::from_secs(1);
        let events = server.poll(later);
        assert_eq!(
            events,
            vec![Event::Disconnected(1, DisconnectReason::Timeout)]
        );
        assert_eq!(server.peer_count(), 0);
        assert_eq!(server.peer_count_for_ip("127.0.0.1".parse().unwrap()), 0);
    }

    #[test]
    fn oversized_send_is_rejected() {
        let mut server = Host::bind("127.0.0.1:0", "sailwind-online").unwrap();
        let big = vec![0u8; protocol::MTU];
        // Peer id 1 does not exist yet, but the size check fires first.
        let err = server.send_unreliable(1, &big).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn garbage_datagrams_do_not_panic_or_connect() {
        let mut server = Host::bind("127.0.0.1:0", "sailwind-online").unwrap();
        let server_addr = server.local_addr().unwrap();
        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        client.connect(server_addr).unwrap();
        for b in 0u16..=255 {
            client.send(&[b as u8; 20]).unwrap();
        }
        let events = server.poll(Instant::now());
        assert!(events.iter().all(|e| !matches!(e, Event::Connected(_))));
        assert_eq!(server.peer_count(), 0);
    }
}
