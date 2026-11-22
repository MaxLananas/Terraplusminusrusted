//! What upstream does around players rather than chunks:
//!
//! * linked-worlds method SERVER: the `terraplusminus:teleportbridge` and `bungeecord:main`
//!   plugin channels (`PluginMessageEvent`, `PlayerJoinEvent`, `sendPluginMessageToBungeeBridge`);
//! * method MULTIVERSE: moving players between linked levels as they cross a boundary
//!   (`PlayerMoveEvent`, `t+-.autoteleport`);
//! * update notices for operators (`Updater`, `t+-.notify.update`).
//!
//! The decisions live in `terra_server::{bridge, autoteleport, updates}`; this is the glue to
//! Horizon's hooks, raw payload channels and handles.

use super::{HostWorld, LevelRegions, Shared, chat_text, log};
use mod_api::{
    BlockKey, Host, IPayloadContext, IPayloadHandler, PayloadBytes, PayloadContextError, PayloadHandleError, PayloadId, PayloadWork, PermissionLevel, PlayerRef, ServerHost, Text,
    TextStyle,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use terra_chunk_generation::{ChunkTicket, WorldHeight};
use terra_server::autoteleport::{self, AutoTeleports, Transition, TransitionEnd};
use terra_server::bridge::{self, BridgeDelivery, PendingArrivals};
use terra_server::messages::ChatMessage;
use terra_server::updates::{UpdateChecker, UpdateStatus, notification_spans};
use terra_server::{LinkedWorldsMethod, TeleportWorld, parse_tpll_arguments};

/// Announced arrivals kept for players who have not joined yet.
const MAX_ARRIVALS: usize = 1024;
const ARRIVAL_LIFETIME: Duration = Duration::from_secs(120);
/// Ticks after a level change during which the landing spot is checked and motion restored.
const SETTLE_TICKS: u64 = 40;

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Method SERVER state, shared with the payload handlers Horizon owns.
pub struct ProxyBridge {
    /// This server's name, from the proxy's `GetServer` answer (set once, as upstream).
    proxy_name: Mutex<Option<String>>,
    /// `linked_worlds.server_name`, known before the proxy answers.
    configured_name: Mutex<Option<String>>,
    arrivals: Mutex<PendingArrivals>,
    /// Players to ask the proxy about, with the tick to do it at.
    get_server_due: Mutex<Vec<(u128, u64)>>,
}

impl Default for ProxyBridge {
    fn default() -> Self {
        Self { proxy_name: Mutex::new(None), configured_name: Mutex::new(None), arrivals: Mutex::new(PendingArrivals::new(MAX_ARRIVALS, ARRIVAL_LIFETIME)), get_server_due: Mutex::default() }
    }
}

impl ProxyBridge {
    pub fn configure(&self, configured: Option<String>) {
        *lock(&self.configured_name) = configured;
    }

    /// `Terraplusminus.getRegisteredServerName`, with the configured name until the proxy answers.
    pub fn server_name(&self) -> Option<String> {
        lock(&self.proxy_name).clone().or_else(|| lock(&self.configured_name).clone())
    }

    pub fn status_line(&self) -> String {
        let proxy = lock(&self.proxy_name).clone();
        let configured = lock(&self.configured_name).clone();
        format!(
            "linked server: {} (proxy: {}, configured: {}), {} arrival(s) waiting",
            self.server_name().unwrap_or_else(|| "unknown".into()),
            proxy.unwrap_or_else(|| "no answer yet".into()),
            configured.unwrap_or_else(|| "none".into()),
            lock(&self.arrivals).len()
        )
    }

    /// `PlayerJoinEvent`: ask for the server name 20 ticks later while it is unknown, and run a
    /// `/tpll` the proxy announced before the player arrived.
    pub fn joined(&self, host: &mut dyn ServerHost, uuid: u128, tick: u64) {
        if lock(&self.proxy_name).is_none() {
            lock(&self.get_server_due).push((uuid, tick + bridge::GET_SERVER_DELAY_TICKS));
        }
        let parked = lock(&self.arrivals).take(uuid, Instant::now());
        if let Some(coordinates) = parked {
            run_tpll_as(host, uuid, &coordinates);
        }
    }

    pub fn left(&self, uuid: u128) {
        lock(&self.get_server_due).retain(|(player, _)| *player != uuid);
    }

    /// Sends the due `GetServer` requests (`Bukkit.getScheduler().runTaskLater(..., 20)`).
    pub fn tick(&self, host: &mut dyn ServerHost, tick: u64) {
        let due: Vec<u128> = {
            let mut pending = lock(&self.get_server_due);
            if pending.is_empty() {
                return;
            }
            let (due, waiting): (Vec<_>, Vec<_>) = pending.drain(..).partition(|(_, at)| *at <= tick);
            *pending = waiting;
            due.into_iter().map(|(uuid, _)| uuid).collect()
        };
        for uuid in due {
            if lock(&self.proxy_name).is_some() {
                return;
            }
            let Some(player) = host.player(uuid) else { continue };
            log("Sending plugin message to BungeeCord to get server name.");
            if let Err(error) = player.send_payload(host, &PayloadId(bridge::BUNGEECORD_CHANNEL.into()), PayloadBytes(bridge::get_server_request())) {
                log(&format!("could not ask the proxy for this server's name: {error:?}"));
            }
        }
    }

    fn deliver(&self, host: &mut dyn ServerHost, delivery: BridgeDelivery) {
        // Anyone connected can write on a raw channel, so only plain coordinates are accepted:
        // never `-p` or a selector, which would run as the target player.
        let coordinates = delivery.coordinates.trim();
        if coordinates.starts_with('-') || coordinates.chars().any(char::is_control) || parse_tpll_arguments(coordinates).is_none() {
            return log(&format!("ignored a teleport bridge message with invalid coordinates `{}`", delivery.coordinates.escape_debug()));
        }
        if host.player(delivery.uuid).is_some() {
            run_tpll_as(host, delivery.uuid, coordinates);
        } else {
            // The proxy sends this as it hands the player over; they usually are not here yet.
            lock(&self.arrivals).park(delivery.uuid, coordinates.to_owned(), Instant::now());
        }
    }

    fn registered(&self, name: String) {
        let mut proxy = lock(&self.proxy_name);
        if proxy.is_some() {
            return;
        }
        if let Some(configured) = lock(&self.configured_name).as_ref()
            && !configured.eq_ignore_ascii_case(&name)
        {
            log(&format!("the proxy calls this server `{name}`, not `{configured}` (linked_worlds.server_name); using the proxy's name"));
        }
        log(&format!("Registered server name: {name}"));
        *proxy = Some(name);
    }
}

/// `player.performCommand("tpll " + coordinates)`: the player runs it with their own rights.
fn run_tpll_as(host: &mut dyn ServerHost, uuid: u128, coordinates: &str) {
    let Some(player) = host.player(uuid) else { return };
    let level = player.permission_level(&*host).unwrap_or(0);
    if !player.entity().run_command(host, &format!("tpll {coordinates}"), PermissionLevel::from_level(i32::from(level))) {
        log(&format!("the proxy's /tpll {coordinates} did not run"));
    }
}

/// Work for the main thread, with the server host.
type MainThreadAction = Arc<dyn Fn(&mut dyn ServerHost) + Send + Sync>;

/// Runs `action` with the server host, now on the main thread or queued there.
fn on_main_thread(context: &mut dyn IPayloadContext, action: MainThreadAction) -> Result<(), PayloadHandleError> {
    struct Work(MainThreadAction);
    impl PayloadWork for Work {
        fn run(&self, context: &mut dyn IPayloadContext) -> Result<(), String> {
            let host = context.server_host().map_err(|error| format!("{error:?}"))?;
            (self.0)(host);
            Ok(())
        }
    }
    match context.server_host() {
        Ok(host) => {
            action(host);
            Ok(())
        }
        Err(PayloadContextError::WrongThread) => context.enqueue_work(Arc::new(Work(action))).map(drop).map_err(|error| PayloadHandleError::Failed(format!("{error:?}"))),
        Err(error) => Err(PayloadHandleError::Failed(format!("{error:?}"))),
    }
}

/// `terraplusminus:teleportbridge`, from TerraPlusMinusProxyBridge.
pub struct TeleportBridgeHandler(pub Arc<ProxyBridge>);

impl IPayloadHandler for TeleportBridgeHandler {
    fn handle(&self, payload: PayloadBytes, context: &mut dyn IPayloadContext) -> Result<(), PayloadHandleError> {
        let Some(delivery) = bridge::parse_delivery(&payload.0) else {
            log("Failed to read plugin message on terraplusminus:teleportbridge");
            return Ok(());
        };
        let state = Arc::clone(&self.0);
        on_main_thread(context, Arc::new(move |host| state.deliver(host, delivery.clone())))
    }
}

/// `bungeecord:main`: only the `GetServer` answer matters.
pub struct BungeeCordHandler(pub Arc<ProxyBridge>);

impl IPayloadHandler for BungeeCordHandler {
    fn handle(&self, payload: PayloadBytes, _: &mut dyn IPayloadContext) -> Result<(), PayloadHandleError> {
        if let Some(name) = bridge::parse_get_server(&payload.0) {
            self.0.registered(name);
        }
        Ok(())
    }
}

/// `sendPluginMessageToBungeeBridge`: asks the proxy to move `player` to the server above or
/// below, where `/tpll` runs again. False when there is no such server or the send failed.
pub fn send_to_linked_server(host: &mut dyn ServerHost, shared: &Shared, proxy: &ProxyBridge, player: PlayerRef, upward: bool, position: terra_server::LatLng) -> bool {
    let linked = &shared.stack.config().linked_worlds;
    let Some(current) = proxy.server_name() else {
        log("/tpll to a linked server: this server's name is unknown (no GetServer answer yet; set linked_worlds.server_name)");
        return false;
    };
    let Some((server, offset)) = bridge::neighbour_server(linked, &current, upward) else { return false };
    let Some(uuid) = player.entity().uuid(host) else { return false };
    let Some(bytes) = bridge::teleport_request(uuid, &server, offset, position) else { return false };
    match player.send_payload(host, &PayloadId(bridge::TELEPORT_BRIDGE_CHANNEL.into()), PayloadBytes(bytes)) {
        Ok(_) => true,
        Err(error) => {
            log(&format!("could not reach the proxy bridge: {error:?}"));
            false
        }
    }
}

pub fn proxy_method(shared: &Shared) -> bool {
    let linked = &shared.stack.config().linked_worlds;
    linked.enabled && linked.method == LinkedWorldsMethod::Servers
}

/// Motion carried across a level change (`Motion.capture` / `restore`).
#[derive(Clone, Copy)]
struct Motion {
    velocity: Option<[f64; 3]>,
    flying: bool,
}

enum Phase {
    /// Decided; the target height is looked up next tick.
    Decided,
    /// Upward into an unloaded column: the generator's surface is being computed.
    Surface { ticket: ChunkTicket, local: (usize, usize), deadline: Instant },
    /// Teleported; checking the landing spot once the player is there, then restoring motion.
    Settling { target: String, until_tick: u64 },
}

struct Active {
    source: String,
    transition: Transition,
    x: f64,
    z: f64,
    rotation: [f32; 2],
    motion: Motion,
    phase: Phase,
}

/// Auto-teleport state: upstream's cooldown tables plus the transitions under way.
#[derive(Default)]
pub struct LevelTransitions {
    rules: AutoTeleports<u128>,
    active: HashMap<u128, Active>,
}

impl LevelTransitions {
    pub fn status_line(&self) -> String {
        format!("auto-teleport: {} transition(s) under way, {} player(s) on cooldown", self.active.len(), self.rules.tracked())
    }

    pub fn forget(&mut self, uuid: u128) {
        self.rules.forget(&uuid);
        self.active.remove(&uuid);
    }
}

fn level_of(host: &dyn Host, levels: &[String], player: PlayerRef) -> Option<String> {
    let current = player.entity().level(host)?;
    levels.iter().find(|id| host.level(id) == Some(current)).cloned()
}

/// Feet and head free, by the level's collision boxes (`Block.isPassable` for full blocks).
fn passable<'a>(host: &'a dyn Host, level: &str, x: i32, z: i32) -> impl Fn(i32) -> bool + 'a {
    let handle = host.level(level);
    move |y: i32| {
        let Some(handle) = handle else { return true };
        let (x, y, z) = (f64::from(x), f64::from(y), f64::from(z));
        handle.collision_boxes(host, [x + 0.001, y + 0.001, z + 0.001, x + 0.999, y + 0.999, z + 0.999]).is_none_or(|boxes| boxes.is_empty())
    }
}

