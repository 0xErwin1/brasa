//! The `json.of` walk: a language value to a `Json` tree (BRS-34,
//! spec: 05 — Stdlib de scripting).
//!
//! The write-side mirror of the `Json` accessors, and the reason
//! `json.stringify` takes any value. It lives in the VM rather than in
//! `brasa_runtime::json_glue` because `Vector`, `Map`, `Set` and
//! `Struct` are arena handles only the VM can read through, exactly
//! like [`crate::display`]. Node construction stays in the glue, which
//! remains the single owner of `serde_json`.
//!
//! # The mapping
//!
//! | Value | JSON |
//! |---|---|
//! | `int` | number |
//! | `float` | number, finite only |
//! | `bool` | boolean |
//! | `string` | string |
//! | `char` | a one-character string |
//! | `unit` | `null` |
//! | `Option` | `None` is `null`, `Some(v)` is `v` |
//! | `Vector`, `Set`, tuple | array |
//! | `Map` | object, string keys only |
//! | struct | object keyed by field name |
//! | `Json` | itself, unchanged |
//!
//! Everything else raises `json.ValueError` naming the type it
//! rejected: an enum, a range, anything callable, an error value, and
//! each of the compiler-known records (`Output`, `Walk`, `Stat`,
//! `Args`, `Response`, `Scope`, `Task`). Rejecting them is deliberate.
//! An enum
//! has no encoding a reader could agree on without a schema — a bare
//! variant name, a tagged object, and a positional array are all
//! defensible — and inventing one here would freeze it into every
//! document a script writes. A record is the shape of a *result*, not
//! of data a script means to publish; a caller that wants one in a
//! document builds the object it actually wants. `json.decode<T>` is
//! the typed bridge that will settle the schema question, and it will
//! settle both directions at once.

use std::rc::Rc;

use brasa_runtime::json_glue::{self, JsonValue};

use crate::value::{ErrorPayload, NativeErrorValue, Value};
use crate::vm::{Signal, Vm, VmResult};

/// How deep a value the walk descends, a bound on the host stack.
///
/// `Vector`, `Map`, `Set` and `Struct` are arena cells, so a value can
/// reach itself — `let v = []; v.push(v)` — and an unbounded walk would
/// take the host down with it. A JSON tree is finite by construction,
/// so there is nothing to detect and report precisely the way
/// `toString` reports a cycle: any value this deep has no JSON
/// representation, cyclic or not, and saying so is the whole answer.
/// The bound matches `brasa_module`'s import-chain limit for the same
/// reason it exists there — a diagnostic instead of a stack overflow.
const MAX_JSON_DEPTH: usize = 128;

