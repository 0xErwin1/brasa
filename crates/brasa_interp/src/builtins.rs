//! The runtime builtin method table: what `.method(...)` does on
//! primitive and container values.
//!
//! Mirrors the checker's M1 table (`brasa_typeck::builtins`,
//! `docs/spec/05-stdlib.md`); the exact stdlib signatures close in M4.
//!
//! M1 decisions recorded here:
//!
//! - String indices are Unicode scalar values throughout: `len` counts
//!   scalars, `slice(from, to)` takes scalar indices (exclusive end,
//!   clamped to the string's bounds, empty when `from >= to`), and
//!   `find` returns a scalar index.
//! - `count` counts non-overlapping occurrences; an empty needle counts
//!   zero. `split("")` yields one string per scalar. `repeat(n)` with
//!   `n <= 0` is the empty string. `toInt`/`toFloat` parse the exact
//!   string (no trimming) and return `None` on failure.
//! - `float.toInt` truncates with Rust `as` semantics: saturating at
//!   the `int` bounds, `NaN` becomes `0`.
//! - `reverse`, `map`, `filter`, and `sortBy` return new vectors;
//!   `push`/`pop` mutate in place.
//! - `sortBy` is a stable sort; keys must be `int`, `float`, `string`,
//!   or `char`, and a `NaN` float key panics with
//!   `panics.AssertionFailed` (`docs/spec/03-types.md`, float rules).

use std::cmp::Ordering;

use crate::interp::{EvalResult, Interp, PanicKind, Signal};
use crate::value::{Value, value_cmp, value_eq};

