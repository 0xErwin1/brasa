//! Runtime values and their structural operations.
//!
//! Per `docs/spec/03-types.md` (value vs reference): primitives and
//! tuples are values; structs, collections, strings, and closures are
//! shared heap references. The interpreter models the shared heap with
//! `Rc<RefCell<...>>` — the M3 VM brings a real GC, this walker only has
//! to make aliased mutation observable (`let v2 = v; v2.push(x)` is
//! visible through `v`). Strings are `Rc<str>`: every string method is
//! pure, so the shared reference never needs interior mutability.
//!
//! Tuples are by-value but immutable (no element assignment exists), so
//! an `Rc<[Value]>` clone is indistinguishable from a copy. Enum
//! payloads are likewise immutable — no syntax assigns through a variant
//! — so `Enum` holds its fields inline; heap values inside the payload
//! are still shared through their own `Rc`s.

use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use brasa_hir::{ExprId, ItemId};
use brasa_resolver::LocalId;

use brasa_runtime::table::{HashKey, HashKeyed, OrderedMap, OrderedSet};

/// A Brasa runtime value.
#[derive(Debug, Clone)]
pub enum Value {
    Int(i64),
    Float(f64),
    Bool(bool),
    Char(char),
    Unit,
    Str(Rc<str>),
    /// Lazy int range (`docs/spec/03-types.md`): two ints + an inclusive
    /// flag, never materialized.
    Range {
        lo: i64,
        hi: i64,
        inclusive: bool,
    },
    Tuple(Rc<[Value]>),
    Vector(Rc<RefCell<Vec<Value>>>),
    /// Insertion-ordered map: the ordered entries plus a hashed position
    /// index over the closed `Hashable` key set (`brasa_runtime::table`).
    Map(Rc<RefCell<OrderedMap<Value>>>),
    /// Insertion-ordered set, same representation rationale as `Map`.
    Set(Rc<RefCell<OrderedSet<Value>>>),
    Option(Option<Rc<Value>>),
    Struct(Rc<StructValue>),
    Enum(Rc<EnumValue>),
    /// A top-level function used as a value.
    Func(ItemId),
    Closure(Rc<ClosureValue>),
    /// A struct method accessed without calling it (`p.dist` as a value).
    BoundMethod(Rc<BoundMethod>),
    /// A builtin method accessed without calling it (`v.push` as a value).
    BoundBuiltin(Rc<BoundBuiltin>),
    /// A stdlib-native error (BRS-41, `docs/spec/05-stdlib.md`): the
    /// canonical qualified name from `brasa_resolver::NATIVE_ERRORS`
    /// plus a human-readable message. The name is the nominal tag
    /// `catch` matches against; the message is what a named arm binds
    /// (and what `toString` renders — the uncaught-error path prepends
    /// the name itself, `crate::finish`).
    NativeError {
        name: &'static str,
        message: Rc<str>,
    },
    /// The `std::proc` `Output` record (BRS-32,
    /// `docs/spec/05-stdlib.md`): captured stdout/stderr plus the exit
    /// code. Immutable after construction, so a shared `Rc` clone is
    /// indistinguishable from a copy.
    ProcOutput(Rc<OutputValue>),
    /// The `fs.tryWalk` record (BRS-66), holding what the traversal
    /// reached and what it could not read.
    Walk(Rc<WalkValue>),
    /// A `std::json` tree (BRS-34, `docs/spec/05-stdlib.md`): immutable
    /// after `parse` and free of language values, so a shared `Rc`
    /// clone is indistinguishable from a copy.
    Json(brasa_runtime::json_glue::JsonRef),
}

/// The fields of a [`Value::ProcOutput`], in declaration order
/// (`stdout`, `stderr`, `code`).
#[derive(Debug)]
pub struct OutputValue {
    pub stdout: Rc<str>,
    pub stderr: Rc<str>,
    pub code: i64,
}

/// The fields of a [`Value::Walk`], in declaration order (`paths`,
/// `unreadable`). Both are `Vector<string>`.
#[derive(Debug)]
pub struct WalkValue {
    pub paths: Value,
    pub unreadable: Value,
}

#[derive(Debug)]
pub struct StructValue {
    pub item: ItemId,
    /// Field values aligned with the `StructDef`'s declaration order.
    pub fields: RefCell<Vec<Value>>,
}

#[derive(Debug)]
pub struct EnumValue {
    pub item: ItemId,
    pub variant: usize,
    pub fields: Vec<Value>,
}

