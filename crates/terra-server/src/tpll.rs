//! `/tpll`: argument parsing (`TpllCommand.parseArguments`) and the teleport decision
//! (`TpllCommand.execute`, `finalizeTeleport`, `handleLinkedWorlds`) as a pure state machine.
//! The host supplies world facts and performs the resulting teleport.

use crate::coordinates::{LatLng, parse_coordinates};
use crate::java_text::parse_java_double;
use terra_projection::{GeoCoordinate, GeographicProjection};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TpllTarget {
    pub position: LatLng,
    /// Explicit height in metres above the datum, before the world's y offset.
    pub height: Option<f64>,
}

/// Java `String.split(" ")`: empty strings between consecutive spaces kept, trailing ones dropped.
fn java_split_space(text: &str) -> Vec<&str> {
    let mut parts: Vec<&str> = text.split(' ').collect();
    while parts.len() > 1 && parts.last() == Some(&"") {
        parts.pop();
    }
    if parts == [""] && !text.is_empty() {
        parts.clear();
    }
    parts
}

pub fn parse_tpll_arguments(arguments: &str) -> Option<TpllTarget> {
    let parts = java_split_space(arguments);
    if parts.len() >= 3
        && let Some(height) = parts.last().and_then(|last| parse_java_double(last))
        && let Some(position) = parse_coordinates(&parts[..parts.len() - 1].join(" "))
    {
        return Some(TpllTarget { position, height: Some(height) });
    }
    parse_coordinates(arguments).map(|position| TpllTarget { position, height: None })
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TeamBounds {
    pub min_latitude: f64,
    pub max_latitude: f64,
    pub min_longitude: f64,
    pub max_longitude: f64,
}

impl TeamBounds {
    /// Upstream treats the restriction as disabled unless all four values are non-zero.
    fn excludes(&self, position: LatLng) -> bool {
        let enabled = self.min_latitude != 0.0 && self.max_latitude != 0.0 && self.min_longitude != 0.0 && self.max_longitude != 0.0;
        enabled
            && (position.latitude < self.min_latitude
                || position.longitude < self.min_longitude
                || position.latitude > self.max_latitude
                || position.longitude > self.max_longitude)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedWorld {
    pub name: String,
    pub offset: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkedWorldsMethod {
    /// Several levels on this server (`MULTIVERSE`).
    Levels,
    /// Several servers behind a proxy (`SERVER`); not available on Horizon.
    Servers,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedWorlds {
    pub enabled: bool,
    pub method: LinkedWorldsMethod,
    pub worlds: Vec<LinkedWorld>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Authorization {
    pub sender_is_target: bool,
    /// `t+-.admin`: ignores team bounds.
    pub admin: bool,
    /// `t+-.tpll.ungenerated-chunks`.
    pub may_generate_chunks: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LevelHeight {
    pub min_y: i32,
    /// Exclusive, as Bukkit's `getMaxHeight`.
    pub max_y: i32,
}

/// World facts the host answers synchronously, from loaded state only.
pub trait TeleportWorld {
    fn level_height(&self, world: &str) -> Option<LevelHeight>;
    fn is_chunk_generated(&self, world: &str, chunk_x: i32, chunk_z: i32) -> bool;
    /// `getHighestBlockYAt` in a generated chunk.
    fn highest_block_y(&self, world: &str, x: i32, z: i32) -> Option<i32>;
}

#[derive(Debug, Clone, PartialEq)]
pub enum TpllStep {
    Usage,
    OutsideProjection,
    OutsideTeamBounds,
    UngeneratedChunkDenied,
    Unreachable,
    LinkedWorldUnavailable { world: String },
    /// The surface height of a not yet generated column is needed: load chunk data for
    /// (`chunk_x`, `chunk_z`) off-tick, then call [`TpllRequest::resume_with_surface`].
    NeedsSurface { chunk_x: i32, chunk_z: i32, local_x: usize, local_z: usize },
    /// `linked` teleports went through linked-world routing; upstream then prints the height as
    /// a Java double (`floor(y) - offset` with a double offset).
    Teleport { world: String, x: f64, y: f64, z: f64, reported: LatLng, reported_height: i32, linked: bool },
}

#[derive(Debug, Clone)]
pub struct TpllRequest {
    world: String,
    target: TpllTarget,
    x: f64,
    z: f64,
    y_offset: i32,
}

fn chunk_of(block: i32) -> i32 {
    block >> 4
}

impl TpllRequest {
    /// Runs `/tpll` up to the first step needing data or a final decision.
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        arguments: &str,
        world_name: &str,
        projection: &dyn GeographicProjection,
        team_bounds: &TeamBounds,
        authorization: Authorization,
        linked: &LinkedWorlds,
        y_offset: i32,
        world: &dyn TeleportWorld,
    ) -> (Option<Self>, TpllStep) {
        let Some(target) = parse_tpll_arguments(arguments) else { return (None, TpllStep::Usage) };
        let Ok(block) = projection.from_geo(GeoCoordinate::new(target.position.longitude, target.position.latitude)) else {
            return (None, TpllStep::OutsideProjection);
        };
        let (x, z) = (block.x, block.y);
        if authorization.sender_is_target && !authorization.admin && team_bounds.excludes(target.position) {
            return (None, TpllStep::OutsideTeamBounds);
        }
        if !authorization.may_generate_chunks && !world.is_chunk_generated(world_name, chunk_of(x.floor() as i32), chunk_of(z.floor() as i32)) {
            return (None, TpllStep::UngeneratedChunkDenied);
        }
        let request = Self { world: world_name.to_owned(), target, x, z, y_offset };
        let (fx, fz) = (x.floor() as i32, z.floor() as i32);
        if let Some(height) = target.height {
            let step = request.finalize(world_name, height + f64::from(y_offset), linked, authorization, world);
            return (Some(request), step);
        }
        if !linked.enabled {
            let step = match world.highest_block_y(world_name, fx, fz) {
                Some(highest) => request.finalize(world_name, f64::from(highest) + 1.0, linked, authorization, world),
                None => request.needs_surface(),
            };
            return (Some(request), step);
        }
        if linked.method == LinkedWorldsMethod::Levels
            && linked.worlds.iter().any(|candidate| candidate.name.eq_ignore_ascii_case(world_name))
            && world.is_chunk_generated(world_name, chunk_of(x.round() as i32), chunk_of(z.round() as i32))
            && let (Some(highest), Some(height)) = (world.highest_block_y(world_name, fx, fz), world.level_height(world_name))
            && highest < height.max_y - 1
            && highest > height.min_y
        {
            let step = request.finalize(world_name, f64::from(highest) + 1.0, linked, authorization, world);
            return (Some(request), step);
        }
        let step = request.needs_surface();
        (Some(request), step)
    }

    fn needs_surface(&self) -> TpllStep {
        let (rounded_x, rounded_z) = (self.x.round() as i32, self.z.round() as i32);
        let (chunk_x, chunk_z) = (chunk_of(rounded_x), chunk_of(rounded_z));
        TpllStep::NeedsSurface { chunk_x, chunk_z, local_x: (rounded_x - (chunk_x << 4)) as usize, local_z: (rounded_z - (chunk_z << 4)) as usize }
    }

    /// Continues after [`TpllStep::NeedsSurface`] with the generator's surface height there.
    pub fn resume_with_surface(&self, surface_height: i32, linked: &LinkedWorlds, authorization: Authorization, world: &dyn TeleportWorld) -> TpllStep {
        self.finalize(&self.world.clone(), f64::from(surface_height.wrapping_add(self.y_offset)) + 1.0, linked, authorization, world)
    }

    fn finalize(&self, world_name: &str, y: f64, linked: &LinkedWorlds, authorization: Authorization, world: &dyn TeleportWorld) -> TpllStep {
        let Some(height) = world.level_height(world_name) else { return TpllStep::LinkedWorldUnavailable { world: world_name.to_owned() } };
        let block_y = y.floor() as i32;
        if block_y > height.max_y {
            return self.linked_world(true, y, f64::from(self.y_offset), world_name, 0, linked, authorization, world);
        }
        if block_y <= height.min_y {
            return self.linked_world(false, y, f64::from(self.y_offset), world_name, 0, linked, authorization, world);
        }
        TpllStep::Teleport { world: world_name.to_owned(), x: self.x, y: f64::from(block_y), z: self.z, reported: self.target.position, reported_height: block_y - self.y_offset, linked: false }
    }

    #[allow(clippy::too_many_arguments)]
    fn linked_world(&self, upward: bool, y: f64, y_offset: f64, world_name: &str, depth: usize, linked: &LinkedWorlds, authorization: Authorization, world: &dyn TeleportWorld) -> TpllStep {
        if !linked.enabled || linked.method != LinkedWorldsMethod::Levels || depth >= linked.worlds.len() {
            return TpllStep::Unreachable;
        }
        let Some(index) = linked.worlds.iter().position(|candidate| candidate.name == world_name) else { return TpllStep::Unreachable };
        let next = if upward { linked.worlds.get(index + 1) } else { index.checked_sub(1).and_then(|previous| linked.worlds.get(previous)) };
        let Some(next) = next else { return TpllStep::Unreachable };
        let Some(height) = world.level_height(&next.name) else { return TpllStep::LinkedWorldUnavailable { world: next.name.clone() } };
        let new_height = y - y_offset + f64::from(next.offset) + 1.0;
        if new_height > f64::from(height.max_y) {
            return self.linked_world(true, new_height, f64::from(next.offset), &next.name, depth + 1, linked, authorization, world);
        }
        if new_height <= f64::from(height.min_y) {
            return self.linked_world(false, new_height, f64::from(next.offset), &next.name, depth + 1, linked, authorization, world);
        }
        if !authorization.may_generate_chunks && !world.is_chunk_generated(&next.name, chunk_of(self.x.floor() as i32), chunk_of(self.z.floor() as i32)) {
            return TpllStep::UngeneratedChunkDenied;
        }
        TpllStep::Teleport {
            world: next.name.clone(),
            x: self.x,
            y: new_height,
            z: self.z,
            reported: self.target.position,
            reported_height: y.floor() as i32 - y_offset as i32,
            linked: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_like_java() {
        assert_eq!(java_split_space("a b "), vec!["a", "b"]);
        assert_eq!(java_split_space(" a"), vec!["", "a"]);
        assert_eq!(java_split_space("a  b"), vec!["a", "", "b"]);
    }

    #[test]
    fn parses_height_suffix() {
        let target = parse_tpll_arguments("48.858370 2.294481 330").unwrap();
        assert_eq!((target.position.latitude, target.height), (48.85837, Some(330.0)));
        let target = parse_tpll_arguments("48.858370, 2.294481").unwrap();
        assert_eq!(target.height, None);
        assert!(parse_tpll_arguments("nowhere").is_none());
    }
}
