//! FlatBuffers encoders for every server-to-client message.
//!
//! Each function builds a complete, identifier-stamped `Envelope` and returns
//! the owned bytes. Plain, allocation-simple building — fine for init-0 scale.

use crate::clock::WorldClock;
use flatbuffers::{FlatBufferBuilder, ForwardsUOffset, Vector, WIPOffset};
use sw_contracts::sw_proto as p;
use sw_contracts::{finish_envelope, PROTOCOL_VERSION};
use sw_world::Cell;

/// A player's presence, ready to serialize into a `PlayerState`.
pub struct PlayerSnap {
    pub player_id: u64,
    pub pos: [f32; 3],
    pub rot: [f32; 4],
    pub aboard_boat: u64,
    pub t_ms: u32,
}

/// A boat's state, ready to serialize into a `BoatState`.
pub struct BoatSnap {
    pub boat_id: u64,
    pub owner: u64,
    pub pos: [f32; 3],
    pub rot: [f32; 4],
    pub vel: [f32; 3],
    pub t_ms: u32,
}

/// A mooring, ready to serialize into a `MoorageRecord`.
pub struct MooringSnap {
    pub boat_id: u64,
    pub owner: u64,
    pub cell: (i32, i32),
    pub pos: [f32; 3],
    pub rot: [f32; 4],
    pub name: String,
    pub created_at: u64,
}

/// Capability manifest values advertised in `ServerHello`.
pub struct Caps {
    pub tick_hz: u8,
    pub snapshot_hz: u8,
    pub aoi_radius_cells: u8,
    pub cell_size_m: f32,
    pub features: &'static [&'static str],
}

/// Encode a `ServerHello`.
#[allow(clippy::too_many_arguments)]
pub fn server_hello(
    seq: u32,
    accepted: bool,
    reason: &str,
    player_id: u64,
    server_name: &str,
    balance: i64,
    caps: &Caps,
    clock: WorldClock,
    weather_seed: u64,
    weather_epoch_day: u32,
) -> Vec<u8> {
    let mut fbb = FlatBufferBuilder::new();
    let reason_off = fbb.create_string(reason);
    let name_off = fbb.create_string(server_name);
    let feature_offs: Vec<WIPOffset<&str>> =
        caps.features.iter().map(|f| fbb.create_string(f)).collect();
    let features = fbb.create_vector(&feature_offs);
    let caps_off = p::CapabilityManifest::create(
        &mut fbb,
        &p::CapabilityManifestArgs {
            protocol_version: PROTOCOL_VERSION,
            features: Some(features),
            tick_hz: caps.tick_hz,
            snapshot_hz: caps.snapshot_hz,
            aoi_radius_cells: caps.aoi_radius_cells,
            cell_size_m: caps.cell_size_m,
        },
    );
    let clock_off = p::WorldClock::create(
        &mut fbb,
        &p::WorldClockArgs {
            day: clock.day,
            time_of_day: clock.time_of_day,
            moon_phase: clock.moon_phase,
        },
    );
    let weather_off = p::WeatherSeed::create(
        &mut fbb,
        &p::WeatherSeedArgs {
            seed: weather_seed,
            epoch_day: weather_epoch_day,
        },
    );
    let hello = p::ServerHello::create(
        &mut fbb,
        &p::ServerHelloArgs {
            accepted,
            reason: Some(reason_off),
            player_id,
            server_name: Some(name_off),
            balance_gold: balance,
            capabilities: Some(caps_off),
            clock: Some(clock_off),
            weather: Some(weather_off),
        },
    );
    finish_envelope(
        &mut fbb,
        seq,
        p::Payload::ServerHello,
        hello.as_union_value(),
    )
}

