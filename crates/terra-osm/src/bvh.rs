//! `BVH.of`: the empty, singleton and quadtree bounding-volume hierarchies. The quadtree is a
//! faithful port of `QuadtreeBVH` because its traversal order decides which of two elements with
//! the same layer and id survives deduplication in `OSMBaker`.

use crate::bounds::{Bounded, Bounds2d};
use terra_elevation::numeric::{java_max, java_min};

const NODE_SPLIT_CAPACITY: usize = 8;
const MIN_LEAF_SIZE: f64 = 16.0;

#[derive(Debug)]
pub enum Bvh<T> {
    Empty,
    Singleton(T),
    Quadtree(Quadtree<T>),
}

impl<T: Bounded> Bvh<T> {
    pub fn of(mut values: Vec<T>) -> Self {
        match values.len() {
            0 => Self::Empty,
            1 => values.pop().map_or(Self::Empty, Self::Singleton),
            _ => Self::Quadtree(Quadtree::new(values)),
        }
    }

    /// `EmptyBVH` reports zero bounds.
    pub fn bounds(&self) -> Bounds2d {
        match self {
            Self::Empty => Bounds2d { min_x: 0.0, max_x: 0.0, min_z: 0.0, max_z: 0.0 },
            Self::Singleton(value) => value.bounds(),
            Self::Quadtree(tree) => tree.bounds,
        }
    }

    pub fn for_each_intersecting<'a>(&'a self, query: &Bounds2d, callback: &mut dyn FnMut(&'a T)) {
        match self {
            Self::Empty => {}
            Self::Singleton(value) => {
                if query.intersects(&value.bounds()) {
                    callback(value);
                }
            }
            Self::Quadtree(tree) => {
                if query.intersects(&tree.bounds) {
                    tree.visit(0, query, callback);
                }
            }
        }
    }

    pub fn len(&self) -> usize {
        match self {
            Self::Empty => 0,
            Self::Singleton(_) => 1,
            Self::Quadtree(tree) => tree.values.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Debug)]
struct Node {
    bounds: Bounds2d,
    children: Option<[Option<usize>; 4]>,
    values: Option<Vec<usize>>,
}

#[derive(Debug)]
pub struct Quadtree<T> {
    bounds: Bounds2d,
    values: Vec<T>,
    nodes: Vec<Node>,
}

impl<T: Bounded> Quadtree<T> {
    fn new(values: Vec<T>) -> Self {
        let mut bounds = Bounds2d { min_x: f64::INFINITY, max_x: f64::NEG_INFINITY, min_z: f64::INFINITY, max_z: f64::NEG_INFINITY };
        for value in &values {
            let b = value.bounds();
            bounds = Bounds2d { min_x: java_min(bounds.min_x, b.min_x), max_x: java_max(bounds.max_x, b.max_x), min_z: java_min(bounds.min_z, b.min_z), max_z: java_max(bounds.max_z, b.max_z) };
        }
        let value_bounds: Vec<Bounds2d> = values.iter().map(Bounded::bounds).collect();
        let mut tree = Self { bounds, values, nodes: vec![Node { bounds, children: None, values: None }] };
        for index in 0..value_bounds.len() {
            tree.insert(0, index, &value_bounds);
        }
        tree
    }

    fn insert(&mut self, node: usize, value: usize, value_bounds: &[Bounds2d]) {
        let mut node = node;
        loop {
            let current = &mut self.nodes[node];
            match &current.values {
                None => current.values = Some(Vec::with_capacity(NODE_SPLIT_CAPACITY)),
                Some(values) if values.len() == NODE_SPLIT_CAPACITY && current.children.is_none() && can_split(&current.bounds) => self.split(node, value_bounds),
                Some(_) => {}
            }
            let current = &self.nodes[node];
            if let Some(children) = current.children
                && let Some(child_index) = child_index(&current.bounds, &value_bounds[value])
            {
                let child = match children[child_index] {
                    Some(child) => child,
                    None => {
                        let bounds = child_bounds(&current.bounds, child_index);
                        self.nodes.push(Node { bounds, children: None, values: None });
                        let created = self.nodes.len() - 1;
                        if let Some(children) = self.nodes[node].children.as_mut() {
                            children[child_index] = Some(created);
                        }
                        created
                    }
                };
                node = child;
                continue;
            }
            if let Some(values) = self.nodes[node].values.as_mut() {
                values.push(value);
            }
            return;
        }
    }