impl Interp<'_> {
    pub(crate) fn call_builtin(&mut self, recv: Value, name: &str, args: Vec<Value>) -> EvalResult {
        // The universal derived `toString` applies to every type; a
        // struct's own method already won during dispatch
        // (`docs/spec/03-types.md`).
        if name == "toString" && args.is_empty() && !matches!(recv, Value::Int(_) | Value::Float(_))
        {
            let text = self.display(&recv)?;
            return Ok(Value::str(text));
        }

        match &recv {
            Value::Int(v) => self.int_builtin(*v, name, &args),
            Value::Float(v) => self.float_builtin(*v, name, &args),
            Value::Str(s) => {
                let s = s.clone();
                self.string_builtin(&s, name, &args)
            }
            Value::Vector(_) => self.vector_builtin(&recv, name, args),
            Value::Map(_) => self.map_builtin(&recv, name, &args),
            Value::Set(_) => self.set_builtin(&recv, name, &args),
            _ => Err(self.builtin_error(name)),
        }
    }

    fn builtin_error(&self, name: &str) -> Signal {
        Signal::Fatal(format!("brasa: unknown builtin method `{name}`"))
    }

    fn int_builtin(&mut self, v: i64, name: &str, args: &[Value]) -> EvalResult {
        match (name, args) {
            ("toFloat", []) => Ok(Value::Float(v as f64)),
            ("toString", []) => Ok(Value::str(v.to_string())),
            _ => Err(self.builtin_error(name)),
        }
    }

    fn float_builtin(&mut self, v: f64, name: &str, args: &[Value]) -> EvalResult {
        match (name, args) {
            ("toInt", []) => Ok(Value::Int(v as i64)),
            ("toString", []) => {
                let text = self.display(&Value::Float(v))?;
                Ok(Value::str(text))
            }
            _ => Err(self.builtin_error(name)),
        }
    }

    fn string_builtin(&mut self, s: &str, name: &str, args: &[Value]) -> EvalResult {
        match (name, args) {
            ("len", []) => Ok(Value::Int(s.chars().count() as i64)),
            ("count", [Value::Str(needle)]) => {
                if needle.is_empty() {
                    return Ok(Value::Int(0));
                }
                Ok(Value::Int(s.matches(needle.as_ref()).count() as i64))
            }
            ("trim", []) => Ok(Value::str(s.trim())),
            ("toUpper", []) => Ok(Value::str(s.to_uppercase())),
            ("toLower", []) => Ok(Value::str(s.to_lowercase())),
            ("contains?", [Value::Str(needle)]) => Ok(Value::Bool(s.contains(needle.as_ref()))),
            ("startsWith?", [Value::Str(prefix)]) => {
                Ok(Value::Bool(s.starts_with(prefix.as_ref())))
            }
            ("endsWith?", [Value::Str(suffix)]) => Ok(Value::Bool(s.ends_with(suffix.as_ref()))),
            ("split", [Value::Str(sep)]) => {
                let parts: Vec<Value> = if sep.is_empty() {
                    s.chars().map(|c| Value::str(c.to_string())).collect()
                } else {
                    s.split(sep.as_ref()).map(Value::str).collect()
                };
                Ok(Value::vector(parts))
            }
            ("lines", []) => Ok(Value::vector(s.lines().map(Value::str).collect())),
            ("chars", []) => Ok(Value::vector(s.chars().map(Value::Char).collect())),
            ("slice", [Value::Int(from), Value::Int(to)]) => {
                let len = s.chars().count() as i64;
                let from = (*from).clamp(0, len) as usize;
                let to = (*to).clamp(0, len) as usize;
                if from >= to {
                    return Ok(Value::str(""));
                }
                let text: String = s.chars().skip(from).take(to - from).collect();
                Ok(Value::str(text))
            }
            ("repeat", [Value::Int(n)]) => {
                if *n <= 0 {
                    return Ok(Value::str(""));
                }
                Ok(Value::str(s.repeat(*n as usize)))
            }
            ("replace", [Value::Str(from), Value::Str(to)]) => {
                Ok(Value::str(s.replace(from.as_ref(), to.as_ref())))
            }
            ("find", [Value::Str(needle)]) => match s.find(needle.as_ref()) {
                Some(byte_index) => {
                    let char_index = s[..byte_index].chars().count() as i64;
                    Ok(Value::some(Value::Int(char_index)))
                }
                None => Ok(Value::NONE),
            },
            ("toInt", []) => Ok(s
                .parse::<i64>()
                .map(|v| Value::some(Value::Int(v)))
                .unwrap_or(Value::NONE)),
            ("toFloat", []) => Ok(s
                .parse::<f64>()
                .map(|v| Value::some(Value::Float(v)))
                .unwrap_or(Value::NONE)),
            _ => Err(self.builtin_error(name)),
        }
    }

    fn vector_builtin(&mut self, recv: &Value, name: &str, args: Vec<Value>) -> EvalResult {
        let Value::Vector(items) = recv else {
            return Err(self.builtin_error(name));
        };

        match (name, args.as_slice()) {
            ("len", []) => Ok(Value::Int(items.borrow().len() as i64)),
            ("push", [value]) => {
                items.borrow_mut().push(value.clone());
                Ok(Value::Unit)
            }
            ("pop", []) => Ok(match items.borrow_mut().pop() {
                Some(value) => Value::some(value),
                None => Value::NONE,
            }),
            ("first", []) => Ok(items
                .borrow()
                .first()
                .map(|v| Value::some(v.clone()))
                .unwrap_or(Value::NONE)),
            ("last", []) => Ok(items
                .borrow()
                .last()
                .map(|v| Value::some(v.clone()))
                .unwrap_or(Value::NONE)),
            ("reverse", []) => {
                let mut reversed = items.borrow().clone();
                reversed.reverse();
                Ok(Value::vector(reversed))
            }
            ("contains?", [value]) => Ok(Value::Bool(
                items.borrow().iter().any(|v| value_eq(v, value)),
            )),
            ("join", [Value::Str(sep)]) => {
                let items = items.borrow().clone();
                let mut parts = Vec::with_capacity(items.len());
                for item in &items {
                    match item {
                        Value::Str(s) => parts.push(s.to_string()),
                        // `join` requires `Vector<string>` (checker
                        // decision in `brasa_typeck::builtins`).
                        _ => {
                            return Err(Signal::Fatal(
                                "brasa: `join` requires a `Vector<string>`".to_string(),
                            ));
                        }
                    }
                }
                Ok(Value::str(parts.join(sep)))
            }
            ("map", [f]) => {
                let snapshot = items.borrow().clone();
                let mut mapped = Vec::with_capacity(snapshot.len());
                for item in snapshot {
                    mapped.push(self.call_value(f.clone(), vec![item])?);
                }
                Ok(Value::vector(mapped))
            }
            ("filter", [f]) => {
                let snapshot = items.borrow().clone();
                let mut kept = Vec::new();
                for item in snapshot {
                    match self.call_value(f.clone(), vec![item.clone()])? {
                        Value::Bool(true) => kept.push(item),
                        Value::Bool(false) => {}
                        _ => {
                            return Err(Signal::Fatal(
                                "brasa: `filter` predicate must return a bool".to_string(),
                            ));
                        }
                    }
                }
                Ok(Value::vector(kept))
            }
            ("each", [f]) => {
                let snapshot = items.borrow().clone();
                for item in snapshot {
                    self.call_value(f.clone(), vec![item])?;
                }
                Ok(Value::Unit)
            }
            ("sortBy", [f]) => {
                let snapshot = items.borrow().clone();
                self.sort_by(snapshot, f.clone())
            }
            _ => Err(self.builtin_error(name)),
        }
    }

    fn sort_by(&mut self, items: Vec<Value>, f: Value) -> EvalResult {
        let mut keyed = Vec::with_capacity(items.len());
        for item in items {
            let key = self.call_value(f.clone(), vec![item.clone()])?;
            match &key {
                Value::Float(v) if v.is_nan() => {
                    return Err(self.panic(
                        PanicKind::AssertionFailed,
                        "cannot sort by a NaN key (floats with NaN do not order)",
                    ));
                }
                Value::Int(_) | Value::Float(_) | Value::Str(_) | Value::Char(_) => {}
                _ => {
                    return Err(Signal::Fatal(
                        "brasa: `sortBy` key must be an int, float, string, or char".to_string(),
                    ));
                }
            }
            keyed.push((key, item));
        }

        keyed.sort_by(|(a, _), (b, _)| value_cmp(a, b).unwrap_or(Ordering::Equal));
        Ok(Value::vector(keyed.into_iter().map(|(_, v)| v).collect()))
    }

    fn map_builtin(&mut self, recv: &Value, name: &str, args: &[Value]) -> EvalResult {
        let Value::Map(entries) = recv else {
            return Err(self.builtin_error(name));
        };

        match (name, args) {
            ("len", []) => Ok(Value::Int(entries.borrow().len() as i64)),
            ("keys", []) => Ok(Value::vector(
                entries.borrow().iter().map(|(k, _)| k.clone()).collect(),
            )),
            ("values", []) => Ok(Value::vector(
                entries.borrow().iter().map(|(_, v)| v.clone()).collect(),
            )),
            ("insert", [key, value]) => {
                let mut entries = entries.borrow_mut();
                match entries.iter_mut().find(|(k, _)| value_eq(k, key)) {
                    Some(entry) => entry.1 = value.clone(),
                    None => entries.push((key.clone(), value.clone())),
                }
                Ok(Value::Unit)
            }
            ("remove", [key]) => {
                let mut entries = entries.borrow_mut();
                match entries.iter().position(|(k, _)| value_eq(k, key)) {
                    Some(index) => Ok(Value::some(entries.remove(index).1)),
                    None => Ok(Value::NONE),
                }
            }
            ("has?", [key]) => Ok(Value::Bool(
                entries.borrow().iter().any(|(k, _)| value_eq(k, key)),
            )),
            ("get", [key]) => Ok(entries
                .borrow()
                .iter()
                .find(|(k, _)| value_eq(k, key))
                .map(|(_, v)| Value::some(v.clone()))
                .unwrap_or(Value::NONE)),
            _ => Err(self.builtin_error(name)),
        }
    }

    fn set_builtin(&mut self, recv: &Value, name: &str, args: &[Value]) -> EvalResult {
        let Value::Set(items) = recv else {
            return Err(self.builtin_error(name));
        };

        match (name, args) {
            ("len", []) => Ok(Value::Int(items.borrow().len() as i64)),
            ("add", [value]) => {
                let mut items = items.borrow_mut();
                if !items.iter().any(|v| value_eq(v, value)) {
                    items.push(value.clone());
                }
                Ok(Value::Unit)
            }
            ("remove", [value]) => {
                let mut items = items.borrow_mut();
                match items.iter().position(|v| value_eq(v, value)) {
                    Some(index) => {
                        items.remove(index);
                        Ok(Value::Bool(true))
                    }
                    None => Ok(Value::Bool(false)),
                }
            }
            ("has?", [value]) => Ok(Value::Bool(
                items.borrow().iter().any(|v| value_eq(v, value)),
            )),
            _ => Err(self.builtin_error(name)),
        }
    }
}

