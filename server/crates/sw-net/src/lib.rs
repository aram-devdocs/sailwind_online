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

use std::collections::{HashMap, VecDeque};
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

/// Hard ceiling on live-peer slots inspected by one fixed-tick poll. At the
/// maximum supported 65,535 peers, the round-robin cursor covers every slot in
/// fewer than 128 ticks.
const MAX_POLL_MAINTENANCE_SCANS: usize = 512;

/// Hard ceiling on host keepalive datagrams sent by one fixed-tick poll.
const MAX_POLL_KEEPALIVE_SENDS: usize = MAX_POLL_MAINTENANCE_SCANS;

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
    maintenance_scans: usize,
    keepalive_sends: usize,
}

struct Peer {
    id: PeerId,
    slot: usize,
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
    peer_slots: Vec<Option<SocketAddr>>,
    // Retired slots join the back of this queue. Only the prefix counted by
    // `available_free_peer_slots` existed when the current poll began, so slot
    // reuse can never alias an event returned by that poll.
    free_peer_slots: VecDeque<usize>,
    available_free_peer_slots: usize,
    peers_per_ip: HashMap<IpAddr, usize>,
    maintenance_cursor: usize,
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
        if max_peers == 0
            || max_peers > u16::MAX as usize
            || max_peers_per_ip == 0
            || max_peers_per_ip > max_peers
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "peer limits must be nonzero, global must not exceed 65535, and per-IP must not exceed global",
            ));
        }

        let socket = UdpSocket::bind(addr)?;
        socket.set_nonblocking(true)?;
        Ok(Host {
            socket,
            peers: HashMap::new(),
            peer_slots: Vec::new(),
            free_peer_slots: VecDeque::new(),
            available_free_peer_slots: 0,
            peers_per_ip: HashMap::new(),
            maintenance_cursor: 0,
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
        let slot = usize::try_from(peer).ok()?.checked_sub(1)?;
        let addr = self.peer_slots.get(slot)?.as_ref().copied()?;
        self.peers
            .get(&addr)
            .filter(|connected| connected.id == peer)
            .map(|_| addr)
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
        let addr = self.peer_addr(peer)?;
        self.peers.get(&addr)?.rtt
    }

    /// Pump the socket and internal timers, returning everything that happened.
    ///
    /// `now` is the caller's tick timestamp; timeouts and ping scheduling are
    /// measured against it. The caller must finish consuming one returned event
    /// batch before polling again: the next poll is the boundary at which peer
    /// slots retired by the prior batch become eligible for reuse.
    pub fn poll(&mut self, now: Instant) -> Vec<Event> {
        self.poll_with_work(now).0
    }

    fn poll_with_work(&mut self, now: Instant) -> (Vec<Event>, PollWork) {
        self.available_free_peer_slots = self.free_peer_slots.len();
        let mut events = Vec::with_capacity(MAX_POLL_EVENTS);
        let mut work = PollWork::default();
        self.drain_socket(now, &mut events, &mut work);
        work.socket_events = events.len();
        self.process_peer_maintenance(now, &mut events, &mut work);
        debug_assert!(work.packets <= MAX_POLL_PACKETS);
        debug_assert!(work.bytes <= MAX_POLL_BYTES);
        debug_assert!(work.socket_events <= MAX_POLL_SOCKET_EVENTS);
        debug_assert!(work.timeout_events <= MAX_POLL_TIMEOUT_EVENTS);
        debug_assert!(work.maintenance_scans <= MAX_POLL_MAINTENANCE_SCANS);
        debug_assert!(work.keepalive_sends <= MAX_POLL_KEEPALIVE_SENDS);
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
        if header.property != protocol::property::CONNECT_REQUEST {
            let Some(peer) = self.peers.get(&addr) else {
                return;
            };
            if header.connection_number != peer.connection_number {
                return;
            }
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
            // LiteNetLib orders this signed Int64 directly. Do not subtract:
            // a wrapped comparison could let a delayed request evict its live
            // successor.
            if req.connect_time < existing.connect_time {
                return;
            }
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

        let Some(slot) = self.allocate_peer_slot() else {
            return;
        };
        let id = PeerId::try_from(slot + 1).expect("bounded peer slot fits PeerId");
        let local_peer_id = i32::try_from(slot).expect("bounded peer slot fits LiteNetLib peer id");

        let accept = protocol::build_connect_accept(
            req.connect_time,
            req.connection_number,
            local_peer_id,
            false,
        );
        let _ = self.socket.send_to(&accept, addr);

        self.insert_peer(Peer {
            id,
            slot,
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
        let slot = peer.slot;
        let addr = peer.addr;
        debug_assert_eq!(id, PeerId::try_from(slot + 1).unwrap());
        debug_assert!(self.peer_slots[slot].is_none());
        *self.peers_per_ip.entry(addr.ip()).or_insert(0) += 1;
        self.peers.insert(addr, peer);
        self.peer_slots[slot] = Some(addr);
    }

    fn allocate_peer_slot(&mut self) -> Option<usize> {
        if self.peer_slots.len() < self.max_peers {
            let slot = self.peer_slots.len();
            self.peer_slots.push(None);
            Some(slot)
        } else if self.available_free_peer_slots > 0 {
            self.available_free_peer_slots -= 1;
            self.free_peer_slots.pop_front()
        } else {
            None
        }
    }

    fn remove_peer_at(&mut self, addr: SocketAddr) -> Option<Peer> {
        let slot = self.peers.get(&addr)?.slot;
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
        debug_assert_eq!(self.peer_slots[slot], Some(addr));
        self.peer_slots[slot] = None;
        self.free_peer_slots.push_back(slot);
        Some(peer)
    }

    fn handle_ping(&mut self, data: &[u8], addr: SocketAddr, now: Instant) {
        let Some(seq) = protocol::read_sequence(data) else {
            return;
        };
        if let Some(peer) = self.peers.get_mut(&addr) {
            peer.last_recv = now;
            let pong = protocol::build_pong(peer.connection_number, seq, dotnet_ticks_now());
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

    fn process_peer_maintenance(
        &mut self,
        now: Instant,
        events: &mut Vec<Event>,
        work: &mut PollWork,
    ) {
        let slot_count = self.peer_slots.len();
        if slot_count == 0 {
            self.maintenance_cursor = 0;
            return;
        }

        let scan_count = slot_count.min(MAX_POLL_MAINTENANCE_SCANS);
        for _ in 0..scan_count {
            if self.maintenance_cursor >= slot_count {
                self.maintenance_cursor = 0;
            }
            let slot = self.maintenance_cursor;
            self.maintenance_cursor += 1;
            work.maintenance_scans += 1;

            let Some(addr) = self.peer_slots[slot] else {
                continue;
            };
            let expired = self
                .peers
                .get(&addr)
                .is_some_and(|peer| now.saturating_duration_since(peer.last_recv) > self.timeout);
            if expired {
                if work.timeout_events < MAX_POLL_TIMEOUT_EVENTS {
                    if let Some(peer) = self.remove_peer_at(addr) {
                        events.push(Event::Disconnected(peer.id, DisconnectReason::Timeout));
                        work.timeout_events += 1;
                    }
                }
                continue;
            }

            if work.keepalive_sends >= MAX_POLL_KEEPALIVE_SENDS {
                continue;
            }
            let Some(peer) = self.peers.get_mut(&addr) else {
                continue;
            };
            if now.saturating_duration_since(peer.last_ping_sent) < PING_INTERVAL {
                continue;
            }
            peer.ping_seq = peer.ping_seq.wrapping_add(1);
            peer.last_ping_sent = now;
            peer.ping_sent_at = Some(now);
            let ping = protocol::build_ping(peer.connection_number, peer.ping_seq);
            let _ = self.socket.send_to(&ping, peer.addr);
            work.keepalive_sends += 1;
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
        let Some(addr) = self.peer_addr(peer) else {
            return Ok(());
        };
        let connection_number = self.peers[&addr].connection_number;
        let packet = protocol::build_unreliable(connection_number, bytes);
        self.socket.send_to(&packet, addr)?;
        Ok(())
    }

    /// Remove one peer immediately and send the LiteNetLib Disconnect packet.
    ///
    /// The caller already owns the corresponding application-session cleanup,
    /// so this does not enqueue a second [`Event::Disconnected`].
    pub fn disconnect(&mut self, peer: PeerId) -> bool {
        let Some(addr) = self.peer_addr(peer) else {
            return false;
        };
        let Some(peer) = self.remove_peer_at(addr) else {
            return false;
        };
        let _ = self.socket.send_to(
            &protocol::build_disconnect(peer.connection_number, peer.connect_time),
            peer.addr,
        );
        true
    }

    /// Gracefully disconnect every peer (Disconnect packets + `Shutdown`
    /// events). Called on ctrl-c so clients learn immediately instead of
    /// waiting for their own timeout.
    pub fn shutdown(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        for peer in self.peers.values() {
            let _ = self.socket.send_to(
                &protocol::build_disconnect(peer.connection_number, peer.connect_time),
                peer.addr,
            );
            events.push(Event::Disconnected(peer.id, DisconnectReason::Shutdown));
        }
        self.peers.clear();
        self.peer_slots.clear();
        self.free_peer_slots.clear();
        self.available_free_peer_slots = 0;
        self.peers_per_ip.clear();
        self.maintenance_cursor = 0;
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

    fn connect_datagram(connection_number: u8, connect_time: i64, key: &str) -> Vec<u8> {
        let data = protocol::write_litenet_string(key);
        protocol::build_connect_request(connection_number, connect_time, 7, 16, &data)
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
        assert!(work.maintenance_scans <= MAX_POLL_MAINTENANCE_SCANS);
        assert!(work.keepalive_sends <= MAX_POLL_KEEPALIVE_SENDS);
        assert!(events.len() <= MAX_POLL_EVENTS);
    }

    fn connect_many(server: &mut Host, count: usize, now: Instant) -> Vec<UdpSocket> {
        let server_addr = server.local_addr().unwrap();
        let mut clients = Vec::with_capacity(count);
        for connect_time in 1..=count {
            let client = client_from("127.0.0.1", server_addr);
            client
                .send(&connect_datagram(0, connect_time as i64, "sailwind-online"))
                .unwrap();
            clients.push(client);
            if connect_time % MAX_POLL_PACKETS == 0 {
                let (events, work) = server.poll_with_work(now);
                assert_bounded_poll(work, &events);
            }
        }
        while server.peer_count() < count {
            let (events, work) = server.poll_with_work(now);
            assert_bounded_poll(work, &events);
        }
        clients
    }

    fn drain_packets(client: &UdpSocket) -> Vec<Vec<u8>> {
        let mut packets = Vec::new();
        let mut buf = [0u8; 64];
        loop {
            match client.recv(&mut buf) {
                Ok(n) => packets.push(buf[..n].to_vec()),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return packets,
                Err(error) => panic!("reading test client: {error}"),
            }
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    struct PeerState {
        id: PeerId,
        slot: usize,
        connect_time: i64,
        connection_number: u8,
        local_peer_id: i32,
        last_recv: Instant,
        last_ping_sent: Instant,
        ping_seq: u16,
        ping_sent_at: Option<Instant>,
        rtt: Option<Duration>,
    }

    fn peer_state(server: &Host, addr: SocketAddr) -> PeerState {
        let peer = &server.peers[&addr];
        PeerState {
            id: peer.id,
            slot: peer.slot,
            connect_time: peer.connect_time,
            connection_number: peer.connection_number,
            local_peer_id: peer.local_peer_id,
            last_recv: peer.last_recv,
            last_ping_sent: peer.last_ping_sent,
            ping_seq: peer.ping_seq,
            ping_sent_at: peer.ping_sent_at,
            rtt: peer.rtt,
        }
    }

    fn assert_peer_indices_consistent(server: &Host) {
        assert_eq!(
            server.peer_slots.len(),
            server.free_peer_slots.len() + server.peers.len()
        );
        assert!(server.available_free_peer_slots <= server.free_peer_slots.len());
        assert!(server.maintenance_cursor <= server.peer_slots.len());

        let mut free = vec![false; server.peer_slots.len()];
        for &slot in &server.free_peer_slots {
            assert!(slot < server.peer_slots.len());
            assert!(!free[slot], "free-list slot {slot} must appear once");
            free[slot] = true;
            assert!(server.peer_slots[slot].is_none());
        }
        for (slot, addr) in server.peer_slots.iter().enumerate() {
            match addr {
                Some(addr) => {
                    let peer = &server.peers[addr];
                    assert_eq!(peer.slot, slot);
                    assert_eq!(peer.id, PeerId::try_from(slot + 1).unwrap());
                    assert!(!free[slot]);
                }
                None => assert!(free[slot]),
            }
        }
    }

    #[test]
    fn nonzero_connection_numbers_isolate_connected_udp_sessions() {
        for connection_number in 1..protocol::MAX_CONNECTION_NUMBER {
            let mut server = Host::bind("127.0.0.1:0", "sailwind-online").unwrap();
            let server_addr = server.local_addr().unwrap();
            let client = client_from("127.0.0.1", server_addr);
            let t0 = Instant::now();
            let connect_time = i64::from(connection_number) + 100;

            client
                .send(&connect_datagram(
                    connection_number,
                    connect_time,
                    "sailwind-online",
                ))
                .unwrap();
            assert_eq!(server.poll(t0), vec![Event::Connected(1)]);

            let accept = drain_packets(&client);
            assert_eq!(accept.len(), 1);
            assert_eq!(
                protocol::Header::from_byte(accept[0][0]).property,
                protocol::property::CONNECT_ACCEPT
            );
            assert_eq!(accept[0][9], connection_number);

            let current_data = protocol::build_unreliable(connection_number, b"current-session");
            client.send(&current_data).unwrap();
            assert_eq!(
                server.poll(t0),
                vec![Event::Data(1, b"current-session".to_vec())]
            );

            server.send_unreliable(1, b"server-current").unwrap();
            let outbound = drain_packets(&client);
            assert_eq!(outbound.len(), 1);
            assert_eq!(
                protocol::Header::from_byte(outbound[0][0]),
                protocol::Header {
                    property: protocol::property::UNRELIABLE,
                    connection_number,
                    fragmented: false,
                }
            );

            let stale_number = (connection_number + protocol::MAX_CONNECTION_NUMBER - 1)
                % protocol::MAX_CONNECTION_NUMBER;
            let stale_data = protocol::build_unreliable(stale_number, b"stale-session");
            client.send(&stale_data).unwrap();
            assert!(
                server.poll(t0).is_empty(),
                "stale-number data must not emit an application event"
            );

            let stale_ping = protocol::build_ping(stale_number, 0x1234);
            client.send(&stale_ping).unwrap();
            assert!(server.poll(t0).is_empty());
            assert!(
                drain_packets(&client).is_empty(),
                "stale-number ping must not amplify into a pong"
            );

            let keepalive_at = t0 + PING_INTERVAL;
            assert!(server.poll(keepalive_at).is_empty());
            let keepalive = drain_packets(&client);
            assert_eq!(keepalive.len(), 1);
            let keepalive_header = protocol::Header::from_byte(keepalive[0][0]);
            assert_eq!(keepalive_header.property, protocol::property::PING);
            assert_eq!(keepalive_header.connection_number, connection_number);
            let sequence = protocol::read_sequence(&keepalive[0]).unwrap();

            let stale_pong = protocol::build_pong(stale_number, sequence, 0);
            client.send(&stale_pong).unwrap();
            assert!(server.poll(keepalive_at).is_empty());
            assert_eq!(
                server.rtt(1),
                None,
                "stale-number pong must not mutate RTT state"
            );

            let stale_disconnect = protocol::build_disconnect(stale_number, connect_time);
            client.send(&stale_disconnect).unwrap();
            assert!(server.poll(keepalive_at).is_empty());
            assert_eq!(server.peer_count(), 1);
            assert!(
                drain_packets(&client).is_empty(),
                "stale-number disconnect must not receive ShutdownOk"
            );

            let current_disconnect = protocol::build_disconnect(connection_number, connect_time);
            client.send(&current_disconnect).unwrap();
            assert_eq!(
                server.poll(keepalive_at),
                vec![Event::Disconnected(1, DisconnectReason::Remote)]
            );
            assert_eq!(server.peer_count(), 0);
        }
    }

    #[test]
    fn stale_connection_number_cannot_refresh_idle_timeout() {
        let stale_packets = [
            protocol::build_unreliable(1, b"stale-data"),
            protocol::build_ping(1, 7).to_vec(),
            protocol::build_pong(1, 7, 0).to_vec(),
            protocol::build_disconnect(1, 20).to_vec(),
        ];

        for stale in stale_packets {
            let mut server = Host::bind("127.0.0.1:0", "sailwind-online").unwrap();
            let server_addr = server.local_addr().unwrap();
            let client = client_from("127.0.0.1", server_addr);
            let t0 = Instant::now();

            client
                .send(&connect_datagram(2, 20, "sailwind-online"))
                .unwrap();
            assert_eq!(server.poll(t0), vec![Event::Connected(1)]);
            assert_eq!(drain_packets(&client).len(), 1);

            client.send(&stale).unwrap();
            let expired_at = t0 + DEFAULT_TIMEOUT + Duration::from_millis(1);
            assert_eq!(
                server.poll(expired_at),
                vec![Event::Disconnected(1, DisconnectReason::Timeout)]
            );
            assert_eq!(server.peer_count(), 0);
            assert!(
                drain_packets(&client).is_empty(),
                "stale connected traffic must not receive any reply"
            );
        }
    }

    #[test]
    fn replaced_endpoint_rejects_packets_from_the_retired_connection_number() {
        let mut server = Host::bind("127.0.0.1:0", "sailwind-online").unwrap();
        let server_addr = server.local_addr().unwrap();
        let client = client_from("127.0.0.1", server_addr);
        let now = Instant::now();

        client
            .send(&connect_datagram(1, 10, "sailwind-online"))
            .unwrap();
        assert_eq!(server.poll(now), vec![Event::Connected(1)]);
        assert_eq!(drain_packets(&client).len(), 1);

        client
            .send(&connect_datagram(2, 20, "sailwind-online"))
            .unwrap();
        assert_eq!(
            server.poll(now),
            vec![
                Event::Disconnected(1, DisconnectReason::Remote),
                Event::Connected(2),
            ]
        );
        assert_eq!(drain_packets(&client).len(), 1);

        let retired = protocol::build_unreliable(1, b"retired-hello");
        client.send(&retired).unwrap();
        assert!(
            server.poll(now).is_empty(),
            "a packet from the replaced connection must not execute in the new session"
        );

        let current = protocol::build_unreliable(2, b"current-hello");
        client.send(&current).unwrap();
        assert_eq!(
            server.poll(now),
            vec![Event::Data(2, b"current-hello".to_vec())]
        );
    }

    #[test]
    fn same_endpoint_connect_requests_only_replace_with_a_strictly_newer_time() {
        let mut server = Host::bind_with_limits("127.0.0.1:0", "sailwind-online", 3, 3).unwrap();
        let server_addr = server.local_addr().unwrap();
        let client = client_from("127.0.0.1", server_addr);
        let addr = client.local_addr().unwrap();
        let t0 = Instant::now();

        client
            .send(&connect_datagram(1, 30, "sailwind-online"))
            .unwrap();
        assert_eq!(server.poll(t0), vec![Event::Connected(1)]);
        assert_eq!(drain_packets(&client).len(), 1);

        client
            .send(&connect_datagram(2, 40, "sailwind-online"))
            .unwrap();
        assert_eq!(
            server.poll(t0 + Duration::from_millis(1)),
            vec![
                Event::Disconnected(1, DisconnectReason::Remote),
                Event::Connected(2),
            ]
        );
        let time_40_accept = drain_packets(&client);
        assert_eq!(time_40_accept.len(), 1);
        assert_eq!(
            i64::from_le_bytes(time_40_accept[0][1..9].try_into().unwrap()),
            40
        );
        assert_eq!(time_40_accept[0][9], 2);

        let state_after_time_40 = peer_state(&server, addr);
        client
            .send(&connect_datagram(1, 30, "sailwind-online"))
            .unwrap();
        assert!(server.poll(t0 + Duration::from_millis(2)).is_empty());
        assert!(drain_packets(&client).is_empty());
        assert_eq!(
            peer_state(&server, addr),
            state_after_time_40,
            "an older request must not mutate live-session or liveness state"
        );
        assert_eq!(server.peer_count(), 1);
        assert_eq!(server.peer_addr(2), Some(addr));
        assert_peer_indices_consistent(&server);

        client
            .send(&protocol::build_unreliable(1, b"retired-session"))
            .unwrap();
        client
            .send(&protocol::build_unreliable(2, b"current-session"))
            .unwrap();
        assert_eq!(
            server.poll(t0 + Duration::from_millis(3)),
            vec![Event::Data(2, b"current-session".to_vec())]
        );

        let state_before_duplicate = peer_state(&server, addr);
        client
            .send(&connect_datagram(2, 40, "sailwind-online"))
            .unwrap();
        assert!(server.poll(t0 + Duration::from_millis(4)).is_empty());
        assert_eq!(drain_packets(&client), time_40_accept);
        assert_eq!(
            peer_state(&server, addr),
            state_before_duplicate,
            "an exact retry must only resend the original accept"
        );

        client
            .send(&connect_datagram(3, 50, "sailwind-online"))
            .unwrap();
        assert_eq!(
            server.poll(t0 + Duration::from_millis(5)),
            vec![
                Event::Disconnected(2, DisconnectReason::Remote),
                Event::Connected(3),
            ]
        );
        let time_50_accept = drain_packets(&client);
        assert_eq!(time_50_accept.len(), 1);
        assert_eq!(
            i64::from_le_bytes(time_50_accept[0][1..9].try_into().unwrap()),
            50
        );
        assert_eq!(time_50_accept[0][9], 3);
        assert_eq!(server.peer_count(), 1);
        assert_eq!(server.peer_addr(3), Some(addr));
        assert_peer_indices_consistent(&server);
    }

    #[test]
    fn delayed_older_request_cannot_black_hole_a_full_capacity_peer() {
        let mut server = Host::bind_with_limits("127.0.0.1:0", "sailwind-online", 1, 1).unwrap();
        let server_addr = server.local_addr().unwrap();
        let client = client_from("127.0.0.1", server_addr);
        let addr = client.local_addr().unwrap();
        let t0 = Instant::now();

        client
            .send(&connect_datagram(1, 30, "sailwind-online"))
            .unwrap();
        assert_eq!(server.poll(t0), vec![Event::Connected(1)]);
        assert_eq!(drain_packets(&client).len(), 1);

        client
            .send(&connect_datagram(2, 40, "sailwind-online"))
            .unwrap();
        assert_eq!(
            server.poll(t0 + Duration::from_millis(1)),
            vec![Event::Disconnected(1, DisconnectReason::Remote)]
        );
        assert_eq!(server.peer_count(), 0);
        assert_eq!(server.free_peer_slots, VecDeque::from([0]));

        client
            .send(&connect_datagram(2, 40, "sailwind-online"))
            .unwrap();
        assert_eq!(
            server.poll(t0 + Duration::from_millis(2)),
            vec![Event::Connected(1)]
        );
        assert_eq!(drain_packets(&client).len(), 1);
        assert!(server.free_peer_slots.is_empty());

        client
            .send(&connect_datagram(1, 30, "sailwind-online"))
            .unwrap();
        client
            .send(&protocol::build_unreliable(1, b"retired-session"))
            .unwrap();
        client
            .send(&protocol::build_unreliable(2, b"current-session"))
            .unwrap();
        assert_eq!(
            server.poll(t0 + Duration::from_millis(3)),
            vec![Event::Data(1, b"current-session".to_vec())],
            "the delayed request must not retire the only slot or drop current-session traffic"
        );
        assert!(drain_packets(&client).is_empty());
        assert_eq!(server.peer_count(), 1);
        assert_eq!(server.peer_addr(1), Some(addr));
        assert_eq!(server.peers[&addr].connect_time, 40);
        assert_eq!(server.peers[&addr].connection_number, 2);
        assert_eq!(server.peers[&addr].last_recv, t0 + Duration::from_millis(3));
        assert!(server.free_peer_slots.is_empty());
        assert_peer_indices_consistent(&server);
    }

    #[test]
    fn preauth_data_flood_is_bounded_and_a_queued_payload_progresses() {
        let mut server = Host::bind("127.0.0.1:0", "sailwind-online").unwrap();
        let server_addr = server.local_addr().unwrap();
        let client = client_from("127.0.0.1", server_addr);

        client
            .send(&connect_datagram(0, 1, "sailwind-online"))
            .unwrap();
        let (events, work) = server.poll_with_work(Instant::now());
        assert_bounded_poll(work, &events);
        assert_eq!(events, vec![Event::Connected(1)]);

        let mut accept = [0u8; protocol::CONNECT_ACCEPT_SIZE];
        client.recv(&mut accept).unwrap();

        let flood = protocol::build_unreliable(0, b"flood");
        for _ in 0..MAX_POLL_PACKETS + 32 {
            client.send(&flood).unwrap();
        }
        let marker = protocol::build_unreliable(0, b"legitimate-marker");
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
            .send(&connect_datagram(0, 1, "sailwind-online"))
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
                .send(&connect_datagram(0, next_connect_time, "sailwind-online"))
                .unwrap();
            next_connect_time += 1;
        }
        client
            .send(&protocol::build_unreliable(0, b"replacement-marker"))
            .unwrap();

        let mut saw_marker = false;
        for _ in 0..8 {
            for _ in 0..MAX_POLL_PACKETS / 4 {
                client
                    .send(&connect_datagram(0, next_connect_time, "sailwind-online"))
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
            .send(&connect_datagram(0, 1, "sailwind-online"))
            .unwrap();
        assert_eq!(server.poll(Instant::now()), vec![Event::Connected(1)]);
        let mut accept = [0u8; protocol::CONNECT_ACCEPT_SIZE];
        client.recv(&mut accept).unwrap();

        let oversized = vec![protocol::property::UNRELIABLE; protocol::MTU + 1];
        client.send(&oversized).unwrap();
        client
            .send(&protocol::build_unreliable(0, b"after-oversized"))
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
                .send(&connect_datagram(0, connect_time, "sailwind-online"))
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
    fn production_peers_have_bounded_fair_keepalive_maintenance() {
        let peer_count = MAX_POLL_MAINTENANCE_SCANS + 1;
        let mut server =
            Host::bind_with_limits("127.0.0.1:0", "sailwind-online", peer_count, peer_count)
                .unwrap();
        let t0 = Instant::now();
        let clients = connect_many(&mut server, peer_count, t0);
        for client in &clients {
            let packets = drain_packets(client);
            assert!(packets.iter().any(|packet| {
                protocol::Header::from_byte(packet[0]).property
                    == protocol::property::CONNECT_ACCEPT
            }));
        }

        let due_at = t0 + PING_INTERVAL;
        let mut pinged = vec![false; peer_count];
        let mut keepalive_sends = 0;
        for _ in 0..3 {
            let (events, work) = server.poll_with_work(due_at);
            assert!(events.is_empty());
            assert_bounded_poll(work, &events);
            keepalive_sends += work.keepalive_sends;
            for (index, client) in clients.iter().enumerate() {
                pinged[index] |= drain_packets(client).iter().any(|packet| {
                    protocol::Header::from_byte(packet[0]).property == protocol::property::PING
                });
            }
            if pinged.iter().all(|ping| *ping) {
                break;
            }
        }
        assert!(
            pinged.iter().all(|ping| *ping),
            "round-robin maintenance must eventually send a keepalive to every live peer"
        );
        assert_eq!(
            keepalive_sends, peer_count,
            "each due peer must receive exactly one keepalive during a complete pass"
        );
        assert_peer_indices_consistent(&server);
    }

    #[test]
    fn production_peer_timeouts_are_scan_bounded_and_eventually_complete() {
        let peer_count = MAX_POLL_MAINTENANCE_SCANS + 1;
        let mut server =
            Host::bind_with_limits("127.0.0.1:0", "sailwind-online", peer_count, peer_count)
                .unwrap();
        let t0 = Instant::now();
        let clients = connect_many(&mut server, peer_count, t0);
        let expired_at = t0 + DEFAULT_TIMEOUT + Duration::from_secs(1);
        let mut timeout_events = 0;
        for poll_index in 0..peer_count.div_ceil(MAX_POLL_TIMEOUT_EVENTS) + 3 {
            let (events, work) = server.poll_with_work(expired_at);
            assert_bounded_poll(work, &events);
            if poll_index == 0 {
                assert_eq!(work.maintenance_scans, MAX_POLL_MAINTENANCE_SCANS);
                assert_eq!(work.timeout_events, MAX_POLL_TIMEOUT_EVENTS);
            }
            timeout_events += events
                .iter()
                .filter(|event| matches!(event, Event::Disconnected(_, DisconnectReason::Timeout)))
                .count();
            assert_peer_indices_consistent(&server);
            if server.peer_count() == 0 {
                break;
            }
        }
        assert_eq!(timeout_events, peer_count);
        assert_eq!(server.peer_count(), 0);
        assert_peer_indices_consistent(&server);
        drop(clients);
    }

    #[test]
    fn bounded_slot_ids_cannot_wrap_or_collide_under_transport_churn() {
        let mut server = Host::bind_with_limits("127.0.0.1:0", "sailwind-online", 2, 2).unwrap();
        let server_addr = server.local_addr().unwrap();
        let client = client_from("127.0.0.1", server_addr);
        let now = Instant::now();
        let mut accept = [0u8; protocol::CONNECT_ACCEPT_SIZE];

        for connect_time in 1..=10_000i64 {
            client
                .send(&connect_datagram(0, connect_time, "sailwind-online"))
                .unwrap();
            let events = server.poll(now);
            let connected = events
                .iter()
                .find_map(|event| match event {
                    Event::Connected(peer) => Some(*peer),
                    _ => None,
                })
                .expect("every replacement must connect");
            assert!(
                connected <= 2,
                "PeerId must be derived from a bounded live slot"
            );

            let n = client.recv(&mut accept).unwrap();
            assert_eq!(n, protocol::CONNECT_ACCEPT_SIZE);
            let local_peer_id = i32::from_le_bytes(accept[11..15].try_into().unwrap());
            assert!((0..2).contains(&local_peer_id));
            assert_peer_indices_consistent(&server);
        }
    }

    #[test]
    fn removed_peer_id_cannot_alias_a_cross_address_connect_in_the_same_poll() {
        let mut server = Host::bind_with_limits("127.0.0.1:0", "sailwind-online", 1, 1).unwrap();
        let server_addr = server.local_addr().unwrap();
        let old_client = client_from("127.0.0.1", server_addr);
        let new_client = client_from("127.0.0.2", server_addr);
        let now = Instant::now();

        old_client
            .send(&connect_datagram(0, 1, "sailwind-online"))
            .unwrap();
        assert_eq!(server.poll(now), vec![Event::Connected(1)]);
        assert_eq!(drain_packets(&old_client).len(), 1);

        old_client
            .send(&protocol::build_unreliable(0, b"old-before-disconnect"))
            .unwrap();
        old_client.send(&protocol::build_disconnect(0, 1)).unwrap();
        new_client
            .send(&connect_datagram(0, 2, "sailwind-online"))
            .unwrap();

        let events = server.poll(now);
        assert_eq!(
            events,
            vec![
                Event::Data(1, b"old-before-disconnect".to_vec()),
                Event::Disconnected(1, DisconnectReason::Remote),
            ]
        );
        assert_eq!(
            server.peer_addr(1),
            None,
            "an id in an earlier event must not resolve to a later peer from the same batch"
        );
        server
            .send_unreliable(1, b"must-not-reach-the-new-peer")
            .unwrap();
        assert!(
            !server.disconnect(1),
            "acting on the old event id must not disconnect the new peer"
        );
        assert!(
            drain_packets(&new_client).is_empty(),
            "the new endpoint must receive neither data nor disconnect for the old id"
        );

        new_client
            .send(&connect_datagram(0, 2, "sailwind-online"))
            .unwrap();
        assert_eq!(server.poll(now), vec![Event::Connected(1)]);
        assert_eq!(server.peer_addr(1), Some(new_client.local_addr().unwrap()));
        let packets = drain_packets(&new_client);
        assert_eq!(packets.len(), 1);
        assert_eq!(
            protocol::Header::from_byte(packets[0][0]).property,
            protocol::property::CONNECT_ACCEPT
        );
        assert_peer_indices_consistent(&server);
    }

    #[test]
    fn full_capacity_same_address_replacement_connects_on_the_next_poll_retry() {
        let mut server = Host::bind_with_limits("127.0.0.1:0", "sailwind-online", 1, 1).unwrap();
        let server_addr = server.local_addr().unwrap();
        let client = client_from("127.0.0.1", server_addr);
        let now = Instant::now();

        client
            .send(&connect_datagram(0, 1, "sailwind-online"))
            .unwrap();
        assert_eq!(server.poll(now), vec![Event::Connected(1)]);
        assert_eq!(drain_packets(&client).len(), 1);

        client
            .send(&connect_datagram(0, 2, "sailwind-online"))
            .unwrap();
        assert_eq!(
            server.poll(now),
            vec![Event::Disconnected(1, DisconnectReason::Remote)],
            "a full-capacity replacement must retire the old id for the complete event batch"
        );
        assert_eq!(server.peer_addr(1), None);
        assert!(drain_packets(&client).is_empty());

        client
            .send(&connect_datagram(0, 2, "sailwind-online"))
            .unwrap();
        assert_eq!(server.poll(now), vec![Event::Connected(1)]);
        assert_eq!(server.peer_addr(1), Some(client.local_addr().unwrap()));
        let packets = drain_packets(&client);
        assert_eq!(packets.len(), 1);
        let local_peer_id = i32::from_le_bytes(packets[0][11..15].try_into().unwrap());
        assert_eq!(local_peer_id, 0);
        assert_peer_indices_consistent(&server);
    }

    #[test]
    fn local_disconnect_during_batch_processing_keeps_remaining_events_unresolvable() {
        let mut server = Host::bind_with_limits("127.0.0.1:0", "sailwind-online", 1, 1).unwrap();
        let server_addr = server.local_addr().unwrap();
        let old_client = client_from("127.0.0.1", server_addr);
        let retry_client = client_from("127.0.0.2", server_addr);
        let now = Instant::now();

        old_client
            .send(&connect_datagram(0, 1, "sailwind-online"))
            .unwrap();
        assert_eq!(server.poll(now), vec![Event::Connected(1)]);
        assert_eq!(drain_packets(&old_client).len(), 1);

        old_client
            .send(&protocol::build_unreliable(0, b"first"))
            .unwrap();
        old_client
            .send(&protocol::build_unreliable(0, b"second"))
            .unwrap();
        let events = server.poll(now);
        assert_eq!(
            events,
            vec![
                Event::Data(1, b"first".to_vec()),
                Event::Data(1, b"second".to_vec()),
            ]
        );

        assert!(server.disconnect(1));
        for event in events.iter().skip(1) {
            let Event::Data(peer, _) = event else {
                panic!("expected the remaining data event");
            };
            assert_eq!(server.peer_addr(*peer), None);
            server.send_unreliable(*peer, b"stale-reply").unwrap();
            assert!(!server.disconnect(*peer));
        }

        retry_client
            .send(&connect_datagram(0, 2, "sailwind-online"))
            .unwrap();
        assert_eq!(server.poll(now), vec![Event::Connected(1)]);
        assert_eq!(
            server.peer_addr(1),
            Some(retry_client.local_addr().unwrap())
        );
        assert_eq!(drain_packets(&retry_client).len(), 1);
        assert_peer_indices_consistent(&server);
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

        first
            .send(&connect_datagram(0, 1, "sailwind-online"))
            .unwrap();
        assert_eq!(server.poll(Instant::now()), vec![Event::Connected(1)]);
        second
            .send(&connect_datagram(0, 2, "sailwind-online"))
            .unwrap();
        assert_eq!(server.poll(Instant::now()), vec![Event::Connected(2)]);
        assert_eq!(server.peer_count(), 2);
        assert_eq!(server.peer_count_for_ip("127.0.0.1".parse().unwrap()), 2);

        let same_source_excess = client_from("127.0.0.1", server_addr);
        same_source_excess
            .send(&connect_datagram(0, 3, "sailwind-online"))
            .unwrap();
        assert!(server.poll(Instant::now()).is_empty());
        assert_eq!(server.peer_count(), 2);

        let other_source = client_from("127.0.0.2", server_addr);
        other_source
            .send(&connect_datagram(0, 4, "sailwind-online"))
            .unwrap();
        assert_eq!(server.poll(Instant::now()), vec![Event::Connected(3)]);
        assert_eq!(server.peer_count(), 3);
        assert_eq!(server.peer_count_for_ip("127.0.0.2".parse().unwrap()), 1);

        let global_excess = client_from("127.0.0.2", server_addr);
        global_excess
            .send(&connect_datagram(0, 5, "sailwind-online"))
            .unwrap();
        assert!(server.poll(Instant::now()).is_empty());
        assert_eq!(server.peer_count(), 3);

        let mut accept = [0u8; protocol::CONNECT_ACCEPT_SIZE];
        first.recv(&mut accept).unwrap();
        first
            .send(&connect_datagram(0, 1, "sailwind-online"))
            .unwrap();
        assert!(server.poll(Instant::now()).is_empty());
        assert_eq!(
            first.recv(&mut accept).unwrap(),
            protocol::CONNECT_ACCEPT_SIZE
        );
        assert_eq!(server.peer_count(), 3);

        first
            .send(&connect_datagram(0, 6, "sailwind-online"))
            .unwrap();
        assert_eq!(
            server.poll(Instant::now()),
            vec![Event::Disconnected(1, DisconnectReason::Remote)]
        );
        assert_eq!(server.peer_count(), 2);
        assert_eq!(server.peer_count_for_ip("127.0.0.1".parse().unwrap()), 1);
        assert_eq!(server.peer_addr(1), None);
        assert_peer_indices_consistent(&server);

        first
            .send(&connect_datagram(0, 6, "sailwind-online"))
            .unwrap();
        assert_eq!(server.poll(Instant::now()), vec![Event::Connected(1)]);
        assert_eq!(server.peer_count(), 3);
        assert_eq!(server.peer_count_for_ip("127.0.0.1".parse().unwrap()), 2);
        assert_peer_indices_consistent(&server);

        assert!(server.disconnect(1));
        assert_eq!(server.peer_count(), 2);
        assert_eq!(server.peer_count_for_ip("127.0.0.1".parse().unwrap()), 1);
        assert_peer_indices_consistent(&server);
        server.shutdown();
        assert_eq!(server.peer_count_for_ip("127.0.0.1".parse().unwrap()), 0);
        assert_eq!(server.peer_count_for_ip("127.0.0.2".parse().unwrap()), 0);
        assert_peer_indices_consistent(&server);
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
            .send(&connect_datagram(0, 0x1234, "sailwind-online"))
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
        let payload = protocol::build_unreliable(0, b"hello-server");
        client.send(&payload).unwrap();
        let events = server.poll(Instant::now());
        assert_eq!(events, vec![Event::Data(1, b"hello-server".to_vec())]);

        // 3) Server -> client unreliable send.
        server.send_unreliable(1, b"hello-client").unwrap();
        let n = client.recv(&mut buf).unwrap();
        assert_eq!(
            &buf[..n],
            &protocol::build_unreliable(0, b"hello-client")[..]
        );

        // 4) Disconnect.
        client.send(&protocol::build_disconnect(0, 0x1234)).unwrap();
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

        client.send(&connect_datagram(0, 1, "wrong-key")).unwrap();
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
            .send(&connect_datagram(0, 1, "sailwind-online"))
            .unwrap();
        server.poll(Instant::now());
        let mut buf = [0u8; 64];
        let _ = client.recv(&mut buf).unwrap(); // drain accept

        client.send(&protocol::build_ping(0, 0x00AB)).unwrap();
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
            .send(&connect_datagram(0, 1, "sailwind-online"))
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
