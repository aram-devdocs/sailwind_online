//! The authoritative server: state, message handlers, and the fixed-tick loop.

use crate::clock::{clock_from_epoch, WorldClock};
use crate::codec::{self, BoatSnap, Caps, MooringSnap, PlayerSnap};
#[cfg(test)]
use crate::config::snapshot_recurrence_ticks;
use crate::config::{
    Config, MAX_PLAYER_ROWS, SNAPSHOT_ENTITIES_PER_PACKET, SNAPSHOT_PACKETS_PER_TICK,
    SNAPSHOT_VISIBILITY_CEILING,
};
use crate::econ_store::{DbLedgerStore, DbMarketStore};
use crate::ratelimit::{BoundedRateLimiter, GlobalRateLimiter, RateLimiter};
use crate::validate;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::io::Write;
use std::net::IpAddr;
use std::ops::Bound::{Excluded, Unbounded};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use sw_contracts::decode_envelope;
use sw_contracts::sw_proto as p;
use sw_econ::{Ledger, Market, MarketAck, Trade, Txn};
use sw_net::{protocol, DisconnectReason, Event, Host, PeerId};
use sw_persist::{Db, MooringRow, PlayerAdmission, MAX_MOORINGS_PAGE_ROWS};
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

/// Global snapshot transport and CPU budgets charged on every fixed server tick.
///
/// Configuration validation derives the exact worst case from these shared
/// constants, including the nominal cadence rounded up to a recipient round.
const SNAPSHOT_ENTITY_SCAN_PER_PACKET: usize = sw_net::DEFAULT_MAX_PEERS;

/// Queued population fanout budgets. Chat is FIFO and rejects new accepted
/// work at the fixed queue boundary; clock state has one coalescing latest-value
/// slot. Both share this fixed per-tick transport budget.
const CHAT_QUEUE_ITEMS: usize = 256;
const CHAT_QUEUE_BYTES: usize = CHAT_QUEUE_ITEMS * (protocol::MTU - protocol::HEADER_SIZE);
const FANOUT_QUEUE_RECIPIENTS: usize = (CHAT_QUEUE_ITEMS + 1) * sw_net::DEFAULT_MAX_PEERS;
const FANOUT_RECIPIENT_SCANS_PER_TICK: usize = sw_net::DEFAULT_MAX_PEERS;
const FANOUT_SENDS_PER_TICK: usize = sw_net::DEFAULT_MAX_PEERS;

/// Persistence work is spread across fixed ticks after each five-second flush
/// boundary. At 1,024 sessions this drains in 128 ticks (about 4.27 seconds).
const DIRTY_DB_UPDATES_PER_TICK: usize = 8;
const SHUTDOWN_FLUSH_ATTEMPTS: usize = 3;

/// Global AoI delivery budget charged on every fixed server tick.
const AOI_WORK_ITEMS_PER_TICK: usize = 8;
const AOI_CELLS_PER_UPDATE: usize = 32;
const CELL_ENTITY_SCAN_PER_WORK: usize = 8;
const CELL_ENTITIES_PER_PACKET: usize = 5;
const CELL_MOORINGS_PER_PACKET: usize = 1;
/// Largest UTF-8 mooring name that fits both unreliable record responses.
///
/// `mooring_name_limit_is_the_largest_mtu_safe_record_name` derives this value
/// through the real codecs and proves that the next byte exceeds the 1,023-byte
/// LiteNetLib payload in at least one of `MoorAck` or `CellSnapshot`.
const MAX_MOORING_NAME_BYTES: usize = 835;
const MOOR_NAME_TOO_LONG_REASON: &str = "mooring name exceeds server limit";

struct CellHydration {
    cell: Cell,
    player_cursor: Option<u64>,
    players_remaining: usize,
    mooring_cursor: Option<i64>,
    players_complete: bool,
    sent_any: bool,
}

#[derive(Clone, Copy)]
struct FanoutRecipient {
    peer: PeerId,
    generation: u64,
}

struct FanoutJob {
    bytes: Vec<u8>,
    sender_cell: Option<Cell>,
    recipients: Arc<[FanoutRecipient]>,
    next_recipient: usize,
}

