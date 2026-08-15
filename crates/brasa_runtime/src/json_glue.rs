//! Backend-agnostic glue for `std::json` (BRS-34,
//! spec: 05 — Stdlib de scripting), shared by the walker and the VM so JSON
//! behavior and every observable message can never drift between
//! backends. Value construction stays in each backend's own builtin
//! table, like `fs_glue` and `proc_env`.
//!
//! Decisions recorded here (mirrored in the spec):
//!
//! - The runtime tree is [`serde_json::Value`] behind a shared `Rc`
//!   ([`JsonValue`]): immutable after `parse`, cycle-free, so plain
//!   reference counting collects it precisely in both backends.
//!   Indexing hands out a copy of the selected subtree — the tree is
//!   immutable, so copying vs sharing is unobservable; O(1) subtree
//!   sharing is a later optimization, not a semantic question.
//! - Objects live in serde_json's default `BTreeMap`, so member order
//!   is the bytewise key order everywhere it can be observed:
//!   `stringify`, `toString`, and `asObject` iteration. The source
//!   document's member order is not preserved (deterministic output
//!   was ruled more valuable than input fidelity).
//! - `stringify` emits compact JSON (no whitespace). It never fails,
//!   because no tree can hold a non-finite number: `parse` cannot read
//!   one, and the builders below refuse to make one. That invariant
//!   used to be a property of parsing alone; since `json.of` builds
//!   trees out of language values, it is the builders that maintain it.
//! - Indexing is TOTAL: a missing key, an out-of-range or negative
//!   position, or a receiver of the wrong JSON kind is `None`, never a
//!   panic — the Option-yielding chain is the module's whole point.
//! - `asInt` succeeds only for integral numbers representable as
//!   `int` (i64); `asFloat` succeeds for every number (an integer
//!   outside the exact f64 range converts approximately, IEEE
//!   semantics). A JSON `2.0` is a float, not an int — no coercions.
//! - `parse` failures raise the native `json.ParseError`; the message
//!   carries serde_json's rendering, which includes line and column.
//! - A document that does not fit a declared type raises the native
//!   `json.DecodeError`, whose message names the path into the document
//!   ([`decode_error`]). The decoder itself is synthesized bytecode, so
//!   only the message lives here — for the same reason every other
//!   observable string does.
//! - A value that has no JSON representation raises the native
//!   `json.ValueError`. Deciding WHICH values those are needs a
//!   backend's value representation, so the walk lives in the backend;
//!   what lives here is the node construction it walks into, and the
//!   one rejection the tree type itself imposes (non-finite numbers).

use std::rc::Rc;

use brasa_resolver::{JSON_DECODE_ERROR, JSON_PARSE_ERROR, JSON_VALUE_ERROR};

/// The shared JSON tree node both backends wrap in their `Value::Json`
/// variant.
pub type JsonValue = serde_json::Value;

/// A shared handle to one (sub)tree.
pub type JsonRef = Rc<JsonValue>;

/// One failed JSON operation: the qualified native-error name
/// (`json.ParseError` reading, `json.ValueError` writing) and its
/// message.
#[derive(Debug)]
pub struct JsonError {
    pub name: &'static str,
    pub message: String,
}

pub fn parse(text: &str) -> Result<JsonRef, JsonError> {
    serde_json::from_str(text)
        .map(Rc::new)
        .map_err(|err| JsonError {
            name: JSON_PARSE_ERROR,
            message: format!("cannot parse JSON: {err}"),
        })
}

/// Renders a tree as compact JSON.
///
/// The `expect` is discharged by the module invariant: serialization
/// can only fail on a non-finite number, and no tree holds one —
/// [`parse`] cannot read one and [`from_float`] refuses to build one.
/// Every other node kind serializes unconditionally.
pub fn stringify(value: &JsonValue) -> String {
    serde_json::to_string(value).expect("no JSON tree holds a non-finite number")
}

/// A `json.ValueError` a backend's walk raises for a value it has no
/// mapping for. The name is fixed here so both sides of the module
/// spell it once.
pub fn value_error(message: String) -> JsonError {
    JsonError {
        name: JSON_VALUE_ERROR,
        message,
    }
}

pub fn null() -> JsonValue {
    JsonValue::Null
}

pub fn from_bool(value: bool) -> JsonValue {
    JsonValue::Bool(value)
}

pub fn from_int(value: i64) -> JsonValue {
    JsonValue::Number(value.into())
}

