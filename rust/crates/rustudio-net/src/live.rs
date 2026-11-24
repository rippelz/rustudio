//! A cancellable Team Create connection for the editor.
//!
//! Call `LiveSession::connect` only in response to the user's live-open action.
//! The four embedded Studio 0.746 templates contain flag names/build constants,
//! never player JSON, tickets, cookies, or a captured shared-string cache.
//!
//! `Ready` waits for the initial place data's end marker and a 1.5-second
//! settle period for follow-up instances. Updates continue after `Ready`;
//! callers must continue draining `events`.

use crate::error::{NetError, Result};
use crate::handshake::{self, ProtocolSyncTemplate, TicketTemplate};
use crate::matchmaker::{LiveJoinConsent, Matchmaker, PollBudget};
use crate::replicator::{packet_id, Decoder, PropValue, RepOp, Schema};
use crate::rtcio::{self, lane_tag, CH_CONTROL0, CH_PING, CH_REPLICATION, TAG_CTRL};
use crate::sessioncrypto::Hello;
use crate::transport::{connect_quic, QuicTarget};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc, Arc, Mutex,
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const REP_CONTROL: [u8; 7] = [0x83, 0x23, 0, 0, 0, 0, 0];
// Studio grants an initial instance quota when the streaming join completes.
// A smaller radius on later quota reports limits the incremental stream.
const INITIAL_STREAM_QUOTA: i32 = 10_000;
const EDITOR_STREAM_REGION_RADIUS: i64 = 65_536;

fn stream_credit(instances: i32, radius: i16) -> [u8; 10] {
    let mut frame = [0x83, 0x1c, 0, 0, 0, 0, 0, 0, 0, 0];
    frame[3..7].copy_from_slice(&instances.to_le_bytes());
    frame[7..9].copy_from_slice(&radius.to_le_bytes());
    frame
}

fn initial_stream_request(schema: &Schema, player: u64) -> std::result::Result<Vec<u8>, String> {
    let mut frame = crate::writer::property(
        schema, player, "Player", "ChararacterRegionId", &PropValue::Vector3([0.0; 3]),
    )?;
    let radius = PropValue::Int(EDITOR_STREAM_REGION_RADIUS);
    let override_radii = crate::writer::event(
        schema, player, "Player", "OverrideStreamRadii", &[radius.clone(), radius],
    )?;
    frame.pop();
    frame.extend_from_slice(&stream_credit(INITIAL_STREAM_QUOTA, i16::MAX)[1..9]);
    frame.extend_from_slice(&override_radii[1..]);
    Ok(frame)
}

const BATCH_SIZE: usize = 512;
const MAX_FRAME_SIZE: usize = 64 << 20;
const QUIET_PERIOD: Duration = Duration::from_millis(1500);
/// Edits written to the replication stream per 20 ms tick, at most: a
/// plugin's burst (hundreds of creates, megabytes of Source) goes out over
/// several ticks, so pings and replication control keep their turn.
const EDITS_PER_TICK: usize = 64;
const BYTES_PER_TICK: usize = 1 << 20;
/// Written edits kept for the log line a server-ended session prints.
const RECENT_EDITS: usize = 32;

#[derive(Debug)]
pub enum LiveEvent {
    /// Join progress, for the joining dialog: a step starting.
    Stage(JoinStage),
    /// The matchmaker is still preparing the server: (attempt, its message).
    Waiting(u32, String),
    Batch(Vec<RepOp>),
    Ready,
    Identity(u64),
    Notice(String),
    /// A persistent document edit was not transmitted; saving must stay blocked.
    EditRejected(String),
    /// The edit of instance `id` (its referent as sent) was not transmitted:
    /// it could not be encoded, or it depends on an instance that was not
    /// created. Saving stays blocked, as for `EditRejected`.
    EditFailed { id: u64, error: String },
    /// Instance `id` was created without property `name`, which this server
    /// cannot take from rustudio (`reason`). The rest of it was sent.
    PropertySkipped { id: u64, name: String, reason: String },
    Error(String),
    Disconnected(String),
}

/// The steps of a join, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum JoinStage {
    SignIn,
    Matchmaking,
    Connecting,
    Handshake,
    Receiving,
}

impl JoinStage {
    pub const ALL: [JoinStage; 5] = [
        JoinStage::SignIn,
        JoinStage::Matchmaking,
        JoinStage::Connecting,
        JoinStage::Handshake,
        JoinStage::Receiving,
    ];
    pub fn label(self) -> &'static str {
        match self {
            JoinStage::SignIn => "Checking your sign-in",
            JoinStage::Matchmaking => "Asking Roblox for the Team Create server",
            JoinStage::Connecting => "Connecting to the server",
            JoinStage::Handshake => "Joining the session",
            JoinStage::Receiving => "Downloading the place",
        }
    }
}

/// Owns the lifetime of the live connection. Dropping it cancels the
/// worker; drop never joins a network thread or blocks the UI. An already
/// running synchronous account/matchmaker request can finish before noticing
/// cancellation, but cancellation prevents subsequent connection work.
enum LiveEdit {
    Event {id: u64, class: String, name: String, args: Vec<PropValue>},
    Property {
        id: u64,
        class: String,
        name: String,
        value: PropValue,
    },
    Create {
        id: u64,
        class: String,
        parent: u64,
        props: Vec<(String, PropValue)>,
    },
    Delete(u64),
    Reparent {
        id: u64,
        parent: u64,
    },
}

