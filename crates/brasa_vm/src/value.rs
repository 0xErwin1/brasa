//! VM runtime values and their structural operations.
//!
//! Mirrors the walker's value semantics (`brasa_interp::value`) over
//! the bytecode module's shape indices: inline scalars, ranges, and
//! `FuncId`s; heap kinds behind the handle aliases below. Structural
//! equality and primitive ordering are line-for-line ports of the
//! walker's `value_eq` / `value_cmp` — the parity oracle demands
//! byte-identical observable behavior.

use std::cell::RefCell;
use std::cmp::Ordering;
use std::rc::Rc;

use brasa_bytecode::{BuiltinId, EnumId, FuncId, StructId};

/// Shared immutable heap handle.
///
/// The GC unit (BRS-29) replaces these two aliases with GC-managed
/// handles; every heap kind in [`Value`] routes through them so the
/// swap touches the aliases, not the interpreter. Until then the VM
/// models the shared heap with `Rc`, exactly like the walker.
pub type Handle<T> = Rc<T>;

/// Shared mutable heap handle (interior mutability); see [`Handle`].
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
    Vector(MutHandle<Vec<Value>>),
    /// Insertion-ordered map with structural key lookup, as in the
    /// walker (a faster table is a later optimization).
    Map(MutHandle<Vec<(Value, Value)>>),
    /// Insertion-ordered set, same representation rationale as `Map`.
    Set(MutHandle<Vec<Value>>),
    Option(Option<Handle<Value>>),
    Struct(Handle<StructValue>),
    Enum(Handle<EnumValue>),
    /// A function-table entry used as a value.
    Func(FuncId),
    Closure(Handle<ClosureValue>),
    /// A struct method accessed without calling it (`p.dist` as a value).
    BoundMethod(Handle<BoundMethod>),
    /// A builtin method accessed without calling it (`v.push` as a value).
    BoundBuiltin(Handle<BoundBuiltin>),
    /// A stdlib-native error: canonical qualified name + message
    /// (`brasa_resolver::NATIVE_ERRORS`).
    NativeError {
        name: &'static str,
        message: Handle<str>,
    },
    /// INTERNAL: the caught-signal value handler dispatch operates on
    /// (`docs/spec/07-bytecode.md`, throw/catch). Never observable in
    /// the language.
    Caught(Handle<Caught>),
    /// INTERNAL: a `for` loop iterator (`iter_new` / `iter_next`).
    /// Never observable in the language.
    Iter(MutHandle<IterState>),
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

/// A lambda plus its captured values, snapshotted at `make_closure` in
/// the capture-order contract (`brasa_codegen` crate docs); copied into
/// the frame's capture slots at call time.
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
    /// chain (innermost first) for the uncaught rendering.
    Panic(PanicValue),
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

    pub fn vector(items: Vec<Value>) -> Value {
        Value::Vector(Rc::new(RefCell::new(items)))
    }

    pub fn some(inner: Value) -> Value {
        Value::Option(Some(Rc::new(inner)))
    }

    pub const NONE: Value = Value::Option(None);
}

/// Structural equality, ported from the walker: floats follow IEEE
/// (`NaN != NaN`), Maps and Sets compare content order-insensitively,
/// functions and closures fall back to identity.
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
            x.shape == y.shape
                && fx.len() == fy.len()
                && fx.iter().zip(fy.iter()).all(|(a, b)| value_eq(a, b))
        }
        (Value::Enum(x), Value::Enum(y)) => {
            x.shape == y.shape
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
/// `float`, `string`, `char`. `None` for incomparable operands,
/// including any float pair involving `NaN`.
pub fn value_cmp(a: &Value, b: &Value) -> Option<Ordering> {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => Some(x.cmp(y)),
        (Value::Float(x), Value::Float(y)) => x.partial_cmp(y),
        (Value::Str(x), Value::Str(y)) => Some(x.cmp(y)),
        (Value::Char(x), Value::Char(y)) => Some(x.cmp(y)),
        _ => None,
    }
}
