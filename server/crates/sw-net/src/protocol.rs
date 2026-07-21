//! LiteNetLib 1.x wire-format primitives.
//!
//! Byte layouts are transcribed from the LiteNetLib 1.x source (the line that
//! the NuGet `LiteNetLib 1.3.1` client used by the game/protocol-smoke sits on;
//! it shares its wire format with the tagged `v1.2.0` release — the C# constants
//! `NetConstants.ProtocolId = 13`, `NetPacket`'s property-byte masks, and the
//! `NetConnectRequestPacket` / `NetConnectAcceptPacket` structures).
//!
//! Only the init-0 subset is implemented: the property-byte header, the
//! ConnectRequest/ConnectAccept handshake, the Unreliable data channel, and
//! Ping/Pong/Disconnect. No reliable channels, no fragmentation, no packet
//! merging.

/// LiteNetLib protocol id (`NetConstants.ProtocolId`). A ConnectRequest whose
/// embedded id differs is answered with `InvalidProtocol`.
pub const PROTOCOL_ID: i32 = 13;

/// `NetConstants.MaxConnectionNumber` — connection numbers are 2 bits (0..=3).
pub const MAX_CONNECTION_NUMBER: u8 = 4;

/// Fixed MTU for init-0 (`NetConstants.PossibleMtu[1]`, "most games standard").
pub const MTU: usize = 1024;

/// Base header size (`NetConstants.HeaderSize`) — the single property byte.
pub const HEADER_SIZE: usize = 1;

/// `NetConnectRequestPacket.HeaderSize`: property + protocol id (4) +
/// connect time (8) + peer id (4) + address-size byte (1).
pub const CONNECT_REQUEST_HEADER_SIZE: usize = 18;

/// `NetConnectAcceptPacket.Size`: property + connect time (8) + connection
/// number (1) + reused flag (1) + peer id (4).
pub const CONNECT_ACCEPT_SIZE: usize = 15;

/// Ping packet size: property + sequence (2).
pub const PING_SIZE: usize = HEADER_SIZE + 2;

/// Pong packet size: property + sequence (2) + remote time ticks (8).
pub const PONG_SIZE: usize = HEADER_SIZE + 10;

/// Disconnect packet size: property + connect time (8).
pub const DISCONNECT_SIZE: usize = HEADER_SIZE + 8;

/// `PacketProperty` discriminants (the low 5 bits of the first byte).
pub mod property {
    pub const UNRELIABLE: u8 = 0;
    pub const CHANNELED: u8 = 1;
    pub const ACK: u8 = 2;
    pub const PING: u8 = 3;
    pub const PONG: u8 = 4;
    pub const CONNECT_REQUEST: u8 = 5;
    pub const CONNECT_ACCEPT: u8 = 6;
    pub const DISCONNECT: u8 = 7;
    pub const UNCONNECTED_MESSAGE: u8 = 8;
    pub const MTU_CHECK: u8 = 9;
    pub const MTU_OK: u8 = 10;
    pub const BROADCAST: u8 = 11;
    pub const MERGED: u8 = 12;
    pub const SHUTDOWN_OK: u8 = 13;
    pub const PEER_NOT_FOUND: u8 = 14;
    pub const INVALID_PROTOCOL: u8 = 15;
    pub const NAT_MESSAGE: u8 = 16;
    pub const EMPTY: u8 = 17;
}

const PROPERTY_MASK: u8 = 0x1F;
const CONNECTION_NUMBER_MASK: u8 = 0x60;
const CONNECTION_NUMBER_SHIFT: u8 = 5;
const FRAGMENTED_FLAG: u8 = 0x80;

/// The decoded first byte of every LiteNetLib packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub property: u8,
    pub connection_number: u8,
    pub fragmented: bool,
}

impl Header {
    /// Decode a header from the packet's first byte.
    #[inline]
    pub fn from_byte(b: u8) -> Header {
        Header {
            property: b & PROPERTY_MASK,
            connection_number: (b & CONNECTION_NUMBER_MASK) >> CONNECTION_NUMBER_SHIFT,
            fragmented: (b & FRAGMENTED_FLAG) != 0,
        }
    }

