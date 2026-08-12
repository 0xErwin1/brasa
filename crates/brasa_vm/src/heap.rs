//! The GC heap (BRS-29): a mark-and-sweep arena for the cycle-capable
//! heap kinds, plus the string interner.
//!
//! Why mark & sweep at all: reference cycles ARE constructible in the
//! language today. The checker accepts recursive struct types
//! (`struct S` with a `Vector<S>` field typechecks), and every mutable
//! container is a shared reference (`docs/spec/03-types.md`), so
//! `s.v.push(s)` closes a cycle that plain `Rc` can never reclaim. The
//! same holds for closures stored inside a container they capture.
//!
//! Why only four kinds live in the arena: a cycle needs an object to
//! gain a reference *after* it was created, and the only post-creation
//! mutations in the language are struct field assignment, vector and
//! map index assignment, and the mutating builtins on `Vector`, `Map`,
//! and `Set`. Every other heap kind (strings, tuples, enum payloads,
//! closures, bound methods, `Option` payloads, caught signals,
//! iterators) is frozen at construction, so it can sit *on* a cycle
//! but never *close* one — for those, `Rc` alone is already a precise
//! collector, and the arena's tracer walks through them to reach the
//! arena cells they reference. Sweeping an unreachable arena cell drops
//! its contents, which breaks the cycle and lets `Rc` reclaim the
//! immutable remainder.
//!
//! Collection runs at safepoints only: the dispatch loop checks the
//! allocation threshold between instructions, so a collection never
//! interrupts an instruction. Nested loops are safepoints too — a
//! builtin's callback never reaches a top-level boundary, so exempting
//! them would let one traversal hold the whole run's garbage (BRS-62).
//! At a safepoint the precise root set is the spec's contract: the
//! value stack, the global slots, and the native root stack that
//! reentrant native calls park their host-local values on.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use brasa_bytecode::StructId;
use brasa_interp::table::{OrderedMap, OrderedSet};

use crate::value::{Caught, IterState, StructValue, Value};

/// Default allocation threshold that arms the first collection; the
/// live count after each collection sets the next threshold via
/// [`GROWTH_FACTOR`], never dropping below this floor.
pub const DEFAULT_GC_THRESHOLD: usize = 1024;

/// Post-collection threshold multiplier over the surviving live count.
const GROWTH_FACTOR: usize = 2;

/// Divisor applied to the last collection's live measure to get the
/// allocation the next collection must earn first. Larger collects more
/// often (less floating garbage, more marking); this is the point where
/// a large-snapshot traversal still matches the pre-BRS-62 wall clock.
///
/// The measure is the root count plus the cells that survived, both of
/// which are live: floating garbage is deliberately excluded. Charging
/// against the whole traced set instead would make the floor grow with
/// the garbage the floor itself permitted — a feedback loop that
/// converges, but at twice the floating garbage. Bounding it by live
/// data keeps the allowance proportional to what the program actually
/// holds. The floor is recomputed only at a collection, so a
/// traversal's allowance outlives it by exactly one collection.
const MARK_AMORTIZATION: usize = 4;

/// An opaque index into the VM heap's arena. Copyable and trivially
/// droppable: reclamation is the collector's job, never `Drop`'s.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GcRef(u32);

/// One arena slot: the payload of a cycle-capable heap kind, or a
/// reusable hole left by the sweeper.
pub(crate) enum HeapCell {
    Vector(RefCell<Vec<Value>>),
    Map(RefCell<OrderedMap<Value>>),
    Set(RefCell<OrderedSet<Value>>),
    Struct(StructValue),
    Free,
}

/// Allocation accounting for BRS-30's benchmarks: totals survive
/// collections, `live` is the current cell count.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct HeapStats {
    pub(crate) allocations: u64,
    pub(crate) collections: u64,
    pub(crate) live: usize,
}

