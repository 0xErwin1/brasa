//! Insertion-ordered `Map` and `Set` storage, shared by the walker and
//! the VM (BRS-30 follow-up).
//!
//! Both backends used to store a plain `Vec` and find keys with a linear
//! `value_eq` scan, which made `insert`, `get`, `has?`, `add` and the
//! `m[k]` index path O(n) and building a map O(n²). The spec closes
//! `Hashable` to `int`, `string`, `char`, `bool` and tuples of those
//! (`docs/spec/03-types.md`), so every legal key projects into owned
//! plain data and can be hashed.
//!
//! The `Vec` stays the source of truth because insertion order is
//! normative (`docs/spec/05-stdlib.md`: `entries`/`keys`/`values`/`for`
//! iterate in insertion order, literals keep the first occurrence's
//! position while the last value wins, `Set` keeps the first
//! occurrence). A side [`HashMap`] from [`HashKey`] to the position in
//! that `Vec` is pure acceleration. Nothing GC-managed lives in the
//! index — [`HashKey`] holds scalars and `Rc<str>`, never a runtime
//! value — so the VM's collector traces exactly what it traced before.
//!
//! Totality: a value that does not project (`float`, a struct, a
//! collection — none of which the checker lets through as a key) falls
//! back to the linear scan instead of panicking. The fallback is
//! container-wide: as soon as one stored key fails to project the index
//! is incomplete, so every lookup scans until that key is removed.

use std::collections::HashMap;
use std::rc::Rc;

/// An owned, hashable projection of a `Hashable` runtime value.
///
/// The variants mirror the closed `Hashable` list. Strings are keyed by
/// **content**, never by handle identity: `Rc<str>` delegates `Hash` and
/// `PartialEq` to `str`, so a VM constant-pool string interned at
/// startup and a structurally equal string built at runtime project to
/// the same key, exactly as `value_eq` requires.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum HashKey {
    Int(i64),
    Str(Rc<str>),
    Char(char),
    Bool(bool),
    Tuple(Box<[HashKey]>),
}

/// A runtime value that can be projected into a [`HashKey`].
///
/// Implementors must keep the projection in agreement with their
/// backend's `value_eq`: two values equal under `value_eq` must project
/// to equal keys, and two values that project to equal keys must be
/// equal under `value_eq`. Returning `None` is always sound — it only
/// costs the linear fallback.
pub trait HashKeyed {
    fn hash_key(&self) -> Option<HashKey>;
}

/// An insertion-ordered map with a hashed position index.
///
/// Complexity, with `n` entries and a projectable key (the only kind the
/// checker admits): [`len`](Self::len), [`get`](Self::get),
/// [`contains_key`](Self::contains_key) and [`insert`](Self::insert) are
/// O(1); [`remove`](Self::remove) is O(n) because deleting from the
/// ordered `Vec` shifts the positions the index stores; iteration is
/// O(n). With a non-projectable key every lookup degrades to the O(n)
/// scan the previous implementation always paid.
#[derive(Debug, Clone)]
pub struct OrderedMap<V> {
    entries: Vec<(V, V)>,
    index: HashMap<HashKey, usize>,
    /// How many stored keys failed to project. While this is non-zero
    /// the index cannot answer a lookup on its own.
    unprojectable: usize,
}

impl<V> Default for OrderedMap<V> {
    fn default() -> Self {
        OrderedMap {
            entries: Vec::new(),
            index: HashMap::new(),
            unprojectable: 0,
        }
    }
}

impl<V> OrderedMap<V> {
    pub fn new() -> OrderedMap<V> {
        OrderedMap::default()
    }

    pub fn with_capacity(capacity: usize) -> OrderedMap<V> {
        OrderedMap {
            entries: Vec::with_capacity(capacity),
            index: HashMap::with_capacity(capacity),
            unprojectable: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The entries in insertion order — the value's observable sequence.
    pub fn entries(&self) -> &[(V, V)] {
        &self.entries
    }

    pub fn iter(&self) -> std::slice::Iter<'_, (V, V)> {
        self.entries.iter()
    }
}

impl<V: HashKeyed> OrderedMap<V> {
    /// Builds a map from entries the caller guarantees have pairwise
    /// distinct keys (a `HashMap`/`BTreeMap` drained into a `Vec`, for
    /// instance). Duplicates would leave the index pointing at the last
    /// occurrence while iteration still yields both, so callers that
    /// cannot guarantee distinctness must use [`insert`](Self::insert).
    pub fn from_distinct_entries(entries: Vec<(V, V)>) -> OrderedMap<V> {
        let mut index = HashMap::with_capacity(entries.len());
        let mut unprojectable = 0;

        for (position, (key, _)) in entries.iter().enumerate() {
            match key.hash_key() {
                Some(hashed) => {
                    index.insert(hashed, position);
                }
                None => unprojectable += 1,
            }
        }

        OrderedMap {
            entries,
            index,
            unprojectable,
        }
    }