    /// Encode this header back into a single byte.
    #[inline]
    pub fn to_byte(self) -> u8 {
        (self.property & PROPERTY_MASK)
            | ((self.connection_number << CONNECTION_NUMBER_SHIFT) & CONNECTION_NUMBER_MASK)
            | if self.fragmented { FRAGMENTED_FLAG } else { 0 }
    }

    /// A plain (non-fragmented, connection-number 0) header for `property`.
    #[inline]
    pub fn plain(property: u8) -> Header {
        Header {
            property,
            connection_number: 0,
            fragmented: false,
        }
    }
}

/// A parsed ConnectRequest packet (fields after the LiteNetLib target address).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectRequest {
    pub connection_number: u8,
    pub protocol_id: i32,
    pub connect_time: i64,
    pub peer_id: i32,
    /// Application connect data (the `NetDataWriter`-encoded connect key).
    pub connect_data: Vec<u8>,
}

/// Parse a ConnectRequest datagram. Returns `None` on any structural mismatch.
pub fn parse_connect_request(buf: &[u8]) -> Option<ConnectRequest> {
    if buf.len() < CONNECT_REQUEST_HEADER_SIZE {
        return None;
    }
    let header = Header::from_byte(buf[0]);
    if header.property != property::CONNECT_REQUEST {
        return None;
    }
    if header.connection_number >= MAX_CONNECTION_NUMBER {
        return None;
    }
    let protocol_id = i32::from_le_bytes(buf[1..5].try_into().ok()?);
    let connect_time = i64::from_le_bytes(buf[5..13].try_into().ok()?);
    let peer_id = i32::from_le_bytes(buf[13..17].try_into().ok()?);
    // Byte at HeaderSize-1 is the serialized SocketAddress length: 16 (IPv4) or
    // 28 (IPv6). The address bytes themselves are irrelevant to us.
    let addr_size = buf[CONNECT_REQUEST_HEADER_SIZE - 1] as usize;
    if addr_size != 16 && addr_size != 28 {
        return None;
    }
    let data_start = CONNECT_REQUEST_HEADER_SIZE + addr_size;
    if buf.len() < data_start {
        return None;
    }
    Some(ConnectRequest {
        connection_number: header.connection_number,
        protocol_id,
        connect_time,
        peer_id,
        connect_data: buf[data_start..].to_vec(),
    })
}

/// Decode a `NetDataWriter`-encoded string: a little-endian `u16` length,
/// where 0 means empty and otherwise `len - 1` UTF-8 bytes follow.
pub fn read_litenet_string(data: &[u8]) -> Option<String> {
    if data.len() < 2 {
        return None;
    }
    let raw = u16::from_le_bytes([data[0], data[1]]);
    if raw == 0 {
        return Some(String::new());
    }
    let n = (raw - 1) as usize;
    if data.len() < 2 + n {
        return None;
    }
    String::from_utf8(data[2..2 + n].to_vec()).ok()
}

/// Encode a string the way `NetDataWriter.Put(string)` does (for tests and for
/// building connect data). Empty strings encode as a bare `0u16`.
pub fn write_litenet_string(s: &str) -> Vec<u8> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(2 + bytes.len());
    if bytes.is_empty() {
        out.extend_from_slice(&0u16.to_le_bytes());
        return out;
    }
    let len = (bytes.len() + 1) as u16;
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(bytes);
    out
}

/// Build a ConnectAccept packet echoing the client's `connect_time`.
pub fn build_connect_accept(
    connect_time: i64,
    connection_number: u8,
    local_peer_id: i32,
    reused: bool,
) -> [u8; CONNECT_ACCEPT_SIZE] {
    let mut b = [0u8; CONNECT_ACCEPT_SIZE];
    b[0] = Header::plain(property::CONNECT_ACCEPT).to_byte();
    b[1..9].copy_from_slice(&connect_time.to_le_bytes());
    b[9] = connection_number;
    b[10] = u8::from(reused);
    b[11..15].copy_from_slice(&local_peer_id.to_le_bytes());
    b
}

