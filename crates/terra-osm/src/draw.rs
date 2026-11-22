//! `DrawFunction`s: how a rasterised OpenStreetMap element changes one column of chunk data.

use std::fmt;

/// The chunk-data builder operations draw functions use (`CachedChunkData.Builder`).
pub trait DrawTarget {
    fn update_water_depth(&mut self, x: usize, z: usize, depth: i32);
    fn update_ocean_depth(&mut self, x: usize, z: usize, depth: i32);
    fn set_surface_block(&mut self, x: usize, z: usize, block: &BlockState);
    /// `NoTrees`: zeroes the tree-cover chance when tree cover is being generated.
    fn clear_tree_cover(&mut self, x: usize, z: usize);
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PropertyValue {
    String(String),
    Int(i32),
    Bool(bool),
}

impl fmt::Display for PropertyValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::String(value) => f.write_str(value),
            Self::Int(value) => write!(f, "{value}"),
            Self::Bool(value) => write!(f, "{value}"),
        }
    }
}

/// A TerraMinusMinus `BlockState`: a namespaced identifier plus explicitly set properties, sorted
/// by name (a later setting of the same property replaces the earlier one).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BlockState {
    pub id: String,
    pub properties: Vec<(String, PropertyValue)>,
}

impl fmt::Display for BlockState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.id)?;
        if !self.properties.is_empty() {
            let rendered: Vec<String> = self.properties.iter().map(|(name, value)| format!("{name}={value}")).collect();
            write!(f, "[{}]", rendered.join(","))?;
        }
        Ok(())
    }
}

/// `Identifier.parse`: `namespace:path`, namespace defaulting to `minecraft`.
pub fn parse_identifier(text: &str) -> Result<String, String> {
    if text.is_empty() {
        return Err("identifier cannot be empty".into());
    }
    let (namespace, path) = text.split_once(':').unwrap_or(("minecraft", text));
    let valid = |part: &str, slash: bool| !part.is_empty() && part.bytes().all(|b| matches!(b, b'_' | b'-' | b'.' | b'0'..=b'9' | b'a'..=b'z') || (slash && b == b'/'));
    if !valid(namespace, false) {
        return Err(format!("invalid identifier namespace in `{text}`"));
    }
    if !valid(path, true) {
        return Err(format!("invalid identifier path in `{text}`"));
    }
    Ok(format!("{namespace}:{path}"))
}

#[derive(Debug, Clone, PartialEq)]
pub enum DrawFunction {
    All(Vec<DrawFunction>),
    WeightAdd { delegate: Box<DrawFunction>, value: i32 },
    WeightClamp { delegate: Box<DrawFunction>, min: i32, max: i32 },
    WeightGreaterThan { delegate: Box<DrawFunction>, value: i32 },
    WeightLessThan { delegate: Box<DrawFunction>, value: i32 },
    Block(BlockState),
    NoTrees,
    Ocean,
    Water,
}

impl DrawFunction {
    pub fn draw_onto(&self, target: &mut dyn DrawTarget, x: usize, z: usize, weight: i32) {
        match self {
            Self::All(delegates) => {
                for delegate in delegates {
                    delegate.draw_onto(target, x, z, weight);
                }
            }
            Self::WeightAdd { delegate, value } => delegate.draw_onto(target, x, z, weight.wrapping_add(*value)),
            Self::WeightClamp { delegate, min, max } => delegate.draw_onto(target, x, z, weight.max(*min).min(*max)),
            Self::WeightGreaterThan { delegate, value } => {
                if weight > *value {
                    delegate.draw_onto(target, x, z, weight);
                }
            }
            Self::WeightLessThan { delegate, value } => {
                if weight < *value {
                    delegate.draw_onto(target, x, z, weight);
                }
            }
            Self::Block(state) => target.set_surface_block(x, z, state),
            Self::NoTrees => target.clear_tree_cover(x, z),
            Self::Ocean => target.update_ocean_depth(x, z, weight),
            Self::Water => target.update_water_depth(x, z, weight),
        }
    }

    /// Every block state this function can place, for registering them with the engine up front.
    pub fn collect_blocks<'a>(&'a self, out: &mut Vec<&'a BlockState>) {
        match self {
            Self::All(delegates) => delegates.iter().for_each(|delegate| delegate.collect_blocks(out)),
            Self::WeightAdd { delegate, .. } | Self::WeightClamp { delegate, .. } | Self::WeightGreaterThan { delegate, .. } | Self::WeightLessThan { delegate, .. } => {
                delegate.collect_blocks(out);
            }
            Self::Block(state) => {
                if !out.contains(&state) {
                    out.push(state);
                }
            }
            Self::NoTrees | Self::Ocean | Self::Water => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_follow_the_substitute_rules() {
        assert_eq!(parse_identifier("bricks").as_deref(), Ok("minecraft:bricks"));
        assert_eq!(parse_identifier("mod:a/b.c-d_e").as_deref(), Ok("mod:a/b.c-d_e"));
        for invalid in ["", "Bricks", ":stone", "minecraft:", "a/b:c", "minecraft:stone:x"] {
            assert!(parse_identifier(invalid).is_err(), "{invalid:?}");
        }
    }
}
