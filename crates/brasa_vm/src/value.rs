//! VM runtime values and their structural operations.
//!
//! Mirrors the value semantics of spec: 03 — Sistema de tipos over
//! the bytecode module's shape indices: inline scalars, ranges, and
//! `FuncId`s; heap kinds behind the handle aliases below. Structural
//! equality and primitive ordering follow spec: 03 — Sistema de tipos
//! (structural equality, and ordering only on the primitives that have
//! it); the conformance corpus pins the observable behavior.

use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::HashSet;
use std::rc::Rc;

use brasa_bytecode::{BuiltinId, EnumId, FuncId, StructId};
use brasa_runtime::table::{HashKey, HashKeyed};

use crate::heap::{GcRef, Heap};

/// Shared immutable heap handle.
///
/// `Rc` is a precise collector for every kind behind these aliases:
/// their edges are fixed at construction, so they can never GAIN a
/// reference and never close a reference cycle (`crate::heap` module
/// docs prove why). The five mutable, cycle-capable kinds — `Vector`,
/// `Map`, `Set`, `Struct`, and the binding cell — instead hold a
/// [`GcRef`] into the mark-and-sweep arena.
pub type Handle<T> = Rc<T>;

/// Shared mutable handle for the internal iterator state; see
/// [`Handle`] (iterators never gain references after creation).
pub type MutHandle<T> = Rc<RefCell<T>>;

/// A Brasa runtime value in the VM.
#[derive(Debug, Clone)]
pub enum Value {
    Int(i64),
    Float(f64),
    Bool(bool),
    Char(char),
    Unit,
    Str(Handle<str>),
    /// Lazy int range: two ints + an inclusive flag, never materialized.
    Range {
        lo: i64,
        hi: i64,
        inclusive: bool,
    },
    Tuple(Handle<[Value]>),
    Vector(GcRef),
    /// Insertion-ordered map with a hashed position index, as in the
    /// hashed tables (`brasa_runtime::table`).
    Map(GcRef),
    /// Insertion-ordered set, same representation rationale as `Map`.
    Set(GcRef),
    Option(Option<Handle<Value>>),
    Struct(GcRef),
    Enum(Handle<EnumValue>),
    /// A function-table entry used as a value.
    Func(FuncId),
    Closure(Handle<ClosureValue>),
    /// A struct method accessed without calling it (`p.dist` as a value).
    BoundMethod(Handle<BoundMethod>),
    /// A builtin method accessed without calling it (`v.push` as a value).
    BoundBuiltin(Handle<BoundBuiltin>),
    /// A stdlib-native error: canonical qualified name + message
    /// (`brasa_resolver::NATIVE_ERRORS`). Boxed because it is the
    /// widest payload any variant carries and the rarest at runtime:
    /// inline it would set the size of every stack slot.
    NativeError(Handle<NativeErrorValue>),
    /// INTERNAL: the caught-signal value handler dispatch operates on
    /// (spec: 07 — Diseño del bytecode, throw/catch). Never observable in
    /// the language.
    Caught(Handle<Caught>),
    /// INTERNAL: a `for` loop iterator (`iter_new` / `iter_next`).
    /// Never observable in the language.
    Iter(MutHandle<IterState>),
    /// INTERNAL: one lexical binding a closure shares with the scope
    /// that binds it (spec: 07 — Diseño del bytecode, closures). It lives
    /// in a frame slot and in the closure's capture list, and only
    /// `make_binding` / `load_binding` / `store_binding` ever see it —
    /// every read of the binding yields its contents, so no language
    /// value is ever a binding.
    Binding(GcRef),
    /// The `std::proc` `Output` record (BRS-32,
    /// spec: 05 — Stdlib de scripting): captured stdout/stderr plus the exit
    /// code. Frozen at construction and free of heap references, so a
    /// plain [`Handle`] is a precise collector for it.
    ProcOutput(Handle<OutputValue>),
    /// The `fs.tryWalk` record (BRS-66), holding what the traversal
    /// reached and what it could not read.
    Walk(Handle<WalkValue>),
    /// A `std::json` tree (BRS-34, spec: 05 — Stdlib de scripting): frozen at
    /// `parse` and free of heap references, so a plain [`Handle`] is a
    /// precise collector for it.
    Json(brasa_runtime::json_glue::JsonRef),
    /// The `std::cli` parsed-arguments record (BRS-112). Frozen after
    /// the parse and free of heap references, like
    /// [`Value::HttpResponse`].
    CliArgs(Handle<ArgsValue>),
    /// The `std::http` response record (BRS-113). Frozen at the end of
    /// the request and free of heap references — headers are kept as
    /// plain pairs rather than as a `Map` value — so a plain [`Handle`]
    /// is a precise collector for it, exactly like [`Value::ProcOutput`].
    HttpResponse(Handle<ResponseValue>),
}