pub(crate) struct Heap {
    cells: Vec<HeapCell>,
    free: Vec<u32>,
    threshold: usize,
    /// Floor for the post-collection threshold (the configured initial
    /// threshold), so a mostly-dead heap re-arms promptly.
    initial_threshold: usize,
    /// Allocations since the last collection, against which the next
    /// collection's marking cost is amortized (see [`Heap::mark_floor`]).
    since_collection: usize,
    /// A fraction of what was live at the last collection, floored at
    /// the initial threshold: the allocation the next collection must
    /// earn before it is worth paying that marking cost again.
    mark_floor: usize,
    stats: HeapStats,
}

impl Heap {
    pub(crate) fn new(threshold: usize) -> Heap {
        Heap {
            cells: Vec::new(),
            free: Vec::new(),
            threshold,
            initial_threshold: threshold,
            since_collection: 0,
            mark_floor: threshold,
            stats: HeapStats::default(),
        }
    }

    // --- allocation ----------------------------------------------------

    fn alloc(&mut self, cell: HeapCell) -> GcRef {
        self.stats.allocations += 1;
        self.stats.live += 1;
        self.since_collection += 1;

        match self.free.pop() {
            Some(ix) => {
                self.cells[ix as usize] = cell;
                GcRef(ix)
            }
            None => {
                let ix = u32::try_from(self.cells.len()).expect("VM heap overflow");
                self.cells.push(cell);
                GcRef(ix)
            }
        }
    }

    pub(crate) fn alloc_vector(&mut self, items: Vec<Value>) -> Value {
        Value::Vector(self.alloc(HeapCell::Vector(RefCell::new(items))))
    }

    pub(crate) fn alloc_map(&mut self, entries: OrderedMap<Value>) -> Value {
        Value::Map(self.alloc(HeapCell::Map(RefCell::new(entries))))
    }

    pub(crate) fn alloc_set(&mut self, items: OrderedSet<Value>) -> Value {
        Value::Set(self.alloc(HeapCell::Set(RefCell::new(items))))
    }

    pub(crate) fn alloc_struct(&mut self, shape: StructId, fields: Vec<Value>) -> Value {
        Value::Struct(self.alloc(HeapCell::Struct(StructValue {
            shape,
            fields: RefCell::new(fields),
        })))
    }

    // --- access --------------------------------------------------------

    pub(crate) fn vector(&self, r: GcRef) -> &RefCell<Vec<Value>> {
        match &self.cells[r.0 as usize] {
            HeapCell::Vector(items) => items,
            _ => unreachable!("GcRef kind mismatch: expected a vector"),
        }
    }

    pub(crate) fn map(&self, r: GcRef) -> &RefCell<OrderedMap<Value>> {
        match &self.cells[r.0 as usize] {
            HeapCell::Map(entries) => entries,
            _ => unreachable!("GcRef kind mismatch: expected a map"),
        }
    }

    pub(crate) fn set(&self, r: GcRef) -> &RefCell<OrderedSet<Value>> {
        match &self.cells[r.0 as usize] {
            HeapCell::Set(items) => items,
            _ => unreachable!("GcRef kind mismatch: expected a set"),
        }
    }

    pub(crate) fn struct_value(&self, r: GcRef) -> &StructValue {
        match &self.cells[r.0 as usize] {
            HeapCell::Struct(s) => s,
            _ => unreachable!("GcRef kind mismatch: expected a struct"),
        }
    }

    // --- collection ----------------------------------------------------

    /// Two conditions, and both are load-bearing.
    ///
    /// The live count arms a collection, as it always has. The
    /// allocation count then keeps it worth doing: marking costs one
    /// visit per reachable value, and the root set now includes
    /// whatever a native traversal parked (BRS-62), which is the
    /// caller's entire collection. `each` over a large vector of ints
    /// allocating one dying object per element keeps `live` near zero,
    /// so the live threshold alone would re-trace the whole snapshot
    /// every `initial_threshold` allocations — quadratic in the
    /// receiver's length. Charging each collection against the
    /// allocation since the last one bounds the total marking work to a
    /// constant factor of the allocation that caused it (see
    /// [`MARK_AMORTIZATION`]).
    pub(crate) fn should_collect(&self) -> bool {
        self.stats.live >= self.threshold && self.since_collection >= self.mark_floor
    }