    /// The position of `key`, through the index when it is complete and
    /// the key projects, otherwise through the linear `eq` scan.
    fn position(&self, key: &V, eq: impl Fn(&V, &V) -> bool) -> Option<usize> {
        if self.unprojectable == 0
            && let Some(hashed) = key.hash_key()
        {
            return self.index.get(&hashed).copied();
        }

        self.entries.iter().position(|(k, _)| eq(k, key))
    }

    pub fn get(&self, key: &V, eq: impl Fn(&V, &V) -> bool) -> Option<&V> {
        self.position(key, eq).map(|ix| &self.entries[ix].1)
    }

    pub fn contains_key(&self, key: &V, eq: impl Fn(&V, &V) -> bool) -> bool {
        self.position(key, eq).is_some()
    }

    /// Upsert: an existing key keeps its position and its original key
    /// value, only the value is replaced.
    pub fn insert(&mut self, key: V, value: V, eq: impl Fn(&V, &V) -> bool) {
        if let Some(ix) = self.position(&key, eq) {
            self.entries[ix].1 = value;
            return;
        }

        let position = self.entries.len();
        match key.hash_key() {
            Some(hashed) => {
                self.index.insert(hashed, position);
            }
            None => self.unprojectable += 1,
        }

        self.entries.push((key, value));
    }

    /// Removes `key` and yields its value. Repairing the index after the
    /// `Vec` shifts is what keeps this O(n); removal is the rare
    /// operation, so the ordered `Vec` stays exact instead of growing
    /// tombstones that iteration would have to skip.
    pub fn remove(&mut self, key: &V, eq: impl Fn(&V, &V) -> bool) -> Option<V> {
        let ix = self.position(key, eq)?;
        let (removed_key, removed_value) = self.entries.remove(ix);

        match removed_key.hash_key() {
            Some(hashed) => {
                self.index.remove(&hashed);
            }
            None => self.unprojectable -= 1,
        }

        for position in self.index.values_mut() {
            if *position > ix {
                *position -= 1;
            }
        }

        Some(removed_value)
    }
}

/// An insertion-ordered set with a hashed position index; the same
/// design and the same complexities as [`OrderedMap`].
#[derive(Debug, Clone)]
pub struct OrderedSet<V> {
    items: Vec<V>,
    index: HashMap<HashKey, usize>,
    unprojectable: usize,
}

impl<V> Default for OrderedSet<V> {
    fn default() -> Self {
        OrderedSet {
            items: Vec::new(),
            index: HashMap::new(),
            unprojectable: 0,
        }
    }
}

impl<V> OrderedSet<V> {
    pub fn new() -> OrderedSet<V> {
        OrderedSet::default()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The elements in insertion order — the value's observable
    /// sequence.
    pub fn items(&self) -> &[V] {
        &self.items
    }

    pub fn iter(&self) -> std::slice::Iter<'_, V> {
        self.items.iter()
    }
}

impl<V: HashKeyed> OrderedSet<V> {
    /// Builds a set from elements the caller guarantees are pairwise
    /// distinct; see [`OrderedMap::from_distinct_entries`].
    pub fn from_distinct_items(items: Vec<V>) -> OrderedSet<V> {
        let mut index = HashMap::with_capacity(items.len());
        let mut unprojectable = 0;

        for (position, item) in items.iter().enumerate() {
            match item.hash_key() {
                Some(hashed) => {
                    index.insert(hashed, position);
                }
                None => unprojectable += 1,
            }
        }

        OrderedSet {
            items,
            index,
            unprojectable,
        }
    }

    fn position(&self, value: &V, eq: impl Fn(&V, &V) -> bool) -> Option<usize> {
        if self.unprojectable == 0
            && let Some(hashed) = value.hash_key()
        {
            return self.index.get(&hashed).copied();
        }

        self.items.iter().position(|item| eq(item, value))
    }

    pub fn contains(&self, value: &V, eq: impl Fn(&V, &V) -> bool) -> bool {
        self.position(value, eq).is_some()
    }

    /// Adds `value` unless an equal element is already present (the
    /// first occurrence is the one kept).
    pub fn add(&mut self, value: V, eq: impl Fn(&V, &V) -> bool) {
        if self.position(&value, eq).is_some() {
            return;
        }

        let position = self.items.len();
        match value.hash_key() {
            Some(hashed) => {
                self.index.insert(hashed, position);
            }
            None => self.unprojectable += 1,
        }

        self.items.push(value);
    }

