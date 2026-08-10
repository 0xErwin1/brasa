//! Typed index arenas for the Brasa compiler.
//!
//! Every compiler phase that builds a tree (AST, HIR, ...) stores its nodes
//! in a [`Store<T>`] and refers to them by a `Copy` [`Id<T>`] rather than by
//! reference or `Box`. This keeps nodes contiguous, keeps IDs cheap to pass
//! around and hash, and lets later phases attach information through side
//! tables keyed by the same ID instead of mutating nodes in place.
//!
//! The `T` type parameter on [`Id<T>`] is phantom: it exists purely so that,
//! for example, an `Id<Expr>` and an `Id<Stmt>` are distinct types even
//! though both are a plain `u32` index at runtime. Mixing them up is a
//! compile error, not a runtime bug.

use core::marker::PhantomData;
use std::hash::{Hash, Hasher};

/// A typed index into a [`Store<T>`].
///
/// `Id<T>` is `Copy`, `Eq`, and `Hash` regardless of what `T` is, but two
/// `Id`s with the same numeric index and different `T` are different types
/// and cannot be substituted for one another:
///
/// ```compile_fail
/// use brasa_arena::Id;
///
/// struct A;
/// struct B;
///
/// let a: Id<A> = Id::new(0);
/// let b: Id<B> = a; // error[E0308]: mismatched types
/// ```
#[repr(transparent)]
#[derive(Debug)]
pub struct Id<T> {
    index: u32,
    _phantom: PhantomData<fn() -> T>,
}

impl<T> Default for Id<T> {
    fn default() -> Self {
        Self {
            index: 0,
            _phantom: PhantomData,
        }
    }
}

impl<T> Copy for Id<T> {}

impl<T> Clone for Id<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> PartialEq for Id<T> {
    fn eq(&self, other: &Self) -> bool {
        self.index == other.index
    }
}

impl<T> Eq for Id<T> {}

impl<T> Hash for Id<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.index.hash(state);
    }
}

impl<T> Id<T> {
    pub const fn new(index: u32) -> Self {
        Self {
            index,
            _phantom: PhantomData,
        }
    }

    pub fn index(&self) -> u32 {
        self.index
    }
}

unsafe impl<T> Send for Id<T> {}
unsafe impl<T> Sync for Id<T> {}

/// An append-only arena of `T`, indexed by [`Id<T>`].
///
/// Values are never removed or reordered, so an `Id<T>` handed out by
/// [`Store::alloc`] stays valid (and keeps pointing at the same value) for
/// the lifetime of the store.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Store<T> {
    data: Vec<T>,
}

impl<T> Store<T> {
    pub fn new() -> Self {
        Self { data: Vec::new() }
    }

    pub fn alloc(&mut self, v: T) -> Id<T> {
        let id = Id::new(self.data.len() as u32);
        self.data.push(v);
        id
    }

    /// Get a reference to the value at the given ID.
    ///
    /// # Panics
    /// Panics if the ID is out of bounds. Use `try_get()` for a
    /// non-panicking version.
    pub fn get(&self, id: &Id<T>) -> &T {
        let index = id.index() as usize;
        assert!(
            index < self.data.len(),
            "Store::get() called with invalid ID: index {} but store only has {} elements",
            index,
            self.data.len()
        );
        &self.data[index]
    }

    /// Try to get a reference to the value at the given ID.
    ///
    /// Returns `None` if the ID is out of bounds.
    pub fn try_get(&self, id: &Id<T>) -> Option<&T> {
        self.data.get(id.index() as usize)
    }

    /// Get a mutable reference to the value at the given ID.
    ///
    /// # Panics
    /// Panics if the ID is out of bounds.
    pub fn get_mut(&mut self, id: &Id<T>) -> &mut T {
        let index = id.index() as usize;
        assert!(
            index < self.data.len(),
            "Store::get_mut() called with invalid ID: index {} but store only has {} elements",
            index,
            self.data.len()
        );
        &mut self.data[index]
    }

    pub fn get_all(&self) -> &[T] {
        &self.data
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (Id<T>, &T)> {
        self.data
            .iter()
            .enumerate()
            .map(|(i, v)| (Id::new(i as u32), v))
    }
}

impl<T> IntoIterator for Store<T> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        self.data.into_iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Widget {
        name: &'static str,
    }

    #[test]
    fn alloc_and_get_round_trip() {
        let mut store = Store::new();

        let a = store.alloc(Widget { name: "a" });
        let b = store.alloc(Widget { name: "b" });

        assert_eq!(store.get(&a).name, "a");
        assert_eq!(store.get(&b).name, "b");
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn try_get_returns_none_for_out_of_bounds_id() {
        let store: Store<Widget> = Store::new();
        let bogus: Id<Widget> = Id::new(0);

        assert!(store.try_get(&bogus).is_none());
    }

    #[test]
    fn ids_are_copy_and_compare_by_index() {
        let mut store = Store::new();
        let a = store.alloc(Widget { name: "a" });
        let a_copy = a;

        assert_eq!(a, a_copy);
        assert_ne!(a, Id::<Widget>::new(a.index() + 1));
    }

    #[test]
    fn iter_yields_ids_paired_with_values_in_insertion_order() {
        let mut store = Store::new();
        let a = store.alloc(Widget { name: "a" });
        let b = store.alloc(Widget { name: "b" });

        let collected: Vec<_> = store.iter().map(|(id, w)| (id, w.name)).collect();

        assert_eq!(collected, vec![(a, "a"), (b, "b")]);
    }
}