/// Encode a `SnapshotDelta`.
pub fn snapshot_delta(
    seq: u32,
    server_tick: u32,
    players: &[PlayerSnap],
    boats: &[BoatSnap],
) -> Vec<u8> {
    let mut fbb = FlatBufferBuilder::new();
    let players_vec = build_players(&mut fbb, players);
    let boats_vec = build_boats(&mut fbb, boats);
    let sd = p::SnapshotDelta::create(
        &mut fbb,
        &p::SnapshotDeltaArgs {
            server_tick,
            players: Some(players_vec),
            boats: Some(boats_vec),
        },
    );
    finish_envelope(
        &mut fbb,
        seq,
        p::Payload::SnapshotDelta,
        sd.as_union_value(),
    )
}

/// Encode an `AoiUpdate`.
pub fn aoi_update(seq: u32, added: &[Cell], removed: &[Cell]) -> Vec<u8> {
    let mut fbb = FlatBufferBuilder::new();
    let added_cells: Vec<p::GridCell> =
        added.iter().map(|c| p::GridCell::new(c.cx, c.cz)).collect();
    let removed_cells: Vec<p::GridCell> = removed
        .iter()
        .map(|c| p::GridCell::new(c.cx, c.cz))
        .collect();
    let added_vec = fbb.create_vector(&added_cells);
    let removed_vec = fbb.create_vector(&removed_cells);
    let au = p::AoiUpdate::create(
        &mut fbb,
        &p::AoiUpdateArgs {
            added: Some(added_vec),
            removed: Some(removed_vec),
        },
    );
    finish_envelope(&mut fbb, seq, p::Payload::AoiUpdate, au.as_union_value())
}

/// Encode a `CellSnapshot`.
pub fn cell_snapshot(
    seq: u32,
    cell: Cell,
    players: &[PlayerSnap],
    boats: &[BoatSnap],
    moorings: &[MooringSnap],
) -> Vec<u8> {
    let mut fbb = FlatBufferBuilder::new();
    let players_vec = build_players(&mut fbb, players);
    let boats_vec = build_boats(&mut fbb, boats);
    let moorings_vec = build_moorings(&mut fbb, moorings);
    let gc = p::GridCell::new(cell.cx, cell.cz);
    let cs = p::CellSnapshot::create(
        &mut fbb,
        &p::CellSnapshotArgs {
            cell: Some(&gc),
            players: Some(players_vec),
            boats: Some(boats_vec),
            moorings: Some(moorings_vec),
        },
    );
    finish_envelope(&mut fbb, seq, p::Payload::CellSnapshot, cs.as_union_value())
}

/// Encode a `MoorAck`.
pub fn moor_ack(seq: u32, accepted: bool, record: Option<&MooringSnap>, reason: &str) -> Vec<u8> {
    let mut fbb = FlatBufferBuilder::new();
    let reason_off = fbb.create_string(reason);
    let record_off = record.map(|m| build_mooring(&mut fbb, m));
    let ma = p::MoorAck::create(
        &mut fbb,
        &p::MoorAckArgs {
            accepted,
            record: record_off,
            reason: Some(reason_off),
        },
    );
    finish_envelope(&mut fbb, seq, p::Payload::MoorAck, ma.as_union_value())
}

/// Encode a `LedgerAck`.
pub fn ledger_ack(seq: u32, ack: &sw_econ::LedgerAck) -> Vec<u8> {
    let mut fbb = FlatBufferBuilder::new();
    let reason_off = fbb.create_string(&ack.reason);
    let la = p::LedgerAck::create(
        &mut fbb,
        &p::LedgerAckArgs {
            txn_id: ack.txn_id,
            accepted: ack.accepted,
            new_balance: ack.new_balance,
            reason: Some(reason_off),
        },
    );
    finish_envelope(&mut fbb, seq, p::Payload::LedgerAck, la.as_union_value())
}