/// The payload of a [`Value::NativeError`]: the canonical qualified
/// name the `catch` tag matches and the message the binding sees.
#[derive(Debug)]
pub struct NativeErrorValue {
    pub name: &'static str,
    pub message: Handle<str>,
}

/// The fields of a [`Value::ProcOutput`], in declaration order
/// (`stdout`, `stderr`, `code`).
#[derive(Debug)]
pub struct OutputValue {
    pub stdout: Handle<str>,
    pub stderr: Handle<str>,
    pub code: i64,
}

/// The fields of a [`Value::HttpResponse`]: the status, the body, and
/// the response headers as lowercased name/value pairs in the order the
/// server sent them.
///
/// Headers are pairs rather than a `Map` value on purpose. A `Map`
/// would be an arena reference the collector has to trace through this
/// record; pairs keep the record frozen and let `header` answer
/// case-insensitively, which is what an HTTP caller actually wants.
#[derive(Debug)]
pub struct ResponseValue {
    pub status: i64,
    pub body: Handle<str>,
    pub headers: Vec<(String, String)>,
}

/// The payload of a [`Value::CliArgs`]: which flags were present, the
/// options that were given a value, and everything positional.
#[derive(Debug)]
pub struct ArgsValue {
    pub flags: Vec<String>,
    pub options: Vec<(String, String)>,
    pub rest: Vec<String>,
}

/// The fields of a [`Value::Walk`], in declaration order (`paths`,
/// `unreadable`). Both are `Vector<string>`, so both are arena
/// handles the collector has to trace through this record.
#[derive(Debug)]
pub struct WalkValue {
    pub paths: Value,
    pub unreadable: Value,
}

#[derive(Debug)]
pub struct StructValue {
    pub shape: StructId,
    /// Field values aligned with the shape's declaration order.
    pub fields: RefCell<Vec<Value>>,
}

#[derive(Debug)]
pub struct EnumValue {
    pub shape: EnumId,
    pub variant: usize,
    pub fields: Vec<Value>,
}

/// A lambda plus its captured bindings, taken at `make_closure` in the
/// capture-order contract (`brasa_codegen` crate docs) and copied into
/// the frame's capture slots at call time.
///
/// A capture is a [`Value::Binding`] whenever some scope can rebind the
/// name, which is what makes the rebinding visible on both sides; where
/// nothing rebinds it, the code generator captures the value directly,
/// because a cell that never changes is indistinguishable from its
/// contents. The list itself never changes after construction.
#[derive(Debug)]
pub struct ClosureValue {
    pub func: FuncId,
    pub captures: Vec<Value>,
}

#[derive(Debug)]
pub struct BoundMethod {
    pub recv: Value,
    pub func: FuncId,
}

#[derive(Debug)]
pub struct BoundBuiltin {
    pub recv: Value,
    pub builtin: BuiltinId,
}

/// An in-flight signal caught by a handler entry: what
/// `jump_if_panic` / `jump_if_tag_ne` peek and `rethrow` resignals.
#[derive(Debug)]
pub enum Caught {
    /// A thrown error carrying its value.
    Error(Value),
    /// A panic: qualified kind name, detail, and the raise-time call
    /// chain (innermost first) for the uncaught rendering. Boxed for
    /// the same reason [`crate::vm::Signal`] boxes it — see there.
    Panic(Box<PanicValue>),
}

#[derive(Debug, Clone)]
pub struct PanicValue {
    pub name: &'static str,
    pub detail: String,
    pub stack: Vec<String>,
}