    fn split(&mut self, node: usize, value_bounds: &[Bounds2d]) {
        let previous = self.nodes[node].values.replace(Vec::with_capacity(NODE_SPLIT_CAPACITY)).unwrap_or_default();
        self.nodes[node].children = Some([None; 4]);
        for value in previous {
            self.insert(node, value, value_bounds);
        }
    }

    fn visit<'a>(&'a self, node: usize, query: &Bounds2d, callback: &mut dyn FnMut(&'a T)) {
        let current = &self.nodes[node];
        if !query.intersects(&current.bounds) {
            return;
        }
        if let Some(children) = &current.children {
            for child in children.iter().flatten() {
                if query.intersects(&self.nodes[*child].bounds) {
                    self.visit(*child, query, callback);
                }
            }
        }
        if let Some(values) = &current.values {
            let all = query.contains(&current.bounds);
            for value in values {
                let value = &self.values[*value];
                if all || query.intersects(&value.bounds()) {
                    callback(value);
                }
            }
        }
    }
}

fn can_split(bounds: &Bounds2d) -> bool {
    java_min(bounds.max_x - bounds.min_x, bounds.max_z - bounds.min_z) > MIN_LEAF_SIZE
}

fn child_index(node: &Bounds2d, value: &Bounds2d) -> Option<usize> {
    let mx = (node.min_x + node.max_x) * 0.5;
    let mz = (node.min_z + node.max_z) * 0.5;
    let x = if value.max_x <= mx {
        0
    } else if value.min_x >= mx {
        1
    } else {
        return None;
    };
    let z = if value.max_z <= mz {
        0
    } else if value.min_z >= mz {
        2
    } else {
        return None;
    };
    Some(x | z)
}

fn child_bounds(node: &Bounds2d, index: usize) -> Bounds2d {
    let mx = (node.min_x + node.max_x) * 0.5;
    let mz = (node.min_z + node.max_z) * 0.5;
    Bounds2d {
        min_x: if index & 1 != 0 { mx } else { node.min_x },
        max_x: if index & 1 != 0 { node.max_x } else { mx },
        min_z: if index & 2 != 0 { mz } else { node.min_z },
        max_z: if index & 2 != 0 { node.max_z } else { mz },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq)]
    struct Item(usize, Bounds2d);

    impl Bounded for Item {
        fn bounds(&self) -> Bounds2d {
            self.1
        }
    }

    fn items() -> Vec<Item> {
        let mut items = Vec::new();
        let mut state = 0x2545_f491_4f6c_dd1du64;
        for index in 0..400 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let x = (state % 4000) as f64 - 2000.0;
            let z = ((state >> 20) % 4000) as f64 - 2000.0;
            let size = ((state >> 40) % 40) as f64;
            items.push(Item(index, Bounds2d::of(x, x + size, z, z + size * 0.5)));
        }
        items
    }

    #[test]
    fn reports_exactly_the_intersecting_values() {
        let tree = Bvh::of(items());
        let reference = items();
        for query in [Bounds2d::of(-100.0, 100.0, -50.0, 300.0), Bounds2d::of(0.0, 16.0, 0.0, 16.0), Bounds2d::of(-3000.0, 3000.0, -3000.0, 3000.0)] {
            let mut found: Vec<usize> = Vec::new();
            tree.for_each_intersecting(&query, &mut |item| found.push(item.0));
            found.sort_unstable();
            let expected: Vec<usize> = reference.iter().filter(|item| query.intersects(&item.1)).map(|item| item.0).collect();
            assert_eq!(found, expected);
        }
    }

    #[test]
    fn small_hierarchies_need_no_tree() {
        assert!(Bvh::<Item>::of(Vec::new()).is_empty());
        let single = Bvh::of(vec![Item(7, Bounds2d::of(0.0, 1.0, 0.0, 1.0))]);
        let mut hits = 0;
        single.for_each_intersecting(&Bounds2d::of(1.0, 2.0, 1.0, 2.0), &mut |_| hits += 1);
        assert_eq!(hits, 1, "touching boxes intersect");
    }
}