/// Per-connection state, created on ClientHello.
struct Session {
    generation: u64,
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
    snapshot_cursor: Option<u64>,
    snapshot_remaining: usize,
    snapshot_last_visit_tick: u32,
    published_cells: HashSet<Cell>,
    hydration_cells: HashSet<Cell>,
    hydration_cursor: Option<Cell>,
    active_hydration: Option<CellHydration>,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct SnapshotTickWork {
    recipient_visits: usize,
    candidates_examined: usize,
    player_states_encoded: usize,
    packets: usize,
    encoded_bytes: usize,
    #[cfg(test)]
    advertised: Vec<(PeerId, u64)>,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct AoiTickWork {
    recipient_visits: usize,
    persisted_queries: usize,
    packets: usize,
    cells_completed: usize,
    moorings_encoded: usize,
    corrupt_moorings_skipped: usize,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct RecipientIndexWork {
    recipient_index_operations: usize,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct FanoutTickWork {
    recipient_scans: usize,
    sends: usize,
    encoded_bytes: usize,
    jobs_completed: usize,
    #[cfg(test)]
    delivered: Vec<(PeerId, u64)>,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct DirtyFlushTickWork {
    db_updates: usize,
}

#[cfg(test)]
#[derive(Debug, Default)]
struct DirtyFlushTestHook {
    failures_remaining: usize,
    attempts: usize,
}

impl RecipientIndexWork {
    fn remove(&mut self, recipients: &mut BTreeSet<PeerId>, peer: PeerId) {
        self.recipient_index_operations += 1;
        recipients.remove(&peer);
    }
}

/// The server.
pub struct Server {
    cfg: Config,
    host: Host,
    db: Db,
    world: World,
    sessions: HashMap<PeerId, Session>,
    player_peers: HashMap<u64, PeerId>,
    player_order: BTreeSet<u64>,
    snapshot_recipients: BTreeSet<PeerId>,
    snapshot_recipient_cursor: Option<PeerId>,
    aoi_recipients: BTreeSet<PeerId>,
    aoi_recipient_cursor: Option<PeerId>,
    chat_fanout: VecDeque<FanoutJob>,
    chat_fanout_bytes: usize,
    clock_fanout: Option<FanoutJob>,
    fanout_audience_cache: Option<Arc<[FanoutRecipient]>>,
    fanout_prefer_clock: bool,
    dirty_players: BTreeSet<u64>,
    flush_players: BTreeSet<u64>,
    flush_paused: bool,
    #[cfg(test)]
    dirty_flush_test: DirtyFlushTestHook,
    identity_players: HashMap<String, u64>,
    next_session_generation: u64,
    seq: u32,
    snapshot_tick: u32,
    boot: Instant,
    epoch_ms: i64,
    weather_seed: u64,
    weather_epoch_day: u32,
    hello_limiter: RateLimiter,
    source_session_limiter: BoundedRateLimiter<IpAddr>,
    reconnect_limiter: BoundedRateLimiter<u64>,
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
            cfg.source_session_min_interval_ms_i64(),
            cfg.max_transport_peers_usize(),
        );
        let reconnect_limiter = BoundedRateLimiter::new(
            cfg.hello_min_interval_ms_i64(),
            identity_players
                .len()
                .max(cfg.max_player_rows_u32() as usize),
        );
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
            player_peers: HashMap::new(),
            player_order: BTreeSet::new(),
            snapshot_recipients: BTreeSet::new(),
            snapshot_recipient_cursor: None,
            aoi_recipients: BTreeSet::new(),
            aoi_recipient_cursor: None,
            chat_fanout: VecDeque::new(),
            chat_fanout_bytes: 0,
            clock_fanout: None,
            fanout_audience_cache: None,
            fanout_prefer_clock: true,
            dirty_players: BTreeSet::new(),
            flush_players: BTreeSet::new(),
            flush_paused: false,
            #[cfg(test)]
            dirty_flush_test: DirtyFlushTestHook::default(),
            identity_players,
            next_session_generation: 0,
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
        tracing::info!(
            %addr,
            server = %self.cfg.server_name,
            nominal_snapshot_cadence_ticks = self.cfg.ticks_per_snapshot(),
            "server started"
        );

        let tick_dt = Duration::from_secs_f64(1.0 / self.cfg.tick_hz as f64);
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

            self.process_aoi_work();
            self.broadcast_snapshots();
            self.process_fanout_work();
            if let Err(e) = self.process_dirty_flush_at(now_ms()) {
                tracing::warn!(error = %e, "bounded dirty flush paused until next cadence");
            }

            if tick % ticks_per_clock_broadcast == 0 {
                self.broadcast_clock();
            }

            if frame_start.duration_since(last_flush) >= FLUSH_INTERVAL {
                self.begin_dirty_flush();
                last_flush = frame_start;
            }

            tick = tick.wrapping_add(1);
            let elapsed = frame_start.elapsed();
            if elapsed < tick_dt {
                std::thread::sleep(tick_dt - elapsed);
            }
        }

        tracing::info!("shutting down");
        let flush_result = self.flush_all();
        let _ = self.host.shutdown();
        flush_result
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
        if let Some(pp) = self
            .player_peers
            .get(&player_id)
            .copied()
            .filter(|&pp| pp != peer)
        {
            if self.host.peer_addr(pp).is_some() && !self.host.disconnect(pp) {
                return Err(anyhow::anyhow!("failed to evict superseded transport peer"));
            }
            self.unregister_session(pp);
            self.hello_limiter.clear(u64::from(pp));
        }

        let mut sub = Subscription::new(self.cfg.aoi_radius_i32());
        let origin = self.world.grid().cell_of(0.0, 0.0);
        let aoi = sub.recenter(origin);
        self.world.place_in_cell(player_id, origin);

        self.register_session(
            peer,
            Session {
                generation: 0,
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
                snapshot_cursor: None,
                snapshot_remaining: 0,
                snapshot_last_visit_tick: 0,
                published_cells: HashSet::new(),
                hydration_cells: HashSet::new(),
                hydration_cursor: None,
                active_hydration: None,
            },
        )?;

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
        self.dirty_players.insert(player_id);

        self.emit_aoi(peer, &aoi);
    }

    /// Schedule a changed AoI for bounded delivery from the fixed-tick loop.
    fn emit_aoi(&mut self, peer: PeerId, aoi: &AoiUpdate) {
        if !aoi.is_empty() {
            self.schedule_aoi(peer);
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
        let name_limit = self
            .cfg
            .max_wire_string_len_usize()
            .min(MAX_MOORING_NAME_BYTES);
        if !validate::string_within_limit(name, name_limit) {
            let bytes = codec::moor_ack(self.next_seq(), false, None, MOOR_NAME_TOO_LONG_REASON);
            self.send_bounded(peer, &bytes, "moor rejection");
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
        if self.chat_fanout.len() >= CHAT_QUEUE_ITEMS
            || self.chat_fanout_bytes > CHAT_QUEUE_BYTES - (protocol::MTU - protocol::HEADER_SIZE)
        {
            return;
        }

        // Per-player chat throttle: a flood beyond the configured rate is dropped
        // before it enters the bounded fanout queue.
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
        let Some(recipients) = self.capture_fanout_recipients() else {
            return;
        };

        self.chat_fanout_bytes += bytes.len();
        debug_assert!(self.retained_fanout_audience_entries() <= FANOUT_QUEUE_RECIPIENTS);
        self.chat_fanout.push_back(FanoutJob {
            bytes,
            sender_cell: Some(cell),
            recipients,
            next_recipient: 0,
        });
    }

    fn on_disconnect(&mut self, peer: PeerId, reason: DisconnectReason) -> anyhow::Result<()> {
        self.hello_limiter.clear(u64::from(peer));
        if let Some(s) = self.unregister_session(peer) {
            self.world.remove(s.player_id);
            // Drop session-scoped message throttles. The reconnect cooldown is
            // intentionally retained in its bounded map, otherwise a known
            // identity can disconnect and rotate source addresses to repeat
            // persistence admission inside one per-player window.
            self.trade_limiter.clear(s.player_id);
            self.client_state_limiter.clear(s.player_id);
            self.chat_limiter.clear(s.player_id);
            self.econ_limiter.clear(s.player_id);
            self.moor_limiter.clear(s.player_id);
            self.flush_players.insert(s.player_id);
            if let Err(error) = self.touch_last_seen_for_flush(s.player_id as i64, now_ms()) {
                self.flush_paused = true;
                return Err(error);
            }
            self.flush_players.remove(&s.player_id);
            self.dirty_players.remove(&s.player_id);
            tracing::info!(peer, player_id = s.player_id, ?reason, "peer disconnected");
        }
        Ok(())
    }

    fn register_session(&mut self, peer: PeerId, mut session: Session) -> anyhow::Result<()> {
        if self.sessions.contains_key(&peer) {
            return Err(anyhow::anyhow!("peer already owns a session"));
        }
        if let Some(existing_peer) = self.player_peers.get(&session.player_id) {
            return Err(anyhow::anyhow!(
                "player already owns session peer {existing_peer}"
            ));
        }
        session.generation = self.allocate_session_generation();
        let player_id = session.player_id;
        let dirty = session.dirty;
        self.sessions.insert(peer, session);
        self.player_peers.insert(player_id, peer);
        self.player_order.insert(player_id);
        self.snapshot_recipients.insert(peer);
        self.fanout_audience_cache = None;
        if dirty {
            self.dirty_players.insert(player_id);
        }
        self.schedule_aoi(peer);
        Ok(())
    }

    fn unregister_session(&mut self, peer: PeerId) -> Option<Session> {
        self.unregister_session_with_work(peer)
            .map(|(session, _)| session)
    }

    fn unregister_session_with_work(
        &mut self,
        peer: PeerId,
    ) -> Option<(Session, RecipientIndexWork)> {
        let session = self.sessions.remove(&peer)?;
        if self.player_peers.get(&session.player_id) == Some(&peer) {
            self.player_peers.remove(&session.player_id);
            self.player_order.remove(&session.player_id);
        }
        let mut work = RecipientIndexWork::default();
        work.remove(&mut self.snapshot_recipients, peer);
        work.remove(&mut self.aoi_recipients, peer);
        self.fanout_audience_cache = None;
        Some((session, work))
    }

    fn allocate_session_generation(&mut self) -> u64 {
        if self.next_session_generation == u64::MAX {
            self.invalidate_fanout_jobs();
            self.next_session_generation = 0;
        }
        self.next_session_generation += 1;
        self.next_session_generation
    }

    fn invalidate_fanout_jobs(&mut self) {
        self.chat_fanout.clear();
        self.chat_fanout_bytes = 0;
        self.clock_fanout = None;
        self.fanout_audience_cache = None;
    }

    fn schedule_aoi(&mut self, peer: PeerId) {
        if !self.sessions.contains_key(&peer) {
            return;
        }
        self.aoi_recipients.insert(peer);
    }

    fn broadcast_snapshots(&mut self) -> SnapshotTickWork {
        self.snapshot_tick = self.snapshot_tick.wrapping_add(1);
        let server_tick = self.snapshot_tick;
        let mut work = SnapshotTickWork::default();
        let recipients = ordered_peers_after(
            &self.snapshot_recipients,
            self.snapshot_recipient_cursor,
            SNAPSHOT_PACKETS_PER_TICK,
        );
        if let Some(&last) = recipients.last() {
            self.snapshot_recipient_cursor = Some(last);
        }
        for peer in recipients {
            let Some((self_pid, cell, cursor, remaining, last_visit_tick)) =
                self.sessions.get(&peer).and_then(|s| {
                    s.cell.map(|cell| {
                        (
                            s.player_id,
                            cell,
                            s.snapshot_cursor,
                            s.snapshot_remaining,
                            s.snapshot_last_visit_tick,
                        )
                    })
                })
            else {
                continue;
            };
            let nominal_cadence = self.cfg.ticks_per_snapshot().min(u64::from(u32::MAX)) as u32;
            if last_visit_tick != 0 && server_tick.wrapping_sub(last_visit_tick) < nominal_cadence {
                continue;
            }
            if let Some(session) = self.sessions.get_mut(&peer) {
                session.snapshot_last_visit_tick = server_tick;
            }
            work.recipient_visits += 1;
            let candidates =
                self.player_candidates_after(Some(self_pid), SNAPSHOT_ENTITY_SCAN_PER_PACKET);
            work.candidates_examined += candidates.len();
            let selected: Vec<u64> = candidates
                .into_iter()
                .filter(|&player_id| player_id != self_pid)
                .filter(|&player_id| {
                    self.session_by_player(player_id)
                        .and_then(|session| session.cell)
                        .is_some_and(|other| {
                            other.chebyshev_distance(cell) <= self.cfg.aoi_radius_i32()
                        })
                })
                .take(SNAPSHOT_VISIBILITY_CEILING)
                .collect();
            if selected.is_empty() {
                if let Some(session) = self.sessions.get_mut(&peer) {
                    session.snapshot_cursor = None;
                    session.snapshot_remaining = 0;
                }
                continue;
            }

            let cursor_position = cursor
                .and_then(|cursor| selected.iter().position(|&player_id| player_id == cursor));
            let remaining = if remaining == 0 || cursor_position.is_none() {
                selected.len()
            } else {
                remaining.min(selected.len())
            };
            let start = cursor_position.map_or(0, |position| (position + 1) % selected.len());
            let emit_count = remaining.min(SNAPSHOT_ENTITIES_PER_PACKET);
            let mut players = Vec::new();
            let mut boats = Vec::new();
            let mut last_examined = None;
            for offset in 0..emit_count {
                let player_id = selected[(start + offset) % selected.len()];
                last_examined = Some(player_id);
                let Some(session) = self.session_by_player(player_id) else {
                    continue;
                };
                players.push(player_snap(session));
                if session.aboard_boat != 0 {
                    boats.push(boat_snap(session));
                }
                #[cfg(test)]
                work.advertised.push((peer, player_id));
            }
            if let Some(session) = self.sessions.get_mut(&peer) {
                session.snapshot_cursor = last_examined;
                session.snapshot_remaining = remaining.saturating_sub(emit_count);
            }
            if players.is_empty() && boats.is_empty() {
                continue;
            }
            let bytes = codec::snapshot_delta(self.next_seq(), server_tick, &players, &boats);
            work.player_states_encoded += players.len();
            if self.send_bounded(peer, &bytes, "snapshot delta") {
                work.packets += 1;
                work.encoded_bytes += bytes.len();
            }
        }
        work
    }

    fn player_candidates_after(&self, cursor: Option<u64>, limit: usize) -> Vec<u64> {
        let limit = limit.min(self.player_order.len());
        let mut candidates = Vec::with_capacity(limit);
        if let Some(cursor) = cursor {
            candidates.extend(
                self.player_order
                    .range((Excluded(cursor), Unbounded))
                    .chain(self.player_order.range(..=cursor))
                    .take(limit)
                    .copied(),
            );
        } else {
            candidates.extend(self.player_order.iter().take(limit).copied());
        }
        candidates
    }

    fn process_aoi_work(&mut self) -> AoiTickWork {
        let mut work = AoiTickWork::default();
        let recipients = ordered_peers_after(
            &self.aoi_recipients,
            self.aoi_recipient_cursor,
            AOI_WORK_ITEMS_PER_TICK,
        );
        if let Some(&last) = recipients.last() {
            self.aoi_recipient_cursor = Some(last);
        }
        for peer in recipients {
            self.aoi_recipients.remove(&peer);
            if !self.sessions.contains_key(&peer) {
                continue;
            }
            work.recipient_visits += 1;
            let needs_more = match self.process_one_aoi_work(peer, &mut work) {
                Ok(needs_more) => needs_more,
                Err(error) => {
                    tracing::warn!(peer, error = %error, "bounded AoI hydration failed");
                    true
                }
            };
            if needs_more {
                self.schedule_aoi(peer);
            }
        }
        work
    }

    fn process_one_aoi_work(
        &mut self,
        peer: PeerId,
        work: &mut AoiTickWork,
    ) -> anyhow::Result<bool> {
        if let Some((added, removed)) = self.next_aoi_chunk(peer) {
            let bytes = codec::aoi_update(self.next_seq(), &added, &removed);
            if self.send_bounded(peer, &bytes, "AoI update") {
                work.packets += 1;
            }
            return Ok(self.session_has_aoi_work(peer));
        }

        let active_players = self.player_order.len();
        let Some((cell, player_cursor, players_remaining, players_complete)) =
            self.prepare_cell_hydration(peer, active_players)
        else {
            return Ok(self.session_has_aoi_work(peer));
        };

        if !players_complete {
            let candidates = self.player_candidates_after(
                player_cursor,
                players_remaining.min(CELL_ENTITY_SCAN_PER_WORK),
            );
            let mut players = Vec::new();
            let mut boats = Vec::new();
            let mut examined = 0usize;
            let mut last_examined = None;
            for player_id in candidates {
                examined += 1;
                last_examined = Some(player_id);
                let Some(session) = self.session_by_player(player_id) else {
                    continue;
                };
                if session.cell != Some(cell) {
                    continue;
                }
                players.push(player_snap(session));
                if session.aboard_boat != 0 {
                    boats.push(boat_snap(session));
                }
                if players.len() == CELL_ENTITIES_PER_PACKET {
                    break;
                }
            }

            let mut completed_scan = false;
            if let Some(session) = self.sessions.get_mut(&peer) {
                if let Some(hydration) = session.active_hydration.as_mut() {
                    hydration.player_cursor = last_examined.or(hydration.player_cursor);
                    hydration.players_remaining =
                        hydration.players_remaining.saturating_sub(examined);
                    if hydration.players_remaining == 0 || examined == 0 {
                        hydration.players_complete = true;
                        completed_scan = true;
                    }
                }
            }

            if !players.is_empty() || !boats.is_empty() {
                let bytes = codec::cell_snapshot(self.next_seq(), cell, &players, &boats, &[]);
                if self.send_bounded(peer, &bytes, "cell player snapshot") {
                    work.packets += 1;
                    if let Some(session) = self.sessions.get_mut(&peer) {
                        if let Some(hydration) = session.active_hydration.as_mut() {
                            hydration.sent_any = true;
                        }
                    }
                }
            }
            if !completed_scan {
                return Ok(true);
            }
            return Ok(self.session_has_aoi_work(peer));
        }

        let mooring_cursor = self
            .sessions
            .get(&peer)
            .and_then(|session| session.active_hydration.as_ref())
            .and_then(|hydration| hydration.mooring_cursor);
        let rows = self.db.moorings_in_cell_after(
            cell.cx,
            cell.cz,
            mooring_cursor,
            CELL_MOORINGS_PER_PACKET.min(MAX_MOORINGS_PAGE_ROWS),
        )?;
        work.persisted_queries += 1;
        if let Some(row) = rows.into_iter().next() {
            let next_cursor = row.boat_id;
            let corrupt_name_len = row.name.len();
            let snapshot = (corrupt_name_len <= MAX_MOORING_NAME_BYTES).then(|| mooring_snap(row));
            if let Some(session) = self.sessions.get_mut(&peer) {
                if let Some(hydration) = session.active_hydration.as_mut() {
                    hydration.mooring_cursor = Some(next_cursor);
                }
            }
            if let Some(snapshot) = snapshot {
                let bytes = codec::cell_snapshot(self.next_seq(), cell, &[], &[], &[snapshot]);
                if self.send_bounded(peer, &bytes, "cell mooring snapshot") {
                    work.packets += 1;
                    work.moorings_encoded += 1;
                    if let Some(session) = self.sessions.get_mut(&peer) {
                        if let Some(hydration) = session.active_hydration.as_mut() {
                            hydration.sent_any = true;
                        }
                    }
                }
            } else {
                work.corrupt_moorings_skipped += 1;
                tracing::warn!(
                    boat_id = next_cursor,
                    name_bytes = corrupt_name_len,
                    max_name_bytes = MAX_MOORING_NAME_BYTES,
                    "legacy mooring row exceeds the protocol field limit"
                );
            }
            return Ok(true);
        }

        let sent_any = self
            .sessions
            .get(&peer)
            .and_then(|session| session.active_hydration.as_ref())
            .is_some_and(|hydration| hydration.sent_any);
        if !sent_any {
            let bytes = codec::cell_snapshot(self.next_seq(), cell, &[], &[], &[]);
            if self.send_bounded(peer, &bytes, "empty cell snapshot") {
                work.packets += 1;
            }
        }
        if let Some(session) = self.sessions.get_mut(&peer) {
            session.active_hydration = None;
        }
        work.cells_completed += 1;
        Ok(self.session_has_aoi_work(peer))
    }

    fn next_aoi_chunk(&mut self, peer: PeerId) -> Option<(Vec<Cell>, Vec<Cell>)> {
        let session = self.sessions.get_mut(&peer)?;
        let mut removed: Vec<Cell> = session
            .published_cells
            .difference(session.sub.cells())
            .copied()
            .collect();
        removed.sort_by_key(|cell| (cell.cz, cell.cx));
        removed.truncate(AOI_CELLS_PER_UPDATE);

        let remaining = AOI_CELLS_PER_UPDATE - removed.len();
        let mut added: Vec<Cell> = session
            .sub
            .cells()
            .difference(&session.published_cells)
            .copied()
            .collect();
        added.sort_by_key(|cell| (cell.cz, cell.cx));
        added.truncate(remaining);
        if added.is_empty() && removed.is_empty() {
            return None;
        }

        for cell in &removed {
            session.published_cells.remove(cell);
            session.hydration_cells.remove(cell);
            if session
                .active_hydration
                .as_ref()
                .is_some_and(|active| active.cell == *cell)
            {
                session.active_hydration = None;
            }
        }
        for &cell in &added {
            session.published_cells.insert(cell);
            session.hydration_cells.insert(cell);
        }
        Some((added, removed))
    }

    fn prepare_cell_hydration(
        &mut self,
        peer: PeerId,
        active_players: usize,
    ) -> Option<(Cell, Option<u64>, usize, bool)> {
        let session = self.sessions.get_mut(&peer)?;
        if let Some(active) = session.active_hydration.as_ref() {
            if !session.sub.contains(active.cell) || !session.published_cells.contains(&active.cell)
            {
                session.active_hydration = None;
            }
        }
        if session.active_hydration.is_none() {
            let cell = next_hydration_cell(&session.hydration_cells, session.hydration_cursor)?;
            session.hydration_cells.remove(&cell);
            session.hydration_cursor = Some(cell);
            session.active_hydration = Some(CellHydration {
                cell,
                player_cursor: None,
                players_remaining: active_players,
                mooring_cursor: None,
                players_complete: active_players == 0,
                sent_any: false,
            });
        }
        let hydration = session.active_hydration.as_ref()?;
        Some((
            hydration.cell,
            hydration.player_cursor,
            hydration.players_remaining,
            hydration.players_complete,
        ))
    }

    fn session_has_aoi_work(&self, peer: PeerId) -> bool {
        self.sessions.get(&peer).is_some_and(|session| {
            session.published_cells != *session.sub.cells()
                || session.active_hydration.is_some()
                || !session.hydration_cells.is_empty()
        })
    }

    /// Entity ids visible to a viewer centred on `cell`: everything within the
    /// configured AoI radius, minus the viewer itself. This bounds a
    /// recipient's snapshot to AoI density, never the global population.
    #[cfg(test)]
    fn players_in_view(&self, cell: Cell, self_pid: u64) -> Vec<u64> {
        self.world
            .entities_in_radius(cell, self.cfg.aoi_radius_i32())
            .into_iter()
            .filter(|&eid| eid != self_pid)
            .collect()
    }

    /// Queue the current derived world clock for bounded delivery. Repeated
    /// cadence events coalesce to the latest clock instead of accumulating one
    /// population-wide job per cadence.
    fn broadcast_clock(&mut self) {
        let clock = self.clock_now();
        let bytes = codec::world_clock(self.next_seq(), clock);
        // Coalescing replaces the previous clock job, so release its captured
        // audience before checking whether the current membership snapshot fits.
        self.clock_fanout = None;
        let Some(recipients) = self.capture_fanout_recipients() else {
            return;
        };
        debug_assert!(self.retained_fanout_audience_entries() <= FANOUT_QUEUE_RECIPIENTS);
        self.clock_fanout = Some(FanoutJob {
            bytes,
            sender_cell: None,
            recipients,
            next_recipient: 0,
        });
    }

    fn capture_fanout_recipients(&mut self) -> Option<Arc<[FanoutRecipient]>> {
        if let Some(cached) = self.fanout_audience_cache.as_ref() {
            return Some(Arc::clone(cached));
        }
        let recipients: Vec<FanoutRecipient> = self
            .snapshot_recipients
            .iter()
            .filter_map(|&peer| {
                self.sessions.get(&peer).map(|session| FanoutRecipient {
                    peer,
                    generation: session.generation,
                })
            })
            .take(self.cfg.max_transport_peers_usize())
            .collect();
        if recipients.is_empty()
            || self
                .retained_fanout_audience_entries()
                .checked_add(recipients.len())
                .is_none_or(|entries| entries > FANOUT_QUEUE_RECIPIENTS)
        {
            return None;
        }
        let recipients = Arc::<[FanoutRecipient]>::from(recipients);
        self.fanout_audience_cache = Some(Arc::clone(&recipients));
        Some(recipients)
    }

    fn retained_fanout_audience_entries(&self) -> usize {
        let mut seen = HashSet::new();
        self.fanout_audience_cache
            .iter()
            .chain(self.chat_fanout.iter().map(|job| &job.recipients))
            .chain(self.clock_fanout.iter().map(|job| &job.recipients))
            .filter(|audience| seen.insert(audience.as_ptr() as usize))
            .map(|audience| audience.len())
            .sum()
    }

    fn process_fanout_work(&mut self) -> FanoutTickWork {
        self.process_fanout_work_with_budget(FANOUT_RECIPIENT_SCANS_PER_TICK, FANOUT_SENDS_PER_TICK)
    }

    fn process_fanout_work_with_budget(
        &mut self,
        recipient_scan_budget: usize,
        send_budget: usize,
    ) -> FanoutTickWork {
        let mut work = FanoutTickWork::default();
        while work.recipient_scans < recipient_scan_budget && work.sends < send_budget {
            let from_clock = self.clock_fanout.is_some()
                && (self.fanout_prefer_clock || self.chat_fanout.is_empty());
            let Some(mut job) = (if from_clock {
                self.clock_fanout.take()
            } else {
                self.chat_fanout.pop_front()
            }) else {
                if self.clock_fanout.is_none() && self.chat_fanout.is_empty() {
                    break;
                }
                self.fanout_prefer_clock = !self.fanout_prefer_clock;
                continue;
            };

            while job.next_recipient < job.recipients.len()
                && work.recipient_scans < recipient_scan_budget
                && work.sends < send_budget
            {
                let recipient = job.recipients[job.next_recipient];
                job.next_recipient += 1;
                work.recipient_scans += 1;

                let should_send = self.sessions.get(&recipient.peer).is_some_and(|session| {
                    session.generation == recipient.generation
                        && job
                            .sender_cell
                            .is_none_or(|cell| session.sub.contains(cell))
                });
                if should_send {
                    self.send(recipient.peer, &job.bytes);
                    work.sends += 1;
                    work.encoded_bytes += job.bytes.len();
                    #[cfg(test)]
                    work.delivered.push((recipient.peer, recipient.generation));
                }
            }

            if job.next_recipient == job.recipients.len() {
                work.jobs_completed += 1;
                if from_clock {
                    self.fanout_prefer_clock = false;
                } else {
                    self.chat_fanout_bytes = self.chat_fanout_bytes.saturating_sub(job.bytes.len());
                    self.fanout_prefer_clock = true;
                }
            } else {
                if from_clock {
                    self.clock_fanout = Some(job);
                } else {
                    self.chat_fanout.push_front(job);
                }
                break;
            }
        }
        work
    }

    fn begin_dirty_flush(&mut self) {
        self.flush_paused = false;
        self.flush_players.append(&mut self.dirty_players);
    }

    fn process_dirty_flush_at(&mut self, now: i64) -> anyhow::Result<DirtyFlushTickWork> {
        let mut work = DirtyFlushTickWork::default();
        if self.flush_paused {
            return Ok(work);
        }
        let player_ids: Vec<u64> = self
            .flush_players
            .iter()
            .take(DIRTY_DB_UPDATES_PER_TICK)
            .copied()
            .collect();
        for player_id in player_ids {
            if let Err(error) = self.touch_last_seen_for_flush(player_id as i64, now) {
                self.flush_paused = true;
                return Err(error);
            }
            self.flush_players.remove(&player_id);
            work.db_updates += 1;
            let still_dirty = self.dirty_players.contains(&player_id);
            if let Some(peer) = self.player_peers.get(&player_id).copied() {
                if let Some(session) = self.sessions.get_mut(&peer) {
                    if session.player_id == player_id {
                        session.dirty = still_dirty;
                    }
                }
            }
        }
        Ok(work)
    }

    fn touch_last_seen_for_flush(&mut self, player_id: i64, now: i64) -> anyhow::Result<()> {
        #[cfg(test)]
        {
            self.dirty_flush_test.attempts += 1;
            if self.dirty_flush_test.failures_remaining > 0 {
                self.dirty_flush_test.failures_remaining -= 1;
                return Err(anyhow::anyhow!("injected dirty flush failure"));
            }
        }
        self.db.touch_last_seen(player_id, now)?;
        Ok(())
    }

    #[cfg(test)]
    fn inject_dirty_flush_failures(&mut self, failures: usize) {
        self.dirty_flush_test.failures_remaining = failures;
    }

    #[cfg(test)]
    fn dirty_flush_attempts(&self) -> usize {
        self.dirty_flush_test.attempts
    }

    fn flush_all(&mut self) -> anyhow::Result<()> {
        let now = now_ms();
        let mut pending: BTreeSet<u64> = self.sessions.values().map(|s| s.player_id).collect();
        pending.extend(self.dirty_players.iter().copied());
        pending.extend(self.flush_players.iter().copied());
        self.flush_players.extend(pending.iter().copied());

        let mut last_error = None;
        for _ in 0..SHUTDOWN_FLUSH_ATTEMPTS {
            if pending.is_empty() {
                return Ok(());
            }
            let attempt: Vec<u64> = pending.iter().copied().collect();
            for player_id in attempt {
                match self.touch_last_seen_for_flush(player_id as i64, now) {
                    Ok(()) => {
                        pending.remove(&player_id);
                        self.flush_players.remove(&player_id);
                        self.dirty_players.remove(&player_id);
                        if let Some(peer) = self.player_peers.get(&player_id).copied() {
                            if let Some(session) = self.sessions.get_mut(&peer) {
                                if session.player_id == player_id {
                                    session.dirty = false;
                                }
                            }
                        }
                    }
                    Err(error) => {
                        last_error = Some(error);
                    }
                }
            }
        }
        let failed = pending.len();
        let error = last_error
            .map(|error| error.to_string())
            .unwrap_or_else(|| "unknown persistence failure".to_string());
        Err(anyhow::anyhow!(
            "shutdown persistence failed for {failed} player(s) after \
             {SHUTDOWN_FLUSH_ATTEMPTS} attempts: {error}"
        ))
    }

    fn session_by_player(&self, player_id: u64) -> Option<&Session> {
        let peer = self.player_peers.get(&player_id)?;
        self.sessions
            .get(peer)
            .filter(|session| session.player_id == player_id)
    }

    fn caps(&self) -> Caps {
        Caps {
            tick_hz: self.cfg.tick_hz_u8(),
            snapshot_hz: self.cfg.snapshot_hz_u8(),
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

    fn send_bounded(&mut self, peer: PeerId, bytes: &[u8], kind: &'static str) -> bool {
        if bytes.len() > protocol::MTU - protocol::HEADER_SIZE {
            tracing::error!(
                peer,
                kind,
                bytes = bytes.len(),
                "bounded packet encoder exceeded transport payload"
            );
            return false;
        }
        self.send(peer, bytes);
        true
    }
}

fn ordered_peers_after(
    peers: &BTreeSet<PeerId>,
    cursor: Option<PeerId>,
    limit: usize,
) -> Vec<PeerId> {
    let limit = limit.min(peers.len());
    match cursor {
        Some(cursor) => peers
            .range((Excluded(cursor), Unbounded))
            .chain(peers.range(..=cursor))
            .take(limit)
            .copied()
            .collect(),
        None => peers.iter().take(limit).copied().collect(),
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

fn next_hydration_cell(cells: &HashSet<Cell>, cursor: Option<Cell>) -> Option<Cell> {
    let key = |cell: &Cell| (cell.cz, cell.cx);
    cursor
        .and_then(|cursor| {
            cells
                .iter()
                .filter(|cell| key(cell) > key(&cursor))
                .min_by_key(|cell| key(cell))
                .copied()
        })
        .or_else(|| cells.iter().min_by_key(|cell| key(cell)).copied())
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
        make_server_with_config_and_db(Config::default(), Db::open_in_memory().unwrap())
    }

    fn make_server_with_db(db: Db) -> Server {
        make_server_with_config_and_db(Config::default(), db)
    }

    fn make_server_with_config(cfg: Config) -> Server {
        make_server_with_config_and_db(cfg, Db::open_in_memory().unwrap())
    }

    fn make_server_with_config_and_db(cfg: Config, db: Db) -> Server {
        let world = World::new(Grid::new(cfg.cell_size_m));
        let identity_players = load_identity_players(&db).unwrap();
        let reconnect_key_budget = identity_players
            .len()
            .max(cfg.max_player_rows_u32() as usize);
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
            player_peers: HashMap::new(),
            player_order: BTreeSet::new(),
            snapshot_recipients: BTreeSet::new(),
            snapshot_recipient_cursor: None,
            aoi_recipients: BTreeSet::new(),
            aoi_recipient_cursor: None,
            chat_fanout: VecDeque::new(),
            chat_fanout_bytes: 0,
            clock_fanout: None,
            fanout_audience_cache: None,
            fanout_prefer_clock: true,
            dirty_players: BTreeSet::new(),
            flush_players: BTreeSet::new(),
            flush_paused: false,
            dirty_flush_test: DirtyFlushTestHook::default(),
            identity_players,
            next_session_generation: 0,
            seq: 0,
            snapshot_tick: 0,
            boot: Instant::now(),
            epoch_ms: 0,
            weather_seed: 0,
            weather_epoch_day: 0,
            hello_limiter: RateLimiter::new(cfg.hello_min_interval_ms_i64()),
            source_session_limiter: BoundedRateLimiter::new(
                cfg.source_session_min_interval_ms_i64(),
                cfg.max_transport_peers_usize(),
            ),
            reconnect_limiter: BoundedRateLimiter::new(
                cfg.hello_min_interval_ms_i64(),
                reconnect_key_budget,
            ),
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

    fn chat_envelope(seq: u32, text: &str, channel: u8) -> Vec<u8> {
        let mut fbb = FlatBufferBuilder::new();
        let text = fbb.create_string(text);
        let chat = p::ChatSend::create(
            &mut fbb,
            &p::ChatSendArgs {
                text: Some(text),
                channel,
            },
        );
        finish_envelope(&mut fbb, seq, p::Payload::ChatSend, chat.as_union_value())
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
        connect_peer_from_with_number(server, "127.0.0.1", 0)
    }

    fn connect_peer_from(server: &mut Server, source_ip: &str) -> (UdpSocket, PeerId) {
        connect_peer_from_with_number(server, source_ip, 0)
    }

    fn connect_peer_from_with_number(
        server: &mut Server,
        source_ip: &str,
        connection_number: u8,
    ) -> (UdpSocket, PeerId) {
        let client = UdpSocket::bind((source_ip, 0)).unwrap();
        client.connect(server.host.local_addr().unwrap()).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let connect_data = protocol::write_litenet_string(CONNECT_KEY);
        let request = protocol::build_connect_request(connection_number, 1, 1, 16, &connect_data);
        client.send(&request).unwrap();

        let peer = match server.host.poll(Instant::now()).as_slice() {
            [Event::Connected(peer)] => *peer,
            events => panic!("expected one connected peer, got {events:?}"),
        };
        let mut accept = [0u8; protocol::CONNECT_ACCEPT_SIZE];
        client.recv(&mut accept).unwrap();
        assert_eq!(accept[9], connection_number);
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
    fn nonzero_transport_session_round_trips_hello_with_its_connection_number() {
        for connection_number in 1..protocol::MAX_CONNECTION_NUMBER {
            let mut server = make_server();
            let (client, peer) =
                connect_peer_from_with_number(&mut server, "127.0.0.1", connection_number);
            let hello = hello_envelope(
                &format!("nonzero-session-{connection_number}"),
                sw_contracts::PROTOCOL_VERSION,
                Some("surface-hash"),
            );

            client
                .send(&protocol::build_unreliable(connection_number, &hello))
                .unwrap();
            let events = server.host.poll(Instant::now());
            assert_eq!(events, vec![Event::Data(peer, hello)]);
            for event in events {
                server.handle_event(event).unwrap();
            }

            let mut packet = [0u8; protocol::MTU];
            let received = client.recv(&mut packet).unwrap();
            assert_eq!(
                protocol::Header::from_byte(packet[0]),
                protocol::Header {
                    property: protocol::property::UNRELIABLE,
                    connection_number,
                    fragmented: false,
                }
            );
            let envelope = decode_envelope(&packet[protocol::HEADER_SIZE..received]).unwrap();
            let server_hello = envelope.payload_as_server_hello().unwrap();
            assert!(server_hello.accepted());
            assert_eq!(server_hello.reason(), Some(""));
        }
    }

    #[test]
    fn retired_connection_number_cannot_authenticate_replacement_endpoint() {
        let mut server = make_server();
        let (client, first_peer) = connect_peer_from_with_number(&mut server, "127.0.0.1", 1);
        let connect_data = protocol::write_litenet_string(CONNECT_KEY);
        let replacement_request = protocol::build_connect_request(2, 2, 1, 16, &connect_data);
        client.send(&replacement_request).unwrap();

        let events = server.host.poll(Instant::now());
        assert_eq!(
            events,
            vec![
                Event::Disconnected(first_peer, DisconnectReason::Remote),
                Event::Connected(2),
            ]
        );
        for event in events {
            server.handle_event(event).unwrap();
        }
        let mut accept = [0u8; protocol::CONNECT_ACCEPT_SIZE];
        assert_eq!(
            client.recv(&mut accept).unwrap(),
            protocol::CONNECT_ACCEPT_SIZE
        );
        assert_eq!(accept[9], 2);

        let hello = hello_envelope(
            "replacement-number-token",
            sw_contracts::PROTOCOL_VERSION,
            Some("surface-hash"),
        );
        client.send(&protocol::build_unreliable(1, &hello)).unwrap();
        assert!(
            server.host.poll(Instant::now()).is_empty(),
            "the retired connection number must not reach authentication"
        );
        assert!(server.sessions.is_empty());

        client.send(&protocol::build_unreliable(2, &hello)).unwrap();
        let current_events = server.host.poll(Instant::now());
        assert_eq!(current_events, vec![Event::Data(2, hello)]);
        for event in current_events {
            server.handle_event(event).unwrap();
        }
        assert_eq!(server.sessions.len(), 1);
        assert!(server.sessions.contains_key(&2));

        let mut response = [0u8; protocol::MTU];
        let received = client.recv(&mut response).unwrap();
        let header = protocol::Header::from_byte(response[0]);
        assert_eq!(header.property, protocol::property::UNRELIABLE);
        assert_eq!(header.connection_number, 2);
        let envelope = decode_envelope(&response[protocol::HEADER_SIZE..received]).unwrap();
        assert!(envelope.payload_as_server_hello().unwrap().accepted());
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
    fn valid_chat_is_preencoded_once_and_fanned_out_fifo_through_connected_peers() {
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
        let expected = [("first watch", 1), ("second watch", 2), ("third watch", 3)];
        let seq_before_chat = server.seq;

        for (index, &(text, channel)) in expected.iter().enumerate() {
            let bytes = chat_envelope(index as u32 + 3, text, channel);
            let admission_ms = 1_000 + index as i64 * server.cfg.chat_min_interval_ms_i64();
            server
                .handle_data_at(sender_peer, &bytes, admission_ms, admission_ms)
                .unwrap();
        }

        assert_eq!(
            server.seq,
            seq_before_chat.wrapping_add(expected.len() as u32)
        );
        let fanout = server.process_fanout_work();
        assert_eq!(fanout.recipient_scans, expected.len() * 2);
        assert_eq!(fanout.sends, expected.len() * 2);
        assert_eq!(fanout.jobs_completed, expected.len());
        assert!(fanout.recipient_scans <= FANOUT_RECIPIENT_SCANS_PER_TICK);
        assert!(fanout.sends <= FANOUT_SENDS_PER_TICK);

        for (index, &(expected_text, expected_channel)) in expected.iter().enumerate() {
            let sender_payload = receive_payload(&sender, p::Payload::ChatBroadcast);
            let observer_payload = receive_payload(&observer, p::Payload::ChatBroadcast);
            assert_eq!(
                sender_payload, observer_payload,
                "every recipient must receive the same pre-encoded FIFO item"
            );

            let env = decode_envelope(&observer_payload).unwrap();
            assert_eq!(env.seq(), seq_before_chat.wrapping_add(index as u32 + 1));
            let broadcast = env.payload_as_chat_broadcast().unwrap();
            assert_eq!(broadcast.player_id(), sender_player);
            assert_eq!(broadcast.display_name(), Some("Skipper"));
            assert_eq!(broadcast.text(), Some(expected_text));
            assert_eq!(broadcast.channel(), expected_channel);
        }
    }

    #[test]
    fn chat_queue_accepts_exactly_256_fifo_items_and_rejects_only_over_cap_inputs() {
        assert_eq!(CHAT_QUEUE_ITEMS, 256);
        let mut server = make_server_with_config(Config {
            chat_min_interval_ms: 0,
            ..Config::default()
        });
        let hello = hello_envelope(
            "queue-boundary-token",
            sw_contracts::PROTOCOL_VERSION,
            Some("surface-hash"),
        );
        deliver_hello_at(&mut server, 1, &hello, 1_000);
        let seq_before_chat = server.seq;

        for index in 0..CHAT_QUEUE_ITEMS {
            let message = format!("queue-{index:03}");
            let bytes = chat_envelope(index as u32 + 1, &message, 4);
            server
                .handle_data_at(1, &bytes, 2_000 + index as i64, 2_000 + index as i64)
                .unwrap();
            assert_eq!(server.seq, seq_before_chat.wrapping_add(index as u32 + 1));
            assert_eq!(server.chat_fanout.len(), index + 1);
        }

        let mut decoded_bytes = 0usize;
        for (index, job) in server.chat_fanout.iter().enumerate() {
            decoded_bytes += job.bytes.len();
            let envelope = decode_envelope(&job.bytes).unwrap();
            assert_eq!(
                envelope.seq(),
                seq_before_chat.wrapping_add(index as u32 + 1)
            );
            let broadcast = envelope.payload_as_chat_broadcast().unwrap();
            assert_eq!(broadcast.text(), Some(format!("queue-{index:03}").as_str()));
        }
        assert_eq!(server.chat_fanout.len(), CHAT_QUEUE_ITEMS);
        assert_eq!(
            server.chat_fanout_bytes, decoded_bytes,
            "the byte counter must exactly equal all accepted FIFO payloads"
        );
        assert!(server.chat_fanout_bytes <= CHAT_QUEUE_BYTES);

        let boundary = (
            server.seq,
            server.chat_fanout.len(),
            server.chat_fanout_bytes,
        );
        for index in CHAT_QUEUE_ITEMS..CHAT_QUEUE_ITEMS + 3 {
            let message = format!("queue-{index:03}");
            let bytes = chat_envelope(index as u32 + 1, &message, 4);
            server
                .handle_data_at(1, &bytes, 2_000 + index as i64, 2_000 + index as i64)
                .unwrap();
            assert_eq!(
                (
                    server.seq,
                    server.chat_fanout.len(),
                    server.chat_fanout_bytes,
                ),
                boundary,
                "the first and every later over-cap input must not consume a sequence or grow either queue bound"
            );
        }
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
        let original_player_id = server.sessions[&first_peer].player_id;
        assert_eq!(server.player_peers[&original_player_id], first_peer);

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
        assert_eq!(player_id, original_player_id);
        assert_eq!(server.player_peers[&player_id], replacement_peer);
        assert_eq!(
            server.player_order.iter().copied().collect::<Vec<_>>(),
            vec![player_id]
        );
        assert_eq!(
            server
                .session_by_player(player_id)
                .map(|session| session.player_id),
            Some(player_id)
        );
        assert!(!server.sessions.contains_key(&first_peer));
        assert!(!server
            .snapshot_recipients
            .iter()
            .any(|&peer| peer == first_peer));
        assert!(!server.aoi_recipients.iter().any(|&peer| peer == first_peer));
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
            .send(&protocol::build_unreliable(0, &hello))
            .unwrap();
        first_client.send(&protocol::build_ping(0, 1)).unwrap();
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
    fn retired_transport_event_cannot_resolve_to_a_later_endpoint_in_its_batch() {
        let cfg = Config {
            max_transport_peers: 1,
            max_transport_peers_per_ip: 1,
            ..Config::default()
        };
        let mut server = make_server_with_config(cfg);
        let (old_client, old_peer) = connect_peer_from(&mut server, "127.0.0.1");
        let server_addr = server.host.local_addr().unwrap();
        let new_client = UdpSocket::bind(("127.0.0.2", 0)).unwrap();
        new_client.connect(server_addr).unwrap();
        new_client
            .set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        let old_hello = hello_envelope(
            "retired-peer-token",
            sw_contracts::PROTOCOL_VERSION,
            Some("surface-hash"),
        );
        let connect_data = protocol::write_litenet_string(CONNECT_KEY);
        let new_request = protocol::build_connect_request(0, 2, 1, 16, &connect_data);

        old_client
            .send(&protocol::build_unreliable(0, &old_hello))
            .unwrap();
        old_client.send(&protocol::build_disconnect(0, 1)).unwrap();
        new_client.send(&new_request).unwrap();
        std::thread::sleep(Duration::from_millis(10));

        let events = server.host.poll(Instant::now());
        assert_eq!(
            events,
            vec![
                Event::Data(old_peer, old_hello),
                Event::Disconnected(old_peer, DisconnectReason::Remote),
            ]
        );
        let Event::Data(data_peer, _) = &events[0] else {
            panic!("the old data event must remain first");
        };
        assert_eq!(
            server.host.peer_addr(*data_peer),
            None,
            "production event handling must not resolve old data against the queued endpoint"
        );
        let Event::Disconnected(disconnected_peer, _) = &events[1] else {
            panic!("the old disconnect event must follow its data");
        };
        assert_eq!(data_peer, disconnected_peer);
        assert_no_outbound_datagram(&new_client);

        new_client.send(&new_request).unwrap();
        let events = server.host.poll(Instant::now());
        assert_eq!(events, vec![Event::Connected(old_peer)]);
        let mut accept = [0u8; protocol::CONNECT_ACCEPT_SIZE];
        assert_eq!(
            new_client.recv(&mut accept).unwrap(),
            protocol::CONNECT_ACCEPT_SIZE
        );
        assert_eq!(
            server.host.peer_addr(old_peer),
            Some(new_client.local_addr().unwrap())
        );
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
    fn reconnect_cooldown_survives_disconnect_and_source_rotation() {
        let mut server = make_server();
        let hello = hello_envelope(
            "disconnect-rotation-token",
            sw_contracts::PROTOCOL_VERSION,
            Some("surface-hash"),
        );
        let first_admission_ms = 1_000;
        let (first_client, first_peer) = connect_peer_from(&mut server, "127.0.0.1");

        deliver_hello_at(&mut server, first_peer, &hello, first_admission_ms);
        assert_eq!(receive_server_hello(&first_client), (true, String::new()));
        let player_id = server.sessions[&first_peer].player_id;

        server
            .on_disconnect(first_peer, DisconnectReason::Remote)
            .unwrap();
        assert!(!server.player_peers.contains_key(&player_id));
        assert!(!server.player_order.contains(&player_id));
        assert!(server.session_by_player(player_id).is_none());
        assert!(!server
            .snapshot_recipients
            .iter()
            .any(|&peer| peer == first_peer));
        assert!(!server.aoi_recipients.iter().any(|&peer| peer == first_peer));
        assert_eq!(
            server.reconnect_limiter.tracked_count(),
            1,
            "a completed disconnect must retain the per-player reconnect window"
        );

        let (rotated_client, rotated_peer) = connect_peer_from(&mut server, "127.0.0.2");
        deliver_hello_at(&mut server, rotated_peer, &hello, first_admission_ms + 1);
        assert_eq!(
            receive_server_hello(&rotated_client),
            (false, "server busy; retry".to_string()),
            "source rotation inside the window must not repeat persistence admission"
        );
        assert!(!server.sessions.contains_key(&rotated_peer));
        assert!(!server.player_peers.contains_key(&player_id));

        let after_expiry_ms = first_admission_ms + server.cfg.hello_min_interval_ms_i64() + 1;
        deliver_hello_at(&mut server, rotated_peer, &hello, after_expiry_ms);
        assert_eq!(
            receive_server_hello(&rotated_client),
            (true, String::new()),
            "the same reconnect must become eligible when the window expires"
        );
        assert_eq!(server.sessions[&rotated_peer].player_id, player_id);
        assert_eq!(server.player_peers[&player_id], rotated_peer);
        assert_eq!(
            server
                .session_by_player(player_id)
                .map(|session| session.player_id),
            Some(player_id)
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
    fn slow_global_gate_still_reserves_its_next_slot_for_another_source() {
        let mut server = make_server_with_config(Config {
            hello_min_interval_ms: 250,
            new_session_min_interval_ms: 1_000,
            ..Config::default()
        });
        let first_admission_ms = 1_000;

        let (attacker_client, attacker_peer) = connect_peer_from(&mut server, "127.0.0.1");
        let attacker = hello_envelope(
            "first-attacker-token",
            sw_contracts::PROTOCOL_VERSION,
            Some("surface-hash"),
        );
        deliver_hello_at(&mut server, attacker_peer, &attacker, first_admission_ms);
        assert_eq!(
            receive_server_hello(&attacker_client),
            (true, String::new())
        );

        let (rotated_client, rotated_peer) = connect_peer_from(&mut server, "127.0.0.1");
        let rotated = hello_envelope(
            "rotated-attacker-token",
            sw_contracts::PROTOCOL_VERSION,
            Some("surface-hash"),
        );
        let (legitimate_client, legitimate_peer) = connect_peer_from(&mut server, "127.0.0.2");
        let next_global_ms = first_admission_ms + 1_000;

        deliver_hello_at(&mut server, rotated_peer, &rotated, next_global_ms);
        assert_eq!(
            receive_server_hello(&rotated_client),
            (false, "server busy; retry".to_string()),
            "the source that consumed the prior global slot must not consume the next one"
        );

        let legitimate = hello_envelope(
            "legitimate-other-source-token",
            sw_contracts::PROTOCOL_VERSION,
            Some("surface-hash"),
        );
        deliver_hello_at(&mut server, legitimate_peer, &legitimate, next_global_ms);
        assert_eq!(
            receive_server_hello(&legitimate_client),
            (true, String::new()),
            "another source must be able to take the next global admission slot"
        );
    }

    #[test]
    fn winning_source_cannot_retake_before_an_entire_other_source_window() {
        let mut server = make_server_with_config(Config {
            hello_min_interval_ms: 250,
            new_session_min_interval_ms: 1_000,
            ..Config::default()
        });

        let (first_client, first_peer) = connect_peer_from(&mut server, "127.0.0.1");
        let first = hello_envelope(
            "attacker-first-token",
            sw_contracts::PROTOCOL_VERSION,
            Some("surface-hash"),
        );
        deliver_hello_at(&mut server, first_peer, &first, 1_000);
        assert_eq!(receive_server_hello(&first_client), (true, String::new()));

        let (early_client, early_peer) = connect_peer_from(&mut server, "127.0.0.1");
        let early = hello_envelope(
            "attacker-at-2001",
            sw_contracts::PROTOCOL_VERSION,
            Some("surface-hash"),
        );
        deliver_hello_at(&mut server, early_peer, &early, 2_001);
        assert_eq!(
            receive_server_hello(&early_client),
            (false, "server busy; retry".to_string())
        );

        let (legitimate_client, legitimate_peer) = connect_peer_from(&mut server, "127.0.0.2");
        let legitimate = hello_envelope(
            "legitimate-at-window-end",
            sw_contracts::PROTOCOL_VERSION,
            Some("surface-hash"),
        );
        deliver_hello_at(&mut server, legitimate_peer, &legitimate, 3_000);
        assert_eq!(
            receive_server_hello(&legitimate_client),
            (true, String::new()),
            "another source must be uncontested even at the last instant of its full window"
        );

        let (late_client, late_peer) = connect_peer_from(&mut server, "127.0.0.1");
        let late = hello_envelope(
            "attacker-at-3001",
            sw_contracts::PROTOCOL_VERSION,
            Some("surface-hash"),
        );
        deliver_hello_at(&mut server, late_peer, &late, 3_001);
        assert_eq!(
            receive_server_hello(&late_client),
            (false, "server busy; retry".to_string()),
            "the attacker must not retake the global gate one millisecond after the reserved window"
        );
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
    fn permanent_shutdown_flush_failure_still_sends_transport_shutdown() {
        let mut server = make_server();
        let (client, peer) = connect_peer(&mut server);
        let hello = hello_envelope(
            "shutdown-failure-token",
            sw_contracts::PROTOCOL_VERSION,
            Some("surface-hash"),
        );
        deliver_hello_at(&mut server, peer, &hello, 1_000);
        assert_eq!(receive_server_hello(&client), (true, String::new()));

        server.inject_dirty_flush_failures(usize::MAX);
        server.running.store(false, Ordering::SeqCst);
        let error = server.run().unwrap_err();
        assert!(
            error.to_string().contains("injected dirty flush failure"),
            "the bounded persistence failure must be returned after shutdown: {error}"
        );
        let player_id = server.sessions[&peer].player_id;
        assert!(
            server.flush_players.contains(&player_id) && server.dirty_players.contains(&player_id),
            "a permanent shutdown failure must retain the dirty player id for diagnosis or retry"
        );

        loop {
            let mut packet = [0u8; protocol::MTU];
            let received = client.recv(&mut packet).unwrap();
            if protocol::Header::from_byte(packet[0]).property == protocol::property::DISCONNECT {
                assert_eq!(received, protocol::DISCONNECT_SIZE);
                break;
            }
        }
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
    use std::collections::{HashMap, HashSet};
    use sw_contracts::{decode_envelope, finish_envelope};
    use sw_world::{cells_in_radius, EntityId, Grid};

    fn make_server(cfg: Config) -> Server {
        let world = World::new(Grid::new(cfg.cell_size_m));
        let hello_limiter = RateLimiter::new(cfg.hello_min_interval_ms_i64());
        let source_session_limiter = BoundedRateLimiter::new(
            cfg.source_session_min_interval_ms_i64(),
            cfg.max_transport_peers_usize(),
        );
        let reconnect_limiter = BoundedRateLimiter::new(
            cfg.hello_min_interval_ms_i64(),
            cfg.max_player_rows_u32() as usize,
        );
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
            player_peers: HashMap::new(),
            player_order: BTreeSet::new(),
            snapshot_recipients: BTreeSet::new(),
            snapshot_recipient_cursor: None,
            aoi_recipients: BTreeSet::new(),
            aoi_recipient_cursor: None,
            chat_fanout: VecDeque::new(),
            chat_fanout_bytes: 0,
            clock_fanout: None,
            fanout_audience_cache: None,
            fanout_prefer_clock: true,
            dirty_players: BTreeSet::new(),
            flush_players: BTreeSet::new(),
            flush_paused: false,
            dirty_flush_test: DirtyFlushTestHook::default(),
            identity_players: HashMap::new(),
            next_session_generation: 0,
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

    fn insert_dense_session(server: &mut Server, peer: PeerId, cell: Cell) {
        insert_dense_session_for_player(server, peer, u64::from(peer), cell);
    }

    fn insert_dense_session_for_player(
        server: &mut Server,
        peer: PeerId,
        player_id: u64,
        cell: Cell,
    ) {
        let mut sub = Subscription::new(server.cfg.aoi_radius_i32());
        sub.recenter(cell);
        server.world.place_in_cell(player_id, cell);
        server
            .register_session(
                peer,
                Session {
                    generation: 0,
                    player_id,
                    identity_hash: format!("identity-{player_id}"),
                    display_name: format!("Player {player_id}"),
                    aboard_boat: player_id,
                    pos: [0.0, 0.0, 0.0],
                    rot: [0.0, 0.0, 0.0, 1.0],
                    vel: [0.0, 0.0, 0.0],
                    t_ms: 0,
                    sub,
                    cell: Some(cell),
                    dirty: false,
                    snapshot_cursor: None,
                    snapshot_remaining: 0,
                    snapshot_last_visit_tick: 0,
                    published_cells: HashSet::new(),
                    hydration_cells: HashSet::new(),
                    hydration_cursor: None,
                    active_hydration: None,
                },
            )
            .unwrap();
    }

    fn retained_chat_audience_entries(server: &Server) -> usize {
        let mut audiences = HashSet::new();
        server
            .chat_fanout
            .iter()
            .filter(|job| audiences.insert(job.recipients.as_ptr() as usize))
            .map(|job| job.recipients.len())
            .sum()
    }

    fn chat_bytes(text: &str) -> Vec<u8> {
        let mut fbb = FlatBufferBuilder::new();
        let text = fbb.create_string(text);
        let chat = p::ChatSend::create(
            &mut fbb,
            &p::ChatSendArgs {
                text: Some(text),
                channel: 0,
            },
        );
        finish_envelope(&mut fbb, 1, p::Payload::ChatSend, chat.as_union_value())
    }

    #[test]
    fn stable_membership_shares_one_immutable_fanout_audience() {
        const LIVE_PEERS: PeerId = 32;
        const CHAT_BURST: usize = 128;

        let mut server = make_server(Config {
            chat_min_interval_ms: 0,
            ..Config::default()
        });
        let center = Cell::new(0, 0);
        for peer in 1..=LIVE_PEERS {
            insert_dense_session(&mut server, peer, center);
        }
        let bytes = chat_bytes("shared audience");
        for index in 0..CHAT_BURST {
            let peer = index as PeerId % LIVE_PEERS + 1;
            server
                .handle_data_at(peer, &bytes, 1_000 + index as i64, 1_000 + index as i64)
                .unwrap();
        }

        assert_eq!(server.chat_fanout.len(), CHAT_BURST);
        assert_eq!(
            retained_chat_audience_entries(&server),
            LIVE_PEERS as usize,
            "Arc references must count one immutable retained audience allocation"
        );
        let first = server.chat_fanout.front().unwrap().recipients.as_ptr();
        assert!(
            server
                .chat_fanout
                .iter()
                .all(|job| std::ptr::eq(job.recipients.as_ptr(), first)),
            "unchanged membership must reuse the exact audience allocation"
        );
    }

    #[test]
    fn membership_churn_retains_only_one_bounded_snapshot_per_enqueued_generation() {
        const LIVE_PEERS: PeerId = 32;
        const CHURNED_JOBS: usize = 64;

        let mut server = make_server(Config {
            chat_min_interval_ms: 0,
            ..Config::default()
        });
        let center = Cell::new(0, 0);
        for peer in 1..=LIVE_PEERS {
            insert_dense_session(&mut server, peer, center);
        }
        let bytes = chat_bytes("generation snapshot");
        for generation in 0..CHURNED_JOBS {
            server
                .handle_data_at(
                    1,
                    &bytes,
                    2_000 + generation as i64,
                    2_000 + generation as i64,
                )
                .unwrap();
            let departed = server.unregister_session(LIVE_PEERS).unwrap();
            server.world.remove(departed.player_id);
            insert_dense_session_for_player(
                &mut server,
                LIVE_PEERS,
                10_000 + generation as u64,
                center,
            );
        }

        let retained = retained_chat_audience_entries(&server);
        assert_eq!(
            retained,
            LIVE_PEERS as usize * CHURNED_JOBS,
            "each membership generation must retain one unique frozen audience, not one allocation per Arc reference"
        );
        assert!(retained <= FANOUT_QUEUE_RECIPIENTS);
    }

    #[test]
    fn queued_fanout_never_wraps_or_delivers_to_a_reused_connection_identity() {
        const RECIPIENTS: PeerId = sw_net::DEFAULT_MAX_PEERS as PeerId;

        let mut server = make_server(Config::default());
        let center = Cell::new(0, 0);
        for peer in 1..=RECIPIENTS {
            insert_dense_session(&mut server, peer, center);
        }
        let original_last_generation = server.sessions[&RECIPIENTS].generation;

        let mut fbb = FlatBufferBuilder::new();
        let text = fbb.create_string("captured audience");
        let chat = p::ChatSend::create(
            &mut fbb,
            &p::ChatSendArgs {
                text: Some(text),
                channel: 0,
            },
        );
        let bytes = finish_envelope(&mut fbb, 1, p::Payload::ChatSend, chat.as_union_value());
        let envelope = decode_envelope(&bytes).unwrap();
        server.on_chat(1, envelope.payload_as_chat_send().unwrap(), 1_000);

        let first = server
            .process_fanout_work_with_budget(RECIPIENTS as usize - 1, RECIPIENTS as usize - 1);
        assert_eq!(first.delivered.len(), RECIPIENTS as usize - 1);

        let departed = server.unregister_session(RECIPIENTS).unwrap();
        server.world.remove(departed.player_id);
        insert_dense_session_for_player(
            &mut server,
            RECIPIENTS,
            u64::from(RECIPIENTS) + 10_000,
            center,
        );
        let replacement_generation = server.sessions[&RECIPIENTS].generation;
        assert_ne!(replacement_generation, original_last_generation);

        let last = server.process_fanout_work_with_budget(1, 1);
        assert_eq!(last.recipient_scans, 1);
        assert!(last.delivered.is_empty());
        assert_eq!(last.jobs_completed, 1);

        let delivered: HashSet<_> = first.delivered.iter().copied().collect();
        assert_eq!(delivered.len(), RECIPIENTS as usize - 1);
        for peer in 1..RECIPIENTS {
            assert!(
                delivered
                    .iter()
                    .any(|(delivered_peer, _)| *delivered_peer == peer),
                "each remaining original recipient must receive exactly once"
            );
        }
        assert!(
            !delivered
                .iter()
                .any(|(peer, generation)| *peer == RECIPIENTS
                    && *generation == replacement_generation),
            "a queued message must never reach a later connection reusing the peer slot"
        );
    }

    #[test]
    fn clock_fanout_uses_the_same_captured_connection_identity_path() {
        let mut server = make_server(Config::default());
        let center = Cell::new(0, 0);
        for peer in 1..=2 {
            insert_dense_session(&mut server, peer, center);
        }
        server.broadcast_clock();
        let first = server.process_fanout_work_with_budget(1, 1);
        assert_eq!(first.delivered.len(), 1);

        let old_generation = server.sessions[&2].generation;
        let departed = server.unregister_session(2).unwrap();
        server.world.remove(departed.player_id);
        insert_dense_session_for_player(&mut server, 2, 22, center);
        assert_ne!(server.sessions[&2].generation, old_generation);

        let last = server.process_fanout_work_with_budget(1, 1);
        assert!(last.delivered.is_empty());
        assert_eq!(last.jobs_completed, 1);
    }

    #[test]
    fn fanout_generation_wrap_invalidates_every_queued_identity() {
        let mut server = make_server(Config::default());
        let center = Cell::new(0, 0);
        insert_dense_session(&mut server, 1, center);
        server.broadcast_clock();
        server.next_session_generation = u64::MAX;

        insert_dense_session(&mut server, 2, center);

        assert!(
            server.clock_fanout.is_none() && server.chat_fanout.is_empty(),
            "generation wrap must invalidate all captured pre-wrap identities"
        );
        assert_eq!(server.chat_fanout_bytes, 0);
        assert_eq!(server.retained_fanout_audience_entries(), 0);
    }

    #[test]
    fn dense_snapshot_broadcast_has_a_fixed_global_packet_budget() {
        const DENSE_SESSIONS: PeerId = 1_024;

        let mut server = make_server(Config::default());
        let center = Cell::new(0, 0);
        for peer in 1..=DENSE_SESSIONS {
            insert_dense_session(&mut server, peer, center);
        }

        let recipient_round = (DENSE_SESSIONS as usize).div_ceil(SNAPSHOT_PACKETS_PER_TICK);
        for _ in 0..recipient_round {
            let work = server.broadcast_snapshots();
            assert!(work.recipient_visits <= SNAPSHOT_PACKETS_PER_TICK);
            assert!(
                work.candidates_examined
                    <= SNAPSHOT_PACKETS_PER_TICK * SNAPSHOT_ENTITY_SCAN_PER_PACKET
            );
            assert!(
                work.player_states_encoded
                    <= SNAPSHOT_PACKETS_PER_TICK * SNAPSHOT_ENTITIES_PER_PACKET
            );
            assert!(work.packets <= SNAPSHOT_PACKETS_PER_TICK);
            assert!(
                work.encoded_bytes
                    <= SNAPSHOT_PACKETS_PER_TICK * (protocol::MTU - protocol::HEADER_SIZE)
            );
        }
        assert!(
            server
                .sessions
                .values()
                .all(|session| session.snapshot_cursor.is_some()),
            "one bounded recipient round must eventually visit every session"
        );

        server.snapshot_recipients = BTreeSet::from([1]);
        server.snapshot_recipient_cursor = None;
        {
            let viewer = server.sessions.get_mut(&1).unwrap();
            viewer.snapshot_cursor = None;
            viewer.snapshot_remaining = 0;
            viewer.snapshot_last_visit_tick = 0;
        }
        let mut encoded_for_viewer = 0usize;
        let mut viewer_visits = 0usize;
        loop {
            let work = server.broadcast_snapshots();
            assert!(work.candidates_examined <= SNAPSHOT_ENTITY_SCAN_PER_PACKET);
            assert!(work.packets <= 1);
            assert!(work.encoded_bytes <= protocol::MTU - protocol::HEADER_SIZE);
            encoded_for_viewer += work.player_states_encoded;
            viewer_visits += work.recipient_visits;
            if server.sessions[&1].snapshot_remaining == 0 {
                break;
            }
        }
        assert_eq!(
            encoded_for_viewer,
            SNAPSHOT_VISIBILITY_CEILING,
            "one deterministic entity sweep must emit the recipient's stable dense visibility ceiling"
        );
        assert!(
            viewer_visits <= SNAPSHOT_VISIBILITY_CEILING.div_ceil(SNAPSHOT_ENTITIES_PER_PACKET),
            "entity cursor must make fixed positive progress on every dense visit"
        );
    }

    #[test]
    fn dense_snapshot_elected_visibility_refreshes_before_client_expiry() {
        const DENSE_SESSIONS: PeerId = 1_024;
        const SIMULATION_TICKS: usize = 600;

        let mut server = make_server(Config::default());
        let center = Cell::new(0, 0);
        for peer in 1..=DENSE_SESSIONS {
            insert_dense_session(&mut server, peer, center);
        }

        let recipient_round_ticks = (DENSE_SESSIONS as usize).div_ceil(SNAPSHOT_PACKETS_PER_TICK);
        let elected_packets = SNAPSHOT_VISIBILITY_CEILING.div_ceil(SNAPSHOT_ENTITIES_PER_PACKET);
        let exact_recurrence_bound = recipient_round_ticks * elected_packets;
        assert_eq!(
            exact_recurrence_bound as u64,
            snapshot_recurrence_ticks(DENSE_SESSIONS, server.cfg.ticks_per_snapshot()).unwrap(),
            "config validation and the production scheduler must share one recurrence proof"
        );
        assert_eq!(exact_recurrence_bound, 96);
        let mut last_advertised = HashMap::<(PeerId, u64), usize>::new();
        let mut elected = HashMap::<PeerId, HashSet<u64>>::new();
        let mut repeated = 0usize;
        let mut maximum_recurrence_gap = 0usize;

        for tick in 0..SIMULATION_TICKS {
            let work = server.broadcast_snapshots();
            assert!(work.recipient_visits <= SNAPSHOT_PACKETS_PER_TICK);
            assert!(
                work.candidates_examined
                    <= SNAPSHOT_PACKETS_PER_TICK * SNAPSHOT_ENTITY_SCAN_PER_PACKET
            );
            assert!(work.packets <= SNAPSHOT_PACKETS_PER_TICK);
            assert!(
                work.encoded_bytes
                    <= SNAPSHOT_PACKETS_PER_TICK * (protocol::MTU - protocol::HEADER_SIZE)
            );

            for &(peer, player_id) in &work.advertised {
                elected.entry(peer).or_default().insert(player_id);
                if let Some(previous) = last_advertised.insert((peer, player_id), tick) {
                    repeated += 1;
                    maximum_recurrence_gap = maximum_recurrence_gap.max(tick - previous);
                    assert!(
                        tick - previous <= exact_recurrence_bound,
                        "peer {peer}'s elected player {player_id} went {} ticks without a refresh, \
                         exceeding the scheduler's derived {exact_recurrence_bound}-tick bound",
                        tick - previous
                    );
                }
            }
        }

        assert_eq!(elected.len(), DENSE_SESSIONS as usize);
        assert!(
            elected
                .values()
                .all(|players| players.len() == SNAPSHOT_VISIBILITY_CEILING),
            "every dense recipient must retain one stable, full visibility ceiling"
        );
        assert!(
            repeated >= DENSE_SESSIONS as usize * SNAPSHOT_VISIBILITY_CEILING,
            "every elected pair must be observed often enough to prove recurrence"
        );
        assert_eq!(
            maximum_recurrence_gap, exact_recurrence_bound,
            "the dense proof must observe the exact worst-case gap, not merely a looser expiry threshold"
        );
    }

    #[test]
    fn sparse_snapshots_keep_the_configured_nominal_cadence() {
        let mut server = make_server(Config {
            tick_hz: 30,
            snapshot_hz: 1,
            max_transport_peers: 2,
            max_transport_peers_per_ip: 2,
            ..Config::default()
        });
        let center = Cell::new(0, 0);
        insert_dense_session(&mut server, 1, center);
        insert_dense_session(&mut server, 2, center);

        let cadence = server.cfg.ticks_per_snapshot() as usize;
        let mut packet_ticks = Vec::new();
        for tick in 0..=cadence * 3 {
            let work = server.broadcast_snapshots();
            if work.packets != 0 {
                assert_eq!(work.packets, 2);
                packet_ticks.push(tick);
            }
        }
        assert_eq!(packet_ticks, vec![0, cadence, cadence * 2, cadence * 3]);
    }

    #[test]
    fn dense_aoi_scheduler_visits_all_1024_recipients_in_one_bounded_round() {
        const DENSE_SESSIONS: PeerId = 1_024;

        let mut server = make_server(Config::default());
        let center = Cell::new(0, 0);
        for peer in 1..=DENSE_SESSIONS {
            insert_dense_session(&mut server, peer, center);
        }

        let recipient_round = (DENSE_SESSIONS as usize).div_ceil(AOI_WORK_ITEMS_PER_TICK);
        for _ in 0..recipient_round {
            let work = server.process_aoi_work();
            assert!(work.recipient_visits <= AOI_WORK_ITEMS_PER_TICK);
            assert!(work.persisted_queries <= AOI_WORK_ITEMS_PER_TICK);
            assert!(work.packets <= AOI_WORK_ITEMS_PER_TICK);
        }

        assert!(
            server
                .sessions
                .values()
                .all(|session| session.published_cells == *session.sub.cells()),
            "the ordered cursor must visit every dirty recipient before wrapping"
        );
        assert_eq!(
            server.aoi_recipients.len(),
            DENSE_SESSIONS as usize,
            "unfinished per-session hydration remains one deduplicated work item per live peer"
        );
    }

    #[test]
    fn hostile_movement_churn_cannot_accumulate_stale_hydration_cells() {
        const MOVES: usize = 20_000;

        let mut server = make_server(Config::default());
        insert_dense_session(&mut server, 1, Cell::new(0, 0));

        for step in 0..MOVES {
            let x = if step % 2 == 0 { 10_000.0 } else { 0.0 };
            let cell = server.world.grid().cell_of(x, 0.0);
            let aoi = {
                let session = server.sessions.get_mut(&1).unwrap();
                let update = session.sub.recenter(cell);
                session.cell = Some(cell);
                update
            };
            server.emit_aoi(1, &aoi);
            server.process_aoi_work();

            let session = &server.sessions[&1];
            assert!(
                session.hydration_cells.len() + usize::from(session.active_hydration.is_some())
                    <= session.published_cells.len(),
                "stale movement history escaped the current published-interest bound"
            );
            assert!(
                session
                    .hydration_cells
                    .iter()
                    .all(|cell| session.published_cells.contains(cell)),
                "removed cells must leave hydration storage immediately"
            );
        }

        let final_cell = Cell::new(0, 0);
        let final_update = {
            let session = server.sessions.get_mut(&1).unwrap();
            let update = session.sub.recenter(final_cell);
            session.cell = Some(final_cell);
            update
        };
        server.emit_aoi(1, &final_update);

        let max_visits = server.sessions[&1].sub.cells().len() * 3 + 2;
        for _ in 0..max_visits {
            server.process_aoi_work();
            if !server.session_has_aoi_work(1) {
                break;
            }
        }
        assert!(
            !server.session_has_aoi_work(1),
            "current desired cells must hydrate within a fixed number of visits independent of movement history"
        );
    }

    #[test]
    fn dirty_flush_error_preserves_old_and_new_work_until_the_next_cadence() {
        const INITIAL_LAST_SEEN: i64 = 100;
        const FAILED_FLUSH_AT: i64 = 1_000;
        const RECOVERED_FLUSH_AT: i64 = 6_000;

        let mut server = make_server(Config::default());
        for peer in 1..=2 {
            let player = server
                .db
                .upsert_player_by_token(
                    &format!("{peer:016x}"),
                    &format!("Player {peer}"),
                    INITIAL_LAST_SEEN,
                )
                .unwrap();
            assert_eq!(player.id, i64::from(peer));
            insert_dense_session(&mut server, peer, Cell::new(0, 0));
        }

        let first_state = state_envelope(1.0, 1.0);
        let envelope = decode_envelope(&first_state).unwrap();
        server.on_client_state(
            1,
            envelope.payload_as_client_state().unwrap(),
            FAILED_FLUSH_AT,
        );
        server.begin_dirty_flush();
        assert_eq!(server.flush_players, BTreeSet::from([1]));
        assert!(server.dirty_players.is_empty());

        server.inject_dirty_flush_failures(1);
        let error = server.process_dirty_flush_at(FAILED_FLUSH_AT).unwrap_err();
        assert!(error.to_string().contains("injected dirty flush failure"));
        assert_eq!(server.dirty_flush_attempts(), 1);
        assert!(server.flush_paused);
        assert_eq!(server.flush_players, BTreeSet::from([1]));
        assert!(server.sessions[&1].dirty);

        let second_state = state_envelope(2.0, 2.0);
        let envelope = decode_envelope(&second_state).unwrap();
        server.on_client_state(
            2,
            envelope.payload_as_client_state().unwrap(),
            FAILED_FLUSH_AT + 1,
        );
        assert_eq!(server.flush_players, BTreeSet::from([1]));
        assert_eq!(server.dirty_players, BTreeSet::from([2]));
        assert!(server.sessions[&2].dirty);

        for tick in 0..100 {
            assert_eq!(
                server
                    .process_dirty_flush_at(FAILED_FLUSH_AT + tick)
                    .unwrap(),
                DirtyFlushTickWork::default(),
                "paused ticks must neither retry nor report persistence work"
            );
        }
        assert_eq!(
            server.dirty_flush_attempts(),
            1,
            "one database error must produce one failed attempt until the next five-second cadence"
        );
        for player_id in 1..=2 {
            assert_eq!(
                server.db.player(player_id).unwrap().unwrap().last_seen,
                INITIAL_LAST_SEEN
            );
        }

        server.begin_dirty_flush();
        assert!(!server.flush_paused);
        assert_eq!(
            server.flush_players,
            BTreeSet::from([1, 2]),
            "the recovery cadence must preserve the active retry and merge newly dirty work"
        );
        assert!(server.dirty_players.is_empty());

        let recovered = server.process_dirty_flush_at(RECOVERED_FLUSH_AT).unwrap();
        assert_eq!(recovered.db_updates, 2);
        assert!(recovered.db_updates <= DIRTY_DB_UPDATES_PER_TICK);
        assert_eq!(server.dirty_flush_attempts(), 3);
        assert!(server.flush_players.is_empty());
        assert!(server.dirty_players.is_empty());
        for player_id in 1..=2 {
            assert_eq!(
                server.db.player(player_id).unwrap().unwrap().last_seen,
                RECOVERED_FLUSH_AT
            );
            assert!(!server.sessions[&(player_id as PeerId)].dirty);
        }
    }

    #[test]
    fn dirty_flush_disconnect_and_shutdown_paths_still_persist_live_sessions() {
        const INITIAL_LAST_SEEN: i64 = 100;

        let mut server = make_server(Config::default());
        for peer in 1..=2 {
            let player = server
                .db
                .upsert_player_by_token(
                    &format!("{peer:016x}"),
                    &format!("Player {peer}"),
                    INITIAL_LAST_SEEN,
                )
                .unwrap();
            assert_eq!(player.id, i64::from(peer));
            insert_dense_session(&mut server, peer, Cell::new(0, 0));
            let state = state_envelope(peer as f32, peer as f32);
            let envelope = decode_envelope(&state).unwrap();
            server.on_client_state(
                peer,
                envelope.payload_as_client_state().unwrap(),
                i64::from(peer),
            );
        }
        server.begin_dirty_flush();
        assert_eq!(server.flush_players, BTreeSet::from([1, 2]));

        server.on_disconnect(1, DisconnectReason::Remote).unwrap();
        assert!(!server.flush_players.contains(&1));
        assert!(!server.sessions.contains_key(&1));
        assert!(
            server.db.player(1).unwrap().unwrap().last_seen > INITIAL_LAST_SEEN,
            "disconnect must synchronously persist the departing session"
        );

        server.flush_all().unwrap();
        assert!(
            server.db.player(2).unwrap().unwrap().last_seen > INITIAL_LAST_SEEN,
            "shutdown flush must synchronously persist every remaining live session"
        );
    }

    #[test]
    fn failed_disconnect_persistence_retries_the_player_id_after_peer_reuse() {
        const INITIAL_LAST_SEEN: i64 = 100;
        const RETRY_AT: i64 = 6_000;

        let mut server = make_server(Config::default());
        for player_id in 1..=2 {
            server
                .db
                .upsert_player_by_token(
                    &format!("{player_id:016x}"),
                    &format!("Player {player_id}"),
                    INITIAL_LAST_SEEN,
                )
                .unwrap();
        }
        insert_dense_session_for_player(&mut server, 1, 1, Cell::new(0, 0));

        server.inject_dirty_flush_failures(1);
        let error = server
            .on_disconnect(1, DisconnectReason::Remote)
            .unwrap_err();
        assert!(error.to_string().contains("injected dirty flush failure"));
        assert_eq!(server.flush_players, BTreeSet::from([1]));
        assert!(server.flush_paused);

        insert_dense_session_for_player(&mut server, 1, 2, Cell::new(0, 0));
        for tick in 0..100 {
            assert_eq!(
                server.process_dirty_flush_at(1_000 + tick).unwrap(),
                DirtyFlushTickWork::default(),
                "a disconnect failure must not retry on every server tick"
            );
        }

        server.begin_dirty_flush();
        let work = server.process_dirty_flush_at(RETRY_AT).unwrap();
        assert_eq!(work.db_updates, 1);
        assert_eq!(server.db.player(1).unwrap().unwrap().last_seen, RETRY_AT);
        assert_eq!(
            server.db.player(2).unwrap().unwrap().last_seen,
            INITIAL_LAST_SEEN,
            "peer-slot reuse must not redirect orphaned persistence work"
        );
        assert!(server.flush_players.is_empty());
    }

    #[test]
    fn shutdown_flush_attempts_all_players_and_retries_only_failures() {
        const INITIAL_LAST_SEEN: i64 = 100;

        let mut server = make_server(Config::default());
        for peer in 1..=2 {
            server
                .db
                .upsert_player_by_token(
                    &format!("{peer:016x}"),
                    &format!("Player {peer}"),
                    INITIAL_LAST_SEEN,
                )
                .unwrap();
            insert_dense_session(&mut server, peer, Cell::new(0, 0));
        }
        server.inject_dirty_flush_failures(1);

        server.flush_all().unwrap();

        assert_eq!(
            server.dirty_flush_attempts(),
            3,
            "the first pass must continue after row one fails, then retry only that row"
        );
        for player_id in 1..=2 {
            assert!(
                server.db.player(player_id).unwrap().unwrap().last_seen > INITIAL_LAST_SEEN,
                "shutdown must persist player {player_id}"
            );
        }
        assert!(server.dirty_players.is_empty());
        assert!(server.flush_players.is_empty());
    }

    #[test]
    fn hostile_population_fanout_and_flush_stay_capped_and_make_progress() {
        const DENSE_SESSIONS: PeerId = 1_024;
        const SIMULATION_TICKS: usize = 603;
        const FLUSH_TICKS: usize = 5 * 30;
        const CLOCK_TICKS: usize = 10 * 30;

        let mut server = make_server(Config::default());
        let center = Cell::new(0, 0);
        for peer in 1..=DENSE_SESSIONS {
            insert_dense_session(&mut server, peer, center);
            let session = server.sessions.get_mut(&peer).unwrap();
            session.dirty = true;
            server.dirty_players.insert(session.player_id);
        }

        let seq_before_chat = server.seq;
        for peer in 1..=DENSE_SESSIONS {
            let mut fbb = FlatBufferBuilder::new();
            let text = fbb.create_string("transport-sized hostile batch");
            let chat = p::ChatSend::create(
                &mut fbb,
                &p::ChatSendArgs {
                    text: Some(text),
                    channel: 0,
                },
            );
            let bytes = finish_envelope(&mut fbb, 1, p::Payload::ChatSend, chat.as_union_value());
            let envelope = decode_envelope(&bytes).unwrap();
            server.on_chat(
                peer,
                envelope.payload_as_chat_send().unwrap(),
                i64::from(peer),
            );
        }
        assert_eq!(
            server.seq.wrapping_sub(seq_before_chat) as usize,
            CHAT_QUEUE_ITEMS,
            "one real maximum transport batch is accepted and excess distinct-player chat is rejected"
        );
        assert_eq!(server.chat_fanout.len(), CHAT_QUEUE_ITEMS);
        assert!(server.chat_fanout_bytes <= CHAT_QUEUE_BYTES);
        assert_eq!(
            server.retained_fanout_audience_entries(),
            DENSE_SESSIONS as usize,
            "all stable-membership jobs and the cache share one audience allocation"
        );

        let mut total_jobs_completed = 0usize;
        let mut total_db_updates = 0usize;
        for tick in 0..SIMULATION_TICKS {
            let fanout = server.process_fanout_work();
            assert!(fanout.recipient_scans <= FANOUT_RECIPIENT_SCANS_PER_TICK);
            assert!(fanout.sends <= FANOUT_SENDS_PER_TICK);
            assert!(
                fanout.encoded_bytes
                    <= FANOUT_SENDS_PER_TICK * (protocol::MTU - protocol::HEADER_SIZE)
            );
            total_jobs_completed += fanout.jobs_completed;

            let flush = server.process_dirty_flush_at(1_700_000_000_000 + tick as i64);
            let flush = flush.unwrap();
            assert!(flush.db_updates <= DIRTY_DB_UPDATES_PER_TICK);
            total_db_updates += flush.db_updates;

            if tick > 0 && tick % FLUSH_TICKS == 0 {
                server.begin_dirty_flush();
            }
            if tick % CLOCK_TICKS == 0 {
                server.broadcast_clock();
                server.broadcast_clock();
                assert!(
                    server.clock_fanout.is_some(),
                    "repeated clock cadence coalesces into one latest-value job"
                );
            }

            assert!(server.chat_fanout.len() <= CHAT_QUEUE_ITEMS);
            assert!(server.chat_fanout_bytes <= CHAT_QUEUE_BYTES);
            assert!(server.clock_fanout.iter().count() <= 1);
            assert!(server.retained_fanout_audience_entries() <= FANOUT_QUEUE_RECIPIENTS);
            assert!(server.dirty_players.len() <= server.cfg.max_player_rows_u32() as usize);
            assert!(server.flush_players.len() <= server.cfg.max_player_rows_u32() as usize);
            assert!(
                server.dirty_players.len() + server.flush_players.len()
                    <= server.cfg.max_player_rows_u32() as usize * 2
            );
        }

        assert!(server.chat_fanout.is_empty());
        assert_eq!(server.chat_fanout_bytes, 0);
        assert_eq!(
            server.retained_fanout_audience_entries(),
            DENSE_SESSIONS as usize,
            "the current membership cache retains one reusable bounded audience"
        );
        assert!(
            server.clock_fanout.is_none(),
            "the last coalesced clock must eventually reach its bounded audience"
        );
        assert!(server.dirty_players.is_empty());
        assert!(server.flush_players.is_empty());
        assert_eq!(total_db_updates, DENSE_SESSIONS as usize);
        assert!(
            total_jobs_completed >= CHAT_QUEUE_ITEMS + 3,
            "all accepted chats and the three coalesced clock jobs must complete fairly"
        );
    }

    #[test]
    fn reconnect_churn_keeps_recipient_indexes_bounded_without_linear_cleanup() {
        const LIVE_PEERS: PeerId = 1_024;
        const CHURN_EVENTS: PeerId = 4_096;

        let mut server = make_server(Config::default());
        let center = Cell::new(0, 0);
        let mut live = VecDeque::new();
        for peer in 1..=LIVE_PEERS {
            insert_dense_session(&mut server, peer, center);
            live.push_back(peer);
        }

        for event in 0..CHURN_EVENTS {
            let departed = live.pop_front().unwrap();
            let (_, work) = server
                .unregister_session_with_work(departed)
                .expect("the selected live session must unregister");
            server.world.remove(u64::from(departed));
            assert_eq!(
                work.recipient_index_operations, 2,
                "disconnect cleanup must perform a fixed pair of ordered-index removals"
            );

            let replacement = LIVE_PEERS + event + 1;
            insert_dense_session(&mut server, replacement, center);
            live.push_back(replacement);

            assert_eq!(server.sessions.len(), LIVE_PEERS as usize);
            assert_eq!(
                server.snapshot_recipients.len(),
                server.sessions.len(),
                "the snapshot index must contain exactly the live peers"
            );
            assert!(
                server.aoi_recipients.len() <= server.sessions.len(),
                "the dirty AoI index must never exceed the live peers"
            );
            assert!(
                server
                    .snapshot_recipients
                    .iter()
                    .all(|peer| server.sessions.contains_key(peer)),
                "disconnect/reconnect churn must not accumulate stale snapshot work"
            );
            assert!(
                server
                    .aoi_recipients
                    .iter()
                    .all(|peer| server.sessions.contains_key(peer)),
                "disconnect/reconnect churn must not accumulate stale AoI work"
            );
        }
    }

    #[test]
    fn fixed_snapshot_chunks_fit_the_unreliable_payload() {
        let players: Vec<PlayerSnap> = (0..SNAPSHOT_ENTITIES_PER_PACKET)
            .map(|id| PlayerSnap {
                player_id: id as u64,
                pos: [f32::MAX; 3],
                rot: [f32::MAX; 4],
                aboard_boat: id as u64,
                t_ms: u32::MAX,
            })
            .collect();
        let boats: Vec<BoatSnap> = (0..SNAPSHOT_ENTITIES_PER_PACKET)
            .map(|id| BoatSnap {
                boat_id: id as u64,
                owner: id as u64,
                pos: [f32::MAX; 3],
                rot: [f32::MAX; 4],
                vel: [f32::MAX; 3],
                t_ms: u32::MAX,
            })
            .collect();
        let mooring = MooringSnap {
            boat_id: u64::MAX,
            owner: u64::MAX,
            cell: (i32::MIN, i32::MAX),
            pos: [f32::MAX; 3],
            rot: [f32::MAX; 4],
            name: "m".repeat(MAX_MOORING_NAME_BYTES),
            created_at: u64::MAX,
        };
        let added: Vec<Cell> = (0..AOI_CELLS_PER_UPDATE / 2)
            .map(|cell| Cell::new(cell as i32, i32::MIN))
            .collect();
        let removed: Vec<Cell> = (0..AOI_CELLS_PER_UPDATE / 2)
            .map(|cell| Cell::new(cell as i32, i32::MAX))
            .collect();
        let payload_limit = protocol::MTU - protocol::HEADER_SIZE;

        for (kind, bytes) in [
            (
                "snapshot delta",
                codec::snapshot_delta(1, 1, &players, &boats),
            ),
            (
                "cell player snapshot",
                codec::cell_snapshot(1, Cell::new(0, 0), &players, &boats, &[]),
            ),
            (
                "cell mooring snapshot",
                codec::cell_snapshot(1, Cell::new(0, 0), &[], &[], &[mooring]),
            ),
            ("AoI update", codec::aoi_update(1, &added, &removed)),
        ] {
            assert!(
                bytes.len() <= payload_limit,
                "{kind} encoded {} bytes beyond the {payload_limit}-byte unreliable payload",
                bytes.len()
            );
        }
    }

    #[test]
    fn radius_sixteen_admission_defers_persisted_cell_hydration() {
        let mut server = make_server(Config {
            aoi_radius_cells: 16,
            ..Config::default()
        });
        let hello_bytes = hello_envelope("tok-wide-aoi", "Wide AoI");
        let hello = decode_envelope(&hello_bytes).unwrap();
        let seq_before = server.seq;

        server
            .on_hello(1, hello.payload_as_client_hello().unwrap())
            .unwrap();

        assert_eq!(
            server.seq.wrapping_sub(seq_before),
            1,
            "admission must send only ServerHello; the 1,089-cell persisted AoI \
             hydration belongs to the bounded fixed-tick scheduler"
        );
    }

    #[test]
    fn radius_sixteen_hydration_has_fixed_tick_bounds_and_eventual_progress() {
        let mut server = make_server(Config {
            aoi_radius_cells: 16,
            ..Config::default()
        });
        let last_cell = Cell::new(16, 16);
        for (boat_id, name) in [
            (1, "x".repeat(MAX_MOORING_NAME_BYTES + 1)),
            (2, "eventual".to_string()),
        ] {
            server
                .db
                .upsert_mooring(&MooringRow {
                    boat_id,
                    owner: 1,
                    cell_x: last_cell.cx,
                    cell_z: last_cell.cz,
                    pos: [0.0; 3],
                    rot: [0.0, 0.0, 0.0, 1.0],
                    name,
                    created_at: 1,
                })
                .unwrap();
        }
        let hello_bytes = hello_envelope("tok-wide-progress", "Wide Progress");
        let hello = decode_envelope(&hello_bytes).unwrap();
        server
            .on_hello(1, hello.payload_as_client_hello().unwrap())
            .unwrap();

        let mut total_queries = 0usize;
        let mut total_completed = 0usize;
        let mut total_moorings = 0usize;
        let mut total_corrupt_skips = 0usize;
        for _ in 0..5_000 {
            let work = server.process_aoi_work();
            assert!(work.recipient_visits <= AOI_WORK_ITEMS_PER_TICK);
            assert!(work.persisted_queries <= AOI_WORK_ITEMS_PER_TICK);
            assert!(work.packets <= AOI_WORK_ITEMS_PER_TICK);
            total_queries += work.persisted_queries;
            total_completed += work.cells_completed;
            total_moorings += work.moorings_encoded;
            total_corrupt_skips += work.corrupt_moorings_skipped;
            if !server.session_has_aoi_work(1) {
                break;
            }
        }

        let session = &server.sessions[&1];
        assert_eq!(session.published_cells, *session.sub.cells());
        assert!(!server.session_has_aoi_work(1));
        assert_eq!(total_completed, 1_089);
        assert_eq!(
            total_queries, 1_091,
            "1,089 cells plus two keyset continuations in the hostile final cell"
        );
        assert_eq!(
            total_corrupt_skips, 1,
            "a directly injected legacy row above the protocol limit is corruption, not accepted content"
        );
        assert_eq!(
            total_moorings, 1,
            "the valid row after an oversized legacy row must still progress"
        );
    }

    #[test]
    fn caps_advertise_configured_aoi() {
        let server = make_server(Config {
            tick_hz: u32::from(u8::MAX),
            snapshot_hz: u32::from(u8::MAX),
            aoi_radius_cells: 5,
            cell_size_m: 2048.0,
            ..Config::default()
        });
        let caps = server.caps();
        assert_eq!(caps.tick_hz, u8::MAX);
        assert_eq!(caps.snapshot_hz, u8::MAX);
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
            cfg.source_session_min_interval_ms_i64(),
            cfg.max_transport_peers_usize(),
        );
        let reconnect_limiter = BoundedRateLimiter::new(
            cfg.hello_min_interval_ms_i64(),
            cfg.max_player_rows_u32() as usize,
        );
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
            player_peers: HashMap::new(),
            player_order: BTreeSet::new(),
            snapshot_recipients: BTreeSet::new(),
            snapshot_recipient_cursor: None,
            aoi_recipients: BTreeSet::new(),
            aoi_recipient_cursor: None,
            chat_fanout: VecDeque::new(),
            chat_fanout_bytes: 0,
            clock_fanout: None,
            fanout_audience_cache: None,
            fanout_prefer_clock: true,
            dirty_players: BTreeSet::new(),
            flush_players: BTreeSet::new(),
            flush_paused: false,
            dirty_flush_test: DirtyFlushTestHook::default(),
            identity_players: HashMap::new(),
            next_session_generation: 0,
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
            cfg.source_session_min_interval_ms_i64(),
            cfg.max_transport_peers_usize(),
        );
        let reconnect_limiter = BoundedRateLimiter::new(
            cfg.hello_min_interval_ms_i64(),
            cfg.max_player_rows_u32() as usize,
        );
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
            player_peers: HashMap::new(),
            player_order: BTreeSet::new(),
            snapshot_recipients: BTreeSet::new(),
            snapshot_recipient_cursor: None,
            aoi_recipients: BTreeSet::new(),
            aoi_recipient_cursor: None,
            chat_fanout: VecDeque::new(),
            chat_fanout_bytes: 0,
            clock_fanout: None,
            fanout_audience_cache: None,
            fanout_prefer_clock: true,
            dirty_players: BTreeSet::new(),
            flush_players: BTreeSet::new(),
            flush_paused: false,
            dirty_flush_test: DirtyFlushTestHook::default(),
            identity_players: HashMap::new(),
            next_session_generation: 0,
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

    #[test]
    fn mooring_name_snapshot_boundary_is_enforced_before_persistence() {
        let mut server = make_server(Config {
            max_wire_string_len: 4_096,
            moor_min_interval_ms: 0,
            ..Config::default()
        });
        let peer: PeerId = 1;
        join(&mut server, peer, "tok-moor-name-boundary");
        let cell = server.world.grid().cell_of(0.0, 0.0);

        let too_long = "x".repeat(MAX_MOORING_NAME_BYTES + 1);
        let seq_before_rejection = server.seq;
        send_moor(
            &mut server,
            peer,
            &moor_envelope(0.0, 0.0, &too_long),
            1_000,
        );
        assert_eq!(
            server.seq,
            seq_before_rejection.wrapping_add(1),
            "the invalid request must receive a bounded MoorAck rejection"
        );
        assert!(
            server
                .db
                .moorings_in_cell(cell.cx, cell.cz)
                .unwrap()
                .is_empty(),
            "a name that cannot hydrate in one unreliable CellSnapshot must never persist"
        );
        assert_eq!(
            server.moor_limiter.tracked_count(),
            0,
            "field validation must run before the persistent-write rate limiter"
        );

        let boundary = "b".repeat(MAX_MOORING_NAME_BYTES);
        send_moor(
            &mut server,
            peer,
            &moor_envelope(0.0, 0.0, &boundary),
            1_001,
        );
        let persisted = server
            .db
            .moorings_in_cell(cell.cx, cell.cz)
            .unwrap()
            .into_iter()
            .next()
            .expect("the exact field boundary must persist");
        assert_eq!(persisted.name, boundary);

        let snapshot = mooring_snap(persisted);
        let cell_payload = codec::cell_snapshot(1, cell, &[], &[], &[snapshot]);
        assert!(
            cell_payload.len() <= protocol::MTU - protocol::HEADER_SIZE,
            "the exact persisted boundary must hydrate in one unreliable CellSnapshot"
        );
        let envelope = decode_envelope(&cell_payload).unwrap();
        let hydrated = envelope
            .payload_as_cell_snapshot()
            .unwrap()
            .moorings()
            .unwrap()
            .get(0);
        assert_eq!(hydrated.name(), Some(boundary.as_str()));

        {
            let session = server.sessions.get_mut(&peer).unwrap();
            session.published_cells = session.sub.cells().clone();
            session.hydration_cells.clear();
            session.active_hydration = Some(CellHydration {
                cell,
                player_cursor: None,
                players_remaining: 0,
                mooring_cursor: None,
                players_complete: true,
                sent_any: false,
            });
        }
        server.schedule_aoi(peer);
        let hydration_work = server.process_aoi_work();
        assert_eq!(hydration_work.persisted_queries, 1);
        assert_eq!(hydration_work.moorings_encoded, 1);
        assert_eq!(hydration_work.corrupt_moorings_skipped, 0);

        let rejection = codec::moor_ack(2, false, None, MOOR_NAME_TOO_LONG_REASON);
        assert!(
            rejection.len() <= protocol::MTU - protocol::HEADER_SIZE,
            "the field-specific rejection must itself stay transport bounded"
        );
    }

    #[test]
    fn mooring_name_keeps_the_lower_configured_wire_limit() {
        let mut server = make_server(Config {
            max_wire_string_len: 16,
            moor_min_interval_ms: 0,
            ..Config::default()
        });
        let peer: PeerId = 1;
        join(&mut server, peer, "tok-config");
        let cell = server.world.grid().cell_of(0.0, 0.0);

        send_moor(
            &mut server,
            peer,
            &moor_envelope(0.0, 0.0, &"x".repeat(17)),
            1_000,
        );
        assert!(
            server
                .db
                .moorings_in_cell(cell.cx, cell.cz)
                .unwrap()
                .is_empty(),
            "the field-specific MTU cap must not weaken a lower configured wire cap"
        );
    }

    #[test]
    fn mooring_name_limit_is_the_largest_mtu_safe_record_name() {
        let payload_limit = protocol::MTU - protocol::HEADER_SIZE;
        let cell = Cell::new(i32::MIN, i32::MAX);
        let encoded_lengths = |name_len: usize| {
            let make_snapshot = || MooringSnap {
                boat_id: u64::MAX,
                owner: u64::MAX,
                cell: (i32::MIN, i32::MAX),
                pos: [f32::MAX; 3],
                rot: [f32::MAX; 4],
                name: "n".repeat(name_len),
                created_at: u64::MAX,
            };
            (
                codec::cell_snapshot(1, cell, &[], &[], &[make_snapshot()]).len(),
                codec::moor_ack(1, true, Some(&make_snapshot()), "").len(),
            )
        };
        let derived_limit = (0..=4_096)
            .take_while(|&name_len| {
                let (cell_len, ack_len) = encoded_lengths(name_len);
                cell_len <= payload_limit && ack_len <= payload_limit
            })
            .last()
            .unwrap();

        assert_eq!(
            MAX_MOORING_NAME_BYTES, derived_limit,
            "the field limit must be derived from both unreliable record encodings"
        );
        let (boundary_cell_len, boundary_ack_len) = encoded_lengths(derived_limit);
        assert!(boundary_cell_len <= payload_limit);
        assert!(boundary_ack_len <= payload_limit);
        let (over_cell_len, over_ack_len) = encoded_lengths(derived_limit + 1);
        assert!(
            over_cell_len > payload_limit || over_ack_len > payload_limit,
            "the next byte must exceed at least one unreliable record encoding"
        );
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

    // ---- disconnect clears session classes and retains reconnect cooldown ----

    #[test]
    fn disconnect_clears_session_limiters_and_retains_reconnect_cooldown() {
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
        // on_disconnect clears each session-scoped class.
        server.trade_limiter.allow(pid, 1_000);
        assert_eq!(server.hello_limiter.tracked_count(), 1);
        assert_eq!(server.reconnect_limiter.tracked_count(), 1);
        assert_eq!(server.client_state_limiter.tracked_count(), 1);
        assert_eq!(server.econ_limiter.tracked_count(), 1);
        assert_eq!(server.chat_limiter.tracked_count(), 1);
        assert_eq!(server.trade_limiter.tracked_count(), 1);
        assert_eq!(server.moor_limiter.tracked_count(), 1);

        server
            .on_disconnect(peer, DisconnectReason::Remote)
            .unwrap();

        assert_eq!(server.hello_limiter.tracked_count(), 0);
        assert_eq!(
            server.reconnect_limiter.tracked_count(),
            1,
            "disconnect must retain the bounded per-player reconnect cooldown"
        );
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
        // Headless load: N simulated clients drive the real client-state, AoI,
        // snapshot, chat/clock fanout, and persistence-flush paths. The per-tick
        // server work must stay under the fixed-tick budget (1 / tick_hz), i.e.
        // the server keeps up with real time at N clients. Timing is wall-clock,
        // so this is `#[ignore]`d out of the required gate (`cargo test` skips it)
        // and run only by the non-blocking load job / `make load-test`.
        const N: u32 = 1_024;
        const TICKS: u32 = 450;
        const TRANSPORT_CHAT_BURST: PeerId = 128;
        const LOAD_EPOCH_MS: i64 = 1_700_000_000_000;

        let cfg = Config::default();
        let tick_dt = Duration::from_secs_f64(1.0 / cfg.tick_hz as f64);
        let mut server = make_server(cfg);
        let session_step_ms = server.cfg.new_session_min_interval_ms_i64();

        // Hostile-density load: every authenticated session occupies the same
        // cell. Snapshot work must remain bounded at the transport ceiling.
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
            // The seed time advances per client so the client-state throttle never
            // drops a placement.
            send_state(
                &mut server,
                peer,
                &motion_envelope(1.0, 0.0, 1.0, 0.0, 0.0, 0.0),
                1_000 + i as i64,
            );
        }
        assert_eq!(server.sessions.len(), N as usize);
        assert_eq!(server.world.len(), N as usize);

        // Drive TICKS simulated ticks and measure the wall-clock server work. The
        // simulated clock advances by a full tick each round so every client's
        // per-tick update clears the throttle window (worst-case load).
        let step_ms = tick_dt.as_millis() as i64 + 1;
        let chat = chat_envelope("release transport burst");
        let start = Instant::now();
        let mut maximum_tick = Duration::ZERO;
        let mut burst_tick = Duration::ZERO;
        for t in 0..TICKS {
            let tick_start = Instant::now();
            let now = 10_000 + (t as i64) * step_ms;
            if t == 0 {
                for peer in 1..=TRANSPORT_CHAT_BURST {
                    server
                        .handle_data_at(peer, &chat, now + i64::from(peer), LOAD_EPOCH_MS)
                        .unwrap();
                }
                assert_eq!(server.chat_fanout.len(), TRANSPORT_CHAT_BURST as usize);
                assert_eq!(
                    server.retained_fanout_audience_entries(),
                    N as usize,
                    "the burst must retain one shared audience allocation, not 128 copies"
                );
                let first = server.chat_fanout.front().unwrap().recipients.as_ptr();
                assert!(
                    server
                        .chat_fanout
                        .iter()
                        .all(|job| std::ptr::eq(job.recipients.as_ptr(), first)),
                    "all jobs captured under stable membership must share one audience"
                );
            }
            for i in 0..N {
                let peer = (i + 1) as PeerId;
                let jitter = (t % 8) as f32; // small in-cell movement
                send_state(
                    &mut server,
                    peer,
                    &motion_envelope(1.0 + jitter, 0.0, 1.0, 0.0, 0.0, 0.0),
                    now,
                );
            }
            let aoi = server.process_aoi_work();
            assert!(aoi.recipient_visits <= AOI_WORK_ITEMS_PER_TICK);
            let snapshots = server.broadcast_snapshots();
            assert!(snapshots.recipient_visits <= SNAPSHOT_PACKETS_PER_TICK);
            assert!(
                snapshots.candidates_examined
                    <= SNAPSHOT_PACKETS_PER_TICK * SNAPSHOT_ENTITY_SCAN_PER_PACKET
            );
            let fanout = server.process_fanout_work();
            assert!(fanout.recipient_scans <= FANOUT_RECIPIENT_SCANS_PER_TICK);
            assert!(fanout.sends <= FANOUT_SENDS_PER_TICK);
            assert!(
                fanout.encoded_bytes
                    <= FANOUT_SENDS_PER_TICK * (protocol::MTU - protocol::HEADER_SIZE)
            );
            let flush = server
                .process_dirty_flush_at(LOAD_EPOCH_MS + i64::from(t))
                .unwrap();
            assert!(flush.db_updates <= DIRTY_DB_UPDATES_PER_TICK);

            if t > 0 && t % (server.cfg.tick_hz * FLUSH_INTERVAL.as_secs() as u32) == 0 {
                server.begin_dirty_flush();
            }
            if t % server.cfg.ticks_per_clock_broadcast() as u32 == 0 {
                server.broadcast_clock();
                server.broadcast_clock();
                assert!(server.clock_fanout.is_some());
            }
            assert!(server.chat_fanout.len() <= CHAT_QUEUE_ITEMS);
            assert!(server.chat_fanout_bytes <= CHAT_QUEUE_BYTES);
            assert!(server.clock_fanout.iter().count() <= 1);
            assert!(server.retained_fanout_audience_entries() <= FANOUT_QUEUE_RECIPIENTS);
            assert!(server.dirty_players.len() <= server.cfg.max_player_rows_u32() as usize);
            assert!(server.flush_players.len() <= server.cfg.max_player_rows_u32() as usize);

            let tick_elapsed = tick_start.elapsed();
            maximum_tick = maximum_tick.max(tick_elapsed);
            if t == 0 {
                burst_tick = tick_elapsed;
            }
        }
        let elapsed = start.elapsed();
        let per_tick = elapsed / TICKS;

        assert!(server.chat_fanout.is_empty());
        assert_eq!(server.chat_fanout_bytes, 0);
        assert_eq!(server.retained_fanout_audience_entries(), N as usize);
        assert!(server.clock_fanout.is_none());

        println!(
            "load: {N} clients x {TICKS} ticks in {elapsed:?} => {per_tick:?}/tick, \
             max {maximum_tick:?}, 128-chat burst tick {burst_tick:?} \
             (real-time budget {tick_dt:?})"
        );
        assert!(
            maximum_tick < tick_dt,
            "whole-tick server work {maximum_tick:?} (burst {burst_tick:?}) exceeded the \
             {tick_dt:?} real-time budget at {N} clients"
        );
    }
}