fn chunk_loaded(host: &dyn Host, level: &str, x: i32, z: i32) -> bool {
    host.level(level).and_then(|handle| host.chunk(handle, [x >> 4, z >> 4])).is_some()
}

pub struct MoveContext<'a> {
    pub shared: &'a Shared,
    pub regions: &'a LevelRegions,
    pub air: &'a [BlockKey],
}

impl LevelTransitions {
    /// `PlayerMoveEvent.onPlayerMove` → `handleWorldTransition`: decides, and leaves the
    /// teleport to the next tick so the move being validated is not disturbed.
    pub fn on_move(&mut self, host: &mut dyn ServerHost, shared: &Shared, uuid: u128, to: [f64; 3], rotation: [f32; 2]) {
        let config = shared.stack.config();
        if !config.linked_worlds.levels_active() || self.active.contains_key(&uuid) {
            return;
        }
        let Some(player) = host.player(uuid) else { return };
        let levels: Vec<String> = config.linked_worlds.worlds.iter().map(|world| world.name.to_ascii_lowercase()).collect();
        let Some(level) = level_of(&*host, &levels, player) else { return };
        let permitted = player.permission_level(&*host).unwrap_or(0) >= config.permissions.autoteleport;
        let Some(transition) = self.rules.on_move(&uuid, &level, to[1], permitted, &config.linked_worlds, config.world, Instant::now()) else { return };
        let flying = player.abilities(&*host).is_some_and(|abilities| abilities.flying);
        let motion = Motion { velocity: player.entity().velocity(&*host), flying };
        self.active.insert(uuid, Active { source: level, transition, x: to[0], z: to[2], rotation, motion, phase: Phase::Decided });
    }

