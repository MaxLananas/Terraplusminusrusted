//! `DoubleCondition` and its registry entries (`equal`, `greater_than`, `less_than`, `and`, `or`, `not`).

#[derive(Debug, Clone, PartialEq)]
pub enum DoubleCondition {
    Equal(f64),
    GreaterThan(f64),
    LessThan(f64),
    And(Vec<DoubleCondition>),
    Or(Vec<DoubleCondition>),
    Not(Box<DoubleCondition>),
}

impl DoubleCondition {
    pub fn test(&self, value: f64) -> bool {
        match self {
            Self::Equal(expected) => *expected == value,
            Self::GreaterThan(limit) => value > *limit,
            Self::LessThan(limit) => value < *limit,
            Self::And(delegates) => delegates.iter().all(|delegate| delegate.test(value)),
            Self::Or(delegates) => delegates.iter().any(|delegate| delegate.test(value)),
            Self::Not(delegate) => !delegate.test(value),
        }
    }
}
