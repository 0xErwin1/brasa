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
//! heap budget between instructions, so a collection never
//! interrupts an instruction. Nested loops are safepoints too — a
//! builtin's callback never reaches a top-level boundary, so exempting
//! them would let one traversal hold the whole run's garbage (BRS-62).
//! At a safepoint the precise root set is the spec's contract: the
//! value stack, the global slots, and the native root stack that
//! reentrant native calls park their host-local values on.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;

use brasa_bytecode::StructId;
use brasa_runtime::table::{OrderedMap, OrderedSet};

use crate::value::{Caught, IterState, StructValue, Value};

/// Default heap budget in bytes that arms the first collection; the
/// bytes retained by each collection set the next budget via
/// [`GROWTH_FACTOR`], never dropping below this floor.
///
/// The budget is a byte figure and not an object count (BRS-100): a
/// count charges a one-element vector and a million-element one the
/// same, so a program built out of large containers could hold hundreds
/// of megabytes of garbage without ever arming the collector, while one
/// built out of small ones collected constantly. One mebibyte is the
/// floor below which collecting is not worth the marking pass on any
/// machine this language targets.
pub const DEFAULT_GC_BUDGET_BYTES: usize = 1024 * 1024;

/// Post-collection budget multiplier over the surviving live bytes.
const GROWTH_FACTOR: usize = 2;

/// Divisor applied to the last collection's live measure to get the
/// allocation the next collection must earn first. Larger collects more
/// often (less floating garbage, more marking); this is the point where
/// a large-snapshot traversal still matches the pre-BRS-62 wall clock.
///
/// The measure is the roots plus the bytes that survived, both of which
/// are live: floating garbage is deliberately excluded. Charging
/// against the whole traced set instead would make the floor grow with
/// the garbage the floor itself permitted — a feedback loop that
/// converges, but at twice the floating garbage. Bounding it by live
/// data keeps the allowance proportional to what the program actually
/// holds. The floor is recomputed only at a collection, so a
/// traversal's allowance outlives it by exactly one collection.
const MARK_AMORTIZATION: usize = 4;

/// Bytes charged for one value slot a container holds. Roots are
/// charged at the same rate, since a root is exactly one such slot.
const SLOT_BYTES: usize = size_of::<Value>();

/// Bytes charged for an arena slot before its contents: the cell itself
/// plus the allocation header and hashed index every container payload
/// carries. Approximate by construction — the point of the figure is
/// that a big container costs proportionally more than a small one, not
/// that it matches the allocator's own bookkeeping byte for byte.
const CELL_BYTES: usize = size_of::<HeapCell>();

/// The retained size charged for one arena cell. Measured from the
/// current element count, so a container grown after its allocation is
/// charged for the growth (see [`Heap::edit_vector`] and its siblings).
fn cell_bytes(cell: &HeapCell) -> usize {
    let slots = match cell {
        HeapCell::Vector(items) => items.borrow().len(),
        HeapCell::Set(items) => items.borrow().len(),
        HeapCell::Map(entries) => 2 * entries.borrow().len(),
        HeapCell::Struct(s) => s.fields.borrow().len(),
        HeapCell::Free => return 0,
    };

    CELL_BYTES + slots * SLOT_BYTES
}

/// One entry of the marking worklist.
enum Pending {
    /// An arena cell already marked, whose contents still have to be
    /// scanned.
    Cell(GcRef),
    /// A shared immutable node, held by this handle until it is scanned
    /// (see [`Heap::visit`] for why this one kind has to be owned).
    Shared(Value),
}

/// Marking scratch space, kept across collections and cleared rather
/// than reallocated (BRS-101): a heap that once needed a mark vector
/// and a visited set of a given size will need them again on the next
/// collection, and rebuilding both per collection made tracing pay for
/// the allocator on every cycle.
#[derive(Default)]
struct MarkState {
    /// One flag per arena slot, resized to the arena at each collection.
    marked: Vec<bool>,
    /// Shared immutable nodes (tuples, closures, ...) already enqueued,
    /// by address: without this, a diamond-shaped DAG of shared `Rc`
    /// structure would be re-walked once per path.
    visited: HashSet<usize>,
    pending: Vec<Pending>,
}

