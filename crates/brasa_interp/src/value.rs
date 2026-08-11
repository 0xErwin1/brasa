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
use std::collections::HashMap;
use std::rc::Rc;

use brasa_hir::{ExprId, ItemId};
use brasa_resolver::LocalId;

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
    /// Insertion-ordered map. A `Vec` of pairs with structural key
    /// lookup keeps the reference walker simple and dependency-free;
    /// speed is an M3 concern (`docs/spec/00-vision.md`, roadmap).
    Map(Rc<RefCell<Vec<(Value, Value)>>>),
    /// Insertion-ordered set, same representation rationale as `Map`.
    Set(Rc<RefCell<Vec<Value>>>),
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

    pub fn some(inner: Value) -> Value {
        Value::Option(Some(Rc::new(inner)))
    }

    pub const NONE: Value = Value::Option(None);
}

/// Structural equality (`docs/spec/03-types.md`: `==` is ALWAYS
/// structural, there is no identity operator). Floats follow IEEE
/// (`NaN != NaN`). Maps and sets compare content order-insensitively —
/// insertion order is an iteration guarantee, not part of the value
/// (M1 decision). Functions and closures have no structure to compare,
/// so they fall back to identity (M1 decision).
pub fn value_eq(a: &Value, b: &Value) -> bool {
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
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(a, b)| value_eq(a, b))
        }
        (Value::Vector(x), Value::Vector(y)) => {
            let (x, y) = (x.borrow(), y.borrow());
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(a, b)| value_eq(a, b))
        }
        (Value::Map(x), Value::Map(y)) => {
            let (x, y) = (x.borrow(), y.borrow());
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.iter().any(|(k2, v2)| value_eq(k, k2) && value_eq(v, v2)))
        }
        (Value::Set(x), Value::Set(y)) => {
            let (x, y) = (x.borrow(), y.borrow());
            x.len() == y.len() && x.iter().all(|a| y.iter().any(|b| value_eq(a, b)))
        }
        (Value::Option(x), Value::Option(y)) => match (x, y) {
            (Some(x), Some(y)) => value_eq(x, y),
            (None, None) => true,
            _ => false,
        },
        (Value::Struct(x), Value::Struct(y)) => {
            let (fx, fy) = (x.fields.borrow(), y.fields.borrow());
            x.item == y.item
                && fx.len() == fy.len()
                && fx.iter().zip(fy.iter()).all(|(a, b)| value_eq(a, b))
        }
        (Value::Enum(x), Value::Enum(y)) => {
            x.item == y.item
                && x.variant == y.variant
                && x.fields.len() == y.fields.len()
                && x.fields
                    .iter()
                    .zip(y.fields.iter())
                    .all(|(a, b)| value_eq(a, b))
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