impl LiveEdit {
    /// One line for the log: kind, class, referent and property names.
    fn describe(&self) -> String {
        match self {
            LiveEdit::Event { id, class, name, .. } => format!("event {class}.{name} on {id:#x}"),
            LiveEdit::Property { id, class, name, .. } => format!("set {class}.{name} on {id:#x}"),
            LiveEdit::Create { id, class, parent, props } => format!(
                "create {class} {id:#x} under {parent:#x} [{}]",
                props.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(", ")
            ),
            LiveEdit::Delete(id) => format!("delete {id:#x}"),
            LiveEdit::Reparent { id, parent } => format!("move {id:#x} under {parent:#x}"),
        }
    }
}

pub struct LiveSession {
    pub events: mpsc::Receiver<LiveEvent>,
    cancel: Arc<AtomicBool>,
    commands: mpsc::Sender<LiveEdit>,
    next_id: Arc<AtomicU64>,
}

impl LiveSession {
    pub fn connect(universe: u64, place: u64) -> Result<Self> {
        Self::connect_as(universe, place, None)
    }

    /// [`LiveSession::connect`] as a saved account (account switching's
    /// `roblox-account-<id>` keyring entry) instead of the active session.
    pub fn connect_as(universe: u64, place: u64, account: Option<u64>) -> Result<Self> {
        if universe == 0 || place == 0 {
            return Err(NetError::Other("A universe and place are required".into()));
        }
        // Bound pending model work: a large place cannot enqueue its entire
        // DataModel while the editor is applying a previous batch.
        let (tx, events) = mpsc::sync_channel(8);
        // Unbounded: every queued edit must reach the worker in order (a
        // dropped one leaves the server half-edited); the worker paces them.
        let (commands, command_rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let next_id = Arc::new(AtomicU64::new(0));
        let worker_id = next_id.clone();
        std::thread::Builder::new()
            .name("rustudio-team-create".into())
            .spawn(move || {
                log::info!("Team Create join started (place={place}, universe={universe})");
                let result = prepare_and_run(
                    universe,
                    place,
                    account,
                    &tx,
                    worker_cancel.clone(),
                    command_rx,
                    worker_id,
                );
                if let Err(error) = result {
                    if !worker_cancel.load(Ordering::Acquire) {
                        // Errors created by this module contain fixed context,
                        // never response bodies or secret-bearing packet bytes.
                        log::error!("Team Create worker failed (place={place}): {error}");
                        let _ = tx.send(LiveEvent::Error(error.to_string()));
                    }
                }
            })
            .map_err(|_| NetError::Other("Could not start Team Create worker".into()))?;
        Ok(Self {
            events,
            cancel,
            commands,
            next_id,
        })
    }
    pub fn send_property(
        &self,
        id: u64,
        class: String,
        name: String,
        value: PropValue,
    ) -> Result<()> {
        self.send_edit(LiveEdit::Property {
            id,
            class,
            name,
            value,
        })
    }
    pub fn send_event(&self, id: u64, class: String, name: String, args: Vec<PropValue>) -> Result<()> {
        self.send_edit(LiveEdit::Event {id,class,name,args})
    }
    pub fn allocate_id(&self) -> Result<u64> {
        self.next_id
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |id| {
                (id != 0 && id as u32 != u32::MAX).then_some(id + 1)
            })
            .map_err(|_| NetError::Other("Team Create instance namespace is unavailable".into()))
    }
    fn send_edit(&self, edit: LiveEdit) -> Result<()> {
        self.commands
            .send(edit)
            .map_err(|_| NetError::Other("Team Create edit queue is unavailable".into()))
    }
    pub fn send_create(
        &self,
        id: u64,
        class: String,
        parent: u64,
        props: Vec<(String, PropValue)>,
    ) -> Result<()> {
        self.send_edit(LiveEdit::Create {
            id,
            class,
            parent,
            props,
        })
    }
    pub fn send_delete(&self, id: u64) -> Result<()> {
        self.send_edit(LiveEdit::Delete(id))
    }
    pub fn send_reparent(&self, id: u64, parent: u64) -> Result<()> {
        self.send_edit(LiveEdit::Reparent { id, parent })
    }
}