/// A JSON number from a float, rejecting the non-finite ones.
///
/// This is the guard that keeps [`stringify`] infallible: JSON has no
/// spelling for `NaN` or an infinity, so `serde_json::Number` refuses
/// to hold one and a tree that contained one could not be rendered.
pub fn from_float(value: f64) -> Result<JsonValue, JsonError> {
    serde_json::Number::from_f64(value)
        .map(JsonValue::Number)
        .ok_or_else(|| {
            value_error(format!(
                "cannot convert to JSON: `{value}` is not a finite number"
            ))
        })
}

pub fn from_string(text: &str) -> JsonValue {
    JsonValue::String(text.to_string())
}

pub fn from_items(items: Vec<JsonValue>) -> JsonValue {
    JsonValue::Array(items)
}

/// A JSON object from `(key, member)` pairs. The pairs land in the
/// sorted map the module docs describe, so the emitted member order is
/// bytewise regardless of the order they arrive in.
pub fn from_members(members: Vec<(String, JsonValue)>) -> JsonValue {
    JsonValue::Object(members.into_iter().collect())
}

/// Object member lookup: `Some` copy of the member subtree, `None` for
/// a missing key or a non-object receiver.
pub fn index_key(value: &JsonValue, key: &str) -> Option<JsonRef> {
    value.get(key).cloned().map(Rc::new)
}

/// Array element lookup: `Some` copy of the element subtree, `None`
/// for a negative or out-of-range position or a non-array receiver.
pub fn index_position(value: &JsonValue, position: i64) -> Option<JsonRef> {
    let position = usize::try_from(position).ok()?;
    value.get(position).cloned().map(Rc::new)
}

pub fn as_string(value: &JsonValue) -> Option<String> {
    value.as_str().map(str::to_string)
}

pub fn as_int(value: &JsonValue) -> Option<i64> {
    value.as_i64()
}

pub fn as_float(value: &JsonValue) -> Option<f64> {
    value.as_number().and_then(serde_json::Number::as_f64)
}

pub fn as_bool(value: &JsonValue) -> Option<bool> {
    value.as_bool()
}

/// Array elements as shared subtree handles, in array order; `None`
/// for a non-array node.
pub fn as_array(value: &JsonValue) -> Option<Vec<JsonRef>> {
    value
        .as_array()
        .map(|items| items.iter().cloned().map(Rc::new).collect())
}

/// Object members as `(key, subtree)` pairs in bytewise key order;
/// `None` for a non-object node.
pub fn as_object(value: &JsonValue) -> Option<Vec<(String, JsonRef)>> {
    value.as_object().map(|members| {
        members
            .iter()
            .map(|(key, member)| (key.clone(), Rc::new(member.clone())))
            .collect()
    })
}

pub fn is_null(value: &JsonValue) -> bool {
    value.is_null()
}

/// The root's name in a decode path, so a failure about the document
/// itself does not render as an empty prefix.
const DOCUMENT_ROOT: &str = "<document>";

/// What a decode failure reports having found where a declared type
/// wanted something else.
///
/// A number answers `int` or `float` by the same rule `as_int` and
/// `as_float` follow — integral and representable is an `int`, anything
/// else is a `float` — so the message never claims a kind the accessor
/// would have refused.
pub fn kind_name(value: &JsonValue) -> &'static str {
    match value {
        JsonValue::Null => "null",
        JsonValue::Bool(_) => "bool",
        JsonValue::Number(_) if as_int(value).is_some() => "int",
        JsonValue::Number(_) => "float",
        JsonValue::String(_) => "string",
        JsonValue::Array(_) => "array",
        JsonValue::Object(_) => "object",
    }
}

