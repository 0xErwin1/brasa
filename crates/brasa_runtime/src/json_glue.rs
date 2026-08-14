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
//! - `stringify` emits compact JSON (no whitespace). It never fails:
//!   a parsed tree contains no non-finite numbers.
//! - Indexing is TOTAL: a missing key, an out-of-range or negative
//!   position, or a receiver of the wrong JSON kind is `None`, never a
//!   panic — the Option-yielding chain is the module's whole point.
//! - `asInt` succeeds only for integral numbers representable as
//!   `int` (i64); `asFloat` succeeds for every number (an integer
//!   outside the exact f64 range converts approximately, IEEE
//!   semantics). A JSON `2.0` is a float, not an int — no coercions.
//! - `parse` failures raise the native `json.ParseError`; the message
//!   carries serde_json's rendering, which includes line and column.

use std::rc::Rc;

use brasa_resolver::JSON_PARSE_ERROR;

/// The shared JSON tree node both backends wrap in their `Value::Json`
/// variant.
pub type JsonValue = serde_json::Value;

/// A shared handle to one (sub)tree.
pub type JsonRef = Rc<JsonValue>;

/// One failed JSON operation: the qualified native-error name
/// (`json.ParseError`) and its message.
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

pub fn stringify(value: &JsonValue) -> String {
    serde_json::to_string(value).expect("a parsed JSON tree always serializes")
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

#[cfg(test)]
mod tests {
    use super::{as_float, as_int, index_key, index_position, parse, stringify};

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
}