impl Drop for LiveSession {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

fn templates() -> (ProtocolSyncTemplate, TicketTemplate) {
    (
        ProtocolSyncTemplate {
            head: include_bytes!("templates/studio_0746_sync_head.bin").to_vec(),
            tail: include_bytes!("templates/studio_0746_sync_tail.bin").to_vec(),
        },
        TicketTemplate {
            middle: include_bytes!("templates/studio_0746_ticket_middle.bin").to_vec(),
            tail: include_bytes!("templates/studio_0746_ticket_tail.bin").to_vec(),
        },
    )
}

fn prepare_and_run(
    universe: u64,
    place: u64,
    account: Option<u64>,
    tx: &mpsc::SyncSender<LiveEvent>,
    cancel: Arc<AtomicBool>,
    commands: mpsc::Receiver<LiveEdit>,
    next_id: Arc<AtomicU64>,
) -> Result<()> {
    if cancel.load(Ordering::Acquire) {
        return Ok(());
    }
    let _ = tx.send(LiveEvent::Stage(JoinStage::SignIn));
    let cookie = match account {
        Some(id) => rustudio_account::session::account_cookie(id)
            .ok_or_else(|| NetError::Other(format!("No saved sign-in for account {id}")))?,
        // The same sign-in the editor uses (Client::open): the cookie file is
        // read too unless the person signed out (it is the only copy where no
        // keyring exists).
        None => rustudio_account::session::load_cookie(
            !rustudio_account::AccountConfig::load().map(|c| c.signed_out).unwrap_or(false),
        )
            .map_err(|_| NetError::Other("Could not load your Roblox sign-in".into()))?
            .ok_or_else(|| NetError::Other("Sign in to Roblox before joining Team Create".into()))?
            .0,
    };
    let session = rustudio_account::Session::validate(cookie)
        .map_err(|_| NetError::Other("Your Roblox sign-in could not be validated".into()))?;
    if cancel.load(Ordering::Acquire) {
        return Ok(());
    }
    let _ = tx.send(LiveEvent::Identity(session.profile.id));
    let _ = tx.send(LiveEvent::Stage(JoinStage::Matchmaking));
    let joined = Matchmaker::new(&session)
        .join_with_progress(
            universe,
            place,
            LiveJoinConsent::user_requested_live_join(),
            PollBudget::default(),
            &mut |attempt, message| {
                if attempt > 1 {
                    let _ = tx.send(LiveEvent::Waiting(attempt, message.to_string()));
                }
            },
        )
        .map_err(|error| match error {
            NetError::Http { status, .. } => {
                NetError::Other(format!("Team Create request returned HTTP {status}"))
            }
            NetError::CannotCollaborate(_) => {
                NetError::Other("This account cannot collaborate on this universe".into())
            }
            NetError::MatchmakerTimeout => NetError::MatchmakerTimeout,
            _ => NetError::Other("Roblox could not prepare this Team Create session".into()),
        })?;
    if cancel.load(Ordering::Acquire) {
        return Ok(());
    }
    // Which server instance this is: a place left by its last editor saves
    // and shuts down, and the next join may land on a new server loaded from
    // that save (edits of the last moments can be missing there for a while).
    log::info!(
        "Team Create server for place {place}: job {}",
        joined.join.game_id.as_deref().unwrap_or("unknown")
    );
    let raw: serde_json::Value = serde_json::from_str(joined.raw.expose())
        .map_err(|_| NetError::Other("Roblox returned invalid join information".into()))?;
    let target = QuicTarget::from_join(&joined.join)
        .map_err(|_| NetError::Other("Roblox returned unsupported transport information".into()))?;
    let (sync, ticket) = templates();
    let opening = vec![
        Hello::from_client_ticket(joined.join.client_ticket.expose())
            .and_then(|hello| hello.encode())
            .map_err(|_| NetError::Other("Roblox returned an unsupported session ticket".into()))?,
        handshake::shared_string_cache_message(&[])?,
        handshake::protocol_sync(&sync, &raw),
        handshake::fresh_place_verification()?,
        handshake::submit_ticket(&ticket, &joined.join, &raw),
        vec![0x8f, 0],
    ];
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| NetError::Other("Could not start Team Create network runtime".into()))?;
    runtime
        .block_on(run(place, target, opening, tx.clone(), cancel, commands, next_id, session.profile.id))
        .map_err(|_| NetError::Other("The Team Create network session failed".into()))
}

#[derive(Default)]
struct InitialState {
    streaming: bool,
    local_user: u64,
    local_player: Option<u64>,
    schema: bool,
    marker: bool,
    instances: bool,
    initial_data_complete: bool,
    last_batch: Option<Instant>,
    delivering: bool,
}

impl InitialState {
    fn can_finish(&self, now: Instant) -> bool {
        self.schema
            && self.marker
            && self.instances
            && self.initial_data_complete
            && !self.delivering
            && self.last_batch.is_some_and(|at| now.duration_since(at) >= QUIET_PERIOD)
    }
}