/// The `json.DecodeError` for one member of one document (BRS-144):
/// where it is, what the declared type wanted, and what the document
/// holds there — `None` for a member the document does not carry at
/// all.
///
/// The path is what makes the error worth reading: a document is a tree
/// and a decode walks all of it, so "decode failed" would leave the
/// caller to find the offending member by hand.
pub fn decode_error(path: &str, expected: &str, found: Option<&JsonValue>) -> JsonError {
    let where_ = if path.is_empty() { DOCUMENT_ROOT } else { path };

    let found = match found {
        Some(value) => kind_name(value),
        None => "no member",
    };

    JsonError {
        name: JSON_DECODE_ERROR,
        message: format!("{where_}: expected {expected}, found {found}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        as_float, as_int, decode_error, from_bool, from_float, from_int, from_items, from_members,
        from_string, index_key, index_position, kind_name, null, parse, stringify,
    };

    /// A decode failure must not claim a kind the accessor would have
    /// refused: `2.0` is a float here for the same reason `asInt`
    /// answers `None` for it.
    #[test]
    fn kind_name_splits_numbers_the_way_the_accessors_do() {
        let tree = parse(r#"{"i": 2, "f": 2.5, "s": "x", "b": true, "n": null, "a": [], "o": {}}"#)
            .expect("valid JSON");

        let kind = |key: &str| kind_name(&index_key(&tree, key).expect("the member exists"));

        assert_eq!(kind("i"), "int");
        assert_eq!(kind("f"), "float");
        assert_eq!(kind("s"), "string");
        assert_eq!(kind("b"), "bool");
        assert_eq!(kind("n"), "null");
        assert_eq!(kind("a"), "array");
        assert_eq!(kind("o"), "object");
        assert_eq!(kind_name(&tree), "object");
    }

    /// The message names WHERE, not only what: an absent member has no
    /// kind to report, and the document root has no path to print.
    #[test]
    fn decode_errors_name_the_path() {
        let found = from_int(7);

        let wrong = decode_error("users[3].email", "string", Some(&found));
        assert_eq!(wrong.name, "json.DecodeError");
        assert_eq!(wrong.message, "users[3].email: expected string, found int");

        let absent = decode_error("port", "int", None);
        assert_eq!(absent.message, "port: expected int, found no member");

        let root = decode_error("", "object", Some(&from_items(vec![])));
        assert_eq!(root.message, "<document>: expected object, found array");
    }

    #[test]
    fn stringify_is_compact_with_sorted_keys() {
        let tree = parse(r#"{"b": 1, "a": [true, null, "x"]}"#).expect("valid JSON");
        assert_eq!(stringify(&tree), r#"{"a":[true,null,"x"],"b":1}"#);
    }

    #[test]
    fn parse_errors_carry_line_and_column() {
        let err = parse("{\n  \"a\": }").expect_err("invalid JSON");
        assert_eq!(err.name, "json.ParseError");
        assert_eq!(
            err.message,
            "cannot parse JSON: expected value at line 2 column 8"
        );
    }

    #[test]
    fn indexing_is_total() {
        let tree = parse(r#"{"a": [10, 20]}"#).expect("valid JSON");

        assert!(index_key(&tree, "missing").is_none());
        assert!(index_position(&tree, 0).is_none());

        let array = index_key(&tree, "a").expect("member exists");
        assert!(index_position(&array, -1).is_none());
        assert!(index_position(&array, 2).is_none());
        assert_eq!(
            index_position(&array, 1).map(|v| as_int(&v)),
            Some(Some(20))
        );
    }

    #[test]
    fn numbers_do_not_coerce() {
        let tree = parse(r#"[2, 2.0]"#).expect("valid JSON");

        let int_node = index_position(&tree, 0).expect("element exists");
        assert_eq!(as_int(&int_node), Some(2));
        assert_eq!(as_float(&int_node), Some(2.0));

        let float_node = index_position(&tree, 1).expect("element exists");
        assert_eq!(as_int(&float_node), None);
        assert_eq!(as_float(&float_node), Some(2.0));
    }

    /// A built tree renders exactly like a parsed one, sorted members
    /// included: the two sides of the module produce the same trees.
    #[test]
    fn built_trees_render_like_parsed_ones() {
        let tree = from_members(vec![
            ("b".to_string(), from_int(1)),
            (
                "a".to_string(),
                from_items(vec![from_bool(true), null(), from_string("x")]),
            ),
        ]);

        assert_eq!(stringify(&tree), r#"{"a":[true,null,"x"],"b":1}"#);
        assert_eq!(
            stringify(&tree),
            stringify(&parse(r#"{"a": [true, null, "x"], "b": 1}"#).expect("valid JSON"))
        );
    }

    /// An int and a float stay different numbers on the way in, the
    /// way `numbers_do_not_coerce` shows they do on the way out.
    #[test]
    fn built_numbers_keep_their_kind() {
        assert_eq!(stringify(&from_int(2)), "2");
        assert_eq!(
            stringify(&from_float(2.0).expect("finite")),
            stringify(&parse("2.0").expect("valid JSON"))
        );
    }

    /// The invariant `stringify`'s `expect` rests on: a non-finite
    /// number is refused at build time, so no tree can hold one.
    #[test]
    fn non_finite_floats_are_refused_at_build_time() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let err = from_float(value).expect_err("not representable");

            assert_eq!(err.name, "json.ValueError");
            assert!(
                err.message.starts_with("cannot convert to JSON: "),
                "unexpected message: {}",
                err.message
            );
        }

        assert_eq!(
            from_float(f64::NAN).expect_err("not representable").message,
            "cannot convert to JSON: `NaN` is not a finite number"
        );
    }
}
