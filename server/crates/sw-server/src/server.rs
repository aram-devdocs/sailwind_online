//! The authoritative server: state, message handlers, and the fixed-tick loop.

use crate::clock::{clock_from_epoch, WorldClock};
use crate::codec::{self, BoatSnap, Caps, MooringSnap, PlayerSnap};
use crate::config::Config;
use crate::econ_store::{DbLedgerStore, DbMarketStore};
use crate::ratelimit::RateLimiter;
use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use sw_contracts::decode_envelope;
use sw_contracts::sw_proto as p;
use sw_econ::{Ledger, Market, MarketAck, Trade, Txn};
use sw_net::{DisconnectReason, Event, Host, PeerId};
use sw_persist::{Db, MooringRow};
use sw_world::{AoiUpdate, Cell, Subscription, World};

/// LiteNetLib connect key clients must present.
const CONNECT_KEY: &str = "sailwind-online";

/// Feature flags advertised in the capability manifest.
const FEATURES: &[&str] = &["unreliable", "aoi", "econ", "market", "moorage", "chat"];

/// `world` table keys.
const KEY_CLOCK_EPOCH: &str = "clock_epoch_ms";
const KEY_WEATHER_SEED: &str = "weather_seed";

/// How often dirty player state is flushed to the database.
const FLUSH_INTERVAL: Duration = Duration::from_secs(5);

/// Per-connection state, created on ClientHello.
struct Session {
    peer: PeerId,
    player_id: u64,
    display_name: String,
    aboard_boat: u64,
    pos: [f32; 3],
    rot: [f32; 4],
    vel: [f32; 3],
    t_ms: u32,
    sub: Subscription,
    cell: Option<Cell>,
    dirty: bool,
}

/// The server.
pub struct Server {
    cfg: Config,
    host: Host,
    db: Db,
    world: World,
    sessions: HashMap<PeerId, Session>,
    seq: u32,
    snapshot_tick: u32,
    boot: Instant,
    epoch_ms: i64,
    weather_seed: u64,
    weather_epoch_day: u32,
    trade_limiter: RateLimiter,
    running: Arc<AtomicBool>,
}

impl Server {
    /// Build the server: open + migrate the DB, initialise world state, bind
    /// the socket. Fails before the readiness line if binding fails.
    pub fn new(cfg: Config, running: Arc<AtomicBool>) -> anyhow::Result<Server> {
        let db = Db::open(&cfg.db_path)?;

        // World clock epoch: first boot stamps "now"; later boots reuse it.
        let now = now_ms();
        let epoch_ms: i64 = db
            .world_get_or_init(KEY_CLOCK_EPOCH, &now.to_string())?
            .parse()
            .unwrap_or(now);

        // Weather seed: generated once, then persisted.
        let seed_str =
            db.world_get_or_init(KEY_WEATHER_SEED, &rand::random::<u64>().to_string())?;
        let weather_seed: u64 = seed_str.parse().unwrap_or(0);
        let weather_epoch_day = clock_from_epoch(epoch_ms, epoch_ms).day;

        let host = Host::bind(&cfg.bind, CONNECT_KEY)?;
        let world = World::new(sw_world::Grid::new(cfg.cell_size_m));
        let trade_limiter = RateLimiter::new(cfg.trade_min_interval_ms_i64());

        Ok(Server {
            cfg,
            host,
            db,
            world,
            sessions: HashMap::new(),
            seq: 0,
            snapshot_tick: 0,
            boot: Instant::now(),
            epoch_ms,
            weather_seed,
            weather_epoch_day,
            trade_limiter,
            running,
        })
    }

    /// Run the fixed-tick loop until ctrl-c flips `running` to false.
    pub fn run(&mut self) -> anyhow::Result<()> {
        let addr = self.host.local_addr()?;
        // Readiness line on stdout (the protocol-smoke harness waits for this).
        println!("listening on {addr}");
        std::io::stdout().flush().ok();
        tracing::info!(%addr, server = %self.cfg.server_name, "server started");

        let tick_dt = Duration::from_secs_f64(1.0 / self.cfg.tick_hz as f64);
        let ticks_per_snapshot = self.cfg.ticks_per_snapshot();
        let ticks_per_clock_broadcast = self.cfg.ticks_per_clock_broadcast();
        let mut tick: u64 = 0;
        let mut last_flush = Instant::now();

        while self.running.load(Ordering::SeqCst) {
            let frame_start = Instant::now();

            let events = self.host.poll(frame_start);
            for ev in events {
                if let Err(e) = self.handle_event(ev) {
                    tracing::warn!(error = %e, "event handling failed");
                }
            }

            if tick % ticks_per_snapshot == 0 {
                self.broadcast_snapshots();
            }

            if tick % ticks_per_clock_broadcast == 0 {
                self.broadcast_clock();
            }

            if frame_start.duration_since(last_flush) >= FLUSH_INTERVAL {
                if let Err(e) = self.flush_dirty() {
                    tracing::warn!(error = %e, "dirty flush failed");
                }
                last_flush = frame_start;
            }

            tick = tick.wrapping_add(1);
            let elapsed = frame_start.elapsed();
            if elapsed < tick_dt {
                std::thread::sleep(tick_dt - elapsed);
            }
        }

        tracing::info!("shutting down");
        self.flush_all()?;
        let _ = self.host.shutdown();
        Ok(())
    }