/// Encode a `ChatBroadcast`.
pub fn chat_broadcast(
    seq: u32,
    player_id: u64,
    display_name: &str,
    text: &str,
    channel: u8,
    t_ms: u32,
) -> Vec<u8> {
    let mut fbb = FlatBufferBuilder::new();
    let name_off = fbb.create_string(display_name);
    let text_off = fbb.create_string(text);
    let cb = p::ChatBroadcast::create(
        &mut fbb,
        &p::ChatBroadcastArgs {
            player_id,
            display_name: Some(name_off),
            text: Some(text_off),
            channel,
            t_ms,
        },
    );
    finish_envelope(
        &mut fbb,
        seq,
        p::Payload::ChatBroadcast,
        cb.as_union_value(),
    )
}

fn build_players<'a>(
    fbb: &mut FlatBufferBuilder<'a>,
    players: &[PlayerSnap],
) -> WIPOffset<Vector<'a, ForwardsUOffset<p::PlayerState<'a>>>> {
    let mut offsets = Vec::with_capacity(players.len());
    for pl in players {
        let pos = p::Vec3::new(pl.pos[0], pl.pos[1], pl.pos[2]);
        let rot = p::QuatC::new(pl.rot[0], pl.rot[1], pl.rot[2], pl.rot[3]);
        let off = p::PlayerState::create(
            fbb,
            &p::PlayerStateArgs {
                player_id: pl.player_id,
                pos: Some(&pos),
                rot: Some(&rot),
                aboard_boat: pl.aboard_boat,
                t_ms: pl.t_ms,
            },
        );
        offsets.push(off);
    }
    fbb.create_vector(&offsets)
}

fn build_boats<'a>(
    fbb: &mut FlatBufferBuilder<'a>,
    boats: &[BoatSnap],
) -> WIPOffset<Vector<'a, ForwardsUOffset<p::BoatState<'a>>>> {
    let mut offsets = Vec::with_capacity(boats.len());
    for b in boats {
        let pos = p::Vec3::new(b.pos[0], b.pos[1], b.pos[2]);
        let rot = p::QuatC::new(b.rot[0], b.rot[1], b.rot[2], b.rot[3]);
        let vel = p::Vec3::new(b.vel[0], b.vel[1], b.vel[2]);
        let off = p::BoatState::create(
            fbb,
            &p::BoatStateArgs {
                boat_id: b.boat_id,
                owner: b.owner,
                pos: Some(&pos),
                rot: Some(&rot),
                vel: Some(&vel),
                t_ms: b.t_ms,
            },
        );
        offsets.push(off);
    }
    fbb.create_vector(&offsets)
}

fn build_moorings<'a>(
    fbb: &mut FlatBufferBuilder<'a>,
    moorings: &[MooringSnap],
) -> WIPOffset<Vector<'a, ForwardsUOffset<p::MoorageRecord<'a>>>> {
    let mut offsets = Vec::with_capacity(moorings.len());
    for m in moorings {
        offsets.push(build_mooring(fbb, m));
    }
    fbb.create_vector(&offsets)
}