impl MarkState {
    /// Clears the previous collection's marks and sizes the mark vector
    /// to the arena. `clear` before `resize` is what makes every slot
    /// unmarked without dropping the buffer.
    fn begin(&mut self, slots: usize) {
        self.marked.clear();
        self.marked.resize(slots, false);
        self.visited.clear();
        self.pending.clear();
    }
}

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
    mark: MarkState,
    threshold_bytes: usize,
    /// Floor for the post-collection budget (the configured initial
    /// budget), so a mostly-dead heap re-arms promptly.
    floor_bytes: usize,
    /// Bytes retained by the live cells: exact at every collection, and
    /// tracked through allocation and container growth in between.
    live_bytes: Cell<usize>,
    /// High-water mark of [`Heap::live_bytes`], which is what says
    /// whether the budget kept the heap's footprint bounded.
    peak_bytes: Cell<usize>,
    /// Bytes allocated since the last collection, against which the
    /// next collection's marking cost is amortized (see
    /// [`Heap::mark_floor_bytes`]). Unlike [`Heap::live_bytes`] this
    /// only ever grows within a collection cycle: it measures
    /// allocation pressure, which freeing does not undo.
    since_collection_bytes: Cell<usize>,
    /// A fraction of what was live at the last collection, floored at
    /// the initial budget: the allocation the next collection must earn
    /// before it is worth paying that marking cost again.
    mark_floor_bytes: usize,
    stats: HeapStats,
}

impl Heap {
    pub(crate) fn new(budget_bytes: usize) -> Heap {
        Heap {
            cells: Vec::new(),
            free: Vec::new(),
            mark: MarkState::default(),
            threshold_bytes: budget_bytes,
            floor_bytes: budget_bytes,
            live_bytes: Cell::new(0),
            peak_bytes: Cell::new(0),
            since_collection_bytes: Cell::new(0),
            mark_floor_bytes: budget_bytes,
            stats: HeapStats::default(),
        }
    }

    // --- allocation ----------------------------------------------------