impl Vm<'_> {
    /// Builds the JSON tree for a language value, or raises
    /// `json.ValueError` if the value has no representation.
    pub(crate) fn json_of(&mut self, value: &Value) -> VmResult<JsonValue> {
        self.build_json(value, 0)
    }

    /// One node of the walk.
    ///
    /// Nothing here allocates in the arena or reenters compiled code,
    /// so no collection can run while a container's contents are held
    /// as a plain copy — unlike [`Vm::display`], which roots its copies
    /// because a `toString` override runs between them.
    fn build_json(&mut self, value: &Value, depth: usize) -> VmResult<JsonValue> {
        if depth > MAX_JSON_DEPTH {
            return Err(value_error(format!(
                "cannot convert to JSON: value nested deeper than {MAX_JSON_DEPTH} levels"
            )));
        }

        let deeper = depth + 1;

        match value {
            Value::Int(v) => Ok(json_glue::from_int(*v)),
            Value::Float(v) => json_glue::from_float(*v).map_err(|err| glue_error(&err)),
            Value::Bool(v) => Ok(json_glue::from_bool(*v)),
            Value::Str(v) => Ok(json_glue::from_string(v)),
            // A char is a string of one: JSON has no character kind,
            // and the alternative — a code point as a number — would
            // stop round-tripping through `asString`.
            Value::Char(v) => Ok(json_glue::from_string(&v.to_string())),
            // `unit` and `None` are both the absence of a value, and
            // JSON spells that once.
            Value::Unit => Ok(json_glue::null()),
            Value::Option(inner) => match inner {
                Some(inner) => self.build_json(inner, deeper),
                None => Ok(json_glue::null()),
            },
            Value::Tuple(items) => {
                let items = items.to_vec();
                self.build_array(&items, deeper)
            }
            Value::Vector(cell) => {
                let items = self.heap.vector(*cell).borrow().clone();
                self.build_array(&items, deeper)
            }
            // A set is an array, not an object: it has elements, not
            // members, and JSON has no set.
            Value::Set(cell) => {
                let items = self.heap.set(*cell).borrow().items().to_vec();
                self.build_array(&items, deeper)
            }
            Value::Map(cell) => {
                let entries = self.heap.map(*cell).borrow().entries().to_vec();
                self.build_object_from_entries(&entries, deeper)
            }
            Value::Struct(cell) => {
                let shape = self.module_struct(self.heap.struct_value(*cell).shape);
                let names: Vec<String> = shape.fields.clone();
                let fields = self.heap.struct_value(*cell).fields.borrow().clone();

                self.build_object(names.into_iter().zip(fields), deeper)
            }
            // Already a tree: the pass-through is what makes
            // `stringify(of(x))` and `stringify(x)` the same text for
            // a `Json`, and what lets a parsed document be embedded in
            // a built one.
            Value::Json(tree) => Ok((**tree).clone()),
            _ => Err(value_error(format!(
                "cannot convert to JSON: a value of type `{}` has no JSON representation",
                self.json_type_name(value)
            ))),
        }
    }

    fn build_array(&mut self, items: &[Value], depth: usize) -> VmResult<JsonValue> {
        let mut nodes = Vec::with_capacity(items.len());
        for item in items {
            nodes.push(self.build_json(item, depth)?);
        }

        Ok(json_glue::from_items(nodes))
    }

    /// Map entries as object members. JSON object keys are strings, so
    /// a map keyed by anything else is rejected rather than having its
    /// keys rendered into strings: `{1: "a"}` and `{"1": "a"}` are
    /// different maps, and a silent rendering would merge them.
    fn build_object_from_entries(
        &mut self,
        entries: &[(Value, Value)],
        depth: usize,
    ) -> VmResult<JsonValue> {
        let mut members = Vec::with_capacity(entries.len());

        for (key, value) in entries {
            let Value::Str(key) = key else {
                return Err(value_error(format!(
                    "cannot convert to JSON: a Map keyed by `{}` has no JSON representation, \
                     because JSON object keys are strings",
                    self.json_type_name(key)
                )));
            };

            members.push((key.to_string(), value.clone()));
        }

        self.build_object(members, depth)
    }

    fn build_object(
        &mut self,
        members: impl IntoIterator<Item = (String, Value)>,
        depth: usize,
    ) -> VmResult<JsonValue> {
        let mut nodes = Vec::new();
        for (name, value) in members {
            nodes.push((name, self.build_json(&value, depth)?));
        }

        Ok(json_glue::from_members(nodes))
    }

    /// What a rejection message calls a value. A struct or an enum is
    /// named by its declaration, since that is the name the script
    /// wrote; everything else is named by its kind.
    fn json_type_name(&self, value: &Value) -> String {
        match value {
            Value::Int(_) => "int".to_string(),
            Value::Float(_) => "float".to_string(),
            Value::Bool(_) => "bool".to_string(),
            Value::Char(_) => "char".to_string(),
            Value::Unit => "unit".to_string(),
            Value::Str(_) => "string".to_string(),
            Value::Range { .. } => "range".to_string(),
            Value::Tuple(_) => "tuple".to_string(),
            Value::Vector(_) => "Vector".to_string(),
            Value::Map(_) => "Map".to_string(),
            Value::Set(_) => "Set".to_string(),
            Value::Option(_) => "Option".to_string(),
            Value::Struct(cell) => self
                .module_struct(self.heap.struct_value(*cell).shape)
                .name
                .clone(),
            Value::Enum(value) => self.module_enum(value.shape).name.clone(),
            Value::Func(_) | Value::Closure(_) => "function".to_string(),
            Value::BoundMethod(_) | Value::BoundBuiltin(_) => "bound method".to_string(),
            Value::NativeError(_) => "error".to_string(),
            Value::ProcOutput(_) => "Output".to_string(),
            Value::Walk(_) => "Walk".to_string(),
            Value::Stat(_) => "Stat".to_string(),
            Value::CliArgs(_) => "Args".to_string(),
            Value::HttpResponse(_) => "Response".to_string(),
            Value::ConcurrentScope(_) => "Scope".to_string(),
            Value::Task(_) => "Task".to_string(),
            Value::Json(_) => "Json".to_string(),
            // The internal variants never reach a builtin argument, but
            // naming them here keeps the walk total: an unrepresentable
            // value is an error the script can catch, never a panic
            // that takes the host down.
            Value::Caught(_) | Value::Iter(_) | Value::Binding(_) => "internal value".to_string(),
        }
    }
}

/// A `json.ValueError` signal. The glue owns the name; this is only its
/// signal form, the way `builtins::native_error` is for the modules
/// dispatched there.
fn value_error(message: String) -> Signal {
    glue_error(&json_glue::value_error(message))
}

fn glue_error(err: &json_glue::JsonError) -> Signal {
    Signal::Error(Value::NativeError(Rc::new(NativeErrorValue {
        name: err.name,
        message: Rc::from(err.message.as_str()),
        payload: ErrorPayload::None,
    })))
}
