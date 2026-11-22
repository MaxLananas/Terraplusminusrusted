//! `PlayerMoveEvent` transitions between linked levels (method MULTIVERSE, `t+-.autoteleport`),
//! with upstream's thresholds, cooldowns and placement rules. The host feeds each move and
//! performs the teleport; everything that decides is here.
//!
//! All levels of this mod share one height range, so `LinkedWorldLayout.upwardThreshold` is the
//! range's exclusive top for every level and `downwardThreshold(i)` is
//! `convertY(top, i - 1, i)`.

use crate::tpll::LinkedWorlds;
use std::collections::HashMap;
use std::hash::Hash;
use std::time::{Duration, Instant};
use terra_chunk_generation::WorldHeight;

/// Blocks below the downward trigger at which a lag-dragged player is moved despite the cooldown.
pub const FAILSAFE_MARGIN: i32 = 32;
/// Keeps a player from bouncing between two levels.
pub const TELEPORT_COOLDOWN: Duration = Duration::from_millis(1000);
/// After a transition that decided against moving the player.
pub const RETRY_COOLDOWN: Duration = Duration::from_millis(250);
/// A transition that never completes stops counting as running after this.
pub const TRANSITION_TIMEOUT: Duration = Duration::from_secs(15);
/// Blocks above the build limit at which a player is moved up even over an empty column.
pub const FREE_ASCENT_MARGIN: i32 = 4;
/// Vanilla's maximum velocity, in blocks per tick.
pub const MAX_SPEED: f64 = 3.92;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transition {
    /// Into the level above. The player is placed on top of the target column, never below
    /// `converted_y`; with no terrain above `band_bottom` the move is called off unless
    /// `clear_of_boundary`.
    Up { target: String, target_offset: i32, band_bottom: i32, converted_y: i32, clear_of_boundary: bool },
    /// Into the level below, at the converted height (raised until the player fits).
    Down { target: String, target_offset: i32, converted_y: i32, failsafe: bool },
}

impl Transition {
    pub fn target(&self) -> &str {
        match self {
            Self::Up { target, .. } | Self::Down { target, .. } => target,
        }
    }
}

/// How a started transition ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionEnd {
    /// The player was teleported: a fresh full cooldown.
    Moved,
    /// Upward, the target column had no terrain: judge the next step soon (retry cooldown).
    Declined,
    /// The chunk or the teleport failed, or the player left: nothing more.
    Abandoned,
}

#[derive(Debug, Default, Clone, Copy)]
struct PlayerState {
    cooldown_until: Option<Instant>,
    retry_until: Option<Instant>,
    in_flight_until: Option<Instant>,
}

impl PlayerState {
    fn active(until: Option<Instant>, now: Instant) -> bool {
        until.is_some_and(|until| now < until)
    }

    fn on_cooldown(&self, now: Instant) -> bool {
        Self::active(self.cooldown_until, now) || Self::active(self.retry_until, now)
    }

    fn idle(&self, now: Instant) -> bool {
        !self.on_cooldown(now) && !Self::active(self.in_flight_until, now)
    }
}

/// The per-player caches upstream keeps (`teleportCooldowns`, `retryCooldowns`,
/// `transitionsInFlight`), with their expiries.
#[derive(Debug)]
pub struct AutoTeleports<P> {
    players: HashMap<P, PlayerState>,
}

impl<P> Default for AutoTeleports<P> {
    fn default() -> Self {
        Self { players: HashMap::new() }
    }
}

impl<P: Eq + Hash + Clone> AutoTeleports<P> {
    /// Players with a cooldown or a transition still running.
    pub fn tracked(&self) -> usize {
        self.players.len()
    }