    pub(crate) fn stats(&self) -> HeapStats {
        self.stats
    }

    /// Lowers the marking allowance to what the arena alone justifies,
    /// for when the roots it was measured against are gone
    /// (`Vm::unroot`). Without it the floor would stay in force until
    /// the next collection, which a low-allocation phase may not reach
    /// for an unbounded time.
    ///
    /// It only ever lowers. The arena's own measure is taken now, not
    /// at the last collection, so a traversal whose callback left many
    /// survivors behind would otherwise raise the floor here and delay
    /// the next collection — the opposite of the point.
    pub(crate) fn relax_mark_floor(&mut self) {
        let arena_only = (self.stats.live / MARK_AMORTIZATION).max(self.initial_threshold);
        self.mark_floor = self.mark_floor.min(arena_only);
    }

    /// Arena slots ever allocated, which is exactly the high-water mark
    /// of simultaneously live objects: [`Heap::alloc`] grows `cells`
    /// only when the free list is empty, and an empty free list means
    /// every slot is live. It is the number that says whether
    /// collection kept up with allocation, which `live` at the end of a
    /// run cannot.
    pub(crate) fn arena_slots(&self) -> usize {
        self.cells.len()
    }

    /// Mark from `roots`, then sweep every unmarked arena cell. Must
    /// only run at a safepoint: no outstanding `RefCell` borrows, and
    /// every live value reachable from the given roots.
    ///
    /// The borrow half binds native code too, now that nested dispatch
    /// loops collect (BRS-62): a `Ref` or `RefMut` held across a call
    /// that reenters compiled code would be mutated by the sweeper. The
    /// borrow checker already enforces it — the cell accessors return a
    /// `&RefCell<..>` borrowed from `&self`, and every reentrant call
    /// needs `&mut self`, so a live `Ref` and a reentry cannot coexist.
    /// What that argument does NOT cover is an accessor handing out a
    /// guard whose lifetime is not tied to the heap borrow (a `Ref`
    /// cloned out of an `Rc<RefCell<..>>`); none exists today, and
    /// adding one would put this precondition back in human hands.
    pub(crate) fn collect<'r>(&mut self, roots: impl Iterator<Item = &'r Value>) {
        let mut marked = vec![false; self.cells.len()];
        let mut pending: Vec<Value> = roots.cloned().collect();
        let root_count = pending.len();

        // Shared immutable nodes (tuples, closures, ...) are traversed
        // once by address: without this, a diamond-shaped DAG of shared
        // `Rc` structure would be re-walked once per path.
        let mut visited: HashSet<usize> = HashSet::new();

        while let Some(value) = pending.pop() {
            self.trace(&value, &mut marked, &mut pending, &mut visited);
        }

        for (ix, cell) in self.cells.iter_mut().enumerate() {
            if !marked[ix] && !matches!(cell, HeapCell::Free) {
                *cell = HeapCell::Free;
                self.free.push(ix as u32);
                self.stats.live -= 1;
            }
        }

        self.stats.collections += 1;
        self.threshold = (self.stats.live * GROWTH_FACTOR).max(self.initial_threshold);
        self.since_collection = 0;
        self.mark_floor =
            ((root_count + self.stats.live) / MARK_AMORTIZATION).max(self.initial_threshold);
    }

    /// Enqueues everything `value` references. Arena cells terminate
    /// re-traversal via their mark bit — every reference cycle passes
    /// through at least one arena cell (module docs), so the walk over
    /// the intervening immutable `Rc` structure always terminates.
    fn trace(
        &self,
        value: &Value,
        marked: &mut [bool],
        pending: &mut Vec<Value>,
        visited: &mut HashSet<usize>,
    ) {
        let mut mark = |r: GcRef, pending: &mut Vec<Value>| {
            if !marked[r.0 as usize] {
                marked[r.0 as usize] = true;
                match &self.cells[r.0 as usize] {
                    HeapCell::Vector(items) => {
                        pending.extend(items.borrow().iter().cloned());
                    }
                    HeapCell::Set(items) => {
                        pending.extend(items.borrow().iter().cloned());
                    }
                    HeapCell::Map(entries) => {
                        for (k, v) in entries.borrow().iter() {
                            pending.push(k.clone());
                            pending.push(v.clone());
                        }
                    }
                    HeapCell::Struct(s) => {
                        pending.extend(s.fields.borrow().iter().cloned());
                    }
                    HeapCell::Free => unreachable!("live values never reference a swept cell"),
                }
            }
        };

        match value {
            Value::Vector(r) | Value::Map(r) | Value::Set(r) | Value::Struct(r) => {
                mark(*r, pending);
            }
            Value::Tuple(items) => {
                if visited.insert(Rc::as_ptr(items) as *const u8 as usize) {
                    pending.extend(items.iter().cloned());
                }
            }
            Value::Option(Some(inner)) => {
                if visited.insert(Rc::as_ptr(inner) as *const u8 as usize) {
                    pending.push((**inner).clone());
                }
            }
            Value::Enum(e) => {
                if visited.insert(Rc::as_ptr(e) as *const u8 as usize) {
                    pending.extend(e.fields.iter().cloned());
                }
            }
            // `Walk` is the one native record whose fields are arena
            // values, so unlike `Output` it has to be walked through
            // (BRS-66). It is frozen at construction, so `Rc` alone
            // would collect it — but the vectors it holds are cells.
            Value::Walk(walk) => {
                if visited.insert(Rc::as_ptr(walk) as *const u8 as usize) {
                    pending.push(walk.paths.clone());
                    pending.push(walk.unreadable.clone());
                }
            }
            Value::Closure(c) => {
                if visited.insert(Rc::as_ptr(c) as *const u8 as usize) {
                    pending.extend(c.captures.iter().cloned());
                }
            }
            Value::BoundMethod(b) => {
                if visited.insert(Rc::as_ptr(b) as *const u8 as usize) {
                    pending.push(b.recv.clone());
                }
            }
            Value::BoundBuiltin(b) => {
                if visited.insert(Rc::as_ptr(b) as *const u8 as usize) {
                    pending.push(b.recv.clone());
                }
            }
            Value::Caught(caught) => {
                if visited.insert(Rc::as_ptr(caught) as *const u8 as usize)
                    && let Caught::Error(inner) = &**caught
                {
                    pending.push(inner.clone());
                }
            }
            Value::Iter(iter) => {
                if visited.insert(Rc::as_ptr(iter) as *const u8 as usize)
                    && let IterState::Items { items, .. } = &*iter.borrow()
                {
                    pending.extend(items.iter().cloned());
                }
            }
            Value::Int(_)
            | Value::Float(_)
            | Value::Bool(_)
            | Value::Char(_)
            | Value::Unit
            | Value::Str(_)
            | Value::Range { .. }
            | Value::Option(None)
            | Value::Func(_)
            | Value::NativeError { .. }
            | Value::ProcOutput(_)
            | Value::Json(_) => {}
        }
    }
}

