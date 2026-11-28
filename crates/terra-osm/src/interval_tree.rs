//! `IntervalTree` over polygon edges, queried with one x coordinate. Only the result set matters
//! upstream (callers sort it), so the structure is a simplified port: edges of zero x-extent are
//! dropped and containment is strict on both ends, exactly as `Interval.contains`.

use crate::geometry::Segment;

const NODE_SPLIT_CAPACITY: usize = 8;

#[derive(Debug, Default)]
struct Node {
    min: f64,
    max: f64,
    children: [Option<Box<Node>>; 2],
    values: Vec<Segment>,
}

#[derive(Debug)]
pub struct IntervalTree {
    root: Option<Node>,
}

impl IntervalTree {
    pub fn new(segments: Vec<Segment>) -> Self {
        let kept: Vec<Segment> = segments.into_iter().filter(|segment| segment.max_x() - segment.min_x() != 0.0).collect();
        if kept.is_empty() {
            return Self { root: None };
        }
        let min = kept.iter().map(Segment::min_x).fold(f64::INFINITY, terra_elevation::numeric::java_min);
        let max = kept.iter().map(Segment::max_x).fold(f64::NEG_INFINITY, terra_elevation::numeric::java_max);
        let mut root = Node { min, max, ..Node::default() };
        for segment in kept {
            insert(&mut root, segment, 0);
        }
        Self { root: Some(root) }
    }

    pub fn for_each_containing(&self, point: f64, callback: &mut dyn FnMut(&Segment)) {
        if let Some(root) = &self.root {
            visit(root, point, callback);
        }
    }
}

fn contains(min: f64, max: f64, point: f64) -> bool {
    min < point && max > point
}

fn insert(node: &mut Node, segment: Segment, depth: usize) {
    let middle = (node.min + node.max) * 0.5;
    if node.values.len() >= NODE_SPLIT_CAPACITY && depth < 48 {
        let child = if segment.max_x() <= middle {
            Some(0)
        } else if segment.min_x() >= middle {
            Some(1)
        } else {
            None
        };
        if let Some(index) = child {
            let (min, max) = if index == 0 { (node.min, middle) } else { (middle, node.max) };
            let child = node.children[index].get_or_insert_with(|| Box::new(Node { min, max, ..Node::default() }));
            insert(child, segment, depth + 1);
            return;
        }
    }
    node.values.push(segment);
}

fn visit(node: &Node, point: f64, callback: &mut dyn FnMut(&Segment)) {
    if !(node.min <= point && node.max >= point) {
        return;
    }
    for child in node.children.iter().flatten() {
        visit(child, point, callback);
    }
    for segment in &node.values {
        if contains(segment.min_x(), segment.max_x(), point) {
            callback(segment);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_a_linear_scan() {
        let mut segments = Vec::new();
        for index in 0..200 {
            let x0 = f64::from(index * 7 % 97) - 40.0;
            let x1 = x0 + f64::from(index % 13);
            segments.push(Segment { x0, z0: 0.0, x1, z1: 1.0 });
        }
        let tree = IntervalTree::new(segments.clone());
        for point in [-40.0, -12.5, 0.5, 3.0, 55.5, 70.0] {
            let mut found = 0;
            tree.for_each_containing(point, &mut |_| found += 1);
            let expected = segments.iter().filter(|s| s.max_x() - s.min_x() != 0.0 && contains(s.min_x(), s.max_x(), point)).count();
            assert_eq!(found, expected, "point {point}");
        }
    }
}