fn build_mooring<'a>(
    fbb: &mut FlatBufferBuilder<'a>,
    m: &MooringSnap,
) -> WIPOffset<p::MoorageRecord<'a>> {
    let name = fbb.create_string(&m.name);
    let cell = p::GridCell::new(m.cell.0, m.cell.1);
    let pos = p::Vec3::new(m.pos[0], m.pos[1], m.pos[2]);
    let rot = p::QuatC::new(m.rot[0], m.rot[1], m.rot[2], m.rot[3]);
    p::MoorageRecord::create(
        fbb,
        &p::MoorageRecordArgs {
            boat_id: m.boat_id,
            owner: m.owner,
            cell: Some(&cell),
            pos: Some(&pos),
            rot: Some(&rot),
            name: Some(name),
            created_at: m.created_at,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use sw_contracts::decode_envelope;

    #[test]
    fn server_hello_roundtrips() {
        let caps = Caps {
            tick_hz: 30,
            snapshot_hz: 4,
            aoi_radius_cells: 2,
            cell_size_m: 1024.0,
            features: &["econ", "chat"],
        };
        let clock = WorldClock {
            day: 3,
            time_of_day: 0.25,
            moon_phase: 0.1,
        };
        let bytes = server_hello(1, true, "", 42, "Test", 500, &caps, clock, 999, 0);
        let env = decode_envelope(&bytes).unwrap();
        let hello = env.payload_as_server_hello().unwrap();
        assert!(hello.accepted());
        assert_eq!(hello.player_id(), 42);
        assert_eq!(hello.balance_gold(), 500);
        let c = hello.capabilities().unwrap();
        assert_eq!(c.protocol_version(), PROTOCOL_VERSION);
        assert_eq!(c.tick_hz(), 30);
        assert_eq!(hello.clock().unwrap().day(), 3);
        assert_eq!(hello.weather().unwrap().seed(), 999);
    }

    #[test]
    fn snapshot_delta_roundtrips() {
        let players = vec![PlayerSnap {
            player_id: 7,
            pos: [1.0, 2.0, 3.0],
            rot: [0.0, 0.0, 0.0, 1.0],
            aboard_boat: 0,
            t_ms: 10,
        }];
        let boats = vec![BoatSnap {
            boat_id: 5,
            owner: 7,
            pos: [4.0, 0.0, 6.0],
            rot: [0.0, 0.0, 0.0, 1.0],
            vel: [1.0, 0.0, 0.0],
            t_ms: 10,
        }];
        let bytes = snapshot_delta(9, 100, &players, &boats);
        let env = decode_envelope(&bytes).unwrap();
        assert_eq!(env.seq(), 9);
        let sd = env.payload_as_snapshot_delta().unwrap();
        assert_eq!(sd.server_tick(), 100);
        let ps = sd.players().unwrap();
        assert_eq!(ps.len(), 1);
        assert_eq!(ps.get(0).player_id(), 7);
        assert_eq!(ps.get(0).pos().unwrap().x(), 1.0);
        let bs = sd.boats().unwrap();
        assert_eq!(bs.get(0).boat_id(), 5);
    }

    #[test]
    fn cell_snapshot_and_moor_ack_roundtrip() {
        let m = MooringSnap {
            boat_id: 1,
            owner: 2,
            cell: (3, -2),
            pos: [1.0, 0.0, 2.0],
            rot: [0.0, 0.0, 0.0, 1.0],
            name: "Dock".into(),
            created_at: 55,
        };
        let bytes = cell_snapshot(1, Cell::new(3, -2), &[], &[], std::slice::from_ref(&m));
        let env = decode_envelope(&bytes).unwrap();
        let cs = env.payload_as_cell_snapshot().unwrap();
        assert_eq!(cs.cell().unwrap().cx(), 3);
        assert_eq!(cs.moorings().unwrap().get(0).name(), Some("Dock"));

        let ack_bytes = moor_ack(2, true, Some(&m), "ok");
        let env = decode_envelope(&ack_bytes).unwrap();
        let ack = env.payload_as_moor_ack().unwrap();
        assert!(ack.accepted());
        assert_eq!(ack.record().unwrap().boat_id(), 1);
    }

    #[test]
    fn ledger_ack_and_chat_roundtrip() {
        let ack = sw_econ::LedgerAck {
            txn_id: 1,
            accepted: true,
            new_balance: 100,
            reason: String::new(),
        };
        let bytes = ledger_ack(1, &ack);
        let env = decode_envelope(&bytes).unwrap();
        let la = env.payload_as_ledger_ack().unwrap();
        assert_eq!(la.new_balance(), 100);

        let bytes = chat_broadcast(2, 7, "Skipper", "ahoy", 0, 123);
        let env = decode_envelope(&bytes).unwrap();
        let cb = env.payload_as_chat_broadcast().unwrap();
        assert_eq!(cb.text(), Some("ahoy"));
        assert_eq!(cb.player_id(), 7);
    }
}