/// Loop iterator state (`iter_new` snapshots collections at loop
/// entry; ranges stay lazy and end on `i64` overflow).
#[derive(Debug)]
pub enum IterState {
    Range {
        next: i64,
        hi: i64,
        inclusive: bool,
        done: bool,
    },
    Items {
        items: Vec<Value>,
        ix: usize,
    },
}

impl IterState {
    pub fn next(&mut self) -> Option<Value> {
        match self {
            IterState::Range {
                next,
                hi,
                inclusive,
                done,
            } => {
                if *done {
                    return None;
                }
                let in_range = if *inclusive {
                    *next <= *hi
                } else {
                    *next < *hi
                };
                if !in_range {
                    return None;
                }

                let current = *next;
                match next.checked_add(1) {
                    Some(n) => *next = n,
                    None => *done = true,
                }
                Some(Value::Int(current))
            }
            IterState::Items { items, ix } => {
                let item = items.get(*ix).cloned()?;
                *ix += 1;
                Some(item)
            }
        }
    }
}

impl Value {
    pub fn str(s: impl AsRef<str>) -> Value {
        Value::Str(Rc::from(s.as_ref()))
    }

    pub fn some(inner: Value) -> Value {
        Value::Option(Some(Rc::new(inner)))
    }

    pub const NONE: Value = Value::Option(None);
}

/// The `Hashable` projection, arm for arm with the key set
/// `brasa_runtime::table` indexes. Strings project by CONTENT, so a
/// constant-pool string
/// interned at VM startup and a structurally equal runtime-built string
/// hash the same — the handle is never part of the key.
impl HashKeyed for Value {
    fn hash_key(&self) -> Option<HashKey> {
        match self {
            Value::Int(i) => Some(HashKey::Int(*i)),
            Value::Str(s) => Some(HashKey::Str(s.clone())),
            Value::Char(c) => Some(HashKey::Char(*c)),
            Value::Bool(b) => Some(HashKey::Bool(*b)),
            Value::Tuple(items) => items
                .iter()
                .map(Value::hash_key)
                .collect::<Option<Box<[HashKey]>>>()
                .map(HashKey::Tuple),
            _ => None,
        }
    }
}

/// Depth at which structural equality starts recording the arena cells
/// it is comparing: below it nothing is recorded at
/// all, and a cycle re-enters its own pairs without bound, so it is
/// always caught past this depth.
const CYCLE_GUARD_DEPTH: usize = 16;

/// The arena cell pairs currently assumed equal, scoped to the
/// derivation path. Hashed rather than scanned so a deep acyclic
/// comparison stays linear, and behind a `RefCell` so the comparators
/// handed to the hashed tables stay `Fn` (`brasa_runtime::table`).
/// Allocated by the first descent that reaches [`CYCLE_GUARD_DEPTH`]
/// and by nothing shallower.
#[derive(Default)]
struct Assumed(RefCell<HashSet<(GcRef, GcRef)>>);

/// Compares one pair of arena cells coinductively: past
/// [`CYCLE_GUARD_DEPTH`], re-entering a pair already being compared
/// yields `true`. Assuming the pair equal and deriving no contradiction
/// from it IS equality on a cyclic value — `==` is always structural
/// (spec: 03 — Sistema de tipos) and there is no identity operator to fall
/// back on.
fn coinductive(
    pair: (GcRef, GcRef),
    depth: usize,
    assumed: Option<&Assumed>,
    compare: impl FnOnce(Option<&Assumed>) -> bool,
) -> bool {
    if depth < CYCLE_GUARD_DEPTH {
        return compare(assumed);
    }

    match assumed {
        Some(assumed) => assuming(assumed, pair, compare),
        None => assuming(&Assumed::default(), pair, compare),
    }
}

fn assuming(
    assumed: &Assumed,
    pair: (GcRef, GcRef),
    compare: impl FnOnce(Option<&Assumed>) -> bool,
) -> bool {
    if !assumed.0.borrow_mut().insert(pair) {
        return true;
    }

    let equal = compare(Some(assumed));
    assumed.0.borrow_mut().remove(&pair);

    equal
}

