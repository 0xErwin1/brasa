//! The per-module constant pool.
//!
//! Interned: inserting an equal constant returns the existing index.
//! Floats intern by bit pattern (spec: 07 — Diseño del bytecode, constant
//! pool): `0.0` and `-0.0` are distinct entries and NaN payloads are
//! preserved. `unit` and bools have dedicated push ops and never enter
//! the pool.

use std::collections::HashMap;

use crate::ConstId;

/// A compile-time constant referenced by [`crate::Op::Const`] (values)
/// or [`crate::Op::JumpIfTagNe`] (nominal tags).
#[derive(Clone, Debug, PartialEq)]
pub enum Constant {
    Int(i64),
    Float(f64),
    Str(String),
    Char(char),
}

/// Interning key: identical to [`Constant`] except floats are compared
/// by bit pattern so the pool can live in a `HashMap`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum ConstKey {
    Int(i64),
    FloatBits(u64),
    Str(String),
    Char(char),
}

impl ConstKey {
    fn of(constant: &Constant) -> ConstKey {
        match constant {
            Constant::Int(v) => ConstKey::Int(*v),
            Constant::Float(v) => ConstKey::FloatBits(v.to_bits()),
            Constant::Str(v) => ConstKey::Str(v.clone()),
            Constant::Char(v) => ConstKey::Char(*v),
        }
    }
}

/// An interned, append-only pool of [`Constant`]s.
#[derive(Debug, Default)]
pub struct ConstPool {
    items: Vec<Constant>,
    index: HashMap<ConstKey, ConstId>,
}

impl ConstPool {
    pub fn new() -> ConstPool {
        ConstPool::default()
    }

    /// Interns `constant`: returns the existing id for an equal entry,
    /// or appends and returns a fresh one.
    pub fn insert(&mut self, constant: Constant) -> ConstId {
        let key = ConstKey::of(&constant);

        if let Some(&id) = self.index.get(&key) {
            return id;
        }

        let id = ConstId(u32::try_from(self.items.len()).expect("constant pool overflow"));
        self.items.push(constant);
        self.index.insert(key, id);
        id
    }

    pub fn get(&self, id: ConstId) -> &Constant {
        &self.items[id.0 as usize]
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Entries in insertion order, paired with their ids.
    pub fn iter(&self) -> impl Iterator<Item = (ConstId, &Constant)> {
        self.items
            .iter()
            .enumerate()
            .map(|(i, c)| (ConstId(i as u32), c))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interning_round_trip() {
        let mut pool = ConstPool::new();

        let a = pool.insert(Constant::Int(42));
        let b = pool.insert(Constant::Str("hi".to_string()));
        let c = pool.insert(Constant::Int(42));
        let d = pool.insert(Constant::Char('x'));

        assert_eq!(a, c, "equal constants intern to the same id");
        assert_ne!(a, b);
        assert_eq!(pool.len(), 3);

        assert_eq!(pool.get(a), &Constant::Int(42));
        assert_eq!(pool.get(b), &Constant::Str("hi".to_string()));
        assert_eq!(pool.get(d), &Constant::Char('x'));
    }

    #[test]
    fn floats_intern_by_bit_pattern() {
        let mut pool = ConstPool::new();

        let pos = pool.insert(Constant::Float(0.0));
        let neg = pool.insert(Constant::Float(-0.0));
        let pos_again = pool.insert(Constant::Float(0.0));
        let nan = pool.insert(Constant::Float(f64::NAN));
        let nan_again = pool.insert(Constant::Float(f64::NAN));

        assert_ne!(pos, neg, "0.0 and -0.0 are distinct entries");
        assert_eq!(pos, pos_again);
        assert_eq!(nan, nan_again, "same-bits NaN interns to one entry");
        assert_eq!(pool.len(), 3);
    }
}
