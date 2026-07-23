//! The authoritative server: state, message handlers, and the fixed-tick loop.

use crate::clock::{clock_from_epoch, WorldClock};
use crate::codec::{self, BoatSnap, Caps, MooringSnap, PlayerSnap};
use crate::config::{Config, MAX_PLAYER_ROWS};
use crate::econ_store::{DbLedgerStore, DbMarketStore};
use crate::ratelimit::{BoundedRateLimiter, GlobalRateLimiter, RateLimiter};
use crate::validate;
use std::collections::HashMap;
use std::io::Write;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use sw_contracts::decode_envelope;
use sw_contracts::sw_proto as p;
use sw_econ::{Ledger, Market, MarketAck, Trade, Txn};
use sw_net::{protocol, DisconnectReason, Event, Host, PeerId};
use sw_persist::{Db, MooringRow, PlayerAdmission};
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
    identity_hash: String,
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
    identity_players: HashMap<String, u64>,
    seq: u32,
    snapshot_tick: u32,
    boot: Instant,
    epoch_ms: i64,
    weather_seed: u64,
    weather_epoch_day: u32,
    hello_limiter: RateLimiter,
    source_session_limiter: BoundedRateLimiter<IpAddr>,
    reconnect_limiter: RateLimiter,
    new_session_limiter: GlobalRateLimiter,
    trade_limiter: RateLimiter,
    client_state_limiter: RateLimiter,
    chat_limiter: RateLimiter,
    econ_limiter: RateLimiter,
    moor_limiter: RateLimiter,
    running: Arc<AtomicBool>,
}