/// A lambda plus its captured environment. Capture is by value at
/// creation time (M1 decision, see `crate::interp`): the visible locals
/// are snapshotted, so rebinding a captured `let mut` afterwards is not
/// observable, while heap values stay shared through their `Rc`s.
#[derive(Debug)]
pub struct ClosureValue {
    pub lambda: ExprId,
    pub captured: HashMap<LocalId, Value>,
    pub self_value: Option<Value>,
}

#[derive(Debug)]
pub struct BoundMethod {
    pub recv: Value,
    pub owner: ItemId,
    pub index: usize,
}

#[derive(Debug)]
pub struct BoundBuiltin {
    pub recv: Value,
    pub name: String,
}

impl Value {
    pub fn str(s: impl AsRef<str>) -> Value {
        Value::Str(Rc::from(s.as_ref()))
    }

    pub fn vector(items: Vec<Value>) -> Value {
        Value::Vector(Rc::new(RefCell::new(items)))
    }

    pub fn map(entries: OrderedMap<Value>) -> Value {
        Value::Map(Rc::new(RefCell::new(entries)))
    }

    pub fn set(items: OrderedSet<Value>) -> Value {
        Value::Set(Rc::new(RefCell::new(items)))
    }

    pub fn some(inner: Value) -> Value {
        Value::Option(Some(Rc::new(inner)))
    }

    pub const NONE: Value = Value::Option(None);
}

/// The `Hashable` projection (`brasa_runtime::table`). Every arm mirrors the
/// corresponding [`value_eq`] arm exactly: scalars compare by content,
/// strings by content (`Rc<str>` hashes and compares as `str`), tuples
/// element-wise with a matching length. Everything outside the closed
/// `Hashable` list — including `float`, whose IEEE `NaN != NaN` has no
/// hash counterpart — projects to `None` and takes the linear fallback.
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

/// Depth at which structural equality starts recording the cycle-capable
/// cells it is comparing. Below it nothing is recorded at all, which
/// keeps the overwhelmingly common shallow acyclic comparison exactly as
/// cheap as it was; a cycle re-enters its own pairs without bound, so it
/// is always caught past this depth.
const CYCLE_GUARD_DEPTH: usize = 16;

/// The cell pairs currently assumed equal, scoped to the derivation
/// path. Hashed rather than scanned so a deep acyclic comparison stays
/// linear, and behind a `RefCell` so the comparators handed to the
/// hashed tables stay `Fn` (`brasa_runtime::table`). Allocated by the first
/// descent that reaches [`CYCLE_GUARD_DEPTH`] and by nothing shallower.
#[derive(Default)]
struct Assumed(RefCell<HashSet<(usize, usize)>>);

/// Compares one pair of cycle-capable cells coinductively: past
/// [`CYCLE_GUARD_DEPTH`], re-entering a pair already being compared
/// yields `true`. Assuming the pair equal and deriving no contradiction
/// from it IS equality on a cyclic value — `==` is always structural
/// (`docs/spec/03-types.md`) and there is no identity operator to fall
/// back on.
fn coinductive(
    pair: (usize, usize),
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
    pair: (usize, usize),
    compare: impl FnOnce(Option<&Assumed>) -> bool,
) -> bool {
    if !assumed.0.borrow_mut().insert(pair) {
        return true;
    }

    let equal = compare(Some(assumed));
    assumed.0.borrow_mut().remove(&pair);

    equal
}

fn cell_id<T: ?Sized>(handle: &Rc<T>) -> usize {
    Rc::as_ptr(handle) as *const u8 as usize
}

/// Structural equality (`docs/spec/03-types.md`: `==` is ALWAYS
/// structural, there is no identity operator). Floats follow IEEE
/// (`NaN != NaN`). Maps and sets compare content order-insensitively —
/// insertion order is an iteration guarantee, not part of the value
/// (M1 decision). Functions and closures have no structure to compare,
/// so they fall back to identity (M1 decision). Values that participate
/// in a reference cycle compare coinductively (see [`coinductive`]).
pub fn value_eq(a: &Value, b: &Value) -> bool {
    eq(a, b, 0, None)
}