// --- string interning --------------------------------------------------

/// Content-keyed string interner: identical string contents share one
/// heap allocation. Strings stay behind `Rc` (they hold no `Value`
/// edges, so they can never be part of a cycle), which makes interning
/// invisible to the collector and to structural equality.
///
/// What is interned: the module constant pool's strings, once at VM
/// startup — every `const` push then reuses one shared allocation.
/// Not interned: runtime-computed strings (`concat`, `toString`,
/// string builtins); hashing every produced string would cost more
/// than the sharing wins. BRS-30 measures this split.
#[derive(Default)]
pub(crate) struct Interner {
    table: HashSet<Rc<str>>,
    hits: u64,
}

impl Interner {
    pub(crate) fn intern(&mut self, content: &str) -> Rc<str> {
        if let Some(existing) = self.table.get(content) {
            self.hits += 1;
            return existing.clone();
        }

        let shared: Rc<str> = Rc::from(content);
        self.table.insert(shared.clone());
        shared
    }

    /// Distinct interned strings, for BRS-30's measurements.
    pub(crate) fn len(&self) -> usize {
        self.table.len()
    }

    /// Lookups served by an existing allocation, for BRS-30.
    pub(crate) fn hits(&self) -> u64 {
        self.hits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn heap() -> Heap {
        Heap::new(DEFAULT_GC_THRESHOLD)
    }

    #[test]
    fn collect_without_roots_reclaims_everything() {
        let mut heap = heap();
        heap.alloc_vector(vec![Value::Int(1)]);
        heap.alloc_map(OrderedMap::from_distinct_entries(vec![(
            Value::Int(1),
            Value::Int(2),
        )]));
        heap.alloc_set(OrderedSet::from_distinct_items(vec![Value::Int(3)]));
        assert_eq!(heap.stats().live, 3);
        assert_eq!(heap.stats().allocations, 3);

        heap.collect(std::iter::empty());

        assert_eq!(heap.stats().live, 0);
        assert_eq!(heap.stats().allocations, 3);
        assert_eq!(heap.stats().collections, 1);
    }

    #[test]
    fn rooted_values_survive_and_stay_readable() {
        let mut heap = heap();
        let root = heap.alloc_vector(vec![Value::Int(7)]);
        heap.alloc_vector(vec![Value::Int(0)]);

        heap.collect(std::iter::once(&root));

        assert_eq!(heap.stats().live, 1);
        let Value::Vector(r) = root else {
            unreachable!()
        };
        assert!(matches!(heap.vector(r).borrow()[0], Value::Int(7)));
    }

    #[test]
    fn unreachable_cycle_is_collected() {
        let mut heap = heap();

        let vector = heap.alloc_vector(Vec::new());
        let strukt = heap.alloc_struct(StructId(0), vec![vector.clone()]);
        let Value::Vector(r) = &vector else {
            unreachable!()
        };
        heap.vector(*r).borrow_mut().push(strukt);

        heap.collect(std::iter::empty());

        assert_eq!(heap.stats().live, 0);
    }

    #[test]
    fn rooted_cycle_survives() {
        let mut heap = heap();

        let vector = heap.alloc_vector(Vec::new());
        let strukt = heap.alloc_struct(StructId(0), vec![vector.clone()]);
        let Value::Vector(r) = &vector else {
            unreachable!()
        };
        heap.vector(*r).borrow_mut().push(strukt);

        heap.collect(std::iter::once(&vector));

        assert_eq!(heap.stats().live, 2);
    }

    #[test]
    fn tracing_walks_through_immutable_rc_structure() {
        let mut heap = heap();

        let inner = heap.alloc_vector(vec![Value::Int(1)]);
        let tuple = Value::Tuple(Rc::from(vec![inner.clone(), inner]));
        let root = Value::some(tuple);

        heap.collect(std::iter::once(&root));

        assert_eq!(heap.stats().live, 1);
    }

    #[test]
    fn swept_slots_are_reused() {
        let mut heap = heap();
        heap.alloc_vector(Vec::new());
        heap.collect(std::iter::empty());

        let recycled = heap.alloc_vector(Vec::new());

        assert!(matches!(recycled, Value::Vector(GcRef(0))));
        assert_eq!(heap.stats().live, 1);
        assert_eq!(heap.stats().allocations, 2);
    }

    #[test]
    fn interner_shares_identical_content() {
        let mut interner = Interner::default();
        let a = interner.intern("hello");
        let b = interner.intern("hello");
        let c = interner.intern("world");

        assert!(Rc::ptr_eq(&a, &b));
        assert!(!Rc::ptr_eq(&a, &c));
        assert_eq!(interner.len(), 2);
        assert_eq!(interner.hits(), 1);
    }
}