    /// Advances every transition under way; called at each tick start.
    pub fn tick(&mut self, host: &mut dyn ServerHost, context: &MoveContext<'_>, tick: u64) {
        if self.active.is_empty() {
            self.rules.prune(Instant::now());
            return;
        }
        let players: Vec<u128> = self.active.keys().copied().collect();
        for uuid in players {
            let Some(mut active) = self.active.remove(&uuid) else { continue };
            match self.advance(host, context, uuid, &mut active, tick) {
                Some(end) => self.rules.finish(&uuid, end, Instant::now()),
                None => {
                    self.active.insert(uuid, active);
                }
            }
        }
    }

    /// `None` while the transition is still under way.
    fn advance(&mut self, host: &mut dyn ServerHost, context: &MoveContext<'_>, uuid: u128, active: &mut Active, tick: u64) -> Option<TransitionEnd> {
        let config = context.shared.stack.config();
        let world: WorldHeight = config.world;
        let Some(player) = host.player(uuid) else { return Some(TransitionEnd::Abandoned) };
        let levels: Vec<String> = config.linked_worlds.worlds.iter().map(|world| world.name.to_ascii_lowercase()).collect();
        let current = level_of(&*host, &levels, player);
        let (bx, bz) = (active.x.floor() as i32, active.z.floor() as i32);
        if let Phase::Settling { target, until_tick } = &active.phase {
            if current.as_deref() == Some(target.as_str()) && chunk_loaded(&*host, target, bx, bz) {
                settle(host, player, target, world, active.rotation);
                restore_motion(host, player, active.motion);
                return Some(TransitionEnd::Moved);
            }
            return (tick >= *until_tick).then_some(TransitionEnd::Moved);
        }
        // Before the teleport: the player must still be where the transition started.
        if current.as_deref() != Some(active.source.as_str()) {
            return Some(TransitionEnd::Abandoned);
        }
        let target = active.transition.target().to_owned();
        let y = match &active.transition {
            Transition::Up { target_offset, .. } => {
                let surface = if chunk_loaded(&*host, &target, bx, bz) {
                    HostWorld { host: &*host, regions: context.regions, air: context.air }.highest_block_y(&target, bx, bz)
                } else {
                    if matches!(active.phase, Phase::Decided) {
                        let (chunk_x, chunk_z) = (bx >> 4, bz >> 4);
                        let Ok(ticket) = context.shared.stack.loader().request(chunk_x, chunk_z) else { return Some(TransitionEnd::Abandoned) };
                        let local = ((bx - (chunk_x << 4)) as usize, (bz - (chunk_z << 4)) as usize);
                        active.phase = Phase::Surface { ticket, local, deadline: Instant::now() + config.generation_wait.min(autoteleport::TRANSITION_TIMEOUT) };
                        return None;
                    }
                    let Phase::Surface { ticket, local, deadline } = &mut active.phase else { return Some(TransitionEnd::Abandoned) };
                    match ticket.try_get() {
                        // The generated column: its top block shifted by the target's offset,
                        // absent where the level clips it away.
                        Some(Ok(terrain)) => {
                            let top = terrain.surface_height(local.0, local.1).saturating_add(*target_offset);
                            (top >= world.min_y).then(|| top.min(world.max_y - 1))
                        }
                        Some(Err(_)) => return Some(TransitionEnd::Abandoned),
                        None if Instant::now() >= *deadline => return Some(TransitionEnd::Abandoned),
                        None => return None,
                    }
                };
                match autoteleport::upward_height(&active.transition, surface) {
                    Some(y) => y,
                    None => return Some(TransitionEnd::Declined),
                }
            }
            Transition::Down { converted_y, .. } => {
                if chunk_loaded(&*host, &target, bx, bz) {
                    autoteleport::free_space_at(world, *converted_y, &passable(&*host, &target, bx, bz))
                } else {
                    *converted_y
                }
            }
        };
        if !player.entity().teleport_to_level(host, &target, [active.x, f64::from(y), active.z], Some(active.rotation)) {
            return Some(TransitionEnd::Abandoned);
        }
        let line = terra_server::messages::linked_world_transition(&config.prefix);
        let _ = player.tell(host, chat_text(&ChatMessage::from(line)));
        active.phase = Phase::Settling { target, until_tick: tick + SETTLE_TICKS };
        None
    }
}