fn eq(a: &Value, b: &Value, depth: usize, assumed: Option<&Assumed>) -> bool {
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
                    .all(|(a, b)| eq(a, b, deeper, assumed))
        }
        (Value::Vector(x), Value::Vector(y)) => {
            coinductive((cell_id(x), cell_id(y)), depth, assumed, |assumed| {
                let (x, y) = (x.borrow(), y.borrow());
                x.len() == y.len()
                    && x.iter()
                        .zip(y.iter())
                        .all(|(a, b)| eq(a, b, deeper, assumed))
            })
        }
        (Value::Map(x), Value::Map(y)) => {
            coinductive((cell_id(x), cell_id(y)), depth, assumed, |assumed| {
                let (x, y) = (x.borrow(), y.borrow());
                x.len() == y.len()
                    && x.iter().all(|(k, v)| {
                        y.get(k, |a, b| eq(a, b, deeper, assumed))
                            .is_some_and(|v2| eq(v, v2, deeper, assumed))
                    })
            })
        }
        (Value::Set(x), Value::Set(y)) => {
            coinductive((cell_id(x), cell_id(y)), depth, assumed, |assumed| {
                let (x, y) = (x.borrow(), y.borrow());
                x.len() == y.len()
                    && x.iter()
                        .all(|a| y.contains(a, |a, b| eq(a, b, deeper, assumed)))
            })
        }
        (Value::Option(x), Value::Option(y)) => match (x, y) {
            (Some(x), Some(y)) => eq(x, y, deeper, assumed),
            (None, None) => true,
            _ => false,
        },
        (Value::Struct(x), Value::Struct(y)) => {
            coinductive((cell_id(x), cell_id(y)), depth, assumed, |assumed| {
                let (fx, fy) = (x.fields.borrow(), y.fields.borrow());
                x.item == y.item
                    && fx.len() == fy.len()
                    && fx
                        .iter()
                        .zip(fy.iter())
                        .all(|(a, b)| eq(a, b, deeper, assumed))
            })
        }
        (Value::Enum(x), Value::Enum(y)) => {
            x.item == y.item
                && x.variant == y.variant
                && x.fields.len() == y.fields.len()
                && x.fields
                    .iter()
                    .zip(y.fields.iter())
                    .all(|(a, b)| eq(a, b, deeper, assumed))
        }
        (Value::Func(x), Value::Func(y)) => x == y,
        (
            Value::NativeError { name, message },
            Value::NativeError {
                name: name2,
                message: message2,
            },
        ) => name == name2 && message == message2,
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
/// `float`, `string`, `char` (`docs/spec/03-types.md`, operator table).
/// Returns `None` for incomparable operands, including any float pair
/// involving `NaN` (IEEE: `NaN` does not order).
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

    fn hash_of(key: &HashKey) -> u64 {
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        hasher.finish()
    }

    /// Every shape a `Map` key or `Set` element can take, plus values
    /// that must NOT project. Two structurally equal strings are built
    /// through separate allocations on purpose.
    fn corpus() -> Vec<Value> {
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
            Value::vector(vec![Value::Int(1)]),
        ]
    }

    /// The correctness crux of the hashed tables (`brasa_runtime::table`): for
    /// every pair that projects, `value_eq` and key equality must be the
    /// same relation, and equal keys must hash equally.
    #[test]
    fn hash_key_agrees_with_value_eq() {
        for a in corpus() {
            for b in corpus() {
                let (Some(ka), Some(kb)) = (a.hash_key(), b.hash_key()) else {
                    continue;
                };

                assert_eq!(
                    value_eq(&a, &b),
                    ka == kb,
                    "projection disagrees with value_eq on {a:?} / {b:?}"
                );
                if ka == kb {
                    assert_eq!(hash_of(&ka), hash_of(&kb), "equal keys hash differently");
                }
            }
        }
    }

    /// Only the closed `Hashable` list projects; everything else takes
    /// the linear fallback rather than being silently mis-keyed.
    #[test]
    fn only_hashable_values_project() {
        for value in corpus() {
            let expected = matches!(
                value,
                Value::Int(_) | Value::Str(_) | Value::Char(_) | Value::Bool(_) | Value::Tuple(_)
            );
            assert_eq!(value.hash_key().is_some(), expected, "on {value:?}");
        }
    }

    /// A tuple is only `Hashable` when every element is, and a nested
    /// non-projectable element must poison the whole projection.
    #[test]
    fn a_tuple_with_a_non_hashable_element_does_not_project() {
        let tuple = Value::Tuple(Rc::from(vec![Value::Int(1), Value::Float(1.0)]));
        assert!(tuple.hash_key().is_none());
    }
}