#[cfg(test)]
mod tests {
    //! `Set` methods are exercised directly: the M1 frontend has no
    //! surface constructor for sets yet (`Set([1, 2, 3])` is rejected by
    //! the resolver), so no golden program can reach them.

    use std::cell::RefCell;
    use std::rc::Rc;

    use brasa_hir::Hir;
    use brasa_resolver::Resolutions;
    use brasa_typeck::TypeTables;

    use crate::interp::Interp;
    use crate::value::{Value, value_eq};

    fn set_of(items: Vec<Value>) -> Value {
        Value::Set(Rc::new(RefCell::new(items)))
    }

    #[test]
    fn set_methods_add_remove_query_and_count() {
        let hir = Hir::new();
        let res = Resolutions::default();
        let types = TypeTables::default();
        let mut out = Vec::new();
        let mut interp = Interp::new(&hir, &res, &types, &mut out, 16);

        let set = set_of(vec![Value::Int(1)]);

        let added = interp
            .call_builtin(set.clone(), "add", vec![Value::Int(2)])
            .expect("add succeeds");
        assert!(value_eq(&added, &Value::Unit));

        // Adding a structural duplicate is a no-op.
        interp
            .call_builtin(set.clone(), "add", vec![Value::Int(2)])
            .expect("duplicate add succeeds");
        let len = interp
            .call_builtin(set.clone(), "len", vec![])
            .expect("len succeeds");
        assert!(value_eq(&len, &Value::Int(2)));

        let has = interp
            .call_builtin(set.clone(), "has?", vec![Value::Int(1)])
            .expect("has? succeeds");
        assert!(value_eq(&has, &Value::Bool(true)));

        let removed = interp
            .call_builtin(set.clone(), "remove", vec![Value::Int(1)])
            .expect("remove succeeds");
        assert!(value_eq(&removed, &Value::Bool(true)));

        let removed_again = interp
            .call_builtin(set.clone(), "remove", vec![Value::Int(1)])
            .expect("second remove succeeds");
        assert!(value_eq(&removed_again, &Value::Bool(false)));
    }

    #[test]
    fn set_renders_in_insertion_order() {
        let hir = Hir::new();
        let res = Resolutions::default();
        let types = TypeTables::default();
        let mut out = Vec::new();
        let mut interp = Interp::new(&hir, &res, &types, &mut out, 16);

        let set = set_of(vec![Value::Int(2), Value::Int(1)]);
        let text = interp.display(&set).expect("display succeeds");
        assert_eq!(text, "Set([2, 1])");
    }
}