    fn alloc(&mut self, cell: HeapCell) -> GcRef {
        self.stats.allocations += 1;
        self.stats.live += 1;
        self.charge(cell_bytes(&cell));

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

    // --- mutation ------------------------------------------------------

    /// Runs `edit` against a vector cell and charges the change in its
    /// element count to the heap budget.
    ///
    /// Every length-changing mutation goes through one of these three
    /// helpers, so a container grown long after it was allocated still
    /// arms the collector. Missing one is not a soundness bug — the
    /// sweeper re-measures the whole live set at each collection — but
    /// it would let that growth go uncharged until the next collection
    /// happened for some other reason.
    ///
    /// The borrow is taken exactly the way the call sites used to take
    /// it themselves: held across `edit`, so an `edit` that reenters the
    /// same cell still panics rather than observing a half-applied
    /// mutation.
    pub(crate) fn edit_vector<R>(&self, r: GcRef, edit: impl FnOnce(&mut Vec<Value>) -> R) -> R {
        let cell = self.vector(r);
        let before = cell.borrow().len();

        let result = edit(&mut cell.borrow_mut());

        self.recharge(before, cell.borrow().len());
        result
    }

    /// [`Heap::edit_vector`] for a map cell; an entry is a key and a
    /// value, so it costs two slots.
    pub(crate) fn edit_map<R>(
        &self,
        r: GcRef,
        edit: impl FnOnce(&mut OrderedMap<Value>) -> R,
    ) -> R {
        let cell = self.map(r);
        let before = cell.borrow().len();

        let result = edit(&mut cell.borrow_mut());

        self.recharge(2 * before, 2 * cell.borrow().len());
        result
    }

    /// [`Heap::edit_vector`] for a set cell.
    pub(crate) fn edit_set<R>(
        &self,
        r: GcRef,
        edit: impl FnOnce(&mut OrderedSet<Value>) -> R,
    ) -> R {
        let cell = self.set(r);
        let before = cell.borrow().len();

        let result = edit(&mut cell.borrow_mut());

        self.recharge(before, cell.borrow().len());
        result
    }

    // --- accounting ----------------------------------------------------

    /// Charges newly retained bytes to both the live total and the
    /// allocation pressure since the last collection.
    fn charge(&self, bytes: usize) {
        let live = self.live_bytes.get() + bytes;
        self.live_bytes.set(live);
        self.peak_bytes.set(self.peak_bytes.get().max(live));
        self.since_collection_bytes
            .set(self.since_collection_bytes.get() + bytes);
    }

    /// Charges a container's change in slot count. Growth is
    /// allocation; shrinking only releases live bytes, because the
    /// pressure that armed the collector was already paid.
    fn recharge(&self, before_slots: usize, after_slots: usize) {
        if after_slots >= before_slots {
            self.charge((after_slots - before_slots) * SLOT_BYTES);
        } else {
            let released = (before_slots - after_slots) * SLOT_BYTES;
            self.live_bytes
                .set(self.live_bytes.get().saturating_sub(released));
        }
    }

    // --- collection ----------------------------------------------------

    /// Two conditions, and both are load-bearing.
    ///
    /// The live bytes arm a collection. The allocated bytes then keep it
    /// worth doing: marking costs one visit per reachable value, and the
    /// root set now includes whatever a native traversal parked
    /// (BRS-62), which is the caller's entire collection. `each` over a
    /// large vector of ints allocating one dying object per element
    /// keeps the live total near zero, so the budget alone would
    /// re-trace the whole snapshot every [`Heap::floor_bytes`] —
    /// quadratic in the receiver's length. Charging each collection
    /// against the allocation since the last one bounds the total
    /// marking work to a constant factor of the allocation that caused
    /// it (see [`MARK_AMORTIZATION`]).
    pub(crate) fn should_collect(&self) -> bool {
        self.live_bytes.get() >= self.threshold_bytes
            && self.since_collection_bytes.get() >= self.mark_floor_bytes
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
        let arena_only = (self.live_bytes.get() / MARK_AMORTIZATION).max(self.floor_bytes);
        self.mark_floor_bytes = self.mark_floor_bytes.min(arena_only);
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

    /// Bytes currently retained by the live cells.
    pub(crate) fn live_bytes(&self) -> usize {
        self.live_bytes.get()
    }

    /// High-water mark of [`Heap::live_bytes`] over the whole run. It
    /// is an upper bound rather than an exact figure: the live total
    /// only becomes exact at a collection, so between two collections it
    /// still counts cells that have already become unreachable.
    pub(crate) fn peak_bytes(&self) -> usize {
        self.peak_bytes.get()
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
        let mut root_count = 0;

        {
            let Heap { cells, mark, .. } = self;
            mark.begin(cells.len());

            for root in roots {
                root_count += 1;
                Heap::visit(mark, root);
            }

            while let Some(item) = mark.pending.pop() {
                match item {
                    Pending::Cell(r) => Heap::scan_cell(cells, mark, r),
                    Pending::Shared(node) => Heap::scan_shared(mark, &node),
                }
            }
        }

        let mut live_bytes = 0;
        let Heap {
            cells,
            free,
            mark,
            stats,
            ..
        } = self;

        for (ix, cell) in cells.iter_mut().enumerate() {
            if mark.marked[ix] {
                live_bytes += cell_bytes(cell);
            } else if !matches!(cell, HeapCell::Free) {
                *cell = HeapCell::Free;
                free.push(ix as u32);
                stats.live -= 1;
            }
        }

        self.live_bytes.set(live_bytes);
        self.stats.collections += 1;
        self.threshold_bytes = (live_bytes * GROWTH_FACTOR).max(self.floor_bytes);
        self.since_collection_bytes.set(0);
        self.mark_floor_bytes =
            ((root_count * SLOT_BYTES + live_bytes) / MARK_AMORTIZATION).max(self.floor_bytes);
    }

    /// Enqueues `value`'s target if it has not been reached yet.
    ///
    /// Scalars enqueue nothing at all, which is what keeps a vector of
    /// ints from costing one worklist entry per element. Arena cells
    /// enqueue an index and terminate re-traversal through their mark
    /// bit; every reference cycle passes through at least one arena cell
    /// (module docs), so the walk over the intervening immutable `Rc`
    /// structure always terminates.
    ///
    /// A shared immutable node is the one case that has to be cloned:
    /// its edges live inside an `Rc` the worklist must keep alive past
    /// the cell borrow it was found through. The clone happens once per
    /// distinct node rather than once per in-edge, because the
    /// de-duplication test runs first.
    fn visit(mark: &mut MarkState, value: &Value) {
        match value {
            Value::Vector(r) | Value::Map(r) | Value::Set(r) | Value::Struct(r) => {
                let slot = r.0 as usize;

                if !mark.marked[slot] {
                    mark.marked[slot] = true;
                    mark.pending.push(Pending::Cell(*r));
                }
            }
            _ => {
                if let Some(address) = shared_address(value)
                    && mark.visited.insert(address)
                {
                    mark.pending.push(Pending::Shared(value.clone()));
                }
            }
        }
    }

    /// Visits every value an arena cell holds, borrowing the cell once
    /// for the whole scan.
    fn scan_cell(cells: &[HeapCell], mark: &mut MarkState, r: GcRef) {
        match &cells[r.0 as usize] {
            HeapCell::Vector(items) => {
                for item in items.borrow().iter() {
                    Heap::visit(mark, item);
                }
            }
            HeapCell::Set(items) => {
                for item in items.borrow().iter() {
                    Heap::visit(mark, item);
                }
            }
            HeapCell::Map(entries) => {
                for (key, value) in entries.borrow().iter() {
                    Heap::visit(mark, key);
                    Heap::visit(mark, value);
                }
            }
            HeapCell::Struct(s) => {
                for field in s.fields.borrow().iter() {
                    Heap::visit(mark, field);
                }
            }
            HeapCell::Free => unreachable!("live values never reference a swept cell"),
        }
    }

    /// Visits every value a shared immutable node holds. Arm for arm
    /// with the `Some` arms of [`shared_address`].
    fn scan_shared(mark: &mut MarkState, value: &Value) {
        match value {
            Value::Tuple(items) => {
                for item in items.iter() {
                    Heap::visit(mark, item);
                }
            }
            Value::Option(Some(inner)) => Heap::visit(mark, inner),
            Value::Enum(e) => {
                for field in &e.fields {
                    Heap::visit(mark, field);
                }
            }
            // `Walk` is the one native record whose fields are arena
            // values, so unlike `Output` it has to be walked through
            // (BRS-66). It is frozen at construction, so `Rc` alone
            // would collect it — but the vectors it holds are cells.
            Value::Walk(walk) => {
                Heap::visit(mark, &walk.paths);
                Heap::visit(mark, &walk.unreadable);
            }
            Value::Closure(c) => {
                for capture in &c.captures {
                    Heap::visit(mark, capture);
                }
            }
            Value::BoundMethod(b) => Heap::visit(mark, &b.recv),
            Value::BoundBuiltin(b) => Heap::visit(mark, &b.recv),
            Value::Caught(caught) => {
                if let Caught::Error(inner) = &**caught {
                    Heap::visit(mark, inner);
                }
            }
            Value::Iter(iter) => {
                if let IterState::Items { items, .. } = &*iter.borrow() {
                    for item in items {
                        Heap::visit(mark, item);
                    }
                }
            }
            _ => unreachable!("only nodes with a shared address are enqueued"),
        }
    }
}

/// The de-duplication identity of a shared immutable node: the address
/// of the `Rc` allocation holding its edges. `None` for the scalar and
/// arena kinds, which the worklist never carries — arena cells have
/// their own mark bit, and scalars have no edges at all.
///
/// Every `Some` arm here must have a matching arm in
/// [`Heap::scan_shared`]: this decides what is enqueued, that one
/// decides what enqueueing means.
fn shared_address(value: &Value) -> Option<usize> {
    let address = match value {
        Value::Tuple(items) => Rc::as_ptr(items) as *const u8,
        Value::Option(Some(inner)) => Rc::as_ptr(inner) as *const u8,
        Value::Enum(e) => Rc::as_ptr(e) as *const u8,
        Value::Walk(walk) => Rc::as_ptr(walk) as *const u8,
        Value::Closure(c) => Rc::as_ptr(c) as *const u8,
        Value::BoundMethod(b) => Rc::as_ptr(b) as *const u8,
        Value::BoundBuiltin(b) => Rc::as_ptr(b) as *const u8,
        Value::Caught(caught) => Rc::as_ptr(caught) as *const u8,
        Value::Iter(iter) => Rc::as_ptr(iter) as *const u8,
        Value::Int(_)
        | Value::Float(_)
        | Value::Bool(_)
        | Value::Char(_)
        | Value::Unit
        | Value::Str(_)
        | Value::Range { .. }
        | Value::Option(None)
        | Value::Func(_)
        | Value::NativeError(_)
        | Value::ProcOutput(_)
        | Value::Json(_)
        | Value::Vector(_)
        | Value::Map(_)
        | Value::Set(_)
        | Value::Struct(_) => return None,
    };

    Some(address as usize)
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
        Heap::new(DEFAULT_GC_BUDGET_BYTES)
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
        heap.edit_vector(*r, |items| items.push(strukt));

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
        heap.edit_vector(*r, |items| items.push(strukt));

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