/// Wrap `data` in an Unreliable packet (property byte + payload).
pub fn build_unreliable(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_SIZE + data.len());
    out.push(Header::plain(property::UNRELIABLE).to_byte());
    out.extend_from_slice(data);
    out
}

/// Build a Ping packet with the given sequence number.
pub fn build_ping(sequence: u16) -> [u8; PING_SIZE] {
    let mut b = [0u8; PING_SIZE];
    b[0] = Header::plain(property::PING).to_byte();
    b[1..3].copy_from_slice(&sequence.to_le_bytes());
    b
}

/// Build a Pong packet echoing `sequence`, carrying our local time in .NET ticks.
pub fn build_pong(sequence: u16, time_ticks: i64) -> [u8; PONG_SIZE] {
    let mut b = [0u8; PONG_SIZE];
    b[0] = Header::plain(property::PONG).to_byte();
    b[1..3].copy_from_slice(&sequence.to_le_bytes());
    b[3..11].copy_from_slice(&time_ticks.to_le_bytes());
    b
}

/// Read the sequence number from a Ping or Pong packet.
pub fn read_sequence(buf: &[u8]) -> Option<u16> {
    if buf.len() < 3 {
        return None;
    }
    Some(u16::from_le_bytes([buf[1], buf[2]]))
}

/// Build a Disconnect packet carrying `connect_time` (validated by the peer).
pub fn build_disconnect(connect_time: i64) -> [u8; DISCONNECT_SIZE] {
    let mut b = [0u8; DISCONNECT_SIZE];
    b[0] = Header::plain(property::DISCONNECT).to_byte();
    b[1..9].copy_from_slice(&connect_time.to_le_bytes());
    b
}

/// Read the `connect_time` from a Disconnect packet.
pub fn read_disconnect_time(buf: &[u8]) -> Option<i64> {
    if buf.len() < DISCONNECT_SIZE {
        return None;
    }
    Some(i64::from_le_bytes(buf[1..9].try_into().ok()?))
}

/// A single-byte control packet (`ShutdownOk`, `InvalidProtocol`, ...).
pub fn build_control(property: u8) -> [u8; HEADER_SIZE] {
    [Header::plain(property).to_byte()]
}