    /// `handleWorldTransition` for a move whose destination feet are at `feet_y`, in `level`.
    /// Returns the transition to perform; the player then counts as moving until
    /// [`AutoTeleports::finish`].
    #[allow(clippy::too_many_arguments)]
    pub fn on_move(&mut self, player: &P, level: &str, feet_y: f64, permitted: bool, linked: &LinkedWorlds, world: WorldHeight, now: Instant) -> Option<Transition> {
        if !linked.levels_active() {
            return None;
        }
        let index = linked.index_of(level)?;
        let state = self.players.get(player).copied().unwrap_or_default();
        if PlayerState::active(state.in_flight_until, now) || !permitted || !feet_y.is_finite() {
            return None;
        }
        let y = feet_y.floor() as i32;
        let offset = |i: usize| linked.worlds[i].offset;
        let convert = |y: i32, from: usize, to: usize| y.wrapping_sub(offset(from)).wrapping_add(offset(to));
        let upward_threshold = world.max_y;
        if index + 1 < linked.worlds.len() && y >= upward_threshold {
            if state.on_cooldown(now) {
                return None;
            }
            let transition = Transition::Up {
                target: linked.worlds[index + 1].name.clone(),
                target_offset: offset(index + 1),
                band_bottom: convert(upward_threshold, index, index + 1),
                converted_y: convert(y, index, index + 1),
                clear_of_boundary: y >= upward_threshold.saturating_add(FREE_ASCENT_MARGIN),
            };
            self.begin(player, now);
            return Some(transition);
        }
        if index == 0 {
            return None;
        }
        let threshold = convert(upward_threshold, index - 1, index);
        if y >= threshold {
            return None;
        }
        let failsafe = y <= threshold.saturating_sub(FAILSAFE_MARGIN);
        if !failsafe && state.on_cooldown(now) {
            return None;
        }
        let transition = Transition::Down { target: linked.worlds[index - 1].name.clone(), target_offset: offset(index - 1), converted_y: convert(y, index, index - 1), failsafe };
        self.begin(player, now);
        Some(transition)
    }

    fn begin(&mut self, player: &P, now: Instant) {
        let state = self.players.entry(player.clone()).or_default();
        state.in_flight_until = Some(now + TRANSITION_TIMEOUT);
        state.cooldown_until = Some(now + TELEPORT_COOLDOWN);
    }

    pub fn finish(&mut self, player: &P, end: TransitionEnd, now: Instant) {
        let Some(state) = self.players.get_mut(player) else { return };
        state.in_flight_until = None;
        match end {
            TransitionEnd::Moved => state.cooldown_until = Some(now + TELEPORT_COOLDOWN),
            TransitionEnd::Declined => {
                state.cooldown_until = None;
                state.retry_until = Some(now + RETRY_COOLDOWN);
            }
            TransitionEnd::Abandoned => {}
        }
    }

    /// A player who left.
    pub fn forget(&mut self, player: &P) {
        self.players.remove(player);
    }

    /// Drops players with nothing running, so the table stays as small as the active players.
    pub fn prune(&mut self, now: Instant) {
        self.players.retain(|_, state| !state.idle(now));
    }
}

/// `transitionUp`'s height: on top of the target column (`surface`, the highest solid block, if
/// any), never below the converted height. `None` calls the move off.
pub fn upward_height(transition: &Transition, surface: Option<i32>) -> Option<i32> {
    let Transition::Up { band_bottom, converted_y, clear_of_boundary, .. } = *transition else { return None };
    if surface.is_none_or(|surface| surface < band_bottom) && !clear_of_boundary {
        return None;
    }
    Some(surface.map_or(converted_y, |surface| converted_y.max(surface.saturating_add(1))))
}

/// `freeSpaceAt`: `y` raised until the player's two blocks are passable. Outside the height range
/// everything is passable.
pub fn free_space_at(world: WorldHeight, y: i32, passable: &dyn Fn(i32) -> bool) -> i32 {
    let min = world.min_y.saturating_add(1);
    let max = world.max_y;
    let open = |y: i32| y < world.min_y || y >= world.max_y || passable(y);
    let mut candidate = y.clamp(min, max);
    while candidate < max && !(open(candidate) && open(candidate + 1)) {
        candidate += 1;
    }
    candidate
}