async fn emit(tx: &mpsc::SyncSender<LiveEvent>, cancel: &AtomicBool, mut event: LiveEvent) -> bool {
    loop {
        if cancel.load(Ordering::Acquire) {
            return false;
        }
        match tx.try_send(event) {
            Ok(()) => return true,
            Err(mpsc::TrySendError::Disconnected(_)) => return false,
            Err(mpsc::TrySendError::Full(value)) => event = value,
        }
        // Yield so QUIC keeps servicing ACKs while the UI catches up.
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn read_frame(stream: &mut quinn::RecvStream) -> Result<Vec<u8>> {
    let mut length = [0; 4];
    stream
        .read_exact(&mut length)
        .await
        .map_err(|e| NetError::Other(format!("The replication stream ended: {e}")))?;
    let word = u32::from_be_bytes(length);
    let size = (word & 0x7fff_ffff) as usize;
    if word & 0x8000_0000 == 0 || size > MAX_FRAME_SIZE {
        return Err(NetError::Other(
            "The server sent an unsupported replication frame".into(),
        ));
    }
    let mut frame = vec![0; size];
    stream
        .read_exact(&mut frame)
        .await
        .map_err(|_| NetError::Other("The replication stream ended during a frame".into()))?;
    Ok(frame)
}

async fn receive_lane(
    place: u64,
    mut stream: quinn::RecvStream,
    tx: mpsc::SyncSender<LiveEvent>,
    cancel: Arc<AtomicBool>,
    seen_sync: Arc<AtomicBool>,
    initial: Arc<Mutex<InitialState>>,
    lane_ended: Arc<AtomicBool>,
    shared_schema: Arc<Mutex<Option<Schema>>>,
    next_id: Arc<AtomicU64>,
) {
    let Some((channel, tag)) = rtcio::read_header(&mut stream).await else {
        return;
    };
    let primary = channel == CH_REPLICATION && tag == lane_tag(1);
    let mut decoder = Decoder::new();
    let mut schema_frame = Vec::new();
    let mut warned_terrain = false;
    let mut capture = if primary {
        std::env::var("RUSTUDIO_TC_STREAM_OUT")
            .ok()
            .and_then(|path| {
                use std::io::Write;
                let mut options = std::fs::OpenOptions::new();
                options.write(true).create(true).truncate(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                let mut f = options.open(path).ok()?;
                f.write_all(&[6, 1, 4, 0, 0, 0, 1]).ok()?;
                Some(f)
            })
    } else {
        None
    };
    // RUSTUDIO_TC_LANES_OUT=DIR: every other lane's frames too, one file per
    // channel and lane (same format), for offline decoding.
    let mut lane_capture = if primary {
        None
    } else {
        std::env::var("RUSTUDIO_TC_LANES_OUT").ok().and_then(|dir| {
            use std::io::Write;
            let name = format!("ch{channel}-{}.stream", tag.iter().map(|b| format!("{b:02x}")).collect::<String>());
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut f = options.open(std::path::Path::new(&dir).join(name)).ok()?;
            f.write_all(&[6, 1, channel, tag[0], tag[1], tag[2], tag[3]]).ok()?;
            Some(f)
        })
    };
    loop {
        let frame = match read_frame(&mut stream).await {
            Ok(frame) => frame,
            Err(error) => {
                if primary {
                    emit(&tx, &cancel, LiveEvent::Disconnected(error.to_string())).await;
                    lane_ended.store(true, Ordering::Release);
                }
                return;
            }
        };
        if cancel.load(Ordering::Acquire) {
            return;
        }
        if let Some(file) = lane_capture.as_mut() {
            use std::io::Write;
            let _ = file.write_all(&rtcio::frame(&frame));
        }
        if !primary || frame.is_empty() {
            continue;
        }
        if let Some(file) = capture.as_mut() {
            use std::io::Write;
            let _ = file.write_all(&rtcio::frame(&frame));
        }
        if frame[0] == packet_id::SET_GLOBALS {
            initial.lock().unwrap().streaming = frame.get(1) == Some(&1);
            match crate::replicator::assigned_peer_scope(&frame[1..]) {
                Ok(scope) => {
                    next_id.store((u64::from(scope) << 32) | 1, Ordering::Release);
                }
                Err(_) => {
                    emit(
                        &tx,
                        &cancel,
                        LiveEvent::Notice(
                            "Instance creation is unavailable for this server configuration".into(),
                        ),
                    )
                    .await;
                }
            }
        }
        if frame[0] == packet_id::PROTOCOL_SYNC {
            seen_sync.store(true, Ordering::Release);
        }
        initial.lock().unwrap().delivering = true;
        if frame[0] == packet_id::NEW_SCHEMA {
            schema_frame = frame.clone();
        }
        let errors = decoder.errors.len();
        let ops = decoder.decode_frame(frame[0], &frame[1..]);
        if !decoder.skipped.is_empty() {
            let kinds: Vec<String> = decoder.skipped.drain(..).map(|k| format!("{k:#04x}")).collect();
            let saved = save_unknown_frame(&schema_frame, &frame, &kinds.join("-"));
            log::warn!(
                "Team Create skipped unknown item {} (place={place}, packet={:#04x}, bytes={}){}",
                kinds.join(", "),
                frame[0],
                frame.len(),
                saved.as_ref().map(|p| format!(", saved to {}", p.display())).unwrap_or_default()
            );
            emit(
                &tx,
                &cancel,
                LiveEvent::Notice(format!(
                    "skipped a live update rustudio can't read yet (item {}); the session continues, but a change from a collaborator may be missing",
                    kinds.join(", ")
                )),
            )
            .await;
        }
        if decoder.errors.len() != errors {
            if frame[0] == packet_id::TERRAIN {
                initial.lock().unwrap().delivering = false;
                if !warned_terrain {
                    let error = decoder.errors.last().unwrap();
                    log::warn!("Team Create skipped an unreadable terrain packet (place={place}): {error}");
                    warned_terrain = true;
                }
                decoder.errors.truncate(errors);
                continue;
            }
            if let Some(error) = decoder.errors.last() {
                eprintln!("Team Create decode error: {error}");
                log::error!(
                    "Team Create decode error (place={place}, channel={channel}, lane={tag:02x?}, packet={:#04x}, bytes={}): {error}",
                    frame[0],
                    frame.len()
                );
            }
            if let Ok(path) = std::env::var("RUSTUDIO_TC_BAD_FRAME") {
                use std::io::Write;
                let mut options = std::fs::OpenOptions::new();
                options.write(true).create(true).truncate(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                if let Ok(mut file) = options.open(path) {
                    let _ = file.write_all(&[6, 1, 4, 0, 0, 0, 1]);
                    let _ = file.write_all(&rtcio::frame(&schema_frame));
                    let _ = file.write_all(&rtcio::frame(&frame));
                }
            }
            emit(
                &tx,
                &cancel,
                LiveEvent::Error(
                    "Some live place data could not be decoded; the session was stopped".into(),
                ),
            )
            .await;
            lane_ended.store(true, Ordering::Release);
            return;
        }
        if frame[0] == packet_id::NEW_SCHEMA {
            *shared_schema.lock().unwrap() = decoder.schema.clone();
        }
        reserve_seen_ids(&next_id, &ops);
        let instances = ops.iter().any(|op| matches!(op, RepOp::NewInstance { .. }));
        {
            let mut state = initial.lock().unwrap();
            if state.local_player.is_none() {
                state.local_player = ops.iter().find_map(|op| match op {
                    RepOp::NewInstance { id, class, props, .. }
                        if class == "Player" && props.iter().any(|(name, value)| {
                            name == "UserId" && matches!(value, PropValue::Int(user) | PropValue::Int64(user) if *user as u64 == state.local_user)
                        }) => Some(*id),
                    _ => None,
                });
            }
        }
        let mut pending = Vec::with_capacity(BATCH_SIZE);
        for op in ops {
            pending.push(op);
            if pending.len() == BATCH_SIZE {
                if !emit(
                    &tx,
                    &cancel,
                    LiveEvent::Batch(std::mem::replace(
                        &mut pending,
                        Vec::with_capacity(BATCH_SIZE),
                    )),
                )
                .await
                {
                    return;
                }
            }
        }
        if !pending.is_empty() && !emit(&tx, &cancel, LiveEvent::Batch(pending)).await {
            return;
        }
        let mut state = initial.lock().unwrap();
        state.schema |= decoder.schema.is_some();
        state.marker |= frame[0] == packet_id::MARKER;
        if instances || (decoder.initial_data_complete && !state.initial_data_complete) {
            state.last_batch = Some(Instant::now());
        }
        state.instances |= instances;
        state.initial_data_complete = decoder.initial_data_complete;
        state.delivering = false;
    }
}

/// Keeps this session's next instance referent past every referent in its
/// scope the server has: scopes are a per-server-run counter, and instances
/// a peer made keep their scoped referents in the saved place (Place A holds 765
/// scope-5 instances while its session got scope 8; a game's first
/// session got 5, later ones 13 and 16 alongside 1462 scope-5 instances).
/// A later server run can hand the same scope out again; allocating from
/// index 1 then reused a saved instance's referent, so a new script took
/// LocalMapAssets' referent and its Destroy() was never sent.
fn reserve_seen_ids(next_id: &AtomicU64, ops: &[RepOp]) {
    let scope = next_id.load(Ordering::Acquire) >> 32;
    if scope <= 1 {
        return;
    }
    let highest = ops
        .iter()
        .filter_map(|op| match op {
            RepOp::NewInstance { id, .. } => Some(*id),
            _ => None,
        })
        .filter(|id| id >> 32 == scope && *id as u32 != u32::MAX)
        .max();
    if let Some(id) = highest {
        next_id.fetch_max(id + 1, Ordering::AcqRel);
    }
}

async fn wait_cancelled(cancel: &AtomicBool) {
    while !cancel.load(Ordering::Acquire) {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn run(
    place: u64,
    target: QuicTarget,
    opening: Vec<Vec<u8>>,
    tx: mpsc::SyncSender<LiveEvent>,
    cancel: Arc<AtomicBool>,
    commands: mpsc::Receiver<LiveEdit>,
    next_id: Arc<AtomicU64>,
    local_user: u64,
) -> Result<()> {
    emit(&tx, &cancel, LiveEvent::Stage(JoinStage::Connecting)).await;
    let (_endpoint, connection, _socket) = tokio::select! {
        connected = connect_quic(&target) => connected
            .map_err(|_| NetError::Other("Could not connect to the Team Create server".into()))?,
        _ = wait_cancelled(&cancel) => return Ok(()),
    };
    if cancel.load(Ordering::Acquire) {
        connection.close(0u32.into(), b"cancelled");
        return Ok(());
    }
    let shared_schema = Arc::new(Mutex::new(None::<Schema>));
    let seen_sync = Arc::new(AtomicBool::new(false));
    let lane_ended = Arc::new(AtomicBool::new(false));
    let initial = Arc::new(Mutex::new(InitialState { local_user, ..InitialState::default() }));
    let mut held_lanes = vec![rtcio::open_lane(&connection, CH_REPLICATION, lane_tag(0)).await?];
    let mut replication = rtcio::open_lane(&connection, CH_REPLICATION, lane_tag(1)).await?;
    for seq in [2, 4, 7, 8, 9, 10, 12, 13] {
        held_lanes.push(rtcio::open_lane(&connection, CH_REPLICATION, lane_tag(seq)).await?);
    }
    let mut replication_control = rtcio::open_lane(&connection, CH_REPLICATION, TAG_CTRL).await?;
    rtcio::send(&mut replication_control, &[0, 0]).await?;
    for (lane, stream) in [(3u32, 2u32), (5, 4), (6, 6), (11, 8)] {
        let mut msg = vec![2, CH_REPLICATION];
        msg.extend_from_slice(&lane.to_be_bytes());
        msg.extend_from_slice(&stream.to_be_bytes());
        rtcio::send(&mut replication_control, &msg).await?;
    }
    let mut ping_control = rtcio::open_lane(&connection, CH_PING, TAG_CTRL).await?;
    rtcio::send(&mut ping_control, &[0, 0]).await?;
    for (lane, stream) in [(2u32, 10u32), (3, 12)] {
        let mut msg = vec![2, CH_PING];
        msg.extend_from_slice(&lane.to_be_bytes());
        msg.extend_from_slice(&stream.to_be_bytes());
        rtcio::send(&mut ping_control, &msg).await?;
    }
    let mut zero_control = rtcio::open_lane(&connection, CH_CONTROL0, TAG_CTRL).await?;
    rtcio::send(&mut zero_control, &[0, 0]).await?;
    let mut ping = rtcio::open_lane(&connection, CH_PING, lane_tag(1)).await?;
    emit(&tx, &cancel, LiveEvent::Stage(JoinStage::Handshake)).await;
    for message in opening {
        rtcio::send(&mut replication, &message).await?;
    }
    let mut interval = tokio::time::interval(Duration::from_millis(20));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_ping = Instant::now();
    let mut control_started = false;
    let mut ready = false;
    let mut sent_stream_request = false;
    let mut rejected_document_edit = false;
    let mut gate = EditGate::default();
    // The last edits written, logged if the server ends the session, so a
    // reset ("Error while receiving data") can be traced to an item.
    let mut recent: std::collections::VecDeque<(Instant, String)> = Default::default();
    let mut dgram_capture = std::env::var("RUSTUDIO_TC_LANES_OUT").ok().and_then(|dir| {
        std::fs::File::create(std::path::Path::new(&dir).join("datagrams.bin")).ok()
    });
    loop {
        tokio::select! {
            dgram = connection.read_datagram() => {
                if let (Ok(d), Some(f)) = (&dgram, dgram_capture.as_mut()) {
                    use std::io::Write;
                    let _ = f.write_all(&rtcio::frame(d));
                }
                if dgram.is_err() { dgram_capture = None; }
            }
            incoming = connection.accept_uni() => {
                match incoming {
                    Ok(stream) => {
                        tokio::spawn(receive_lane(place, stream, tx.clone(), cancel.clone(), seen_sync.clone(), initial.clone(), lane_ended.clone(), shared_schema.clone(), next_id.clone()));
                    }
                    Err(_) => {
                        emit(&tx, &cancel, LiveEvent::Disconnected("The Team Create connection ended".into())).await;
                        break;
                    }
                }
            }
            _ = interval.tick() => {
                if cancel.load(Ordering::Acquire) || lane_ended.load(Ordering::Acquire) { break; }
                if seen_sync.swap(false, Ordering::AcqRel) {
                    rtcio::send(&mut replication, &REP_CONTROL).await?;
                    control_started = true;
                }
                let streaming_player = {
                    let state = initial.lock().unwrap();
                    if !sent_stream_request && state.streaming && state.initial_data_complete {
                        state.local_player
                    } else {
                        None
                    }
                };
                if let Some(player) = streaming_player {
                    let request = shared_schema.lock().unwrap().as_ref()
                        .map(|schema| initial_stream_request(schema, player));
                    match request {
                        Some(Ok(frame)) => {
                            rtcio::send(&mut replication, &frame).await?;
                            log::info!("Team Create requested initial terrain streaming (place={place})");
                        }
                        Some(Err(error)) => log::warn!("Team Create initial streaming request: {error}"),
                        None => {}
                    }
                    sent_stream_request = true;
                }
                let mut written = 0;
                for _ in 0..EDITS_PER_TICK {
                    if written >= BYTES_PER_TICK { break; }
                    let Ok(edit) = commands.try_recv() else { break; };
                    if rejected_document_edit && matches!(&edit,LiveEdit::Event {class,name,..} if class=="TeamCreatePublishService" && name=="TeamCreatePlaceUploadRequest") {
                        emit(&tx,&cancel,LiveEvent::EditRejected("Save blocked because a local edit could not be synchronized; retain a local copy and reopen the live place".into())).await;
                        continue;
                    }
                    let what = edit.describe();
                    let outcomes = {
                        let schema = shared_schema.lock().unwrap();
                        gate.encode(schema.as_ref(), edit)
                    };
                    for outcome in outcomes {
                        match outcome {
                            Outcome::Frame(frame) => {
                                written += frame.len();
                                if recent.len() == RECENT_EDITS { recent.pop_front(); }
                                recent.push_back((Instant::now(), format!("{what} ({} bytes)", frame.len())));
                                log::debug!("Team Create wrote {what} ({} bytes)", frame.len());
                                rtcio::send(&mut replication, &frame).await?;
                            }
                            Outcome::Failed { id, error } => {
                                rejected_document_edit = true;
                                emit(&tx, &cancel, LiveEvent::EditFailed { id, error }).await;
                            }
                            Outcome::Skipped { id, name, reason } => {
                                emit(&tx, &cancel, LiveEvent::PropertySkipped { id, name, reason }).await;
                            }
                            Outcome::Notice(message) => {
                                emit(&tx, &cancel, LiveEvent::Notice(message)).await;
                            }
                        }
                    }
                }
                if last_ping.elapsed() >= Duration::from_secs(1) {
                    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_micros() as u64;
                    rtcio::send(&mut ping, &rtcio::ping_message(timestamp)).await?;
                    if control_started { rtcio::send(&mut replication, &REP_CONTROL).await?; }
                    last_ping = Instant::now();
                }
                let can_finish = !ready && initial.lock().unwrap().can_finish(Instant::now());
                if can_finish {
                    if !emit(&tx, &cancel, LiveEvent::Ready).await { break; }
                    ready = true;
                }
            }
        }
    }
    if !cancel.load(Ordering::Acquire) && !recent.is_empty() {
        log::warn!("Team Create session ended (place={place}); the last {} edit(s) written, oldest first:", recent.len());
        for (at, what) in &recent {
            log::warn!("  {:.1} s before the end: {what}", at.elapsed().as_secs_f32());
        }
    }
    connection.close(0u32.into(), b"editor closed session");
    Ok(())
}

/// What one queued edit turns into on the wire.
#[derive(Debug)]
enum Outcome {
    Frame(Vec<u8>),
    /// A document edit that was not sent.
    Failed { id: u64, error: String },
    /// A created instance's property that was left out.
    Skipped { id: u64, name: String, reason: String },
    /// A non-document message (presence, events) that could not be sent.
    Notice(String),
}

/// Encodes queued edits against the server schema and keeps the stream
/// causally valid: an instance whose create was not sent never has a child
/// created under it, a reparent into or of it, a property change or a
/// reference to it written, since the server drops the whole session on an
/// item naming an instance it doesn't have ("Error while receiving data").
#[derive(Default)]
struct EditGate {
    failed: std::collections::HashSet<u64>,
    /// Shared strings (script Source) already sent on this connection.
    shared: crate::writer::SharedSent,
}

impl EditGate {
    fn encode(&mut self, schema: Option<&Schema>, edit: LiveEdit) -> Vec<Outcome> {
        let Some(schema) = schema else {
            let error = "The live schema is not ready".to_string();
            return vec![match edit {
                LiveEdit::Event { .. } => Outcome::Notice(error),
                LiveEdit::Property { class, .. } if class == "Player" => Outcome::Notice(error),
                LiveEdit::Property { id, .. } | LiveEdit::Create { id, .. } | LiveEdit::Reparent { id, .. } | LiveEdit::Delete(id) => {
                    self.failed.insert(id);
                    Outcome::Failed { id, error }
                }
            }];
        };
        let failed_ref = |failed: &std::collections::HashSet<u64>, value: &PropValue| {
            matches!(value, PropValue::Ref(Some(target)) if failed.contains(target))
        };
        match edit {
            LiveEdit::Event { id, class, name, args } => vec![match crate::writer::event(schema, id, &class, &name, &args) {
                Ok(frame) => Outcome::Frame(frame),
                Err(error) => Outcome::Notice(error),
            }],
            LiveEdit::Property { id, class, name, value } => {
                let document = class != "Player";
                if self.failed.contains(&id) || failed_ref(&self.failed, &value) {
                    return vec![Outcome::Failed { id, error: format!("{class}.{name} refers to an instance that was not created on the server") }];
                }
                vec![match crate::writer::property_with(schema, id, &class, &name, &value, &mut self.shared) {
                    Ok(frame) => Outcome::Frame(frame),
                    Err(error) if document => Outcome::Failed { id, error },
                    Err(error) => Outcome::Notice(error),
                }]
            }
            LiveEdit::Create { id, class, parent, mut props } => {
                if self.failed.contains(&parent) {
                    self.failed.insert(id);
                    return vec![Outcome::Failed { id, error: format!("{class} was not created because its parent was not") }];
                }
                let mut out = Vec::new();
                props.retain(|(name, value)| {
                    let keep = !failed_ref(&self.failed, value);
                    if !keep {
                        out.push(Outcome::Skipped { id, name: name.clone(), reason: "it refers to an instance that was not created".into() });
                    }
                    keep
                });
                match crate::writer::create_partial_with(schema, id, &class, parent, &props, &mut self.shared) {
                    Ok((frame, skipped)) => {
                        out.extend(skipped.into_iter().map(|(name, reason)| Outcome::Skipped { id, name, reason }));
                        out.insert(0, Outcome::Frame(frame));
                    }
                    Err(error) => {
                        self.failed.insert(id);
                        out = vec![Outcome::Failed { id, error }];
                    }
                }
                out
            }
            // Never created there: nothing to delete.
            LiveEdit::Delete(id) if self.failed.contains(&id) => Vec::new(),
            LiveEdit::Delete(id) => vec![Outcome::Frame(crate::writer::delete(id))],
            LiveEdit::Reparent { id, parent } => {
                if self.failed.contains(&id) || self.failed.contains(&parent) {
                    return vec![Outcome::Failed { id, error: "The move was not sent because the instance or its new parent was not created on the server".into() }];
                }
                vec![match crate::writer::reparent(schema, id, parent) {
                    Ok(frame) => Outcome::Frame(frame),
                    Err(error) => Outcome::Failed { id, error },
                }]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replicator::{SchemaClass, SchemaProp};

    #[test]
    fn initial_stream_request_matches_studio() {
        use crate::replicator::SchemaEvent;
        let mut schema = Schema {
            classes: vec![SchemaClass {
                name: "Player".into(),
                props: vec![SchemaProp { name: "ChararacterRegionId".into(), ty: 0x16 }],
                events: vec![SchemaEvent {
                    name: "OverrideStreamRadii".into(), arg_types: vec![0x0a, 0x0a],
                }],
            }],
            ..Schema::default()
        };
        schema.global_props.resize(0x1c96, (0, 1));
        schema.global_props[0x1c95] = (0, 0);
        schema.global_events.resize(0x104f, (0, 1));
        schema.global_events[0x104e] = (0, 0);
        // Reserve the preceding indices for other descriptors.
        schema.classes[0].props.push(SchemaProp { name: "Name".into(), ty: 0x31 });
        schema.classes[0].events.push(SchemaEvent { name: "Other".into(), arg_types: vec![] });
        let frame = initial_stream_request(&schema, 0x30aa).unwrap();
        assert_eq!(frame, [
            0x83, 0x03, 0x01, 0xaa, 0x30, 0, 0, 0x95, 0x1c, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0x1c, 0, 0x10, 0x27, 0, 0, 0xff, 0x7f,
            0x07, 0x01, 0xaa, 0x30, 0, 0, 0x4e, 0x10,
            0x80, 0x80, 0x08, 0x80, 0x80, 0x08, 0,
        ]);
    }

    fn schema() -> Schema {
        let prop = |name: &str, ty| SchemaProp { name: name.into(), ty };
        Schema {
            classes: vec![
                SchemaClass { name: "Folder".into(), props: vec![prop("Name", 0x31), prop("Capabilities", 0x37)], events: vec![] },
                SchemaClass { name: "ModuleScript".into(), props: vec![prop("Name", 0x31), prop("Source", 0x06)], events: vec![] },
                SchemaClass { name: "ObjectValue".into(), props: vec![prop("Name", 0x31), prop("Value", 0x1c)], events: vec![] },
            ],
            global_props: vec![(0, 0), (0, 1), (1, 0), (1, 1), (2, 0), (2, 1)],
            ..Default::default()
        }
    }

    fn create(id: u64, class: &str, parent: u64, props: Vec<(&str, PropValue)>) -> LiveEdit {
        LiveEdit::Create { id, class: class.into(), parent, props: props.into_iter().map(|(n, v)| (n.into(), v)).collect() }
    }

    /// A reused scope: new referents start past the saved instances' ones.
    #[test]
    fn allocation_skips_referents_the_server_already_has() {
        let next = AtomicU64::new((5 << 32) | 1);
        let new = |id: u64| RepOp::NewInstance { id, class: "Folder".into(), parent: Some(1), props: vec![] };
        reserve_seen_ids(&next, &[new((5 << 32) | 0xa709e), new((5 << 32) | 0x10), new(0x3d6d), new((7 << 32) | 0xffff_fff0)]);
        assert_eq!(next.load(Ordering::Acquire), (5 << 32) | 0xa709f);
        reserve_seen_ids(&next, &[new((5 << 32) | 3)]);
        assert_eq!(next.load(Ordering::Acquire), (5 << 32) | 0xa709f, "never goes back");
        let unassigned = AtomicU64::new(0);
        reserve_seen_ids(&unassigned, &[new((5 << 32) | 3)]);
        assert_eq!(unassigned.load(Ordering::Acquire), 0);
    }

    fn frames(out: &[Outcome]) -> usize {
        out.iter().filter(|o| matches!(o, Outcome::Frame(_))).count()
    }

    /// A create that can't be sent takes down every edit that names it:
    /// children, moves into it, property changes and references.
    #[test]
    fn a_failed_create_blocks_every_edit_that_names_it() {
        let s = schema();
        let mut gate = EditGate::default();
        let out = gate.encode(Some(&s), create(10, "NotAClass", 1, vec![]));
        assert!(matches!(out[..], [Outcome::Failed { id: 10, .. }]));
        let out = gate.encode(Some(&s), create(11, "Folder", 10, vec![("Name", PropValue::String("Child".into()))]));
        assert!(matches!(out[..], [Outcome::Failed { id: 11, .. }]), "{out:?}");
        let out = gate.encode(Some(&s), create(12, "ModuleScript", 11, vec![]));
        assert!(matches!(out[..], [Outcome::Failed { id: 12, .. }]), "grandchildren too");
        let out = gate.encode(Some(&s), LiveEdit::Reparent { id: 5, parent: 11 });
        assert!(matches!(out[..], [Outcome::Failed { id: 5, .. }]));
        let out = gate.encode(Some(&s), LiveEdit::Property { id: 6, class: "ObjectValue".into(), name: "Value".into(), value: PropValue::Ref(Some(12)) });
        assert!(matches!(out[..], [Outcome::Failed { id: 6, .. }]));
        assert!(gate.encode(Some(&s), LiveEdit::Delete(10)).is_empty(), "nothing to delete on the server");
        // Unrelated edits still go out.
        assert_eq!(frames(&gate.encode(Some(&s), create(13, "Folder", 1, vec![]))), 1);
        assert_eq!(frames(&gate.encode(Some(&s), LiveEdit::Delete(7))), 1);
    }

    /// One property the server can't take drops only itself.
    #[test]
    fn unsupported_properties_are_skipped_not_the_instance() {
        let s = schema();
        let mut gate = EditGate::default();
        let out = gate.encode(Some(&s), create(20, "Folder", 1, vec![
            ("Name", PropValue::String("Kit".into())),
            ("Capabilities", PropValue::Bool(true)),
            ("NotReplicated", PropValue::Bool(true)),
        ]));
        assert!(matches!(out[0], Outcome::Frame(_)), "{out:?}");
        let skipped: Vec<_> = out.iter().filter_map(|o| match o { Outcome::Skipped { name, .. } => Some(name.as_str()), _ => None }).collect();
        assert_eq!(skipped, ["Capabilities", "NotReplicated"]);
        let Outcome::Frame(frame) = &out[0] else { unreachable!() };
        let mut dec = crate::replicator::Decoder::new();
        dec.schema = Some(s.clone());
        let ops = dec.decode_frame(frame[0], &frame[1..]);
        assert!(dec.errors.is_empty(), "{:?}", dec.errors);
        assert!(matches!(&ops[0], RepOp::NewInstance { id: 20, props, .. } if props.len() == 1));
        // A reference to an instance that wasn't created is dropped too.
        gate.encode(Some(&s), create(21, "Nope", 1, vec![]));
        let out = gate.encode(Some(&s), create(22, "ObjectValue", 1, vec![("Value", PropValue::Ref(Some(21)))]));
        assert!(matches!(out[..], [Outcome::Frame(_), Outcome::Skipped { id: 22, .. }]), "{out:?}");
    }

    #[test]
    fn quiet_gap_before_the_end_of_initial_data_cannot_finish_a_join() {
        let start = Instant::now();
        let mut state = InitialState {
            schema: true,
            marker: true,
            instances: true,
            last_batch: Some(start),
            ..InitialState::default()
        };
        let mut decoder = Decoder::new();
        decoder.decode_frame(packet_id::DATA, &[0x10, 12, 0, 0, 0, 0]);
        state.initial_data_complete = decoder.initial_data_complete;
        assert!(!state.can_finish(start + QUIET_PERIOD));

        decoder.decode_frame(packet_id::DATA, &[0x10, 13, 0, 0, 0, 0]);
        assert!(decoder.errors.is_empty());
        state.initial_data_complete = decoder.initial_data_complete;
        let finished_at = start + QUIET_PERIOD * 2;
        state.last_batch = Some(finished_at);
        assert!(!state.can_finish(finished_at));
        assert!(state.can_finish(finished_at + QUIET_PERIOD));
    }
}

/// Writes a message with an unknown item (after its schema message, in the
/// `RUSTUDIO_TC_BAD_FRAME` format) to `<cache>/rustudio/tc-unknown/` so the
/// item can be decoded later. Keeps the newest 20.
fn save_unknown_frame(schema: &[u8], frame: &[u8], kinds: &str) -> Option<std::path::PathBuf> {
    use std::io::Write;
    let dir = std::env::var("RUSTUDIO_CACHE_DIR")
        .ok()
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("XDG_CACHE_HOME")
                .filter(|s| !s.is_empty())
                .map(std::path::PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache")))
                .map(|d| d.join("rustudio"))
        })?
        .join("tc-unknown");
    std::fs::create_dir_all(&dir).ok()?;
    if let Ok(entries) = std::fs::read_dir(&dir) {
        let mut files: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        if files.len() >= 20 {
            files.sort();
            for old in &files[..files.len() - 19] {
                let _ = std::fs::remove_file(old);
            }
        }
    }
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let path = dir.join(format!("{stamp}-item-{kinds}.stream"));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path).ok()?;
    file.write_all(&[6, 1, 4, 0, 0, 0, 1]).ok()?;
    file.write_all(&rtcio::frame(schema)).ok()?;
    file.write_all(&rtcio::frame(frame)).ok()?;
    Some(path)
}