/// Build a full ConnectRequest exactly as the LiteNetLib client would, used by
/// the golden tests (and as executable documentation of the format).
pub fn build_connect_request(
    connection_number: u8,
    connect_time: i64,
    peer_id: i32,
    addr_size: u8,
    connect_data: &[u8],
) -> Vec<u8> {
    let mut b = vec![0u8; CONNECT_REQUEST_HEADER_SIZE + addr_size as usize + connect_data.len()];
    b[0] = Header {
        property: property::CONNECT_REQUEST,
        connection_number,
        fragmented: false,
    }
    .to_byte();
    b[1..5].copy_from_slice(&PROTOCOL_ID.to_le_bytes());
    b[5..13].copy_from_slice(&connect_time.to_le_bytes());
    b[13..17].copy_from_slice(&peer_id.to_le_bytes());
    b[CONNECT_REQUEST_HEADER_SIZE - 1] = addr_size;
    // The address bytes stay zeroed — their content is never inspected.
    let data_start = CONNECT_REQUEST_HEADER_SIZE + addr_size as usize;
    b[data_start..].copy_from_slice(connect_data);
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_byte_roundtrip_golden() {
        // property=ConnectRequest(5), connection_number=1 -> 0x25.
        let h = Header {
            property: property::CONNECT_REQUEST,
            connection_number: 1,
            fragmented: false,
        };
        assert_eq!(h.to_byte(), 0x25);
        assert_eq!(Header::from_byte(0x25), h);

        // property=Pong(4), fragmented -> 0x84.
        let f = Header {
            property: property::PONG,
            connection_number: 0,
            fragmented: true,
        };
        assert_eq!(f.to_byte(), 0x84);
        assert_eq!(Header::from_byte(0x84), f);

        // connection_number is 2 bits: value 3 shifts to 0x60.
        let c = Header {
            property: property::UNRELIABLE,
            connection_number: 3,
            fragmented: false,
        };
        assert_eq!(c.to_byte(), 0x60);
        assert_eq!(Header::from_byte(0x60), c);
    }

    #[test]
    fn header_from_byte_is_total() {
        for b in 0u16..=255 {
            let b = b as u8;
            // Re-encoding a decoded header reproduces the low-7-bit meaning; the
            // top bit (fragmented) round-trips too, so the whole byte is stable.
            assert_eq!(Header::from_byte(b).to_byte(), b);
        }
    }

    #[test]
    fn connect_accept_golden_bytes() {
        let b = build_connect_accept(0x0102_0304_0506_0708, 2, 0x1122_3344, false);
        assert_eq!(
            b,
            [
                property::CONNECT_ACCEPT, // 0x06
                0x08,
                0x07,
                0x06,
                0x05,
                0x04,
                0x03,
                0x02,
                0x01, // connect_time LE
                0x02, // connection number
                0x00, // reused = false
                0x44,
                0x33,
                0x22,
                0x11, // peer id LE
            ]
        );
    }

    #[test]
    fn connect_accept_reused_flag() {
        let b = build_connect_accept(0, 0, 0, true);
        assert_eq!(b[10], 1);
    }

    #[test]
    fn ping_pong_golden_bytes() {
        assert_eq!(build_ping(0x0201), [property::PING, 0x01, 0x02]);
        let pong = build_pong(0x0201, 0x0A09_0807_0605_0403);
        assert_eq!(
            pong,
            [
                property::PONG,
                0x01,
                0x02, // sequence LE
                0x03,
                0x04,
                0x05,
                0x06,
                0x07,
                0x08,
                0x09,
                0x0A, // ticks LE
            ]
        );
        assert_eq!(read_sequence(&pong), Some(0x0201));
    }

    #[test]
    fn disconnect_golden_roundtrip() {
        let d = build_disconnect(0x0102_0304_0506_0708);
        assert_eq!(d[0], property::DISCONNECT);
        assert_eq!(read_disconnect_time(&d), Some(0x0102_0304_0506_0708));
    }

    #[test]
    fn unreliable_wraps_payload() {
        let p = build_unreliable(&[0xDE, 0xAD, 0xBE, 0xEF]);
        assert_eq!(p, [property::UNRELIABLE, 0xDE, 0xAD, 0xBE, 0xEF]);
    }

    #[test]
    fn litenet_string_roundtrip() {
        let key = "sailwind-online";
        let enc = write_litenet_string(key);
        // 15 UTF-8 bytes -> length prefix 16 (0x10, 0x00).
        assert_eq!(enc[0], 0x10);
        assert_eq!(enc[1], 0x00);
        assert_eq!(enc.len(), 2 + 15);
        assert_eq!(read_litenet_string(&enc).as_deref(), Some(key));

        let empty = write_litenet_string("");
        assert_eq!(empty, vec![0x00, 0x00]);
        assert_eq!(read_litenet_string(&empty).as_deref(), Some(""));
    }

    #[test]
    fn connect_request_roundtrips_through_parser() {
        let data = write_litenet_string("sailwind-online");
        let dg = build_connect_request(0, 0x1122_3344_5566_7788, 42, 16, &data);
        let req = parse_connect_request(&dg).expect("parse");
        assert_eq!(req.connection_number, 0);
        assert_eq!(req.protocol_id, PROTOCOL_ID);
        assert_eq!(req.connect_time, 0x1122_3344_5566_7788);
        assert_eq!(req.peer_id, 42);
        assert_eq!(
            read_litenet_string(&req.connect_data).as_deref(),
            Some("sailwind-online")
        );
    }

    #[test]
    fn connect_request_rejects_bad_addr_size() {
        let data = write_litenet_string("k");
        let dg = build_connect_request(0, 1, 1, 20, &data); // 20 is neither 16 nor 28
        assert!(parse_connect_request(&dg).is_none());
    }

    #[test]
    fn connect_request_rejects_truncated() {
        assert!(parse_connect_request(&[property::CONNECT_REQUEST; 4]).is_none());
    }
}