/// `Motion.capped`: the velocity to restore after the teleport.
pub fn capped_velocity(velocity: [f64; 3]) -> [f64; 3] {
    let speed = velocity.iter().map(|component| component * component).sum::<f64>().sqrt();
    if !speed.is_finite() {
        return [0.0; 3];
    }
    if speed > MAX_SPEED { velocity.map(|component| component * MAX_SPEED / speed) } else { velocity }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tpll::{LinkedWorld, LinkedWorldsMethod};

    const WORLD: WorldHeight = WorldHeight { min_y: -64, max_y: 2032 };

    fn layout() -> LinkedWorlds {
        LinkedWorlds {
            enabled: true,
            method: LinkedWorldsMethod::Levels,
            worlds: vec![
                LinkedWorld { name: "terraplusminus:low".into(), offset: 2032 },
                LinkedWorld { name: "terraplusminus:earth".into(), offset: 0 },
                LinkedWorld { name: "terraplusminus:high".into(), offset: -2032 },
            ],
        }
    }

    #[test]
    fn upward_at_the_build_limit_with_cooldown() {
        let linked = layout();
        let now = Instant::now();
        let mut teleports = AutoTeleports::default();
        assert_eq!(teleports.on_move(&1, "terraplusminus:earth", 2031.9, true, &linked, WORLD, now), None);
        let up = teleports.on_move(&1, "terraplusminus:earth", 2032.0, true, &linked, WORLD, now).unwrap();
        assert_eq!(up, Transition::Up { target: "terraplusminus:high".into(), target_offset: -2032, band_bottom: 0, converted_y: 0, clear_of_boundary: false });
        // Running: nothing else starts, even later.
        assert_eq!(teleports.on_move(&1, "terraplusminus:earth", 2040.0, true, &linked, WORLD, now + Duration::from_secs(5)), None);
        teleports.finish(&1, TransitionEnd::Moved, now);
        assert_eq!(teleports.on_move(&1, "terraplusminus:earth", 2040.0, true, &linked, WORLD, now + Duration::from_millis(999)), None, "cooldown");
        let clear = teleports.on_move(&1, "terraplusminus:earth", 2036.0, true, &linked, WORLD, now + Duration::from_millis(1000)).unwrap();
        assert!(matches!(clear, Transition::Up { clear_of_boundary: true, converted_y: 4, .. }), "{clear:?}");
        // The top level has nowhere to go up; permission and linked state gate everything.
        assert_eq!(teleports.on_move(&2, "terraplusminus:high", 5000.0, true, &linked, WORLD, now), None);
        assert_eq!(teleports.on_move(&3, "terraplusminus:earth", 2032.0, false, &linked, WORLD, now), None);
        assert_eq!(teleports.on_move(&3, "minecraft:overworld", 2032.0, true, &linked, WORLD, now), None);
        let mut disabled = layout();
        disabled.enabled = false;
        assert_eq!(teleports.on_move(&3, "terraplusminus:earth", 2032.0, true, &disabled, WORLD, now), None);
    }

    #[test]
    fn downward_below_the_band_with_failsafe() {
        let linked = layout();
        let now = Instant::now();
        let mut teleports = AutoTeleports::default();
        // earth's band starts where low's top (2032) lands: 2032 - 2032 + 0 = 0.
        assert_eq!(teleports.on_move(&1, "terraplusminus:earth", 0.0, true, &linked, WORLD, now), None);
        let down = teleports.on_move(&1, "terraplusminus:earth", -0.5, true, &linked, WORLD, now).unwrap();
        assert_eq!(down, Transition::Down { target: "terraplusminus:low".into(), target_offset: 2032, converted_y: 2031, failsafe: false });
        teleports.finish(&1, TransitionEnd::Moved, now);
        assert_eq!(teleports.on_move(&1, "terraplusminus:earth", -5.0, true, &linked, WORLD, now), None, "cooldown");
        let forced = teleports.on_move(&1, "terraplusminus:earth", -32.0, true, &linked, WORLD, now).unwrap();
        assert!(matches!(forced, Transition::Down { failsafe: true, converted_y: 2000, .. }), "{forced:?}");
        // The failsafe never overrides a transition already running.
        assert_eq!(teleports.on_move(&1, "terraplusminus:earth", -60.0, true, &linked, WORLD, now), None);
        assert_eq!(teleports.on_move(&2, "terraplusminus:low", -60.0, true, &linked, WORLD, now), None, "nothing below the lowest level");
    }

    #[test]
    fn declined_transitions_retry_sooner_and_tables_shrink() {
        let linked = layout();
        let now = Instant::now();
        let mut teleports = AutoTeleports::default();
        assert!(teleports.on_move(&1, "terraplusminus:earth", 2032.0, true, &linked, WORLD, now).is_some());
        teleports.finish(&1, TransitionEnd::Declined, now);
        assert_eq!(teleports.on_move(&1, "terraplusminus:earth", 2032.0, true, &linked, WORLD, now + Duration::from_millis(249)), None);
        assert!(teleports.on_move(&1, "terraplusminus:earth", 2032.0, true, &linked, WORLD, now + Duration::from_millis(250)).is_some());
        teleports.finish(&1, TransitionEnd::Abandoned, now);
        teleports.prune(now + Duration::from_secs(2));
        assert_eq!(teleports.tracked(), 0);
        // A transition that never finishes stops blocking after the timeout.
        assert!(teleports.on_move(&2, "terraplusminus:earth", 2032.0, true, &linked, WORLD, now).is_some());
        assert!(teleports.on_move(&2, "terraplusminus:earth", 2032.0, true, &linked, WORLD, now + TRANSITION_TIMEOUT).is_some());
        teleports.forget(&2);
        assert_eq!(teleports.tracked(), 0);
    }

    #[test]
    fn placement_follows_upstream() {
        let up = Transition::Up { target: "t".into(), target_offset: -2032, band_bottom: 0, converted_y: 1, clear_of_boundary: false };
        assert_eq!(upward_height(&up, None), None, "empty column: stay");
        assert_eq!(upward_height(&up, Some(-1)), None, "terrain only below the band: stay");
        assert_eq!(upward_height(&up, Some(0)), Some(1));
        assert_eq!(upward_height(&up, Some(40)), Some(41), "on top of the clipped mountain");
        let flying = Transition::Up { target: "t".into(), target_offset: -2032, band_bottom: 0, converted_y: 1, clear_of_boundary: true };
        assert_eq!(upward_height(&flying, None), Some(1));
        assert_eq!(upward_height(&flying, Some(-30)), Some(1), "never below the converted height");
        let down = Transition::Down { target: "t".into(), target_offset: 0, converted_y: 5, failsafe: false };
        assert_eq!(upward_height(&down, Some(5)), None);

        let solid_up_to_10 = |y: i32| y > 10;
        assert_eq!(free_space_at(WORLD, 5, &solid_up_to_10), 11);
        assert_eq!(free_space_at(WORLD, 20, &solid_up_to_10), 20);
        assert_eq!(free_space_at(WORLD, -500, &|_| true), -63, "clamped above the floor");
        assert_eq!(free_space_at(WORLD, 5000, &|_| false), 2032, "the top is open");
        assert_eq!(free_space_at(WORLD, 2030, &|y| y != 2030), 2031, "the head block above the range is open");
    }

    #[test]
    fn velocity_is_capped_and_sanitised() {
        assert_eq!(capped_velocity([0.1, -0.2, 0.3]), [0.1, -0.2, 0.3]);
        let fast = capped_velocity([0.0, -10.0, 0.0]);
        assert!((fast[1] + MAX_SPEED).abs() < 1e-12);
        assert_eq!(capped_velocity([f64::NAN, 0.0, 0.0]), [0.0; 3]);
        assert_eq!(capped_velocity([f64::INFINITY, 0.0, 0.0]), [0.0; 3]);
    }
}