/// Raises a player who landed inside blocks (a build, or terrain the prediction missed) to the
/// first spot where feet and head are free.
fn settle(host: &mut dyn ServerHost, player: PlayerRef, level: &str, world: WorldHeight, rotation: [f32; 2]) {
    let Some([x, y, z]) = player.entity().position(&*host) else { return };
    let (bx, by, bz) = (x.floor() as i32, y.floor() as i32, z.floor() as i32);
    let free = autoteleport::free_space_at(world, by, &passable(&*host, level, bx, bz));
    if free > by {
        let _ = player.entity().teleport_to_level(host, level, [x, f64::from(free), z], Some(rotation));
    }
}

fn restore_motion(host: &mut dyn ServerHost, player: PlayerRef, motion: Motion) {
    if let Some(velocity) = motion.velocity {
        let _ = player.entity().set_velocity(host, autoteleport::capped_velocity(velocity));
    }
    if motion.flying
        && let Some(mut abilities) = player.abilities(&*host)
        && abilities.may_fly
        && !abilities.flying
    {
        abilities.flying = true;
        let _ = player.set_abilities(host, abilities);
    }
}

/// The update notice as upstream colours it.
pub fn update_notice(checker: &UpdateChecker, latest: &str) -> Text {
    notification_spans(MOD_ID_DISPLAY, checker.current(), latest).into_iter().fold(Text::literal(""), |parent, (text, color)| {
        let mut style = TextStyle::default();
        style.color = Some(color.to_owned());
        parent.append(Text::literal(text).with_style(style))
    })
}

/// `%plugin%` in upstream's notice: the plugin's name.
pub const MOD_ID_DISPLAY: &str = "Terraplusminus";

/// Starts a release check when one is due; the outcome is logged from its thread.
pub fn check_for_updates(checker: &Arc<UpdateChecker>, user_agent: &str) {
    if !checker.claim(Instant::now()) {
        return;
    }
    let current = checker.current().to_owned();
    let started = checker.spawn_check(user_agent.to_owned(), move |status| match status {
        UpdateStatus::Available { latest } => log(&format!("A new {MOD_ID_DISPLAY} update is now available! {current} -> {latest}")),
        UpdateStatus::UpToDate { .. } | UpdateStatus::NotChecked => {}
        UpdateStatus::Failed(reason) => log(&format!("update check failed: {reason}")),
    });
    if !started {
        let _ = checker.record(Err("could not start the update check thread".into()));
    }
}