/// Structural equality, ported from the walker: floats follow IEEE
/// (`NaN != NaN`), Maps and Sets compare content order-insensitively,
/// functions and closures fall back to identity, and values in a
/// reference cycle compare coinductively (see [`coinductive`]). Takes
/// the heap to resolve the arena-managed container kinds.
pub fn value_eq(heap: &Heap, a: &Value, b: &Value) -> bool {
    eq(heap, a, b, 0, None)
}

fn eq(heap: &Heap, a: &Value, b: &Value, depth: usize, assumed: Option<&Assumed>) -> bool {
    let deeper = depth + 1;
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => x == y,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Char(x), Value::Char(y)) => x == y,
        (Value::Unit, Value::Unit) => true,
        (Value::Str(x), Value::Str(y)) => x == y,
        (
            Value::Range { lo, hi, inclusive },
            Value::Range {
                lo: lo2,
                hi: hi2,
                inclusive: inclusive2,
            },
        ) => lo == lo2 && hi == hi2 && inclusive == inclusive2,
        (Value::Tuple(x), Value::Tuple(y)) => {
            x.len() == y.len()
                && x.iter()
                    .zip(y.iter())
                    .all(|(a, b)| eq(heap, a, b, deeper, assumed))
        }
        (Value::Vector(x), Value::Vector(y)) => coinductive((*x, *y), depth, assumed, |assumed| {
            let (x, y) = (heap.vector(*x).borrow(), heap.vector(*y).borrow());
            x.len() == y.len()
                && x.iter()
                    .zip(y.iter())
                    .all(|(a, b)| eq(heap, a, b, deeper, assumed))
        }),
        (Value::Map(x), Value::Map(y)) => coinductive((*x, *y), depth, assumed, |assumed| {
            let (x, y) = (heap.map(*x).borrow(), heap.map(*y).borrow());
            x.len() == y.len()
                && x.iter().all(|(k, v)| {
                    y.get(k, |a, b| eq(heap, a, b, deeper, assumed))
                        .is_some_and(|v2| eq(heap, v, v2, deeper, assumed))
                })
        }),
        (Value::Set(x), Value::Set(y)) => coinductive((*x, *y), depth, assumed, |assumed| {
            let (x, y) = (heap.set(*x).borrow(), heap.set(*y).borrow());
            x.len() == y.len()
                && x.iter()
                    .all(|a| y.contains(a, |a, b| eq(heap, a, b, deeper, assumed)))
        }),
        (Value::Option(x), Value::Option(y)) => match (x, y) {
            (Some(x), Some(y)) => eq(heap, x, y, deeper, assumed),
            (None, None) => true,
            _ => false,
        },
        (Value::Struct(x), Value::Struct(y)) => coinductive((*x, *y), depth, assumed, |assumed| {
            let (x, y) = (heap.struct_value(*x), heap.struct_value(*y));
            let (fx, fy) = (x.fields.borrow(), y.fields.borrow());
            x.shape == y.shape
                && fx.len() == fy.len()
                && fx
                    .iter()
                    .zip(fy.iter())
                    .all(|(a, b)| eq(heap, a, b, deeper, assumed))
        }),
        (Value::Enum(x), Value::Enum(y)) => {
            x.shape == y.shape
                && x.variant == y.variant
                && x.fields.len() == y.fields.len()
                && x.fields
                    .iter()
                    .zip(y.fields.iter())
                    .all(|(a, b)| eq(heap, a, b, deeper, assumed))
        }
        (Value::Func(x), Value::Func(y)) => x == y,
        (Value::NativeError(x), Value::NativeError(y)) => {
            x.name == y.name && x.message == y.message
        }
        (Value::Closure(x), Value::Closure(y)) => Rc::ptr_eq(x, y),
        (Value::BoundMethod(x), Value::BoundMethod(y)) => Rc::ptr_eq(x, y),
        (Value::BoundBuiltin(x), Value::BoundBuiltin(y)) => Rc::ptr_eq(x, y),
        (Value::ProcOutput(x), Value::ProcOutput(y)) => {
            x.stdout == y.stdout && x.stderr == y.stderr && x.code == y.code
        }
        // Structural over the tree (serde_json's `PartialEq`); note
        // JSON `1` and `1.0` are different numbers — no coercions.
        (Value::Json(x), Value::Json(y)) => x == y,
        _ => false,
    }
}