impl Server {
    /// Build the server: open + migrate the DB, initialise world state, bind
    /// the socket. Fails before the readiness line if binding fails.
    pub fn new(cfg: Config, running: Arc<AtomicBool>) -> anyhow::Result<Server> {
        let db = Db::open(&cfg.db_path)?;
        let identity_players = load_identity_players(&db)?;

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

        let host = Host::bind_with_limits(
            &cfg.bind,
            CONNECT_KEY,
            cfg.max_transport_peers_usize(),
            cfg.max_transport_peers_per_ip_usize(),
        )?;
        let world = World::new(sw_world::Grid::new(cfg.cell_size_m));
        let hello_limiter = RateLimiter::new(cfg.hello_min_interval_ms_i64());
        let source_session_limiter = BoundedRateLimiter::new(
            cfg.hello_min_interval_ms_i64(),
            cfg.max_transport_peers_usize(),
        );
        let reconnect_limiter = RateLimiter::new(cfg.hello_min_interval_ms_i64());
        let new_session_limiter = GlobalRateLimiter::new(cfg.new_session_min_interval_ms_i64());
        let trade_limiter = RateLimiter::new(cfg.trade_min_interval_ms_i64());
        let client_state_limiter = RateLimiter::new(cfg.client_state_min_interval_ms_i64());
        let chat_limiter = RateLimiter::new(cfg.chat_min_interval_ms_i64());
        let econ_limiter = RateLimiter::new(cfg.econ_min_interval_ms_i64());
        let moor_limiter = RateLimiter::new(cfg.moor_min_interval_ms_i64());

        Ok(Server {
            cfg,
            host,
            db,
            world,
            sessions: HashMap::new(),
            identity_players,
            seq: 0,
            snapshot_tick: 0,
            boot: Instant::now(),
            epoch_ms,
            weather_seed,
            weather_epoch_day,
            hello_limiter,
            source_session_limiter,
            reconnect_limiter,
            new_session_limiter,
            trade_limiter,
            client_state_limiter,
            chat_limiter,
            econ_limiter,
            moor_limiter,
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
        self.handle_data_at(peer, bytes, self.admission_ms(), now_ms())
    }

    fn handle_data_at(
        &mut self,
        peer: PeerId,
        bytes: &[u8],
        admission_ms: i64,
        persistence_ms: i64,
    ) -> anyhow::Result<()> {
        // Verified decode: hostile/garbage datagrams are simply dropped.
        let Ok(env) = decode_envelope(bytes) else {
            return Ok(());
        };
        match env.payload_type() {
            p::Payload::ClientHello => {
                if let Some(h) = env.payload_as_client_hello() {
                    self.on_hello_at(peer, h, admission_ms, persistence_ms)?;
                }
            }
            p::Payload::ClientState => {
                if let Some(cs) = env.payload_as_client_state() {
                    self.on_client_state(peer, cs, admission_ms);
                }
            }
            p::Payload::EconTxn => {
                if let Some(t) = env.payload_as_econ_txn() {
                    self.on_econ(peer, t, admission_ms, persistence_ms)?;
                }
            }
            p::Payload::MarketTradeRequest => {
                if let Some(r) = env.payload_as_market_trade_request() {
                    self.on_trade(peer, r, admission_ms, persistence_ms)?;
                }
            }
            p::Payload::MoorRequest => {
                if let Some(m) = env.payload_as_moor_request() {
                    self.on_moor(peer, m, admission_ms, persistence_ms)?;
                }
            }
            p::Payload::ChatSend => {
                if let Some(c) = env.payload_as_chat_send() {
                    self.on_chat(peer, c, admission_ms);
                }
            }
            _ => {}
        }
        Ok(())
    }

    #[cfg(test)]
    fn on_hello(&mut self, peer: PeerId, hello: p::ClientHello<'_>) -> anyhow::Result<()> {
        self.on_hello_at(peer, hello, self.admission_ms(), now_ms())
    }

    fn on_hello_at(
        &mut self,
        peer: PeerId,
        hello: p::ClientHello<'_>,
        admission_ms: i64,
        persistence_ms: i64,
    ) -> anyhow::Result<()> {
        if !self.hello_limiter.allow(u64::from(peer), admission_ms) {
            return Ok(());
        }

        if hello.protocol_version() != sw_contracts::PROTOCOL_VERSION {
            let reason = format!(
                "protocol version mismatch: client {}, server {}",
                hello.protocol_version(),
                sw_contracts::PROTOCOL_VERSION
            );
            self.reject_hello(peer, &reason);
            return Ok(());
        }

        let max_string_len = self.cfg.max_wire_string_len_usize();
        if let Err(reason) = validate_hello_string(
            hello.api_surface_hash(),
            "API surface hash",
            true,
            max_string_len,
        ) {
            self.reject_hello(peer, &reason);
            return Ok(());
        }
        let token = match validate_hello_string(hello.token(), "token", true, max_string_len) {
            Ok(Some(value)) => value,
            Ok(None) => unreachable!("required hello string validated as absent"),
            Err(reason) => {
                self.reject_hello(peer, &reason);
                return Ok(());
            }
        };
        let display_name = match validate_hello_string(
            hello.display_name(),
            "display name",
            false,
            max_string_len,
        ) {
            Ok(value) => value,
            Err(reason) => {
                self.reject_hello(peer, &reason);
                return Ok(());
            }
        };
        if let Err(reason) =
            validate_hello_string(hello.game_build(), "game build", false, max_string_len)
        {
            self.reject_hello(peer, &reason);
            return Ok(());
        }
        if let Err(reason) =
            validate_hello_string(hello.mod_version(), "mod version", false, max_string_len)
        {
            self.reject_hello(peer, &reason);
            return Ok(());
        }

        let name = display_name.unwrap_or("sailor").to_string();

        let identity_hash = token_hash(token);
        if let Some((player_id, identity_matches)) = self
            .sessions
            .get(&peer)
            .map(|session| (session.player_id, session.identity_hash == identity_hash))
        {
            if !identity_matches {
                self.reject_hello(peer, "identity change requires a new connection");
                return Ok(());
            }

            let balance = self.db.player_balance(player_id as i64)?;
            self.send_accepted_hello(peer, player_id, balance);
            return Ok(());
        }

        let source_ip = match self.host.peer_addr(peer) {
            Some(addr) => addr.ip(),
            #[cfg(test)]
            None => test_peer_ip(peer),
            #[cfg(not(test))]
            None => return Ok(()),
        };
        if !self.source_session_limiter.allow(source_ip, admission_ms) {
            self.reject_hello(peer, "server busy; retry");
            return Ok(());
        }

        let known_player_id = self.identity_players.get(&identity_hash).copied();
        if known_player_id
            .is_some_and(|player_id| !self.reconnect_limiter.allow(player_id, admission_ms))
        {
            self.reject_hello(peer, "server busy; retry");
            return Ok(());
        }
        if known_player_id.is_none() && !self.new_session_limiter.allow(admission_ms) {
            self.reject_hello(peer, "server busy; retry");
            return Ok(());
        }

        let player = match self.db.admit_player_by_token(
            &identity_hash,
            &name,
            persistence_ms,
            self.cfg.max_player_rows_u32(),
        )? {
            PlayerAdmission::Admitted(player) => player,
            PlayerAdmission::CapacityReached => {
                self.reject_hello(peer, "server player capacity reached");
                return Ok(());
            }
        };
        let player_id = u64::try_from(player.id)
            .map_err(|_| anyhow::anyhow!("admitted player id must be nonnegative"))?;
        if let Some(expected_player_id) = known_player_id {
            if player_id != expected_player_id {
                return Err(anyhow::anyhow!(
                    "persisted identity index disagrees with admitted player"
                ));
            }
        } else {
            if self.identity_players.len() >= MAX_PLAYER_ROWS as usize {
                return Err(anyhow::anyhow!(
                    "persisted identity index reached its hard ceiling"
                ));
            }
            self.identity_players
                .insert(identity_hash.clone(), player_id);
            let _ = self.reconnect_limiter.allow(player_id, admission_ms);
        }

        // Drop any prior session for this identity (reconnect from a new peer).
        let stale: Vec<PeerId> = self
            .sessions
            .iter()
            .filter(|(&pp, s)| pp != peer && s.player_id == player_id)
            .map(|(&pp, _)| pp)
            .collect();
        for pp in stale {
            if self.host.peer_addr(pp).is_some() && !self.host.disconnect(pp) {
                return Err(anyhow::anyhow!("failed to evict superseded transport peer"));
            }
            self.sessions.remove(&pp);
            self.hello_limiter.clear(u64::from(pp));
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
                identity_hash,
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

        self.send_accepted_hello(peer, player_id, player.gold);

        // Emit the join-time interest set so a freshly connected player learns
        // its surrounding cells (and their contents, e.g. persisted moorings)
        // without having to first cross a cell boundary.
        self.emit_aoi(peer, &aoi);
        Ok(())
    }

    fn send_accepted_hello(&mut self, peer: PeerId, player_id: u64, balance_gold: i64) {
        let bytes = codec::server_hello(
            self.next_seq(),
            true,
            "",
            player_id,
            &self.cfg.server_name,
            balance_gold,
            &self.caps(),
            self.clock_now(),
            self.weather_seed,
            self.weather_epoch_day,
        );
        self.send(peer, &bytes);
    }

    fn reject_hello(&mut self, peer: PeerId, reason: &str) {
        let bytes = codec::server_hello(
            self.next_seq(),
            false,
            reason,
            0,
            &self.cfg.server_name,
            0,
            &self.caps(),
            self.clock_now(),
            self.weather_seed,
            self.weather_epoch_day,
        );
        self.send(peer, &bytes);
    }

    fn on_client_state(&mut self, peer: PeerId, cs: p::ClientState<'_>, now_ms: i64) {
        let Some(player_id) = self.sessions.get(&peer).map(|s| s.player_id) else {
            return;
        };

        // Per-player throttle: a client-state flood beyond the configured rate is
        // dropped before it can drive the grid/AoI recompute. Keyed by player, so
        // it is rotation-proof and memory-bounded exactly like the trade limiter.
        if !self.client_state_limiter.allow(player_id, now_ms) {
            return;
        }

        // Value validation beyond the FlatBuffers verifier: a non-finite (NaN/Inf)
        // pos/rot/vel drops the message, and an out-of-bounds finite coordinate is
        // clamped, BEFORE any of it reaches the cell math. Without this a hostile
        // Inf position saturates the `as i32` cast to `i32::MAX` and the block-offset
        // add in `cells_in_radius` overflows (fixes the #19 nit).
        let Some(motion) =
            validate::sanitize_motion(vec3_of(cs.pos()), quat_of(cs.rot()), vec3_of(cs.vel()))
        else {
            return;
        };

        let aoi;
        {
            let Some(s) = self.sessions.get_mut(&peer) else {
                return;
            };
            s.pos = motion.pos;
            s.rot = motion.rot;
            s.vel = motion.vel;
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

    fn on_econ(
        &mut self,
        peer: PeerId,
        txn: p::EconTxn<'_>,
        admission_ms: i64,
        persistence_ms: i64,
    ) -> anyhow::Result<()> {
        let Some(player_id) = self.sessions.get(&peer).map(|s| s.player_id) else {
            return Ok(());
        };

        // Reject an over-long note before it can be stored/logged. The scalar
        // amount is left to the ledger, which already guards overflow and the
        // non-negative-balance invariant with checked math.
        let note = txn.note().unwrap_or("");
        if !validate::string_within_limit(note, self.cfg.max_wire_string_len_usize()) {
            return Ok(());
        }

        // Idempotency-first: a replay of an already-committed txn returns the
        // recorded balance and is never throttled, so an app-level resend after
        // packet loss stays safe under the rate limit. Only a genuinely new txn is
        // charged against the aggregate per-player econ throttle.
        let txn_id = txn.txn_id();
        let already_applied = self.db.lookup_txn(txn_id as i64)?.is_some();
        if !already_applied && !self.econ_limiter.allow(player_id, admission_ms) {
            tracing::debug!(player_id, txn_id, "econ txn rate limited");
            return Ok(());
        }

        let txn = Txn {
            txn_id,
            amount_gold: txn.amount_gold(),
            kind: txn.kind(),
            note: note.to_string(),
        };

        let ack = {
            let mut ledger = Ledger::new(DbLedgerStore::new(&self.db, persistence_ms));
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

    /// Handle a shared-market trade: throttle new trades with the aggregate
    /// per-player limiter, apply the trade idempotently against the
    /// authoritative per-port stock/price, and reply with the resulting state.
    fn on_trade(
        &mut self,
        peer: PeerId,
        req: p::MarketTradeRequest<'_>,
        admission_ms: i64,
        persistence_ms: i64,
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
        // packet loss stays safe even under the rate limit. Only a genuinely new
        // trade is charged against the aggregate per-player throttle — the
        // attacker-supplied `port_id` never opens a fresh bucket, so rotating it
        // cannot raise a player's trade throughput.
        let already_applied = self.db.lookup_trade(trade.txn_id)?.is_some();
        if !already_applied && !self.trade_limiter.allow(player_id, admission_ms) {
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
            let mut market = Market::new(DbMarketStore::new(&self.db, persistence_ms));
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

    fn on_moor(
        &mut self,
        peer: PeerId,
        req: p::MoorRequest<'_>,
        admission_ms: i64,
        persistence_ms: i64,
    ) -> anyhow::Result<()> {
        let Some((owner, aboard)) = self
            .sessions
            .get(&peer)
            .map(|s| (s.player_id, s.aboard_boat))
        else {
            return Ok(());
        };
        // Use the boarded boat id, falling back to the player id as a stable key.
        let boat_id = if aboard != 0 { aboard } else { owner };

        // Value validation: a non-finite/out-of-bounds mooring pose is rejected or
        // clamped before it reaches `cell_of` (same overflow class as ClientState),
        // and an over-long name is rejected before it is persisted.
        let Some(motion) =
            validate::sanitize_motion(vec3_of(req.pos()), quat_of(req.rot()), [0.0; 3])
        else {
            return Ok(());
        };
        let pos = motion.pos;
        let rot = motion.rot;
        let name = req.name().unwrap_or("mooring");
        if !validate::string_within_limit(name, self.cfg.max_wire_string_len_usize()) {
            return Ok(());
        }
        let name = name.to_string();

        // Per-player throttle: a MoorRequest commits a persistent moorage record,
        // so an unthrottled flood is a real DoS on persistent state (the #21 trade
        // class). Reject a new moor beyond the configured rate BEFORE the write,
        // keyed by player so it is rotation-proof and memory-bounded exactly like
        // the trade/econ/chat limiters.
        if !self.moor_limiter.allow(owner, admission_ms) {
            tracing::debug!(owner, "moor request rate limited");
            return Ok(());
        }

        let cell = self.world.grid().cell_of(pos[0], pos[2]);
        let created = persistence_ms;

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

    fn on_chat(&mut self, peer: PeerId, chat: p::ChatSend<'_>, now_ms: i64) {
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

        // Reject an over-long line before it is broadcast (a single datagram must
        // not amplify into unbounded rebroadcast bytes).
        let text = chat.text().unwrap_or("");
        if !validate::string_within_limit(text, self.cfg.max_wire_string_len_usize()) {
            return;
        }

        // Per-player chat throttle: a flood beyond the configured rate is dropped
        // before it fans out to every AoI subscriber.
        if !self.chat_limiter.allow(sender_player, now_ms) {
            return;
        }

        let text = text.to_string();
        let channel = chat.channel();
        let t_ms = self.uptime_ms();
        let bytes =
            codec::chat_broadcast(self.next_seq(), sender_player, &name, &text, channel, t_ms);
        if bytes.len() > protocol::MTU - protocol::HEADER_SIZE {
            return;
        }

        // Deliver to every session whose AoI currently includes the sender's cell.
        let recipients: Vec<PeerId> = self
            .sessions
            .values()
            .filter(|r| r.sub.contains(cell))
            .map(|r| r.peer)
            .collect();

        for target in recipients {
            self.send(target, &bytes);
        }
    }

    fn on_disconnect(&mut self, peer: PeerId, reason: DisconnectReason) -> anyhow::Result<()> {
        self.hello_limiter.clear(u64::from(peer));
        if let Some(s) = self.sessions.remove(&peer) {
            self.world.remove(s.player_id);
            // Drop the player's throttle state across every message class: a
            // departed player's entries are useless and leaving them behind would
            // let connection churn accrete stale entries in the limiter maps.
            self.trade_limiter.clear(s.player_id);
            self.reconnect_limiter.clear(s.player_id);
            self.client_state_limiter.clear(s.player_id);
            self.chat_limiter.clear(s.player_id);
            self.econ_limiter.clear(s.player_id);
            self.moor_limiter.clear(s.player_id);
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

    fn admission_ms(&self) -> i64 {
        self.boot.elapsed().as_millis().min(i64::MAX as u128) as i64
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

fn load_identity_players(db: &Db) -> anyhow::Result<HashMap<String, u64>> {
    load_identity_players_up_to(db, MAX_PLAYER_ROWS)
}

fn load_identity_players_up_to(db: &Db, max_players: u32) -> anyhow::Result<HashMap<String, u64>> {
    let rows = db.player_identities(max_players.saturating_add(1))?;
    if rows.len() > max_players as usize {
        return Err(anyhow::anyhow!(
            "players table exceeds the hard identity ceiling of {max_players}"
        ));
    }

    rows.into_iter()
        .map(|(identity_hash, player_id)| {
            if identity_hash.len() != 16
                || !identity_hash.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                return Err(anyhow::anyhow!(
                    "persisted player identity hash has an invalid shape"
                ));
            }
            let player_id = u64::try_from(player_id)
                .map_err(|_| anyhow::anyhow!("persisted player id must be nonnegative"))?;
            Ok((identity_hash, player_id))
        })
        .collect()
}

#[cfg(test)]
fn test_peer_ip(peer: PeerId) -> IpAddr {
    IpAddr::V6(std::net::Ipv6Addr::from(u128::from(peer) + 1))
}

fn validate_hello_string<'a>(
    value: Option<&'a str>,
    field: &str,
    required: bool,
    max_len: usize,
) -> Result<Option<&'a str>, String> {
    if required && value.is_none_or(str::is_empty) {
        return Err(format!("missing {field}"));
    }
    if value.is_some_and(|string| !validate::string_within_limit(string, max_len)) {
        return Err(format!("{field} exceeds maximum length ({max_len} bytes)"));
    }
    Ok(value)
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
    fn persisted_identity_index_fails_closed_at_its_hard_bound() {
        let db = Db::open_in_memory().unwrap();
        db.upsert_player_by_token(&token_hash("first"), "First", 1)
            .unwrap();
        db.upsert_player_by_token(&token_hash("second"), "Second", 1)
            .unwrap();

        assert!(load_identity_players_up_to(&db, 1).is_err());
    }

    #[test]
    fn vec_and_quat_defaults() {
        assert_eq!(vec3_of(None), [0.0, 0.0, 0.0]);
        assert_eq!(quat_of(None), [0.0, 0.0, 0.0, 1.0]);
    }
}

#[cfg(test)]
mod handshake_tests {
    use super::*;
    use flatbuffers::FlatBufferBuilder;
    use std::net::UdpSocket;
    use std::time::Duration;
    use sw_contracts::{decode_envelope, finish_envelope};
    use sw_net::{protocol, Event};
    use sw_world::Grid;

    fn make_server() -> Server {
        make_server_with_db(Db::open_in_memory().unwrap())
    }

    fn make_server_with_db(db: Db) -> Server {
        let cfg = Config::default();
        let world = World::new(Grid::new(cfg.cell_size_m));
        let identity_players = load_identity_players(&db).unwrap();
        Server {
            host: Host::bind_with_limits(
                "127.0.0.1:0",
                CONNECT_KEY,
                cfg.max_transport_peers_usize(),
                cfg.max_transport_peers_per_ip_usize(),
            )
            .unwrap(),
            db,
            world,
            sessions: HashMap::new(),
            identity_players,
            seq: 0,
            snapshot_tick: 0,
            boot: Instant::now(),
            epoch_ms: 0,
            weather_seed: 0,
            weather_epoch_day: 0,
            hello_limiter: RateLimiter::new(cfg.hello_min_interval_ms_i64()),
            source_session_limiter: BoundedRateLimiter::new(
                cfg.hello_min_interval_ms_i64(),
                cfg.max_transport_peers_usize(),
            ),
            reconnect_limiter: RateLimiter::new(cfg.hello_min_interval_ms_i64()),
            new_session_limiter: GlobalRateLimiter::new(cfg.new_session_min_interval_ms_i64()),
            trade_limiter: RateLimiter::new(cfg.trade_min_interval_ms_i64()),
            client_state_limiter: RateLimiter::new(cfg.client_state_min_interval_ms_i64()),
            chat_limiter: RateLimiter::new(cfg.chat_min_interval_ms_i64()),
            econ_limiter: RateLimiter::new(cfg.econ_min_interval_ms_i64()),
            moor_limiter: RateLimiter::new(cfg.moor_min_interval_ms_i64()),
            running: Arc::new(AtomicBool::new(true)),
            cfg,
        }
    }

    #[test]
    fn production_hello_admission_uses_monotonic_uptime() {
        let mut server = make_server();
        let hello = hello_envelope(
            "handshake-token",
            sw_contracts::PROTOCOL_VERSION,
            Some("surface-hash"),
        );
        assert!(server.hello_limiter.allow(1, 86_400_000));

        deliver_hello(&mut server, 1, &hello);

        assert!(
            !server.sessions.contains_key(&1),
            "boot-elapsed time must not jump forward to the Unix epoch"
        );
    }

    #[test]
    fn hello_separates_admission_time_from_persistence_time() {
        const ADMISSION_MS: i64 = 2_000;
        const FIRST_EPOCH_MS: i64 = 1_700_000_000_000;
        const RETURN_EPOCH_MS: i64 = 1_700_000_010_000;

        let mut server = make_server();
        let hello = hello_envelope(
            "clock-domain-token",
            sw_contracts::PROTOCOL_VERSION,
            Some("surface-hash"),
        );
        let env = decode_envelope(&hello).unwrap();
        server
            .on_hello_at(
                1,
                env.payload_as_client_hello().unwrap(),
                ADMISSION_MS,
                FIRST_EPOCH_MS,
            )
            .unwrap();

        let player_id = server.sessions[&1].player_id as i64;
        let created = server.db.player(player_id).unwrap().unwrap();
        assert_eq!(created.created_at, FIRST_EPOCH_MS);
        assert_eq!(created.last_seen, FIRST_EPOCH_MS);

        let hello_interval_ms = server.cfg.hello_min_interval_ms_i64();
        let seq_after_first = server.seq;
        let env = decode_envelope(&hello).unwrap();
        server
            .on_hello_at(
                1,
                env.payload_as_client_hello().unwrap(),
                ADMISSION_MS + hello_interval_ms - 1,
                RETURN_EPOCH_MS,
            )
            .unwrap();
        assert_eq!(server.seq, seq_after_first);
        assert_eq!(server.db.player(player_id).unwrap().unwrap(), created);

        let env = decode_envelope(&hello).unwrap();
        server
            .on_hello_at(
                1,
                env.payload_as_client_hello().unwrap(),
                ADMISSION_MS + hello_interval_ms,
                RETURN_EPOCH_MS,
            )
            .unwrap();
        assert_ne!(server.seq, seq_after_first);
        assert_eq!(server.db.player(player_id).unwrap().unwrap(), created);

        let env = decode_envelope(&hello).unwrap();
        server
            .on_hello_at(
                2,
                env.payload_as_client_hello().unwrap(),
                ADMISSION_MS + hello_interval_ms,
                RETURN_EPOCH_MS,
            )
            .unwrap();
        let returned = server.db.player(player_id).unwrap().unwrap();
        assert_eq!(server.sessions[&2].player_id as i64, player_id);
        assert_eq!(returned.created_at, FIRST_EPOCH_MS);
        assert_eq!(returned.last_seen, RETURN_EPOCH_MS);
    }

    fn hello_envelope(
        token: &str,
        protocol_version: u16,
        api_surface_hash: Option<&str>,
    ) -> Vec<u8> {
        hello_envelope_with_strings(
            Some(token),
            Some("Sailor"),
            Some("game-build"),
            Some("mod-version"),
            api_surface_hash,
            protocol_version,
        )
    }

    fn hello_envelope_with_strings(
        token: Option<&str>,
        display_name: Option<&str>,
        game_build: Option<&str>,
        mod_version: Option<&str>,
        api_surface_hash: Option<&str>,
        protocol_version: u16,
    ) -> Vec<u8> {
        let mut fbb = FlatBufferBuilder::new();
        let token = token.map(|value| fbb.create_string(value));
        let name = display_name.map(|value| fbb.create_string(value));
        let game_build = game_build.map(|value| fbb.create_string(value));
        let mod_version = mod_version.map(|value| fbb.create_string(value));
        let api_hash = api_surface_hash.map(|value| fbb.create_string(value));
        let hello = p::ClientHello::create(
            &mut fbb,
            &p::ClientHelloArgs {
                protocol_version,
                display_name: name,
                token,
                game_build,
                mod_version,
                api_surface_hash: api_hash,
            },
        );
        finish_envelope(&mut fbb, 1, p::Payload::ClientHello, hello.as_union_value())
    }

    fn state_envelope() -> Vec<u8> {
        let mut fbb = FlatBufferBuilder::new();
        let pos = p::Vec3::new(175.0, 4.0, -125.0);
        let rot = p::QuatC::new(0.1, 0.2, 0.3, 0.9);
        let vel = p::Vec3::new(2.0, 3.0, 4.0);
        let state = p::ClientState::create(
            &mut fbb,
            &p::ClientStateArgs {
                pos: Some(&pos),
                rot: Some(&rot),
                vel: Some(&vel),
                aboard_boat: 99,
                t_ms: 4321,
            },
        );
        finish_envelope(&mut fbb, 2, p::Payload::ClientState, state.as_union_value())
    }

    fn deliver_hello(server: &mut Server, peer: PeerId, bytes: &[u8]) {
        let env = decode_envelope(bytes).unwrap();
        server
            .on_hello(peer, env.payload_as_client_hello().unwrap())
            .unwrap();
    }

    fn deliver_hello_at(server: &mut Server, peer: PeerId, bytes: &[u8], now_ms: i64) {
        let env = decode_envelope(bytes).unwrap();
        server
            .on_hello_at(peer, env.payload_as_client_hello().unwrap(), now_ms, now_ms)
            .unwrap();
    }

    fn connect_peer(server: &mut Server) -> (UdpSocket, PeerId) {
        connect_peer_from(server, "127.0.0.1")
    }

    fn connect_peer_from(server: &mut Server, source_ip: &str) -> (UdpSocket, PeerId) {
        let client = UdpSocket::bind((source_ip, 0)).unwrap();
        client.connect(server.host.local_addr().unwrap()).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let connect_data = protocol::write_litenet_string(CONNECT_KEY);
        let request = protocol::build_connect_request(0, 1, 1, 16, &connect_data);
        client.send(&request).unwrap();

        let peer = match server.host.poll(Instant::now()).as_slice() {
            [Event::Connected(peer)] => *peer,
            events => panic!("expected one connected peer, got {events:?}"),
        };
        let mut accept = [0u8; protocol::CONNECT_ACCEPT_SIZE];
        client.recv(&mut accept).unwrap();
        (client, peer)
    }

    fn receive_server_hello(client: &UdpSocket) -> (bool, String) {
        loop {
            let mut packet = [0u8; protocol::MTU];
            let received = client.recv(&mut packet).unwrap();
            assert_eq!(
                protocol::Header::from_byte(packet[0]).property,
                protocol::property::UNRELIABLE
            );
            let env = decode_envelope(&packet[protocol::HEADER_SIZE..received]).unwrap();
            if let Some(hello) = env.payload_as_server_hello() {
                return (hello.accepted(), hello.reason().unwrap_or("").to_string());
            }
        }
    }

    fn assert_no_outbound_datagram(client: &UdpSocket) {
        client
            .set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        let mut packet = [0u8; protocol::MTU];
        let error = client.recv(&mut packet).unwrap_err();
        assert!(
            matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ),
            "expected no outbound datagram, got {error}"
        );
    }

    fn receive_payload(client: &UdpSocket, expected: p::Payload) -> Vec<u8> {
        loop {
            let mut packet = [0u8; protocol::MTU];
            let received = client.recv(&mut packet).unwrap();
            assert_eq!(
                protocol::Header::from_byte(packet[0]).property,
                protocol::property::UNRELIABLE
            );
            let payload = packet[protocol::HEADER_SIZE..received].to_vec();
            let env = decode_envelope(&payload).unwrap();
            if env.payload_type() == expected {
                return payload;
            }
        }
    }

    #[test]
    fn protocol_mismatch_is_rejected_before_session_creation() {
        let mut server = make_server();
        let (client, peer) = connect_peer(&mut server);
        let bytes = hello_envelope(
            "handshake-token",
            sw_contracts::PROTOCOL_VERSION + 1,
            Some("surface-hash"),
        );

        deliver_hello(&mut server, peer, &bytes);

        assert!(!server.sessions.contains_key(&peer));
        assert_eq!(
            receive_server_hello(&client),
            (
                false,
                format!(
                    "protocol version mismatch: client {}, server {}",
                    sw_contracts::PROTOCOL_VERSION + 1,
                    sw_contracts::PROTOCOL_VERSION
                )
            )
        );
    }

    #[test]
    fn api_surface_hash_must_be_present_nonempty_and_bounded() {
        let max_len = Config::default().max_wire_string_len_usize();
        let too_long = "x".repeat(max_len + 1);
        let cases = [
            (None, "missing API surface hash".to_string()),
            (Some(""), "missing API surface hash".to_string()),
            (
                Some(too_long.as_str()),
                format!("API surface hash exceeds maximum length ({max_len} bytes)"),
            ),
        ];

        for (hash, expected_reason) in cases {
            let mut server = make_server();
            let (client, peer) = connect_peer(&mut server);
            let bytes = hello_envelope("handshake-token", sw_contracts::PROTOCOL_VERSION, hash);
            deliver_hello(&mut server, peer, &bytes);
            assert!(
                !server.sessions.contains_key(&peer),
                "invalid API surface hash created a session"
            );
            assert_eq!(receive_server_hello(&client), (false, expected_reason));
        }
    }

    #[test]
    fn every_client_hello_string_is_bounded_before_persistence_or_session_creation() {
        let max_len = Config::default().max_wire_string_len_usize();
        let too_long = "x".repeat(max_len + 1);
        let cases = [
            (
                "token",
                hello_envelope_with_strings(
                    Some(&too_long),
                    Some("Sailor"),
                    Some("game-build"),
                    Some("mod-version"),
                    Some("surface-hash"),
                    sw_contracts::PROTOCOL_VERSION,
                ),
                format!("token exceeds maximum length ({max_len} bytes)"),
            ),
            (
                "display name",
                hello_envelope_with_strings(
                    Some("handshake-token"),
                    Some(&too_long),
                    Some("game-build"),
                    Some("mod-version"),
                    Some("surface-hash"),
                    sw_contracts::PROTOCOL_VERSION,
                ),
                format!("display name exceeds maximum length ({max_len} bytes)"),
            ),
            (
                "game build",
                hello_envelope_with_strings(
                    Some("handshake-token"),
                    Some("Sailor"),
                    Some(&too_long),
                    Some("mod-version"),
                    Some("surface-hash"),
                    sw_contracts::PROTOCOL_VERSION,
                ),
                format!("game build exceeds maximum length ({max_len} bytes)"),
            ),
            (
                "mod version",
                hello_envelope_with_strings(
                    Some("handshake-token"),
                    Some("Sailor"),
                    Some("game-build"),
                    Some(&too_long),
                    Some("surface-hash"),
                    sw_contracts::PROTOCOL_VERSION,
                ),
                format!("mod version exceeds maximum length ({max_len} bytes)"),
            ),
            (
                "API surface hash",
                hello_envelope_with_strings(
                    Some("handshake-token"),
                    Some("Sailor"),
                    Some("game-build"),
                    Some("mod-version"),
                    Some(&too_long),
                    sw_contracts::PROTOCOL_VERSION,
                ),
                format!("API surface hash exceeds maximum length ({max_len} bytes)"),
            ),
        ];

        for (field, bytes, expected_reason) in cases {
            let mut server = make_server();
            let (client, peer) = connect_peer(&mut server);

            deliver_hello(&mut server, peer, &bytes);

            assert!(
                !server.sessions.contains_key(&peer),
                "over-long {field} created a session"
            );
            assert!(
                server.db.player(1).unwrap().is_none(),
                "over-long {field} persisted a player"
            );
            assert_eq!(receive_server_hello(&client), (false, expected_reason));
        }
    }

    #[test]
    fn token_and_api_hash_are_required_while_other_hello_strings_remain_optional() {
        let required_cases = [
            (
                "missing token",
                hello_envelope_with_strings(
                    None,
                    Some("Sailor"),
                    Some("game-build"),
                    Some("mod-version"),
                    Some("surface-hash"),
                    sw_contracts::PROTOCOL_VERSION,
                ),
                "missing token",
            ),
            (
                "empty token",
                hello_envelope_with_strings(
                    Some(""),
                    Some("Sailor"),
                    Some("game-build"),
                    Some("mod-version"),
                    Some("surface-hash"),
                    sw_contracts::PROTOCOL_VERSION,
                ),
                "missing token",
            ),
            (
                "missing API surface hash",
                hello_envelope_with_strings(
                    Some("handshake-token"),
                    Some("Sailor"),
                    Some("game-build"),
                    Some("mod-version"),
                    None,
                    sw_contracts::PROTOCOL_VERSION,
                ),
                "missing API surface hash",
            ),
            (
                "empty API surface hash",
                hello_envelope_with_strings(
                    Some("handshake-token"),
                    Some("Sailor"),
                    Some("game-build"),
                    Some("mod-version"),
                    Some(""),
                    sw_contracts::PROTOCOL_VERSION,
                ),
                "missing API surface hash",
            ),
        ];

        for (case, bytes, expected_reason) in required_cases {
            let mut server = make_server();
            let (client, peer) = connect_peer(&mut server);

            deliver_hello(&mut server, peer, &bytes);

            assert!(
                !server.sessions.contains_key(&peer),
                "{case} created a session"
            );
            assert!(
                server.db.player(1).unwrap().is_none(),
                "{case} persisted a player"
            );
            assert_eq!(
                receive_server_hello(&client),
                (false, expected_reason.to_string())
            );
        }

        for (display_name, game_build, mod_version, expected_name) in [
            (None, None, None, "sailor"),
            (Some(""), Some(""), Some(""), ""),
        ] {
            let mut server = make_server();
            let bytes = hello_envelope_with_strings(
                Some("handshake-token"),
                display_name,
                game_build,
                mod_version,
                Some("surface-hash"),
                sw_contracts::PROTOCOL_VERSION,
            );

            deliver_hello(&mut server, 1, &bytes);

            assert_eq!(server.sessions[&1].display_name, expected_name);
        }
    }

    #[test]
    fn valid_protocol_and_api_surface_hash_create_session() {
        let mut server = make_server();
        let bytes = hello_envelope(
            "handshake-token",
            sw_contracts::PROTOCOL_VERSION,
            Some("surface-hash"),
        );

        deliver_hello(&mut server, 1, &bytes);

        assert!(server.sessions.contains_key(&1));
    }

    #[test]
    fn near_limit_display_name_and_chat_cannot_emit_an_oversized_datagram() {
        let mut server = make_server();
        let (client, peer) = connect_peer(&mut server);
        let (observer, observer_peer) = connect_peer(&mut server);
        let max_len = server.cfg.max_wire_string_len_usize();
        let display_name = "n".repeat(max_len);
        let text = "t".repeat(max_len);
        let hello = hello_envelope_with_strings(
            Some("handshake-token"),
            Some(&display_name),
            Some("game-build"),
            Some("mod-version"),
            Some("surface-hash"),
            sw_contracts::PROTOCOL_VERSION,
        );
        deliver_hello_at(&mut server, peer, &hello, 1_000);
        assert_eq!(receive_server_hello(&client), (true, String::new()));
        let observer_hello = hello_envelope_with_strings(
            Some("observer-token"),
            Some("Observer"),
            Some("game-build"),
            Some("mod-version"),
            Some("surface-hash"),
            sw_contracts::PROTOCOL_VERSION,
        );
        let observer_admission_ms = 1_000
            + server
                .cfg
                .hello_min_interval_ms_i64()
                .max(server.cfg.new_session_min_interval_ms_i64());
        deliver_hello_at(
            &mut server,
            observer_peer,
            &observer_hello,
            observer_admission_ms,
        );
        assert_eq!(receive_server_hello(&observer), (true, String::new()));
        client
            .set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        for socket in [&client, &observer] {
            socket
                .set_read_timeout(Some(Duration::from_millis(20)))
                .unwrap();
            loop {
                let mut join_update = [0u8; sw_net::protocol::MTU];
                match socket.recv(&mut join_update) {
                    Ok(_) => {}
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) =>
                    {
                        break;
                    }
                    Err(error) => panic!("failed to drain join update: {error}"),
                }
            }
        }

        let encoded = codec::chat_broadcast(2, 1, &display_name, &text, 0, 1_000);
        assert!(
            encoded.len() > sw_net::protocol::MTU - sw_net::protocol::HEADER_SIZE,
            "the regression input must exceed the fixed unfragmented payload"
        );

        let mut fbb = FlatBufferBuilder::new();
        let text = fbb.create_string(&text);
        let chat = p::ChatSend::create(
            &mut fbb,
            &p::ChatSendArgs {
                text: Some(text),
                channel: 0,
            },
        );
        let bytes = finish_envelope(&mut fbb, 2, p::Payload::ChatSend, chat.as_union_value());
        let env = decode_envelope(&bytes).unwrap();
        let seq_before_chat = server.seq;
        server.on_chat(peer, env.payload_as_chat_send().unwrap(), 1_000);

        assert_eq!(
            server.seq,
            seq_before_chat.wrapping_add(1),
            "one invalid broadcast must be rejected once, not encoded once per recipient"
        );
        assert_no_outbound_datagram(&client);
        assert_no_outbound_datagram(&observer);
    }

    #[test]
    fn valid_chat_is_preencoded_once_and_fanned_out_through_connected_peers() {
        let mut server = make_server();
        let (sender, sender_peer) = connect_peer(&mut server);
        let (observer, observer_peer) = connect_peer(&mut server);
        let sender_hello = hello_envelope_with_strings(
            Some("sender-token"),
            Some("Skipper"),
            Some("game-build"),
            Some("mod-version"),
            Some("surface-hash"),
            sw_contracts::PROTOCOL_VERSION,
        );
        deliver_hello_at(&mut server, sender_peer, &sender_hello, 1_000);
        assert_eq!(receive_server_hello(&sender), (true, String::new()));
        let observer_hello = hello_envelope_with_strings(
            Some("observer-token"),
            Some("Observer"),
            Some("game-build"),
            Some("mod-version"),
            Some("surface-hash"),
            sw_contracts::PROTOCOL_VERSION,
        );
        let observer_admission_ms = 1_000
            + server
                .cfg
                .hello_min_interval_ms_i64()
                .max(server.cfg.new_session_min_interval_ms_i64());
        deliver_hello_at(
            &mut server,
            observer_peer,
            &observer_hello,
            observer_admission_ms,
        );
        assert_eq!(receive_server_hello(&observer), (true, String::new()));

        let sender_player = server.sessions[&sender_peer].player_id;
        let mut fbb = FlatBufferBuilder::new();
        let text = fbb.create_string("fair winds");
        let chat = p::ChatSend::create(
            &mut fbb,
            &p::ChatSendArgs {
                text: Some(text),
                channel: 2,
            },
        );
        let bytes = finish_envelope(&mut fbb, 3, p::Payload::ChatSend, chat.as_union_value());
        let seq_before_chat = server.seq;

        server
            .handle_data_at(sender_peer, &bytes, 1_000, 1_000)
            .unwrap();

        assert_eq!(server.seq, seq_before_chat.wrapping_add(1));
        let sender_payload = receive_payload(&sender, p::Payload::ChatBroadcast);
        let observer_payload = receive_payload(&observer, p::Payload::ChatBroadcast);
        assert_eq!(
            sender_payload, observer_payload,
            "every recipient must receive the one pre-encoded broadcast"
        );

        let env = decode_envelope(&sender_payload).unwrap();
        assert_eq!(env.seq(), seq_before_chat.wrapping_add(1));
        let broadcast = env.payload_as_chat_broadcast().unwrap();
        assert_eq!(broadcast.player_id(), sender_player);
        assert_eq!(broadcast.display_name(), Some("Skipper"));
        assert_eq!(broadcast.text(), Some("fair winds"));
        assert_eq!(broadcast.channel(), 2);
    }

    #[test]
    fn repeated_valid_hello_preserves_established_session_and_only_resends_server_hello() {
        let mut server = make_server();
        let (client, peer) = connect_peer(&mut server);
        let hello_interval_ms = server.cfg.hello_min_interval_ms_i64();
        let hello = hello_envelope(
            "handshake-token",
            sw_contracts::PROTOCOL_VERSION,
            Some("surface-hash"),
        );
        deliver_hello_at(&mut server, peer, &hello, 1_000);
        assert_eq!(receive_server_hello(&client), (true, String::new()));

        let state = state_envelope();
        let env = decode_envelope(&state).unwrap();
        server.on_client_state(peer, env.payload_as_client_state().unwrap(), 1_000);

        let established = &server.sessions[&peer];
        let player_id = established.player_id;
        let display_name = established.display_name.clone();
        let aboard_boat = established.aboard_boat;
        let pos = established.pos;
        let rot = established.rot;
        let vel = established.vel;
        let t_ms = established.t_ms;
        let cell = established.cell;
        let subscribed_cells = established.sub.cells().clone();
        let world_cell = server.world.cell_of_entity(player_id);
        let seq_before_retry = server.seq;

        deliver_hello_at(&mut server, peer, &hello, 1_000 + hello_interval_ms);

        let retried = &server.sessions[&peer];
        assert_eq!(retried.player_id, player_id);
        assert_eq!(retried.display_name, display_name);
        assert_eq!(retried.aboard_boat, aboard_boat);
        assert_eq!(retried.pos, pos);
        assert_eq!(retried.rot, rot);
        assert_eq!(retried.vel, vel);
        assert_eq!(retried.t_ms, t_ms);
        assert_eq!(retried.cell, cell);
        assert_eq!(retried.sub.cells(), &subscribed_cells);
        assert_eq!(server.world.cell_of_entity(player_id), world_cell);
        assert_eq!(server.seq, seq_before_retry.wrapping_add(1));
        assert_eq!(receive_server_hello(&client), (true, String::new()));
    }

    #[test]
    fn superseded_peer_retry_cannot_reclaim_identity_from_accepted_replacement() {
        let mut server = make_server();
        let (first_client, first_peer) = connect_peer(&mut server);
        let hello = hello_envelope(
            "shared-identity-token",
            sw_contracts::PROTOCOL_VERSION,
            Some("surface-hash"),
        );
        let first_admission_ms = 1_000;

        deliver_hello_at(&mut server, first_peer, &hello, first_admission_ms);
        assert_eq!(receive_server_hello(&first_client), (true, String::new()));

        let (replacement_client, replacement_peer) = connect_peer(&mut server);
        let replacement_admission_ms = first_admission_ms
            + server
                .cfg
                .hello_min_interval_ms_i64()
                .max(server.cfg.new_session_min_interval_ms_i64());
        deliver_hello_at(
            &mut server,
            replacement_peer,
            &hello,
            replacement_admission_ms,
        );
        assert_eq!(
            receive_server_hello(&replacement_client),
            (true, String::new())
        );
        let player_id = server.sessions[&replacement_peer].player_id;
        let world_cell = server.world.cell_of_entity(player_id);
        assert!(!server.sessions.contains_key(&first_peer));
        assert!(
            server.host.peer_addr(first_peer).is_none(),
            "a superseded logical session must be removed from the transport"
        );
        assert_eq!(
            server.host.peer_count(),
            1,
            "superseded peers must not retain transport quota"
        );

        loop {
            let mut packet = [0u8; protocol::MTU];
            let received = first_client.recv(&mut packet).unwrap();
            if protocol::Header::from_byte(packet[0]).property == protocol::property::DISCONNECT {
                assert_eq!(received, protocol::DISCONNECT_SIZE);
                break;
            }
        }

        first_client
            .send(&protocol::build_unreliable(&hello))
            .unwrap();
        first_client.send(&protocol::build_ping(1)).unwrap();
        assert!(
            server.host.poll(Instant::now()).is_empty(),
            "data and keepalive traffic from the evicted address must be ignored"
        );

        assert_eq!(server.sessions.len(), 1);
        assert!(!server.sessions.contains_key(&first_peer));
        assert_eq!(server.sessions[&replacement_peer].player_id, player_id);
        assert_eq!(server.world.len(), 1);
        assert_eq!(server.world.cell_of_entity(player_id), world_cell);
    }

    #[test]
    fn persisted_offline_identity_does_not_compete_with_fresh_token_admission() {
        let persisted_token = "persisted-offline-token";
        let db = Db::open_in_memory().unwrap();
        let persisted = db
            .upsert_player_by_token(&token_hash(persisted_token), "Returning", 100)
            .unwrap();
        let mut server = make_server_with_db(db);
        let admission_ms = 1_000;

        let (fresh_client, fresh_peer) = connect_peer_from(&mut server, "127.0.0.1");
        let fresh = hello_envelope(
            "fresh-attacker-token",
            sw_contracts::PROTOCOL_VERSION,
            Some("surface-hash"),
        );
        deliver_hello_at(&mut server, fresh_peer, &fresh, admission_ms);
        assert_eq!(receive_server_hello(&fresh_client), (true, String::new()));

        let (returning_client, returning_peer) = connect_peer_from(&mut server, "127.0.0.2");
        let returning = hello_envelope(
            persisted_token,
            sw_contracts::PROTOCOL_VERSION,
            Some("surface-hash"),
        );
        deliver_hello_at(&mut server, returning_peer, &returning, admission_ms);

        assert_eq!(
            receive_server_hello(&returning_client),
            (true, String::new()),
            "a persisted identity must use reconnect admission even while the new-token gate is busy"
        );
        assert_eq!(
            server.sessions[&returning_peer].player_id,
            persisted.id as u64
        );
    }

    #[test]
    fn same_identity_port_rotation_does_not_starve_another_source() {
        let mut server = make_server();
        let (first_client, first_peer) = connect_peer_from(&mut server, "127.0.0.1");
        let returning = hello_envelope(
            "returning-identity-token",
            sw_contracts::PROTOCOL_VERSION,
            Some("surface-hash"),
        );
        let first_admission_ms = 1_000;
        deliver_hello_at(&mut server, first_peer, &returning, first_admission_ms);
        assert_eq!(receive_server_hello(&first_client), (true, String::new()));

        let (replacement_client, replacement_peer) = connect_peer_from(&mut server, "127.0.0.1");
        let (other_client, other_peer) = connect_peer_from(&mut server, "127.0.0.2");
        let next_admission_ms = first_admission_ms
            + server
                .cfg
                .hello_min_interval_ms_i64()
                .max(server.cfg.new_session_min_interval_ms_i64());

        deliver_hello_at(&mut server, replacement_peer, &returning, next_admission_ms);
        assert_eq!(
            receive_server_hello(&replacement_client),
            (true, String::new())
        );

        let other = hello_envelope(
            "other-source-token",
            sw_contracts::PROTOCOL_VERSION,
            Some("surface-hash"),
        );
        deliver_hello_at(&mut server, other_peer, &other, next_admission_ms);

        assert_eq!(receive_server_hello(&other_client), (true, String::new()));
        assert_eq!(server.sessions.len(), 2);
        assert!(!server.sessions.contains_key(&first_peer));
        assert!(server.sessions.contains_key(&replacement_peer));
        assert!(server.sessions.contains_key(&other_peer));
    }

    #[test]
    fn one_source_cannot_monopolize_new_identity_admission() {
        let mut server = make_server();
        let (first_client, first_peer) = connect_peer_from(&mut server, "127.0.0.1");
        let first = hello_envelope(
            "first-source-token",
            sw_contracts::PROTOCOL_VERSION,
            Some("surface-hash"),
        );
        let first_admission_ms = 1_000;
        deliver_hello_at(&mut server, first_peer, &first, first_admission_ms);
        assert_eq!(receive_server_hello(&first_client), (true, String::new()));

        let (rotated_client, rotated_peer) = connect_peer_from(&mut server, "127.0.0.1");
        let rotated = hello_envelope(
            "rotated-source-token",
            sw_contracts::PROTOCOL_VERSION,
            Some("surface-hash"),
        );
        let (other_client, other_peer) = connect_peer_from(&mut server, "127.0.0.2");
        let next_global_ms = first_admission_ms + server.cfg.new_session_min_interval_ms_i64();

        deliver_hello_at(&mut server, rotated_peer, &rotated, next_global_ms);
        assert_eq!(
            receive_server_hello(&rotated_client),
            (false, "server busy; retry".to_string())
        );

        let other = hello_envelope(
            "other-source-token",
            sw_contracts::PROTOCOL_VERSION,
            Some("surface-hash"),
        );
        deliver_hello_at(&mut server, other_peer, &other, next_global_ms);

        assert_eq!(receive_server_hello(&other_client), (true, String::new()));
        assert_eq!(server.sessions.len(), 2);
        assert!(server.sessions.contains_key(&first_peer));
        assert!(!server.sessions.contains_key(&rotated_peer));
        assert!(server.sessions.contains_key(&other_peer));
    }

    #[test]
    fn duplicate_hello_burst_is_dropped_before_response_or_session_work() {
        let mut server = make_server();
        let (client, peer) = connect_peer(&mut server);
        let hello_interval_ms = server.cfg.hello_min_interval_ms_i64();
        let hello = hello_envelope(
            "handshake-token",
            sw_contracts::PROTOCOL_VERSION,
            Some("surface-hash"),
        );
        deliver_hello_at(&mut server, peer, &hello, 1_000);
        assert_eq!(receive_server_hello(&client), (true, String::new()));

        let state = state_envelope();
        let env = decode_envelope(&state).unwrap();
        server.on_client_state(peer, env.payload_as_client_state().unwrap(), 1_000);

        deliver_hello_at(&mut server, peer, &hello, 1_000 + hello_interval_ms);
        assert_eq!(receive_server_hello(&client), (true, String::new()));

        let established = &server.sessions[&peer];
        let player_id = established.player_id;
        let identity_hash = established.identity_hash.clone();
        let display_name = established.display_name.clone();
        let aboard_boat = established.aboard_boat;
        let pos = established.pos;
        let rot = established.rot;
        let vel = established.vel;
        let t_ms = established.t_ms;
        let cell = established.cell;
        let subscribed_cells = established.sub.cells().clone();
        let world_len = server.world.len();
        let world_cell = server.world.cell_of_entity(player_id);
        let seq_after_allowed_retry = server.seq;

        for _ in 0..1_000 {
            deliver_hello_at(&mut server, peer, &hello, 1_000 + hello_interval_ms + 1);
        }

        let after_flood = &server.sessions[&peer];
        assert_eq!(after_flood.player_id, player_id);
        assert_eq!(after_flood.identity_hash, identity_hash);
        assert_eq!(after_flood.display_name, display_name);
        assert_eq!(after_flood.aboard_boat, aboard_boat);
        assert_eq!(after_flood.pos, pos);
        assert_eq!(after_flood.rot, rot);
        assert_eq!(after_flood.vel, vel);
        assert_eq!(after_flood.t_ms, t_ms);
        assert_eq!(after_flood.cell, cell);
        assert_eq!(after_flood.sub.cells(), &subscribed_cells);
        assert_eq!(server.sessions.len(), 1);
        assert_eq!(server.world.len(), world_len);
        assert_eq!(server.world.cell_of_entity(player_id), world_cell);
        assert_eq!(server.seq, seq_after_allowed_retry);
        assert_eq!(server.hello_limiter.tracked_count(), 1);
        assert_no_outbound_datagram(&client);
    }

    #[test]
    fn rotating_fresh_peers_cannot_bypass_global_new_session_admission() {
        let mut server = make_server();
        let admission_ms = 1_000;

        for peer in 1..=64 {
            let hello = hello_envelope(
                &format!("rotated-token-{peer}"),
                sw_contracts::PROTOCOL_VERSION,
                Some("surface-hash"),
            );
            deliver_hello_at(&mut server, peer, &hello, admission_ms);
            server
                .on_disconnect(peer, DisconnectReason::Remote)
                .unwrap();
        }

        assert!(
            server.db.player(1).unwrap().is_some(),
            "the first valid new session should be admitted"
        );
        assert!(
            server.db.player(2).unwrap().is_none(),
            "one admission window must create at most one player row across all peer ids"
        );
        assert_eq!(
            server.hello_limiter.tracked_count(),
            0,
            "disconnect churn must still clear per-peer buckets"
        );

        let next = hello_envelope(
            "next-window-token",
            sw_contracts::PROTOCOL_VERSION,
            Some("surface-hash"),
        );
        let next_admission_ms = admission_ms + server.cfg.new_session_min_interval_ms_i64();
        deliver_hello_at(&mut server, 65, &next, next_admission_ms);
        assert!(
            server.db.player(2).unwrap().is_some(),
            "normal admission must resume after the monotonic global window"
        );
    }

    #[test]
    fn player_capacity_refuses_new_identity_without_state_and_allows_reconnect() {
        let mut server = make_server();
        server.cfg.max_player_rows = 1;
        let interval = server
            .cfg
            .hello_min_interval_ms_i64()
            .max(server.cfg.new_session_min_interval_ms_i64());

        let (first_client, first_peer) = connect_peer(&mut server);
        let first = hello_envelope(
            "capacity-existing-token",
            sw_contracts::PROTOCOL_VERSION,
            Some("surface-hash"),
        );
        deliver_hello_at(&mut server, first_peer, &first, 1_000);
        assert_eq!(receive_server_hello(&first_client), (true, String::new()));
        let existing_player = server.sessions[&first_peer].player_id;
        server
            .on_disconnect(first_peer, DisconnectReason::Remote)
            .unwrap();
        assert!(server.sessions.is_empty());
        assert_eq!(server.world.len(), 0);

        let (refused_client, refused_peer) = connect_peer(&mut server);
        let new_identity = hello_envelope(
            "capacity-new-token",
            sw_contracts::PROTOCOL_VERSION,
            Some("surface-hash"),
        );
        deliver_hello_at(&mut server, refused_peer, &new_identity, 1_000 + interval);
        assert_eq!(
            receive_server_hello(&refused_client),
            (false, "server player capacity reached".to_string())
        );
        assert!(!server.sessions.contains_key(&refused_peer));
        assert_eq!(server.world.len(), 0);
        assert!(
            server
                .db
                .player(existing_player as i64 + 1)
                .unwrap()
                .is_none(),
            "capacity refusal must not create a player row"
        );

        let (returning_client, returning_peer) = connect_peer(&mut server);
        deliver_hello_at(&mut server, returning_peer, &first, 1_000 + 2 * interval);
        assert_eq!(
            receive_server_hello(&returning_client),
            (true, String::new())
        );
        assert_eq!(server.sessions[&returning_peer].player_id, existing_player);
        assert_eq!(server.world.len(), 1);
    }

    #[test]
    fn established_duplicate_does_not_consume_global_new_session_admission() {
        let mut server = make_server();
        let interval = server.cfg.new_session_min_interval_ms_i64();
        let established = hello_envelope(
            "established-token",
            sw_contracts::PROTOCOL_VERSION,
            Some("surface-hash"),
        );
        deliver_hello_at(&mut server, 1, &established, 1_000);
        assert!(server.sessions.contains_key(&1));

        deliver_hello_at(&mut server, 1, &established, 1_000 + interval);
        let fresh = hello_envelope(
            "fresh-token",
            sw_contracts::PROTOCOL_VERSION,
            Some("surface-hash"),
        );
        deliver_hello_at(&mut server, 2, &fresh, 1_000 + interval);

        assert!(
            server.sessions.contains_key(&2),
            "idempotent established hello must not charge the global admission gate"
        );
    }

    #[test]
    fn hello_limit_is_independent_per_peer() {
        let mut server = make_server();
        let first = hello_envelope(
            "first-token",
            sw_contracts::PROTOCOL_VERSION,
            Some("surface-hash"),
        );
        let second = hello_envelope(
            "second-token",
            sw_contracts::PROTOCOL_VERSION,
            Some("surface-hash"),
        );

        deliver_hello_at(&mut server, 1, &first, 1_000);
        let seq_after_first = server.seq;
        deliver_hello_at(&mut server, 1, &first, 1_001);
        assert_eq!(server.seq, seq_after_first);

        let second_admission_ms = 1_000 + server.cfg.new_session_min_interval_ms_i64();
        deliver_hello_at(&mut server, 2, &second, second_admission_ms);
        assert_ne!(server.seq, seq_after_first);
        assert!(server.sessions.contains_key(&1));
        assert!(server.sessions.contains_key(&2));
        assert_eq!(server.hello_limiter.tracked_count(), 2);
    }

    #[test]
    fn disconnect_clears_hello_limit_for_immediate_peer_id_reuse() {
        let mut server = make_server();
        let (client, peer) = connect_peer(&mut server);
        let first = hello_envelope(
            "first-token",
            sw_contracts::PROTOCOL_VERSION + 1,
            Some("surface-hash"),
        );
        deliver_hello_at(&mut server, peer, &first, 1_000);
        assert!(!receive_server_hello(&client).0);
        assert!(!server.sessions.contains_key(&peer));
        assert_eq!(server.hello_limiter.tracked_count(), 1);

        server
            .on_disconnect(peer, DisconnectReason::Remote)
            .unwrap();
        assert_eq!(server.hello_limiter.tracked_count(), 0);
        let seq_before_reuse = server.seq;

        let reused = hello_envelope(
            "reused-peer-token",
            sw_contracts::PROTOCOL_VERSION,
            Some("surface-hash"),
        );
        deliver_hello_at(&mut server, peer, &reused, 1_001);

        assert!(server.sessions.contains_key(&peer));
        assert_ne!(server.seq, seq_before_reuse);
        assert_eq!(server.hello_limiter.tracked_count(), 1);
        assert_eq!(receive_server_hello(&client), (true, String::new()));
    }

    #[test]
    fn same_peer_cannot_replace_an_established_session_with_a_different_identity() {
        let mut server = make_server();
        let (client, peer) = connect_peer(&mut server);
        let hello_interval_ms = server.cfg.hello_min_interval_ms_i64();
        let first = hello_envelope(
            "first-token",
            sw_contracts::PROTOCOL_VERSION,
            Some("surface-hash"),
        );
        deliver_hello_at(&mut server, peer, &first, 1_000);
        assert_eq!(receive_server_hello(&client), (true, String::new()));

        let player_id = server.sessions[&peer].player_id;
        let subscribed_cells = server.sessions[&peer].sub.cells().clone();
        let world_len = server.world.len();
        let world_cell = server.world.cell_of_entity(player_id);
        let seq_before_replacement = server.seq;
        let replacement = hello_envelope(
            "different-token",
            sw_contracts::PROTOCOL_VERSION,
            Some("surface-hash"),
        );

        deliver_hello_at(&mut server, peer, &replacement, 1_000 + hello_interval_ms);

        assert_eq!(server.sessions.len(), 1);
        assert_eq!(server.sessions[&peer].player_id, player_id);
        assert_eq!(server.sessions[&peer].sub.cells(), &subscribed_cells);
        assert_eq!(server.world.len(), world_len);
        assert_eq!(server.world.cell_of_entity(player_id), world_cell);
        assert_eq!(server.seq, seq_before_replacement.wrapping_add(1));
        assert_eq!(
            receive_server_hello(&client),
            (
                false,
                "identity change requires a new connection".to_string()
            )
        );
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
        let hello_limiter = RateLimiter::new(cfg.hello_min_interval_ms_i64());
        let source_session_limiter = BoundedRateLimiter::new(
            cfg.hello_min_interval_ms_i64(),
            cfg.max_transport_peers_usize(),
        );
        let reconnect_limiter = RateLimiter::new(cfg.hello_min_interval_ms_i64());
        let new_session_limiter = GlobalRateLimiter::new(cfg.new_session_min_interval_ms_i64());
        let trade_limiter = RateLimiter::new(cfg.trade_min_interval_ms_i64());
        let client_state_limiter = RateLimiter::new(cfg.client_state_min_interval_ms_i64());
        let chat_limiter = RateLimiter::new(cfg.chat_min_interval_ms_i64());
        let econ_limiter = RateLimiter::new(cfg.econ_min_interval_ms_i64());
        let moor_limiter = RateLimiter::new(cfg.moor_min_interval_ms_i64());
        Server {
            host: Host::bind_with_limits(
                "127.0.0.1:0",
                CONNECT_KEY,
                cfg.max_transport_peers_usize(),
                cfg.max_transport_peers_per_ip_usize(),
            )
            .unwrap(),
            db: Db::open_in_memory().unwrap(),
            world,
            sessions: HashMap::new(),
            identity_players: HashMap::new(),
            seq: 0,
            snapshot_tick: 0,
            boot: Instant::now(),
            epoch_ms: 0,
            weather_seed: 0,
            weather_epoch_day: 0,
            hello_limiter,
            source_session_limiter,
            reconnect_limiter,
            new_session_limiter,
            trade_limiter,
            client_state_limiter,
            chat_limiter,
            econ_limiter,
            moor_limiter,
            running: Arc::new(AtomicBool::new(true)),
            cfg,
        }
    }

    fn hello_envelope(token: &str, name: &str) -> Vec<u8> {
        let mut fbb = FlatBufferBuilder::new();
        let token_off = fbb.create_string(token);
        let name_off = fbb.create_string(name);
        let api_hash_off = fbb.create_string("test-api-surface");
        let hello = p::ClientHello::create(
            &mut fbb,
            &p::ClientHelloArgs {
                protocol_version: sw_contracts::PROTOCOL_VERSION,
                display_name: Some(name_off),
                token: Some(token_off),
                api_surface_hash: Some(api_hash_off),
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
        server.on_client_state(peer, cs.payload_as_client_state().unwrap(), 1_000);

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
/// in-memory server, pinning the acceptance properties for #21 — a trade
/// mutates the shared per-port stock, a replay of the same txn_id never
/// double-applies (and is not throttled), a genuinely new trade inside the
/// aggregate per-player window is rejected, and rotating the port_id cannot
/// bypass that throttle or grow the limiter map.
#[cfg(test)]
mod market_dispatch_tests {
    use super::*;
    use flatbuffers::FlatBufferBuilder;
    use sw_contracts::{decode_envelope, finish_envelope};
    use sw_world::Grid;

    fn make_server(cfg: Config) -> Server {
        let world = World::new(Grid::new(cfg.cell_size_m));
        let hello_limiter = RateLimiter::new(cfg.hello_min_interval_ms_i64());
        let source_session_limiter = BoundedRateLimiter::new(
            cfg.hello_min_interval_ms_i64(),
            cfg.max_transport_peers_usize(),
        );
        let reconnect_limiter = RateLimiter::new(cfg.hello_min_interval_ms_i64());
        let new_session_limiter = GlobalRateLimiter::new(cfg.new_session_min_interval_ms_i64());
        let trade_limiter = RateLimiter::new(cfg.trade_min_interval_ms_i64());
        let client_state_limiter = RateLimiter::new(cfg.client_state_min_interval_ms_i64());
        let chat_limiter = RateLimiter::new(cfg.chat_min_interval_ms_i64());
        let econ_limiter = RateLimiter::new(cfg.econ_min_interval_ms_i64());
        let moor_limiter = RateLimiter::new(cfg.moor_min_interval_ms_i64());
        Server {
            host: Host::bind_with_limits(
                "127.0.0.1:0",
                CONNECT_KEY,
                cfg.max_transport_peers_usize(),
                cfg.max_transport_peers_per_ip_usize(),
            )
            .unwrap(),
            db: Db::open_in_memory().unwrap(),
            world,
            sessions: HashMap::new(),
            identity_players: HashMap::new(),
            seq: 0,
            snapshot_tick: 0,
            boot: Instant::now(),
            epoch_ms: 0,
            weather_seed: 0,
            weather_epoch_day: 0,
            hello_limiter,
            source_session_limiter,
            reconnect_limiter,
            new_session_limiter,
            trade_limiter,
            client_state_limiter,
            chat_limiter,
            econ_limiter,
            moor_limiter,
            running: Arc::new(AtomicBool::new(true)),
            cfg,
        }
    }

    fn hello_envelope(token: &str, name: &str) -> Vec<u8> {
        let mut fbb = FlatBufferBuilder::new();
        let token_off = fbb.create_string(token);
        let name_off = fbb.create_string(name);
        let api_hash_off = fbb.create_string("test-api-surface");
        let hello = p::ClientHello::create(
            &mut fbb,
            &p::ClientHelloArgs {
                protocol_version: sw_contracts::PROTOCOL_VERSION,
                display_name: Some(name_off),
                token: Some(token_off),
                api_surface_hash: Some(api_hash_off),
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
            .on_trade(
                peer,
                env.payload_as_market_trade_request().unwrap(),
                now_ms,
                now_ms,
            )
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

    #[test]
    fn port_rotation_does_not_bypass_the_per_player_throttle() {
        // Regression for the security review's DoS finding: an attacker that
        // sends a fresh, attacker-chosen port_id on every message must NOT get a
        // fresh throttle bucket. The throttle is aggregate per PLAYER, so only
        // the first trade in the window lands regardless of how many distinct
        // ports are rotated through; the rest never touch the DB.
        let mut server = make_server(Config {
            trade_min_interval_ms: 250,
            ..Config::default()
        });
        let peer: PeerId = 1;
        join(&mut server, peer, "tok-rotate");

        // First trade (port 100, t=1000) is admitted.
        apply_trade(&mut server, peer, 1, 100, 5, 10, 100, 1000);
        assert_eq!(server.db.market_state(100, 5).unwrap(), Some((10, 100)));

        // Rotate distinct, attacker-chosen port_ids inside the window, each with
        // a fresh txn_id (so idempotency does not cover it). Every one must be
        // throttled by the per-player bound and leave the DB untouched.
        for port in 200u32..1_200 {
            apply_trade(
                &mut server,
                peer,
                1_000 + port as u64,
                port,
                5,
                10,
                100,
                1_001,
            );
            assert_eq!(
                server.db.market_state(port, 5).unwrap(),
                None,
                "rotated port {port} must not bypass the per-player throttle"
            );
        }

        // The limiter is keyed by player, so 2^32 distinct port_ids cannot grow
        // it: it holds exactly the one player key.
        assert_eq!(server.trade_limiter.tracked_count(), 1);
    }

    #[test]
    fn disconnect_clears_the_players_limiter_entry() {
        // A stale entry for a departed player is useless memory; on_disconnect
        // must drop it so attacker connection churn cannot leave residue behind.
        let mut server = make_server(Config {
            trade_min_interval_ms: 250,
            ..Config::default()
        });
        let peer: PeerId = 1;
        join(&mut server, peer, "tok-dc");

        apply_trade(&mut server, peer, 1, 10, 5, 10, 100, 1000);
        assert_eq!(server.trade_limiter.tracked_count(), 1);

        server
            .on_disconnect(peer, DisconnectReason::Remote)
            .unwrap();
        assert_eq!(
            server.trade_limiter.tracked_count(),
            0,
            "the disconnected player's limiter entry must be cleared"
        );
    }
}

/// Server-hardening (#24): per-message input validation and per-class rate
/// limits exercised against the real handlers. These pin that a hostile
/// ClientState (non-finite / absurd pos) is rejected or clamped before it can
/// drive unbounded cell math (the #19 nit), that an over-long wire string is
/// refused, and that every throttled message class (client-state, chat, econ)
/// drops a flooding client while staying memory-bounded and clearing on
/// disconnect — mirroring the rotation-proof trade limiter from #21.
#[cfg(test)]
mod input_hardening_tests {
    use super::*;
    use flatbuffers::FlatBufferBuilder;
    use sw_contracts::{decode_envelope, finish_envelope};
    use sw_world::Grid;

    fn make_server(cfg: Config) -> Server {
        let world = World::new(Grid::new(cfg.cell_size_m));
        let hello_limiter = RateLimiter::new(cfg.hello_min_interval_ms_i64());
        let source_session_limiter = BoundedRateLimiter::new(
            cfg.hello_min_interval_ms_i64(),
            cfg.max_transport_peers_usize(),
        );
        let reconnect_limiter = RateLimiter::new(cfg.hello_min_interval_ms_i64());
        let new_session_limiter = GlobalRateLimiter::new(cfg.new_session_min_interval_ms_i64());
        let trade_limiter = RateLimiter::new(cfg.trade_min_interval_ms_i64());
        let client_state_limiter = RateLimiter::new(cfg.client_state_min_interval_ms_i64());
        let chat_limiter = RateLimiter::new(cfg.chat_min_interval_ms_i64());
        let econ_limiter = RateLimiter::new(cfg.econ_min_interval_ms_i64());
        let moor_limiter = RateLimiter::new(cfg.moor_min_interval_ms_i64());
        Server {
            host: Host::bind_with_limits(
                "127.0.0.1:0",
                CONNECT_KEY,
                cfg.max_transport_peers_usize(),
                cfg.max_transport_peers_per_ip_usize(),
            )
            .unwrap(),
            db: Db::open_in_memory().unwrap(),
            world,
            sessions: HashMap::new(),
            identity_players: HashMap::new(),
            seq: 0,
            snapshot_tick: 0,
            boot: Instant::now(),
            epoch_ms: 0,
            weather_seed: 0,
            weather_epoch_day: 0,
            hello_limiter,
            source_session_limiter,
            reconnect_limiter,
            new_session_limiter,
            trade_limiter,
            client_state_limiter,
            chat_limiter,
            econ_limiter,
            moor_limiter,
            running: Arc::new(AtomicBool::new(true)),
            cfg,
        }
    }

    fn join_at(
        server: &mut Server,
        peer: PeerId,
        token: &str,
        admission_ms: i64,
        persistence_ms: i64,
    ) -> u64 {
        let mut fbb = FlatBufferBuilder::new();
        let token_off = fbb.create_string(token);
        let name_off = fbb.create_string("Sailor");
        let api_hash_off = fbb.create_string("test-api-surface");
        let hello = p::ClientHello::create(
            &mut fbb,
            &p::ClientHelloArgs {
                protocol_version: sw_contracts::PROTOCOL_VERSION,
                display_name: Some(name_off),
                token: Some(token_off),
                api_surface_hash: Some(api_hash_off),
                ..Default::default()
            },
        );
        let bytes = finish_envelope(&mut fbb, 1, p::Payload::ClientHello, hello.as_union_value());
        let env = decode_envelope(&bytes).unwrap();
        server
            .on_hello_at(
                peer,
                env.payload_as_client_hello().unwrap(),
                admission_ms,
                persistence_ms,
            )
            .unwrap();
        server.sessions[&peer].player_id
    }

    fn join(server: &mut Server, peer: PeerId, token: &str) -> u64 {
        let admission_ms = server.admission_ms();
        join_at(server, peer, token, admission_ms, now_ms())
    }

    #[allow(clippy::too_many_arguments)]
    fn motion_envelope(px: f32, py: f32, pz: f32, vx: f32, vy: f32, vz: f32) -> Vec<u8> {
        let mut fbb = FlatBufferBuilder::new();
        let pos = p::Vec3::new(px, py, pz);
        let rot = p::QuatC::new(0.0, 0.0, 0.0, 1.0);
        let vel = p::Vec3::new(vx, vy, vz);
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

    fn send_state(server: &mut Server, peer: PeerId, env_bytes: &[u8], now_ms: i64) {
        let env = decode_envelope(env_bytes).unwrap();
        server.on_client_state(peer, env.payload_as_client_state().unwrap(), now_ms);
    }

    fn econ_envelope(txn_id: u64, amount: i64, note: &str) -> Vec<u8> {
        let mut fbb = FlatBufferBuilder::new();
        let note_off = fbb.create_string(note);
        let txn = p::EconTxn::create(
            &mut fbb,
            &p::EconTxnArgs {
                txn_id,
                amount_gold: amount,
                kind: 0,
                note: Some(note_off),
            },
        );
        finish_envelope(&mut fbb, 3, p::Payload::EconTxn, txn.as_union_value())
    }

    fn send_econ(server: &mut Server, peer: PeerId, env_bytes: &[u8], now_ms: i64) {
        let env = decode_envelope(env_bytes).unwrap();
        server
            .on_econ(peer, env.payload_as_econ_txn().unwrap(), now_ms, now_ms)
            .unwrap();
    }

    fn moor_envelope(x: f32, z: f32, name: &str) -> Vec<u8> {
        let mut fbb = FlatBufferBuilder::new();
        let pos = p::Vec3::new(x, 0.0, z);
        let rot = p::QuatC::new(0.0, 0.0, 0.0, 1.0);
        let name_off = fbb.create_string(name);
        let req = p::MoorRequest::create(
            &mut fbb,
            &p::MoorRequestArgs {
                pos: Some(&pos),
                rot: Some(&rot),
                name: Some(name_off),
            },
        );
        finish_envelope(&mut fbb, 6, p::Payload::MoorRequest, req.as_union_value())
    }

    fn send_moor(server: &mut Server, peer: PeerId, env_bytes: &[u8], now_ms: i64) {
        let env = decode_envelope(env_bytes).unwrap();
        server
            .on_moor(peer, env.payload_as_moor_request().unwrap(), now_ms, now_ms)
            .unwrap();
    }

    fn trade_envelope(txn_id: u64, qty: i64) -> Vec<u8> {
        let mut fbb = FlatBufferBuilder::new();
        let req = p::MarketTradeRequest::create(
            &mut fbb,
            &p::MarketTradeRequestArgs {
                txn_id,
                port_id: 10,
                item_id: 5,
                qty,
                unit_price: 100,
            },
        );
        finish_envelope(
            &mut fbb,
            7,
            p::Payload::MarketTradeRequest,
            req.as_union_value(),
        )
    }

    fn chat_envelope(text: &str) -> Vec<u8> {
        let mut fbb = FlatBufferBuilder::new();
        let text_off = fbb.create_string(text);
        let chat = p::ChatSend::create(
            &mut fbb,
            &p::ChatSendArgs {
                text: Some(text_off),
                channel: 0,
            },
        );
        finish_envelope(&mut fbb, 5, p::Payload::ChatSend, chat.as_union_value())
    }

    #[test]
    fn backward_wall_clock_does_not_wedge_any_message_rate_limiter() {
        let mut server = make_server(Config {
            client_state_min_interval_ms: 250,
            chat_min_interval_ms: 250,
            econ_min_interval_ms: 250,
            trade_min_interval_ms: 250,
            moor_min_interval_ms: 250,
            ..Config::default()
        });
        let peer: PeerId = 1;
        let player_id = join(&mut server, peer, "tok-clock-liveness");

        let first_admission_ms = 1_000;
        let next_admission_ms = 1_250;
        let first_epoch_ms = 2_000;
        let corrected_epoch_ms = 500;

        for (bytes, admission_ms, epoch_ms) in [
            (
                motion_envelope(1.0, 0.0, 0.0, 0.0, 0.0, 0.0),
                first_admission_ms,
                first_epoch_ms,
            ),
            (
                motion_envelope(2.0, 0.0, 0.0, 0.0, 0.0, 0.0),
                next_admission_ms,
                corrected_epoch_ms,
            ),
            (chat_envelope("first"), first_admission_ms, first_epoch_ms),
            (
                chat_envelope("second"),
                next_admission_ms,
                corrected_epoch_ms,
            ),
            (
                econ_envelope(1, 100, "first"),
                first_admission_ms,
                first_epoch_ms,
            ),
            (
                econ_envelope(2, 50, "second"),
                next_admission_ms,
                corrected_epoch_ms,
            ),
            (trade_envelope(11, 10), first_admission_ms, first_epoch_ms),
            (trade_envelope(12, 5), next_admission_ms, corrected_epoch_ms),
            (
                moor_envelope(0.0, 0.0, "first"),
                first_admission_ms,
                first_epoch_ms,
            ),
            (
                moor_envelope(0.0, 0.0, "second"),
                next_admission_ms,
                corrected_epoch_ms,
            ),
        ] {
            server
                .handle_data_at(peer, &bytes, admission_ms, epoch_ms)
                .unwrap();
        }

        assert_eq!(server.sessions[&peer].pos[0], 2.0);
        assert_eq!(server.db.player_balance(player_id as i64).unwrap(), 150);
        assert_eq!(server.db.market_state(10, 5).unwrap(), Some((15, 100)));
        for admitted_at in [
            server.client_state_limiter.last_accepted_ms(player_id),
            server.chat_limiter.last_accepted_ms(player_id),
            server.econ_limiter.last_accepted_ms(player_id),
            server.trade_limiter.last_accepted_ms(player_id),
            server.moor_limiter.last_accepted_ms(player_id),
        ] {
            assert_eq!(admitted_at, Some(next_admission_ms));
        }
        let cell = server.world.grid().cell_of(0.0, 0.0);
        let moorings = server.db.moorings_in_cell(cell.cx, cell.cz).unwrap();
        assert_eq!(moorings[0].name, "second");
        assert_eq!(moorings[0].created_at, first_epoch_ms);
    }

    // ---- PART 1a: per-message input validation ----

    #[test]
    fn nonfinite_client_state_is_dropped_and_cannot_drive_unbounded_cell_math() {
        // The #19 nit: a hostile Inf/NaN position saturates the `as i32` cast to
        // `i32::MAX`, and the `center.cx + dx` block offset in `cells_in_radius`
        // then overflows (panics in debug, wraps in release). The handler must
        // drop the message before any of it reaches the grid — so this call must
        // NOT panic and must leave the player at its origin cell.
        let cfg = Config::default();
        // Stagger each variant past the client-state throttle window so EVERY
        // non-finite variant clears the per-player throttle and genuinely reaches
        // `validate::sanitize_motion`. Sent at one `now_ms` they would all fall
        // inside the 20 ms window and only the first would exercise the guard —
        // the rest would drop VACUOUSLY at the limiter, hiding a regression.
        let throttle_step = cfg.client_state_min_interval_ms as i64 + 1;
        let mut server = make_server(cfg);
        let peer: PeerId = 1;
        let pid = join(&mut server, peer, "tok-nan");
        let origin = server.world.grid().cell_of(0.0, 0.0);

        for (i, (px, py, pz)) in [
            (f32::INFINITY, 0.0, 0.0),
            (f32::NEG_INFINITY, 0.0, 0.0),
            (f32::NAN, 0.0, f32::NAN),
            (0.0, 0.0, f32::INFINITY),
        ]
        .into_iter()
        .enumerate()
        {
            let now_ms = 1_000 + i as i64 * throttle_step;
            let env = motion_envelope(px, py, pz, 0.0, 0.0, 0.0);
            send_state(&mut server, peer, &env, now_ms);
            // Dropped by sanitize_motion (not the throttle): the player never
            // moved off its origin cell. Fails for ANY variant if the guard is gone.
            assert_eq!(server.sessions[&peer].pos, [0.0, 0.0, 0.0]);
            assert_eq!(server.world.cell_of_entity(pid), Some(origin));
        }
    }

    #[test]
    fn huge_finite_client_state_is_clamped_to_a_bounded_cell() {
        // A finite-but-absurd coordinate is clamped to the world-coordinate bound
        // before it reaches the grid, so the resulting cell is finite and far from
        // the i32 saturation edge — no overflow, bounded work.
        let mut server = make_server(Config::default());
        let peer: PeerId = 1;
        let pid = join(&mut server, peer, "tok-huge");

        let env = motion_envelope(1.0e30, 0.0, -1.0e30, 1.0e12, 0.0, 0.0);
        send_state(&mut server, peer, &env, 1_000);

        let clamped_cell = server
            .world
            .grid()
            .cell_of(validate::MAX_WORLD_COORD_M, -validate::MAX_WORLD_COORD_M);
        assert_eq!(server.world.cell_of_entity(pid), Some(clamped_cell));
        assert_eq!(server.sessions[&peer].pos[0], validate::MAX_WORLD_COORD_M);
        assert_eq!(server.sessions[&peer].pos[2], -validate::MAX_WORLD_COORD_M);
        // Velocity was clamped too.
        assert_eq!(server.sessions[&peer].vel[0], validate::MAX_VELOCITY_MPS);
    }

    #[test]
    fn oversized_chat_and_econ_note_are_rejected() {
        let mut server = make_server(Config {
            max_wire_string_len: 16,
            ..Config::default()
        });
        let peer: PeerId = 1;
        join(&mut server, peer, "tok-str");

        // An econ note past the cap drops the whole txn: nothing is committed.
        let long_note: String = "x".repeat(64);
        let env = econ_envelope(1, 100, &long_note);
        send_econ(&mut server, peer, &env, 1_000);
        assert!(
            server.db.lookup_txn(1).unwrap().is_none(),
            "an over-long econ note must be rejected before the ledger"
        );

        // A within-cap note is accepted.
        let env = econ_envelope(2, 100, "ok");
        send_econ(&mut server, peer, &env, 1_000);
        assert!(server.db.lookup_txn(2).unwrap().is_some());

        // An over-long chat line is a safe no-op (no panic, no broadcast work).
        let mut fbb = FlatBufferBuilder::new();
        let text_off = fbb.create_string(&"y".repeat(64));
        let chat = p::ChatSend::create(
            &mut fbb,
            &p::ChatSendArgs {
                text: Some(text_off),
                channel: 0,
            },
        );
        let bytes = finish_envelope(&mut fbb, 4, p::Payload::ChatSend, chat.as_union_value());
        let env = decode_envelope(&bytes).unwrap();
        server.on_chat(peer, env.payload_as_chat_send().unwrap(), 1_000);
        // The over-long chat was refused before it reached the limiter.
        assert_eq!(server.chat_limiter.tracked_count(), 0);
    }

    // ---- PART 1b: per-class rate limits ----

    #[test]
    fn client_state_flood_is_throttled_per_player() {
        // A per-player client-state throttle drops a flood before it drives the
        // grid/AoI recompute. The first update in the window lands and moves the
        // player; every further update inside the window is dropped, so the player
        // stays at the first accepted position.
        let mut server = make_server(Config {
            client_state_min_interval_ms: 250,
            ..Config::default()
        });
        let peer: PeerId = 1;
        let pid = join(&mut server, peer, "tok-cs");

        let step = server.world.grid().cell_size_m;
        // First update at t=1000 lands.
        send_state(
            &mut server,
            peer,
            &motion_envelope(step * 3.0, 0.0, 0.0, 0.0, 0.0, 0.0),
            1_000,
        );
        let cell_after_first = server.world.cell_of_entity(pid);
        assert_eq!(server.sessions[&peer].pos[0], step * 3.0);

        // A flood of further updates inside the 250ms window is dropped: the
        // player never advances past the first accepted position, and the limiter
        // is keyed by player so it holds exactly one entry.
        for i in 0..1_000 {
            send_state(
                &mut server,
                peer,
                &motion_envelope(step * (10 + i) as f32, 0.0, 0.0, 0.0, 0.0, 0.0),
                1_050,
            );
        }
        assert_eq!(server.sessions[&peer].pos[0], step * 3.0);
        assert_eq!(server.world.cell_of_entity(pid), cell_after_first);
        assert_eq!(server.client_state_limiter.tracked_count(), 1);

        // Once the window elapses the next update is admitted.
        send_state(
            &mut server,
            peer,
            &motion_envelope(step * 20.0, 0.0, 0.0, 0.0, 0.0, 0.0),
            1_300,
        );
        assert_eq!(server.sessions[&peer].pos[0], step * 20.0);
    }

    #[test]
    fn new_econ_txn_flood_is_throttled_but_idempotent_replay_is_not() {
        let mut server = make_server(Config {
            econ_min_interval_ms: 250,
            ..Config::default()
        });
        let peer: PeerId = 1;
        join(&mut server, peer, "tok-econ");

        // First txn at t=1000 lands.
        send_econ(&mut server, peer, &econ_envelope(1, 100, "a"), 1_000);
        assert!(server.db.lookup_txn(1).unwrap().is_some());

        // A *new* txn inside the window is throttled: nothing is committed.
        send_econ(&mut server, peer, &econ_envelope(2, 50, "b"), 1_050);
        assert!(
            server.db.lookup_txn(2).unwrap().is_none(),
            "a new econ txn inside the window must be throttled"
        );

        // A replay of the already-applied txn 1 inside the window is NOT throttled
        // (idempotency-first), so an app-level resend after packet loss stays safe.
        send_econ(&mut server, peer, &econ_envelope(1, 100, "a"), 1_050);
        assert!(server.db.lookup_txn(1).unwrap().is_some());

        // Once the window elapses the new txn is admitted.
        send_econ(&mut server, peer, &econ_envelope(2, 50, "b"), 1_300);
        assert!(server.db.lookup_txn(2).unwrap().is_some());

        // The limiter is keyed by player, so it holds exactly one entry.
        assert_eq!(server.econ_limiter.tracked_count(), 1);
    }

    #[test]
    fn moor_flood_is_throttled_before_the_persistent_write() {
        // MoorRequest mutates persistent state (it commits a moorage record), so an
        // unthrottled flood is a real DoS -- the same class fixed for trades in #21.
        // The per-player moor throttle must reject a new moor inside the window
        // BEFORE the persistent write, mirroring on_trade/on_econ: the first moor
        // lands, a flood inside the window never touches the DB, and the limiter is
        // keyed by player so it stays bounded to exactly one entry.
        let mut server = make_server(Config {
            moor_min_interval_ms: 250,
            ..Config::default()
        });
        let peer: PeerId = 1;
        join(&mut server, peer, "tok-moor");

        // The mooring is keyed by boat_id, falling back to the player id (the
        // player is not aboard a boat here), so every moor upserts the same row --
        // the persisted `name` is what reveals whether a throttled moor committed.
        let cell = server.world.grid().cell_of(0.0, 0.0);
        let persisted_name = |s: &Server| {
            s.db.moorings_in_cell(cell.cx, cell.cz).unwrap()[0]
                .name
                .clone()
        };

        // First moor at t=1000 lands and is persisted.
        send_moor(&mut server, peer, &moor_envelope(0.0, 0.0, "first"), 1_000);
        assert_eq!(persisted_name(&server), "first");

        // A flood of further moors inside the 250ms window is rejected before the
        // write: the persisted mooring keeps the first name and the limiter holds
        // exactly one (per-player) entry no matter how many messages arrive.
        for _ in 0..1_000 {
            send_moor(&mut server, peer, &moor_envelope(0.0, 0.0, "flood"), 1_050);
        }
        assert_eq!(
            persisted_name(&server),
            "first",
            "a moor inside the window must be rejected before the persistent write"
        );
        assert_eq!(server.moor_limiter.tracked_count(), 1);

        // Once the window elapses the next moor is admitted and overwrites the row.
        send_moor(&mut server, peer, &moor_envelope(0.0, 0.0, "third"), 1_300);
        assert_eq!(persisted_name(&server), "third");

        // Disconnect drops the departed player's moor throttle entry so connection
        // churn cannot leave residue behind, mirroring the other message classes.
        server
            .on_disconnect(peer, DisconnectReason::Remote)
            .unwrap();
        assert_eq!(
            server.moor_limiter.tracked_count(),
            0,
            "the disconnected player's moor limiter entry must be cleared"
        );
    }

    fn send_chat(server: &mut Server, peer: PeerId, text: &str, now_ms: i64) {
        let bytes = chat_envelope(text);
        let env = decode_envelope(&bytes).unwrap();
        server.on_chat(peer, env.payload_as_chat_send().unwrap(), now_ms);
    }

    #[test]
    fn chat_handler_is_wired_to_a_memory_bounded_per_player_throttle() {
        // The chat handler consults the per-player throttle before it fans a line
        // out to every AoI subscriber. Driving the real handler with a flood must
        // leave the limiter keyed by exactly one player (rotation-proof, stale-
        // evicting), so connection/traffic churn cannot grow it without bound. The
        // shared RateLimiter's own unit tests pin that within-window messages are
        // dropped; here we pin that the handler is wired to it and stays bounded.
        let mut server = make_server(Config {
            chat_min_interval_ms: 250,
            ..Config::default()
        });
        let peer: PeerId = 1;
        join(&mut server, peer, "tok-chat");

        // A flood of chat lines inside one window from the same player.
        for _ in 0..1_000 {
            send_chat(&mut server, peer, "flood", 1_050);
        }
        assert_eq!(
            server.chat_limiter.tracked_count(),
            1,
            "the chat throttle must be keyed by player and stay bounded under a flood"
        );
    }

    // ---- disconnect clears every class ----

    #[test]
    fn disconnect_clears_every_message_class_limiter() {
        let mut server = make_server(Config {
            hello_min_interval_ms: 250,
            client_state_min_interval_ms: 250,
            chat_min_interval_ms: 250,
            econ_min_interval_ms: 250,
            trade_min_interval_ms: 250,
            moor_min_interval_ms: 250,
            ..Config::default()
        });
        let peer: PeerId = 1;
        let pid = join(&mut server, peer, "tok-dc-all");

        send_state(
            &mut server,
            peer,
            &motion_envelope(1.0, 0.0, 1.0, 0.0, 0.0, 0.0),
            1_000,
        );
        send_econ(&mut server, peer, &econ_envelope(1, 100, "a"), 1_000);
        send_chat(&mut server, peer, "hi", 1_000);
        send_moor(&mut server, peer, &moor_envelope(0.0, 0.0, "m"), 1_000);
        // Seed the trade limiter directly (its handler needs a market envelope,
        // covered in the market dispatch suite); the point here is that
        // on_disconnect clears every class.
        server.trade_limiter.allow(pid, 1_000);
        assert_eq!(server.hello_limiter.tracked_count(), 1);
        assert_eq!(server.client_state_limiter.tracked_count(), 1);
        assert_eq!(server.econ_limiter.tracked_count(), 1);
        assert_eq!(server.chat_limiter.tracked_count(), 1);
        assert_eq!(server.trade_limiter.tracked_count(), 1);
        assert_eq!(server.moor_limiter.tracked_count(), 1);

        server
            .on_disconnect(peer, DisconnectReason::Remote)
            .unwrap();

        assert_eq!(server.hello_limiter.tracked_count(), 0);
        assert_eq!(server.client_state_limiter.tracked_count(), 0);
        assert_eq!(server.econ_limiter.tracked_count(), 0);
        assert_eq!(server.chat_limiter.tracked_count(), 0);
        assert_eq!(server.trade_limiter.tracked_count(), 0);
        assert_eq!(server.moor_limiter.tracked_count(), 0);
    }

    // ---- PART 3: headless load test ----

    #[test]
    #[ignore = "load/perf test: wall-clock timed; run via `make load-test` or the non-blocking CI load job"]
    fn load_n_clients_stay_within_the_tick_budget() {
        // Headless load: N simulated clients drive the *real* client-state handler
        // and snapshot broadcast every tick against the in-memory server. The
        // per-tick server work must stay under the fixed-tick budget (1 / tick_hz),
        // i.e. the server keeps up with real time at N clients. Timing is
        // wall-clock, so this is `#[ignore]`d out of the required gate (`cargo test`
        // skips it) and run only by the non-blocking load job / `make load-test`.
        const N: u32 = 200;
        const TICKS: u32 = 60;
        const LOAD_EPOCH_MS: i64 = 1_700_000_000_000;

        let cfg = Config::default();
        let tick_dt = Duration::from_secs_f64(1.0 / cfg.tick_hz as f64);
        let cell = cfg.cell_size_m;
        let mut server = make_server(cfg);
        let session_step_ms = server.cfg.new_session_min_interval_ms_i64();

        // Join N clients, each seeded into a distinct cell on a roughly square
        // grid so AoI density is realistic and bounded, not all stacked together.
        let side = (N as f64).sqrt().ceil() as u32;
        for i in 0..N {
            let peer = (i + 1) as PeerId;
            let session_offset_ms = i64::from(i) * session_step_ms;
            join_at(
                &mut server,
                peer,
                &format!("tok-load-{i}"),
                1_000 + session_offset_ms,
                LOAD_EPOCH_MS + session_offset_ms,
            );
            let cx = (i % side) as f32;
            let cz = (i / side) as f32;
            // The seed time advances per client so the client-state throttle never
            // drops a placement.
            send_state(
                &mut server,
                peer,
                &motion_envelope(cx * cell + 1.0, 0.0, cz * cell + 1.0, 0.0, 0.0, 0.0),
                1_000 + i as i64,
            );
        }
        assert_eq!(server.sessions.len(), N as usize);
        assert_eq!(server.world.len(), N as usize);

        // Drive TICKS simulated ticks and measure the wall-clock server work. The
        // simulated clock advances by a full tick each round so every client's
        // per-tick update clears the throttle window (worst-case load).
        let step_ms = tick_dt.as_millis() as i64 + 1;
        let start = Instant::now();
        for t in 0..TICKS {
            let now = 10_000 + (t as i64) * step_ms;
            for i in 0..N {
                let peer = (i + 1) as PeerId;
                let cx = (i % side) as f32;
                let cz = (i / side) as f32;
                let jitter = (t % 8) as f32; // small in-cell movement
                send_state(
                    &mut server,
                    peer,
                    &motion_envelope(
                        cx * cell + 1.0 + jitter,
                        0.0,
                        cz * cell + 1.0,
                        0.0,
                        0.0,
                        0.0,
                    ),
                    now,
                );
            }
            server.broadcast_snapshots();
        }
        let elapsed = start.elapsed();
        let per_tick = elapsed / TICKS;

        println!(
            "load: {N} clients x {TICKS} ticks in {elapsed:?} => {per_tick:?}/tick (real-time budget {tick_dt:?})"
        );
        assert!(
            per_tick < tick_dt,
            "per-tick server work {per_tick:?} exceeded the {tick_dt:?} real-time budget at {N} clients"
        );
    }
}
