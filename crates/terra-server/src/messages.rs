//! The chat text of TerraPlusMinus' commands, reproducing upstream strings and number
//! formatting (`Double.toString` concatenation, root-locale `String.format`).

use crate::config::ServerConfig;
use crate::java_text::{java_double_to_string, java_format_fixed};
use crate::tpll::{LinkedWorldsMethod, TpllStep};
use terra_projection::{GeographicProjection, ProjectedCoordinate};

pub const DEFAULT_PREFIX: &str = "§2§lT+- §8» ";

pub fn tpll_usage(prefix: &str) -> String {
    format!("{prefix}§7Invalid coordinates or command usage!\nUsage: /tpll <latitude> <longitude> [height]\n       /tpll -p <player/@selector> <latitude> <longitude> [height]")
}

pub fn unavailable(prefix: &str) -> String {
    format!("{prefix}§cThis location is currently unavailable. Please try again later.")
}

/// Lines for a finished `/tpll` step, in order. A teleport's lines assume the move was accepted.
pub fn tpll_step_lines(step: &TpllStep, prefix: &str) -> Vec<String> {
    match step {
        TpllStep::Usage => vec![tpll_usage(prefix)],
        TpllStep::OutsideProjection => vec![format!("{prefix}§cLocation is not within projection bounds.")],
        TpllStep::OutsideTeamBounds => vec![format!("{prefix}§cYou cannot tpll to these coordinates, because this area is being worked on by another build team.")],
        TpllStep::UngeneratedChunkDenied => {
            vec![format!("{prefix}§cYou cannot tpll to these coordinates, because the chunk is not generated yet and you are not allowed to generate new chunks.")]
        }
        TpllStep::Unreachable => vec![format!("{prefix}§cThese coordinates cannot be reached on this server.")],
        TpllStep::LinkedWorldUnavailable { .. } | TpllStep::NeedsSurface { .. } => vec![unavailable(prefix)],
        TpllStep::Teleport { reported, reported_height, linked, .. } => {
            let latitude = java_double_to_string(reported.latitude);
            let longitude = java_double_to_string(reported.longitude);
            if *linked {
                vec![
                    format!("{prefix}§7Teleporting to linked world..."),
                    format!("{prefix}§7Teleported to {latitude}, {longitude}, {}.", java_double_to_string(f64::from(*reported_height))),
                ]
            } else {
                vec![format!("{prefix}§7Teleported to {latitude}, {longitude}, {reported_height}.")]
            }
        }
    }
}

/// `/where` at block position (`x`, `z`), `projection` including the terrain offset.
pub fn where_line(projection: &dyn GeographicProjection, x: f64, z: f64, prefix: &str) -> String {
    match projection.to_geo(ProjectedCoordinate::new(x, z)) {
        Ok(geo) => format!("{prefix}§7Your coordinates are:\n§8{}, {}§7.", java_double_to_string(geo.latitude), java_double_to_string(geo.longitude)),
        Err(_) => format!("{prefix}§cYou are currently outside of the world's projection and your location in the Minecraft world has no equivalent on Earth."),
    }
}

/// The map link upstream attaches to the `/where` line as a click event.
pub fn where_map_url(projection: &dyn GeographicProjection, x: f64, z: f64) -> Option<String> {
    let geo = projection.to_geo(ProjectedCoordinate::new(x, z)).ok()?;
    Some(format!("https://maps.google.com/maps?t=k&q=loc:{}+{}", java_double_to_string(geo.latitude), java_double_to_string(geo.longitude)))
}

pub fn offset_lines(config: &ServerConfig, prefix: &str) -> Vec<String> {
    let mut lines = vec![format!("{prefix}§7Offsets:"), format!("{prefix}§7 | X: §8{}", config.generator.x_offset)];
    let linked = &config.linked_worlds;
    if linked.enabled && linked.method == LinkedWorldsMethod::Levels {
        lines.extend(linked.worlds.iter().map(|world| format!("{prefix}§9 {}§7 | Y: §8{}", world.name, world.offset)));
    } else {
        lines.push(format!("{prefix}§7 | Y: §8{}", config.generator.y_offset));
    }
    lines.push(format!("{prefix}§7 | Z: §8{}", config.generator.z_offset));
    lines
}

/// `/distortion` at block position (`x`, `z`), one chat message with line breaks.
pub fn distortion_message(projection: &dyn GeographicProjection, x: f64, z: f64) -> String {
    let tissot = projection.to_geo(ProjectedCoordinate::new(x, z)).and_then(|geo| projection.tissot(geo.longitude, geo.latitude));
    match tissot {
        Ok([area, angular, max_scale, min_scale]) => format!(
            "§6Distortion Analysis\n§7Avg scale: §f{} m/m\n§7Max scale: §f{}\n§7Min scale: §f{}\n§7Angular distortion: §f± {}°\n§7Area factor: §f{}",
            java_format_fixed((max_scale + min_scale) / 2.0, 6),
            java_format_fixed(max_scale, 6),
            java_format_fixed(min_scale, 6),
            java_format_fixed(angular.to_degrees(), 4),
            java_format_fixed(area, 6),
        ),
        Err(_) => "§cYou are outside the projection bounds.".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordinates::LatLng;

    #[test]
    fn teleport_lines_follow_upstream_number_formatting() {
        let reported = LatLng { latitude: 50.0, longitude: -113.25 };
        let step = TpllStep::Teleport { world: "earth".into(), x: 0.0, y: 0.0, z: 0.0, reported, reported_height: 3000, linked: false };
        assert_eq!(tpll_step_lines(&step, "P "), vec!["P §7Teleported to 50.0, -113.25, 3000.".to_owned()]);
        let linked = TpllStep::Teleport { world: "sky".into(), x: 0.0, y: 0.0, z: 0.0, reported, reported_height: 3000, linked: true };
        assert_eq!(tpll_step_lines(&linked, "")[1], "§7Teleported to 50.0, -113.25, 3000.0.");
    }

    #[test]
    fn offsets_list_linked_levels_instead_of_y() {
        let mut config = ServerConfig::default();
        config.generator.y_offset = -5;
        assert!(offset_lines(&config, "").contains(&"§7 | Y: §8-5".to_owned()));
        config.linked_worlds.enabled = true;
        config.linked_worlds.worlds.push(crate::tpll::LinkedWorld { name: "sky".into(), offset: -2032 });
        let lines = offset_lines(&config, "");
        assert_eq!(lines[2], "§9 sky§7 | Y: §8-2032");
        assert_eq!(lines.len(), 4);
    }
}