/// Primitive ordering for `<`/`<=`/`>`/`>=` and sort keys: `int`,
/// `float`, `string`, `char`. `None` for incomparable operands,
/// including any float pair involving `NaN`.
#[inline]
pub fn value_cmp(a: &Value, b: &Value) -> Option<Ordering> {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => Some(x.cmp(y)),
        (Value::Float(x), Value::Float(y)) => x.partial_cmp(y),
        (Value::Str(x), Value::Str(y)) => Some(x.cmp(y)),
        (Value::Char(x), Value::Char(y)) => Some(x.cmp(y)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::hash::{DefaultHasher, Hash, Hasher};

    use super::*;
    use crate::heap::DEFAULT_GC_BUDGET_BYTES;

    fn hash_of(key: &HashKey) -> u64 {
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        hasher.finish()
    }

    /// Every shape a `Map` key or `Set` element can take, plus values
    /// that must NOT project. The two `"alpha"`s stand in for the VM's
    /// split string story — a constant-pool string interned at startup
    /// versus one built at runtime — as separate allocations.
    fn corpus(heap: &mut Heap) -> Vec<Value> {
        vec![
            Value::Int(0),
            Value::Int(1),
            Value::str("alpha"),
            Value::str(String::from("al") + "pha"),
            Value::str("beta"),
            Value::Char('a'),
            Value::Char('b'),
            Value::Bool(true),
            Value::Bool(false),
            Value::Tuple(Rc::from(vec![Value::Int(1), Value::str("a")])),
            Value::Tuple(Rc::from(vec![Value::Int(1), Value::str("a")])),
            Value::Tuple(Rc::from(vec![Value::Int(1), Value::str("b")])),
            Value::Tuple(Rc::from(vec![Value::Int(1)])),
            Value::Tuple(Rc::from(Vec::new())),
            Value::Float(1.0),
            Value::Unit,
            heap.alloc_vector(vec![Value::Int(1)]),
        ]
    }

    /// The correctness crux of the hashed tables
    /// (`brasa_runtime::table`): for every pair that projects,
    /// `value_eq` and key equality must be the same relation, and equal
    /// keys must hash equally. Two structurally equal strings behind
    /// different allocations are the case that would break if the
    /// projection keyed on the handle instead of the content.
    #[test]
    fn hash_key_agrees_with_value_eq() {
        let mut heap = Heap::new(DEFAULT_GC_BUDGET_BYTES);
        for a in corpus(&mut heap) {
            for b in corpus(&mut heap) {
                let (Some(ka), Some(kb)) = (a.hash_key(), b.hash_key()) else {
                    continue;
                };

                assert_eq!(
                    value_eq(&heap, &a, &b),
                    ka == kb,
                    "projection disagrees with value_eq on {a:?} / {b:?}"
                );
                if ka == kb {
                    assert_eq!(hash_of(&ka), hash_of(&kb), "equal keys hash differently");
                }
            }
        }
    }

    #[test]
    fn equal_string_content_behind_distinct_allocations_hashes_the_same() {
        let interned = Value::str("alpha");
        let built = Value::str(String::from("al") + "pha");
        let (Some(a), Some(b)) = (interned.hash_key(), built.hash_key()) else {
            panic!("string keys must project");
        };

        assert_eq!(a, b);
        assert_eq!(hash_of(&a), hash_of(&b));
    }

    /// Only the closed `Hashable` list projects; everything else takes
    /// the linear fallback rather than being silently mis-keyed.
    #[test]
    fn only_hashable_values_project() {
        let mut heap = Heap::new(DEFAULT_GC_BUDGET_BYTES);
        for value in corpus(&mut heap) {
            let expected = matches!(
                value,
                Value::Int(_) | Value::Str(_) | Value::Char(_) | Value::Bool(_) | Value::Tuple(_)
            );
            assert_eq!(value.hash_key().is_some(), expected, "on {value:?}");
        }
    }

    #[test]
    fn a_tuple_with_a_non_hashable_element_does_not_project() {
        let tuple = Value::Tuple(Rc::from(vec![Value::Int(1), Value::Float(1.0)]));
        assert!(tuple.hash_key().is_none());
    }
}