    fn handle_event(&mut self, ev: Event) -> anyhow::Result<()> {
        match ev {
            Event::Connected(peer) => {
                tracing::debug!(peer, "peer connected (awaiting hello)");
            }
            Event::Data(peer, bytes) => self.handle_data(peer, &bytes)?,
            Event::Disconnected(peer, reason) => self.on_disconnect(peer, reason)?,
        }
        Ok(())
    }

    fn handle_data(&mut self, peer: PeerId, bytes: &[u8]) -> anyhow::Result<()> {
        // Verified decode: hostile/garbage datagrams are simply dropped.
        let Ok(env) = decode_envelope(bytes) else {
            return Ok(());
        };
        match env.payload_type() {
            p::Payload::ClientHello => {
                if let Some(h) = env.payload_as_client_hello() {
                    self.on_hello(peer, h)?;
                }
            }
            p::Payload::ClientState => {
                if let Some(cs) = env.payload_as_client_state() {
                    self.on_client_state(peer, cs);
                }
            }
            p::Payload::EconTxn => {
                if let Some(t) = env.payload_as_econ_txn() {
                    self.on_econ(peer, t)?;
                }
            }
            p::Payload::MarketTradeRequest => {
                if let Some(r) = env.payload_as_market_trade_request() {
                    self.on_trade(peer, r, now_ms())?;
                }
            }
            p::Payload::MoorRequest => {
                if let Some(m) = env.payload_as_moor_request() {
                    self.on_moor(peer, m)?;
                }
            }
            p::Payload::ChatSend => {
                if let Some(c) = env.payload_as_chat_send() {
                    self.on_chat(peer, c);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn on_hello(&mut self, peer: PeerId, hello: p::ClientHello<'_>) -> anyhow::Result<()> {
        let token = hello.token().unwrap_or("");
        let name = hello.display_name().unwrap_or("sailor").to_string();

        if token.is_empty() {
            // Auth is assertion-only, but a token must at least be present.
            let bytes = codec::server_hello(
                self.next_seq(),
                false,
                "missing token",
                0,
                &self.cfg.server_name,
                0,
                &self.caps(),
                self.clock_now(),
                self.weather_seed,
                self.weather_epoch_day,
            );
            self.send(peer, &bytes);
            return Ok(());
        }

        let now = now_ms();
        let player = self
            .db
            .upsert_player_by_token(&token_hash(token), &name, now)?;
        let player_id = player.id as u64;

        // Drop any prior session for this identity (reconnect from a new peer).
        let stale: Vec<PeerId> = self
            .sessions
            .iter()
            .filter(|(&pp, s)| pp != peer && s.player_id == player_id)
            .map(|(&pp, _)| pp)
            .collect();
        for pp in stale {
            self.sessions.remove(&pp);
        }

        let mut sub = Subscription::new(self.cfg.aoi_radius_i32());
        let origin = self.world.grid().cell_of(0.0, 0.0);
        let aoi = sub.recenter(origin);
        self.world.place_in_cell(player_id, origin);

        self.sessions.insert(
            peer,
            Session {
                peer,
                player_id,
                display_name: name,
                aboard_boat: 0,
                pos: [0.0, 0.0, 0.0],
                rot: [0.0, 0.0, 0.0, 1.0],
                vel: [0.0, 0.0, 0.0],
                t_ms: 0,
                sub,
                cell: Some(origin),
                dirty: true,
            },
        );

        tracing::info!(peer, player_id, name = %self.sessions[&peer].display_name, "hello accepted");

        let bytes = codec::server_hello(
            self.next_seq(),
            true,
            "",
            player_id,
            &self.cfg.server_name,
            player.gold,
            &self.caps(),
            self.clock_now(),
            self.weather_seed,
            self.weather_epoch_day,
        );
        self.send(peer, &bytes);

        // Emit the join-time interest set so a freshly connected player learns
        // its surrounding cells (and their contents, e.g. persisted moorings)
        // without having to first cross a cell boundary.
        self.emit_aoi(peer, &aoi);
        Ok(())
    }

    fn on_client_state(&mut self, peer: PeerId, cs: p::ClientState<'_>) {
        let aoi;
        {
            let Some(s) = self.sessions.get_mut(&peer) else {
                return;
            };
            s.pos = vec3_of(cs.pos());
            s.rot = quat_of(cs.rot());
            s.vel = vec3_of(cs.vel());
            s.aboard_boat = cs.aboard_boat();
            s.t_ms = cs.t_ms();
            s.dirty = true;

            let player_id = s.player_id;
            self.world.place(player_id, s.pos[0], s.pos[2]);
            let cell = self
                .world
                .cell_of_entity(player_id)
                .unwrap_or_else(|| self.world.grid().cell_of(s.pos[0], s.pos[2]));
            aoi = s.sub.recenter(cell);
            s.cell = Some(cell);
        }

        self.emit_aoi(peer, &aoi);
    }

    /// Send an AoI delta to `peer`: the added/removed cell list followed by a
    /// full [`codec::cell_snapshot`] for each newly entered cell. A no-op when
    /// the delta is empty (the player stayed in the same cell).
    fn emit_aoi(&mut self, peer: PeerId, aoi: &AoiUpdate) {
        if aoi.is_empty() {
            return;
        }

        let bytes = codec::aoi_update(self.next_seq(), &aoi.added, &aoi.removed);
        self.send(peer, &bytes);

        for &cell in &aoi.added {
            let (players, boats, moorings) = match self.gather_cell(cell) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(error = %e, "gather cell failed");
                    continue;
                }
            };
            let bytes = codec::cell_snapshot(self.next_seq(), cell, &players, &boats, &moorings);
            self.send(peer, &bytes);
        }
    }

    fn on_econ(&mut self, peer: PeerId, txn: p::EconTxn<'_>) -> anyhow::Result<()> {
        let Some(player_id) = self.sessions.get(&peer).map(|s| s.player_id) else {
            return Ok(());
        };
        let txn = Txn {
            txn_id: txn.txn_id(),
            amount_gold: txn.amount_gold(),
            kind: txn.kind(),
            note: txn.note().unwrap_or("").to_string(),
        };

        let ack = {
            let mut ledger = Ledger::new(DbLedgerStore::new(&self.db, now_ms()));
            ledger.apply(player_id, &txn)?
        };
        tracing::debug!(
            player_id,
            txn_id = txn.txn_id,
            accepted = ack.accepted,
            balance = ack.new_balance,
            "econ txn"
        );

        let bytes = codec::ledger_ack(self.next_seq(), &ack);
        self.send(peer, &bytes);
        Ok(())
    }

    /// Handle a shared-market trade: throttle new trades per (player, port),
    /// apply the trade idempotently against the authoritative per-port
    /// stock/price, and reply with the resulting state.
    fn on_trade(
        &mut self,
        peer: PeerId,
        req: p::MarketTradeRequest<'_>,
        now_ms: i64,
    ) -> anyhow::Result<()> {
        let Some(player_id) = self.sessions.get(&peer).map(|s| s.player_id) else {
            return Ok(());
        };
        let trade = Trade {
            txn_id: req.txn_id(),
            port_id: req.port_id(),
            item_id: req.item_id(),
            qty: req.qty(),
            unit_price: req.unit_price(),
        };

        // Idempotency-first: a replay of an already-committed trade returns the
        // recorded state and is never throttled, so an app-level resend after
        // packet loss stays safe even under the per-port rate limit. Only a
        // genuinely new trade is charged against the token bucket.
        let already_applied = self.db.lookup_trade(trade.txn_id)?.is_some();
        if !already_applied && !self.trade_limiter.allow(player_id, trade.port_id, now_ms) {
            let (stock, price) = self
                .db
                .market_state(trade.port_id, trade.item_id)?
                .unwrap_or((0, 0));
            let ack = MarketAck {
                txn_id: trade.txn_id,
                accepted: false,
                port_id: trade.port_id,
                item_id: trade.item_id,
                stock,
                price,
                reason: "rate limited".to_string(),
            };
            tracing::debug!(player_id, port = trade.port_id, "market trade rate limited");
            let bytes = codec::market_state_ack(self.next_seq(), &ack);
            self.send(peer, &bytes);
            return Ok(());
        }

        let ack = {
            let mut market = Market::new(DbMarketStore::new(&self.db, now_ms));
            market.apply(&trade)?
        };
        tracing::debug!(
            player_id,
            txn_id = trade.txn_id,
            port = trade.port_id,
            item = trade.item_id,
            accepted = ack.accepted,
            stock = ack.stock,
            price = ack.price,
            "market trade"
        );

        let bytes = codec::market_state_ack(self.next_seq(), &ack);
        self.send(peer, &bytes);
        Ok(())
    }

    fn on_moor(&mut self, peer: PeerId, req: p::MoorRequest<'_>) -> anyhow::Result<()> {
        let Some((owner, aboard)) = self
            .sessions
            .get(&peer)
            .map(|s| (s.player_id, s.aboard_boat))
        else {
            return Ok(());
        };
        // Use the boarded boat id, falling back to the player id as a stable key.
        let boat_id = if aboard != 0 { aboard } else { owner };
        let pos = vec3_of(req.pos());
        let rot = quat_of(req.rot());
        let name = req.name().unwrap_or("mooring").to_string();
        let cell = self.world.grid().cell_of(pos[0], pos[2]);
        let created = now_ms();

        let row = MooringRow {
            boat_id: boat_id as i64,
            owner: owner as i64,
            cell_x: cell.cx,
            cell_z: cell.cz,
            pos,
            rot,
            name: name.clone(),
            created_at: created,
        };
        self.db.upsert_mooring(&row)?;
        tracing::info!(
            owner,
            boat_id,
            cell_x = cell.cx,
            cell_z = cell.cz,
            "mooring saved"
        );

        let snap = MooringSnap {
            boat_id,
            owner,
            cell: (cell.cx, cell.cz),
            pos,
            rot,
            name,
            created_at: created as u64,
        };
        let bytes = codec::moor_ack(self.next_seq(), true, Some(&snap), "");
        self.send(peer, &bytes);
        Ok(())
    }

    fn on_chat(&mut self, peer: PeerId, chat: p::ChatSend<'_>) {
        let Some((sender_player, name, cell)) = self
            .sessions
            .get(&peer)
            .map(|s| (s.player_id, s.display_name.clone(), s.cell))
        else {
            return;
        };
        let Some(cell) = cell else {
            return;
        };
        let text = chat.text().unwrap_or("").to_string();
        let channel = chat.channel();
        let t_ms = self.uptime_ms();

        // Deliver to every session whose AoI currently includes the sender's cell.
        let recipients: Vec<PeerId> = self
            .sessions
            .values()
            .filter(|r| r.sub.contains(cell))
            .map(|r| r.peer)
            .collect();

        for target in recipients {
            let bytes =
                codec::chat_broadcast(self.next_seq(), sender_player, &name, &text, channel, t_ms);
            self.send(target, &bytes);
        }
    }

    fn on_disconnect(&mut self, peer: PeerId, reason: DisconnectReason) -> anyhow::Result<()> {
        if let Some(s) = self.sessions.remove(&peer) {
            self.world.remove(s.player_id);
            self.db.touch_last_seen(s.player_id as i64, now_ms())?;
            tracing::info!(peer, player_id = s.player_id, ?reason, "peer disconnected");
        }
        Ok(())
    }

    fn broadcast_snapshots(&mut self) {
        self.snapshot_tick = self.snapshot_tick.wrapping_add(1);
        let server_tick = self.snapshot_tick;

        let recipients: Vec<(PeerId, u64, Cell)> = self
            .sessions
            .values()
            .filter_map(|s| s.cell.map(|c| (s.peer, s.player_id, c)))
            .collect();

        for (peer, self_pid, cell) in recipients {
            let mut players = Vec::new();
            let mut boats = Vec::new();
            for eid in self.players_in_view(cell, self_pid) {
                if let Some(s) = self.session_by_player(eid) {
                    players.push(player_snap(s));
                    if s.aboard_boat != 0 {
                        boats.push(boat_snap(s));
                    }
                }
            }
            if players.is_empty() && boats.is_empty() {
                continue;
            }
            let bytes = codec::snapshot_delta(self.next_seq(), server_tick, &players, &boats);
            self.send(peer, &bytes);
        }
    }

    /// Entity ids visible to a viewer centred on `cell`: everything within the
    /// configured AoI radius, minus the viewer itself. This bounds a
    /// recipient's snapshot to AoI density, never the global population.
    fn players_in_view(&self, cell: Cell, self_pid: u64) -> Vec<u64> {
        self.world
            .entities_in_radius(cell, self.cfg.aoi_radius_i32())
            .into_iter()
            .filter(|&eid| eid != self_pid)
            .collect()
    }

    /// Broadcast the current world clock to every connected session. The clock
    /// is derived authority (see [`clock_from_epoch`]); the weather seed is
    /// join-only in `ServerHello` and is deliberately not rebroadcast here.
    fn broadcast_clock(&mut self) {
        let clock = self.clock_now();
        let peers: Vec<PeerId> = self.sessions.keys().copied().collect();
        for peer in peers {
            let bytes = codec::world_clock(self.next_seq(), clock);
            self.send(peer, &bytes);
        }
    }

    fn gather_cell(
        &self,
        cell: Cell,
    ) -> anyhow::Result<(Vec<PlayerSnap>, Vec<BoatSnap>, Vec<MooringSnap>)> {
        let mut players = Vec::new();
        let mut boats = Vec::new();
        for eid in self.world.entities_in(cell) {
            if let Some(s) = self.session_by_player(eid) {
                players.push(player_snap(s));
                if s.aboard_boat != 0 {
                    boats.push(boat_snap(s));
                }
            }
        }
        let moorings = self
            .db
            .moorings_in_cell(cell.cx, cell.cz)?
            .into_iter()
            .map(mooring_snap)
            .collect();
        Ok((players, boats, moorings))
    }

    fn flush_dirty(&mut self) -> anyhow::Result<()> {
        let now = now_ms();
        let dirty: Vec<(PeerId, u64)> = self
            .sessions
            .iter()
            .filter(|(_, s)| s.dirty)
            .map(|(&peer, s)| (peer, s.player_id))
            .collect();
        for (peer, player_id) in dirty {
            self.db.touch_last_seen(player_id as i64, now)?;
            if let Some(s) = self.sessions.get_mut(&peer) {
                s.dirty = false;
            }
        }
        Ok(())
    }

    fn flush_all(&mut self) -> anyhow::Result<()> {
        let now = now_ms();
        let ids: Vec<u64> = self.sessions.values().map(|s| s.player_id).collect();
        for player_id in ids {
            self.db.touch_last_seen(player_id as i64, now)?;
        }
        Ok(())
    }

    fn session_by_player(&self, player_id: u64) -> Option<&Session> {
        self.sessions.values().find(|s| s.player_id == player_id)
    }

    fn caps(&self) -> Caps {
        Caps {
            tick_hz: self.cfg.tick_hz.min(u8::MAX as u32) as u8,
            snapshot_hz: self.cfg.snapshot_hz.min(u8::MAX as u32) as u8,
            aoi_radius_cells: self.cfg.aoi_radius_cells.min(u8::MAX as u32) as u8,
            cell_size_m: self.world.grid().cell_size_m,
            features: FEATURES,
        }
    }

    fn clock_now(&self) -> WorldClock {
        clock_from_epoch(self.epoch_ms, now_ms())
    }

    fn next_seq(&mut self) -> u32 {
        self.seq = self.seq.wrapping_add(1);
        self.seq
    }

    fn uptime_ms(&self) -> u32 {
        self.boot.elapsed().as_millis() as u32
    }

    fn send(&mut self, peer: PeerId, bytes: &[u8]) {
        if let Err(e) = self.host.send_unreliable(peer, bytes) {
            tracing::warn!(peer, error = %e, "send failed");
        }
    }
}

fn player_snap(s: &Session) -> PlayerSnap {
    PlayerSnap {
        player_id: s.player_id,
        pos: s.pos,
        rot: s.rot,
        aboard_boat: s.aboard_boat,
        t_ms: s.t_ms,
    }
}

fn boat_snap(s: &Session) -> BoatSnap {
    BoatSnap {
        boat_id: s.aboard_boat,
        owner: s.player_id,
        pos: s.pos,
        rot: s.rot,
        vel: s.vel,
        t_ms: s.t_ms,
    }
}

fn mooring_snap(row: MooringRow) -> MooringSnap {
    MooringSnap {
        boat_id: row.boat_id as u64,
        owner: row.owner as u64,
        cell: (row.cell_x, row.cell_z),
        pos: row.pos,
        rot: row.rot,
        name: row.name,
        created_at: row.created_at as u64,
    }
}

fn vec3_of(v: Option<&p::Vec3>) -> [f32; 3] {
    v.map(|v| [v.x(), v.y(), v.z()]).unwrap_or([0.0, 0.0, 0.0])
}

fn quat_of(q: Option<&p::QuatC>) -> [f32; 4] {
    q.map(|q| [q.x(), q.y(), q.z(), q.w()])
        .unwrap_or([0.0, 0.0, 0.0, 1.0])
}

/// Current Unix time in milliseconds.
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// FNV-1a hash of a token, hex-encoded. NOTE: this is a stable identity key,
/// not a cryptographic hash — init-0 auth is token-assertion only.
fn token_hash(token: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in token.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_hash_is_stable_and_distinct() {
        assert_eq!(token_hash("abc"), token_hash("abc"));
        assert_ne!(token_hash("abc"), token_hash("abd"));
        assert_eq!(token_hash("abc").len(), 16);
    }

    #[test]
    fn vec_and_quat_defaults() {
        assert_eq!(vec3_of(None), [0.0, 0.0, 0.0]);
        assert_eq!(quat_of(None), [0.0, 0.0, 0.0, 1.0]);
    }
}

/// Interest-management hardening: these exercise the real message handlers and
/// the per-recipient visibility decision against an in-memory server, pinning
/// the quantitative AoI acceptance (initial interest set on join, added/removed
/// on a cross-cell move, and a snapshot bounded to AoI density not population).
#[cfg(test)]
mod aoi_harden_tests {
    use super::*;
    use flatbuffers::FlatBufferBuilder;
    use std::collections::HashSet;
    use sw_contracts::{decode_envelope, finish_envelope};
    use sw_world::{cells_in_radius, EntityId, Grid};

    fn make_server(cfg: Config) -> Server {
        let world = World::new(Grid::new(cfg.cell_size_m));
        let trade_limiter = RateLimiter::new(cfg.trade_min_interval_ms_i64());
        Server {
            host: Host::bind("127.0.0.1:0", CONNECT_KEY).unwrap(),
            db: Db::open_in_memory().unwrap(),
            world,
            sessions: HashMap::new(),
            seq: 0,
            snapshot_tick: 0,
            boot: Instant::now(),
            epoch_ms: 0,
            weather_seed: 0,
            weather_epoch_day: 0,
            trade_limiter,
            running: Arc::new(AtomicBool::new(true)),
            cfg,
        }
    }

    fn hello_envelope(token: &str, name: &str) -> Vec<u8> {
        let mut fbb = FlatBufferBuilder::new();
        let token_off = fbb.create_string(token);
        let name_off = fbb.create_string(name);
        let hello = p::ClientHello::create(
            &mut fbb,
            &p::ClientHelloArgs {
                protocol_version: sw_contracts::PROTOCOL_VERSION,
                display_name: Some(name_off),
                token: Some(token_off),
                ..Default::default()
            },
        );
        finish_envelope(&mut fbb, 1, p::Payload::ClientHello, hello.as_union_value())
    }

    fn state_envelope(x: f32, z: f32) -> Vec<u8> {
        let mut fbb = FlatBufferBuilder::new();
        let pos = p::Vec3::new(x, 0.0, z);
        let rot = p::QuatC::new(0.0, 0.0, 0.0, 1.0);
        let vel = p::Vec3::new(0.0, 0.0, 0.0);
        let cs = p::ClientState::create(
            &mut fbb,
            &p::ClientStateArgs {
                pos: Some(&pos),
                rot: Some(&rot),
                vel: Some(&vel),
                aboard_boat: 0,
                t_ms: 0,
            },
        );
        finish_envelope(&mut fbb, 2, p::Payload::ClientState, cs.as_union_value())
    }

    #[test]
    fn join_emits_configured_initial_interest_set() {
        // A joining player's subscription must be the full block for the
        // *configured* radius, not the hardcoded default — the join-time AoI is
        // sized by config.
        let mut server = make_server(Config {
            aoi_radius_cells: 3,
            ..Config::default()
        });
        let peer: PeerId = 1;
        let bytes = hello_envelope("tok-join", "Joiner");
        let env = decode_envelope(&bytes).unwrap();
        server
            .on_hello(peer, env.payload_as_client_hello().unwrap())
            .unwrap();

        let origin = server.world.grid().cell_of(0.0, 0.0);
        let expected: HashSet<Cell> = cells_in_radius(origin, 3).into_iter().collect();
        let sub = &server.sessions[&peer].sub;
        assert_eq!(sub.cells(), &expected);
        assert_eq!(sub.cells().len(), 49); // (2*3+1)^2
    }

    #[test]
    fn cross_cell_move_adds_new_block_and_removes_vacated_cells() {
        let mut server = make_server(Config::default()); // radius 2, cell 1024 m
        let peer: PeerId = 7;
        let hello_bytes = hello_envelope("tok-move", "Mover");
        let hello = decode_envelope(&hello_bytes).unwrap();
        server
            .on_hello(peer, hello.payload_as_client_hello().unwrap())
            .unwrap();

        let origin = server.world.grid().cell_of(0.0, 0.0);
        let before: HashSet<Cell> = server.sessions[&peer].sub.cells().clone();
        assert!(before.contains(&origin));

        // Jump far in +X so the old block is fully vacated.
        let far_x = 10.0 * server.world.grid().cell_size_m;
        let state_bytes = state_envelope(far_x, 0.0);
        let cs = decode_envelope(&state_bytes).unwrap();
        server.on_client_state(peer, cs.payload_as_client_state().unwrap());

        let after = server.sessions[&peer].sub.cells();
        let new_center = server.world.grid().cell_of(far_x, 0.0);
        assert!(after.contains(&new_center), "new block added");
        assert!(!after.contains(&origin), "vacated origin removed");
        for cell in &before {
            assert!(!after.contains(cell), "vacated cell {cell:?} still in view");
        }
        // The world index tracks the move: the mover left the origin cell.
        assert!(server.world.entities_in(origin).is_empty());
    }

    #[test]
    fn snapshot_visibility_tracks_aoi_density_not_population() {
        // Bandwidth guarantee: a recipient only sees entities inside its AoI
        // block, so per-recipient cost scales with local density, never with the
        // global entity population.
        let mut server = make_server(Config::default()); // radius 2
        let viewer: EntityId = 1;
        let center = Cell::new(0, 0);
        server.world.place_in_cell(viewer, center);

        let near_cells = [
            Cell::new(0, 0),
            Cell::new(1, 0),
            Cell::new(-2, 2),
            Cell::new(2, -1),
        ];
        let mut near_ids = Vec::new();
        let mut id: EntityId = 100;
        for &cell in &near_cells {
            server.world.place_in_cell(id, cell);
            near_ids.push(id);
            id += 1;
        }

        // A large far-flung population well outside the radius-2 block.
        let mut far_ids = Vec::new();
        for k in 0..500 {
            server.world.place_in_cell(id, Cell::new(100 + k, 100));
            far_ids.push(id);
            id += 1;
        }

        let visible: HashSet<EntityId> =
            server.players_in_view(center, viewer).into_iter().collect();

        for nid in &near_ids {
            assert!(
                visible.contains(nid),
                "in-range entity {nid} must be visible"
            );
        }
        assert!(
            !visible.contains(&viewer),
            "viewer is never echoed to itself"
        );
        for fid in &far_ids {
            assert!(!visible.contains(fid), "far entity {fid} must not leak in");
        }
        // The bound is AoI density, orders of magnitude below the population.
        assert!(visible.len() < far_ids.len());
    }

    #[test]
    fn caps_advertise_configured_aoi() {
        let server = make_server(Config {
            aoi_radius_cells: 5,
            cell_size_m: 2048.0,
            ..Config::default()
        });
        let caps = server.caps();
        assert_eq!(caps.aoi_radius_cells, 5);
        assert_eq!(caps.cell_size_m, 2048.0);
    }
}

/// Shared-market dispatch: these exercise the real `on_trade` handler against an
/// in-memory server, pinning the three acceptance properties for #21 — a trade
/// mutates the shared per-port stock, a replay of the same txn_id never
/// double-applies (and is not throttled), and a genuinely new trade inside the
/// per-(player, port) window is rejected.
#[cfg(test)]
mod market_dispatch_tests {
    use super::*;
    use flatbuffers::FlatBufferBuilder;
    use sw_contracts::{decode_envelope, finish_envelope};
    use sw_world::Grid;

    fn make_server(cfg: Config) -> Server {
        let world = World::new(Grid::new(cfg.cell_size_m));
        let trade_limiter = RateLimiter::new(cfg.trade_min_interval_ms_i64());
        Server {
            host: Host::bind("127.0.0.1:0", CONNECT_KEY).unwrap(),
            db: Db::open_in_memory().unwrap(),
            world,
            sessions: HashMap::new(),
            seq: 0,
            snapshot_tick: 0,
            boot: Instant::now(),
            epoch_ms: 0,
            weather_seed: 0,
            weather_epoch_day: 0,
            trade_limiter,
            running: Arc::new(AtomicBool::new(true)),
            cfg,
        }
    }

    fn hello_envelope(token: &str, name: &str) -> Vec<u8> {
        let mut fbb = FlatBufferBuilder::new();
        let token_off = fbb.create_string(token);
        let name_off = fbb.create_string(name);
        let hello = p::ClientHello::create(
            &mut fbb,
            &p::ClientHelloArgs {
                protocol_version: sw_contracts::PROTOCOL_VERSION,
                display_name: Some(name_off),
                token: Some(token_off),
                ..Default::default()
            },
        );
        finish_envelope(&mut fbb, 1, p::Payload::ClientHello, hello.as_union_value())
    }

    fn trade_envelope(txn_id: u64, port: u32, item: u32, qty: i64, price: i64) -> Vec<u8> {
        let mut fbb = FlatBufferBuilder::new();
        let req = p::MarketTradeRequest::create(
            &mut fbb,
            &p::MarketTradeRequestArgs {
                txn_id,
                port_id: port,
                item_id: item,
                qty,
                unit_price: price,
            },
        );
        finish_envelope(
            &mut fbb,
            2,
            p::Payload::MarketTradeRequest,
            req.as_union_value(),
        )
    }

    fn join(server: &mut Server, peer: PeerId, token: &str) {
        let bytes = hello_envelope(token, "Trader");
        let env = decode_envelope(&bytes).unwrap();
        server
            .on_hello(peer, env.payload_as_client_hello().unwrap())
            .unwrap();
    }

    fn apply_trade(
        server: &mut Server,
        peer: PeerId,
        txn_id: u64,
        port: u32,
        item: u32,
        qty: i64,
        price: i64,
        now_ms: i64,
    ) {
        let bytes = trade_envelope(txn_id, port, item, qty, price);
        let env = decode_envelope(&bytes).unwrap();
        server
            .on_trade(peer, env.payload_as_market_trade_request().unwrap(), now_ms)
            .unwrap();
    }

    #[test]
    fn trade_mutates_shared_port_stock_and_replay_is_idempotent() {
        let mut server = make_server(Config::default());
        let peer: PeerId = 1;
        join(&mut server, peer, "tok-trade");

        // A first trade sells 40 units into port 10 / item 5 at price 100.
        apply_trade(&mut server, peer, 1, 10, 5, 40, 100, 1000);
        assert_eq!(server.db.market_state(10, 5).unwrap(), Some((40, 100)));

        // A replay of the same txn_id (even with a different payload) must not
        // double-apply and must not be throttled despite being inside the
        // window: the shared stock stays at 40.
        apply_trade(&mut server, peer, 1, 10, 5, 999, 999, 1050);
        assert_eq!(server.db.market_state(10, 5).unwrap(), Some((40, 100)));
    }

    #[test]
    fn new_trade_inside_the_window_is_rate_limited() {
        let mut server = make_server(Config {
            trade_min_interval_ms: 250,
            ..Config::default()
        });
        let peer: PeerId = 1;
        join(&mut server, peer, "tok-rl");

        // First trade accepted at t=1000.
        apply_trade(&mut server, peer, 1, 10, 5, 40, 100, 1000);
        assert_eq!(server.db.market_state(10, 5).unwrap(), Some((40, 100)));

        // A *new* txn_id at t=1050 (50ms later, inside the 250ms window) is
        // throttled: it must not touch the shared stock.
        apply_trade(&mut server, peer, 2, 10, 5, 5, 100, 1050);
        assert_eq!(server.db.market_state(10, 5).unwrap(), Some((40, 100)));
        assert!(server.db.lookup_trade(2).unwrap().is_none());

        // Once the window elapses (t=1300, 300ms after the accepted trade) the
        // new trade is admitted and the shared stock advances.
        apply_trade(&mut server, peer, 2, 10, 5, 5, 100, 1300);
        assert_eq!(server.db.market_state(10, 5).unwrap(), Some((45, 100)));
    }

    #[test]
    fn a_trade_before_hello_is_ignored() {
        // No session for the peer -> the handler is a safe no-op, never a panic.
        let mut server = make_server(Config::default());
        apply_trade(&mut server, 99, 1, 10, 5, 40, 100, 1000);
        assert_eq!(server.db.market_state(10, 5).unwrap(), None);
    }
}