    /// Removes `value`, reporting whether it was present. O(n) for the
    /// same reason as [`OrderedMap::remove`].
    pub fn remove(&mut self, value: &V, eq: impl Fn(&V, &V) -> bool) -> bool {
        let Some(ix) = self.position(value, eq) else {
            return false;
        };

        let removed = self.items.remove(ix);
        match removed.hash_key() {
            Some(hashed) => {
                self.index.remove(&hashed);
            }
            None => self.unprojectable -= 1,
        }

        for position in self.index.values_mut() {
            if *position > ix {
                *position -= 1;
            }
        }

        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in value: `Opaque` deliberately fails to project, which
    /// is the only way to reach the linear fallback from a test (the
    /// checker keeps non-`Hashable` keys out of real programs).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Probe {
        Int(i64),
        Text(&'static str),
        Opaque(u8),
    }

    impl HashKeyed for Probe {
        fn hash_key(&self) -> Option<HashKey> {
            match self {
                Probe::Int(i) => Some(HashKey::Int(*i)),
                Probe::Text(s) => Some(HashKey::Str(Rc::from(*s))),
                Probe::Opaque(_) => None,
            }
        }
    }

    fn eq(a: &Probe, b: &Probe) -> bool {
        a == b
    }

    fn keys(map: &OrderedMap<Probe>) -> Vec<Probe> {
        map.iter().map(|(k, _)| *k).collect()
    }

    #[test]
    fn insert_appends_new_keys_and_upserts_in_place() {
        let mut map = OrderedMap::new();
        map.insert(Probe::Int(1), Probe::Int(10), eq);
        map.insert(Probe::Int(2), Probe::Int(20), eq);
        map.insert(Probe::Int(1), Probe::Int(99), eq);

        assert_eq!(keys(&map), vec![Probe::Int(1), Probe::Int(2)]);
        assert_eq!(map.get(&Probe::Int(1), eq), Some(&Probe::Int(99)));
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn remove_repairs_every_shifted_position() {
        let mut map = OrderedMap::new();
        for i in 0..8 {
            map.insert(Probe::Int(i), Probe::Int(i * 10), eq);
        }

        assert_eq!(map.remove(&Probe::Int(3), eq), Some(Probe::Int(30)));
        assert_eq!(map.remove(&Probe::Int(3), eq), None);

        for i in (0..8).filter(|i| *i != 3) {
            assert_eq!(
                map.get(&Probe::Int(i), eq),
                Some(&Probe::Int(i * 10)),
                "key {i} lost its value after the shift"
            );
        }
        assert_eq!(
            keys(&map),
            vec![0, 1, 2, 4, 5, 6, 7]
                .into_iter()
                .map(Probe::Int)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_non_projectable_key_falls_back_to_the_linear_scan() {
        let mut map = OrderedMap::new();
        map.insert(Probe::Opaque(1), Probe::Int(1), eq);
        map.insert(Probe::Int(2), Probe::Int(2), eq);
        map.insert(Probe::Opaque(1), Probe::Int(3), eq);

        assert_eq!(map.len(), 2);
        assert_eq!(map.get(&Probe::Opaque(1), eq), Some(&Probe::Int(3)));
        assert_eq!(map.get(&Probe::Int(2), eq), Some(&Probe::Int(2)));
        assert!(!map.contains_key(&Probe::Opaque(9), eq));

        // Dropping the unprojectable key re-arms the index.
        assert_eq!(map.remove(&Probe::Opaque(1), eq), Some(Probe::Int(3)));
        assert_eq!(map.get(&Probe::Int(2), eq), Some(&Probe::Int(2)));
    }

    #[test]
    fn string_keys_are_content_addressed() {
        let mut map = OrderedMap::new();
        map.insert(Probe::Text("alpha"), Probe::Int(1), eq);
        map.insert(Probe::Text("alpha"), Probe::Int(2), eq);

        assert_eq!(map.len(), 1);
        assert_eq!(map.get(&Probe::Text("alpha"), eq), Some(&Probe::Int(2)));
    }

    #[test]
    fn set_keeps_the_first_occurrence_and_repairs_on_removal() {
        let mut set = OrderedSet::new();
        for i in [3, 1, 3, 2, 1] {
            set.add(Probe::Int(i), eq);
        }

        assert_eq!(
            set.items(),
            [Probe::Int(3), Probe::Int(1), Probe::Int(2)].as_slice()
        );

        assert!(set.remove(&Probe::Int(3), eq));
        assert!(!set.remove(&Probe::Int(3), eq));
        assert!(set.contains(&Probe::Int(1), eq));
        assert!(set.contains(&Probe::Int(2), eq));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn from_distinct_entries_indexes_every_key() {
        let map = OrderedMap::from_distinct_entries(vec![
            (Probe::Text("a"), Probe::Int(1)),
            (Probe::Text("b"), Probe::Int(2)),
        ]);

        assert_eq!(map.get(&Probe::Text("b"), eq), Some(&Probe::Int(2)));
        assert!(!map.contains_key(&Probe::Text("c"), eq));
    }
}
