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
//!   string — no trimming, Rust `str::parse` semantics — and throw the
//!   native `string.ParseError` on failure (BRS-41,
//!   `docs/spec/05-stdlib.md`).
//! - `float.toInt` truncates with Rust `as` semantics: saturating at
//!   the `int` bounds, `NaN` becomes `0`.
//! - `reverse`, `map`, `filter`, and `sortBy` return new vectors;
//!   `push`/`pop` mutate in place.
//!
//! M4 string-surface decisions (BRS-31, `docs/spec/05-stdlib.md`):
//!
//! - `bytes` yields the UTF-8 byte values as ints (0..=255).
//!   `string.reverse` reverses Unicode scalars (no grapheme handling,
//!   consistent with `chars`/`len`). `trimStart`/`trimEnd` strip
//!   Unicode whitespace like `trim`.
//! - `padStart(width, pad)`/`padEnd(width, pad)` count Unicode scalars
//!   like `len`; the pad string repeats cyclically and is truncated to
//!   land exactly on `width`; a string already at/over `width` or an
//!   empty `pad` returns the string unchanged.
//! - The regex methods take the pattern as a plain string in Rust
//!   `regex`-crate syntax; an invalid pattern throws the native
//!   `string.RegexError`. Compiled patterns are cached per run, keyed
//!   by the pattern text.
//! - `captures` returns the full match first (group 0), then every
//!   capture group in order; a non-participating group is the empty
//!   string. `replaceRe` replaces every non-overlapping match and
//!   expands `$1`/`${name}` group references (with `$$` as a literal
//!   `$`) in the replacement, the `regex` crate's `replace_all`
//!   semantics. `scan` returns every non-overlapping full match.
//! - `sortBy` is a stable sort; keys must be `int`, `float`, `string`,
//!   or `char`, and a `NaN` float key panics with
//!   `panics.AssertionFailed` (`docs/spec/03-types.md`, float rules).

use std::cmp::Ordering;
use std::rc::Rc;

use brasa_resolver::{
    PROC_NON_ZERO_EXIT, PROC_SPAWN_ERROR, STRING_PARSE_ERROR, STRING_REGEX_ERROR,
};

use crate::interp::{EvalResult, Interp, PanicKind, Signal};
use crate::proc_env::{
    env_lookup, merged_env, non_zero_exit_message, run_command, shell_argv, valid_env_name,
};
use crate::table::{OrderedMap, OrderedSet};
use crate::value::{OutputValue, Value, WalkValue, value_cmp, value_eq};
use crate::{fs_glue, io_glue, json_glue, num_glue, time_glue};

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
            Value::ProcOutput(output) => {
                let output = output.clone();
                self.proc_output_builtin(&output, name, &args)
            }
            Value::Walk(walk) => {
                let walk = walk.clone();
                self.walk_builtin(&walk, name, &args)
            }
            Value::Json(tree) => {
                let tree = tree.clone();
                json_builtin(&tree, name, &args).ok_or_else(|| self.builtin_error(name))
            }
            // The `Json` accessors flatten through `Option<Json>`
            // (BRS-34, `docs/spec/05-stdlib.md`): `None` propagates,
            // except `null?`, which is `false` — absent is not `null`.
            Value::Option(inner) => {
                let inner = inner.clone();
                json_option_builtin(inner.as_deref(), name, &args)
                    .ok_or_else(|| self.builtin_error(name))
            }
            _ => Err(self.builtin_error(name)),
        }
    }

    /// The `Walk` record's field accessors (BRS-66), the same shape as
    /// `proc_output_builtin`.
    fn walk_builtin(&mut self, walk: &WalkValue, name: &str, args: &[Value]) -> EvalResult {
        match (name, args) {
            ("paths", []) => Ok(walk.paths.clone()),
            ("unreadable", []) => Ok(walk.unreadable.clone()),
            _ => Err(self.builtin_error(name)),
        }
    }

    /// The `Output` record's field accessors (BRS-32,
    /// `docs/spec/05-stdlib.md`): receiver-only builtins that yield the
    /// field value, matching the field-read path in `eval_field`.
    fn proc_output_builtin(
        &mut self,
        output: &OutputValue,
        name: &str,
        args: &[Value],
    ) -> EvalResult {
        match (name, args) {
            ("stdout", []) => Ok(Value::Str(output.stdout.clone())),
            ("stderr", []) => Ok(Value::Str(output.stderr.clone())),
            ("code", []) => Ok(Value::Int(output.code)),
            _ => Err(self.builtin_error(name)),
        }
    }

    fn builtin_error(&self, name: &str) -> Signal {
        Signal::Fatal(format!("brasa: unknown builtin method `{name}`"))
    }

    /// `toFixed` asks for a decimal count a `f64` can back; anything
    /// else is a programmer error, so it panics rather than throwing —
    /// the same rule `time.sleep` and `rand.int` follow for arguments
    /// outside their domain.
    fn check_fixed_digits(&self, digits: i64) -> Result<(), Signal> {
        if num_glue::digits_in_range(digits) {
            return Ok(());
        }
        Err(self.panic(
            PanicKind::AssertionFailed,
            format!(
                "`toFixed` takes 0 to {} digits, got {digits}",
                num_glue::MAX_DIGITS
            ),
        ))
    }

    /// Raises a stdlib-native error: an ordinary error signal carrying
    /// a [`Value::NativeError`], caught by naming its qualified name or
    /// by `_` like any thrown value.
    fn native_error(&self, name: &'static str, message: String) -> Signal {
        Signal::Error(Value::NativeError {
            name,
            message: Rc::from(message),
        })
    }

    fn int_builtin(&mut self, v: i64, name: &str, args: &[Value]) -> EvalResult {
        match (name, args) {
            ("toFloat", []) => Ok(Value::Float(v as f64)),
            ("toFixed", [Value::Int(digits)]) => {
                let digits = *digits;
                self.check_fixed_digits(digits)?;
                Ok(Value::str(num_glue::int_to_fixed(v, digits)))
            }
            ("toString", []) => Ok(Value::str(v.to_string())),
            _ => Err(self.builtin_error(name)),
        }
    }

    fn float_builtin(&mut self, v: f64, name: &str, args: &[Value]) -> EvalResult {
        match (name, args) {
            ("toInt", []) => Ok(Value::Int(v as i64)),
            ("toFixed", [Value::Int(digits)]) => {
                let digits = *digits;
                self.check_fixed_digits(digits)?;
                Ok(Value::str(num_glue::float_to_fixed(v, digits)))
            }
            ("toString", []) => {
                let text = self.display(&Value::Float(v))?;
                Ok(Value::str(text))
            }
            _ => Err(self.builtin_error(name)),
        }
    }

    /// Compiles `pattern` through the per-run cache; an invalid pattern
    /// throws the native `string.RegexError`. `regex::Regex` clones
    /// share the compiled program, so handing out clones is cheap.
    fn compile_regex(&mut self, pattern: &str) -> Result<regex::Regex, Signal> {
        if let Some(re) = self.regex_cache.get(pattern) {
            return Ok(re.clone());
        }

        let re = regex::Regex::new(pattern).map_err(|_| {
            self.native_error(STRING_REGEX_ERROR, format!("invalid regex {pattern:?}"))
        })?;
        self.regex_cache.insert(pattern.to_string(), re.clone());
        Ok(re)
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
            ("trimStart", []) => Ok(Value::str(s.trim_start())),
            ("trimEnd", []) => Ok(Value::str(s.trim_end())),
            ("reverse", []) => Ok(Value::str(s.chars().rev().collect::<String>())),
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
            ("bytes", []) => Ok(Value::vector(
                s.bytes().map(|b| Value::Int(b as i64)).collect(),
            )),
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
            ("padStart" | "padEnd", [Value::Int(width), Value::Str(pad)]) => {
                let len = s.chars().count();
                if *width <= len as i64 || pad.is_empty() {
                    return Ok(Value::str(s));
                }

                let missing = *width as usize - len;
                let filler: String = pad.chars().cycle().take(missing).collect();
                let text = if name == "padStart" {
                    format!("{filler}{s}")
                } else {
                    format!("{s}{filler}")
                };
                Ok(Value::str(text))
            }
            ("replace", [Value::Str(from), Value::Str(to)]) => {
                Ok(Value::str(s.replace(from.as_ref(), to.as_ref())))
            }
            ("match?", [Value::Str(pattern)]) => {
                let re = self.compile_regex(pattern)?;
                Ok(Value::Bool(re.is_match(s)))
            }
            ("captures", [Value::Str(pattern)]) => {
                let re = self.compile_regex(pattern)?;
                match re.captures(s) {
                    Some(caps) => {
                        let groups: Vec<Value> = caps
                            .iter()
                            .map(|group| Value::str(group.map_or("", |m| m.as_str())))
                            .collect();
                        Ok(Value::some(Value::vector(groups)))
                    }
                    None => Ok(Value::NONE),
                }
            }
            ("replaceRe", [Value::Str(pattern), Value::Str(with)]) => {
                let re = self.compile_regex(pattern)?;
                Ok(Value::str(re.replace_all(s, with.as_ref())))
            }
            ("scan", [Value::Str(pattern)]) => {
                let re = self.compile_regex(pattern)?;
                Ok(Value::vector(
                    re.find_iter(s).map(|m| Value::str(m.as_str())).collect(),
                ))
            }
            ("find", [Value::Str(needle)]) => match s.find(needle.as_ref()) {
                Some(byte_index) => {
                    let char_index = s[..byte_index].chars().count() as i64;
                    Ok(Value::some(Value::Int(char_index)))
                }
                None => Ok(Value::NONE),
            },
            ("toInt", []) => s.parse::<i64>().map(Value::Int).map_err(|_| {
                self.native_error(STRING_PARSE_ERROR, format!("cannot parse {s:?} as int"))
            }),
            ("toFloat", []) => s.parse::<f64>().map(Value::Float).map_err(|_| {
                self.native_error(STRING_PARSE_ERROR, format!("cannot parse {s:?} as float"))
            }),
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
            ("sort", []) => {
                let snapshot = items.borrow().clone();
                self.sort_natural(snapshot)
            }
            ("reduce", [init, f]) => {
                let snapshot = items.borrow().clone();
                let mut acc = init.clone();
                for item in snapshot {
                    acc = self.call_value(f.clone(), vec![acc, item])?;
                }
                Ok(acc)
            }
            ("find", [f]) => {
                let snapshot = items.borrow().clone();
                for item in snapshot {
                    match self.call_value(f.clone(), vec![item.clone()])? {
                        Value::Bool(true) => return Ok(Value::some(item)),
                        Value::Bool(false) => {}
                        _ => {
                            return Err(Signal::Fatal(
                                "brasa: `find` predicate must return a bool".to_string(),
                            ));
                        }
                    }
                }
                Ok(Value::NONE)
            }
            // `any?` short-circuits on the first `true`, `all?` on the
            // first `false`; the empty vector is `false`/`true`.
            ("any?" | "all?", [f]) => {
                let deciding = name == "any?";
                let snapshot = items.borrow().clone();
                for item in snapshot {
                    match self.call_value(f.clone(), vec![item])? {
                        Value::Bool(found) => {
                            if found == deciding {
                                return Ok(Value::Bool(deciding));
                            }
                        }
                        _ => {
                            return Err(Signal::Fatal(format!(
                                "brasa: `{name}` predicate must return a bool"
                            )));
                        }
                    }
                }
                Ok(Value::Bool(!deciding))
            }
            // Pairs up to the shorter length; the leftovers of the
            // longer vector are dropped.
            ("zip", [Value::Vector(other)]) => {
                let left = items.borrow().clone();
                let right = other.borrow().clone();
                let pairs = left
                    .into_iter()
                    .zip(right)
                    .map(|(a, b)| Value::Tuple(Rc::from(vec![a, b])))
                    .collect();
                Ok(Value::vector(pairs))
            }
            ("flatten", []) => {
                let snapshot = items.borrow().clone();
                let mut flat = Vec::new();
                for item in snapshot {
                    match item {
                        Value::Vector(inner) => flat.extend(inner.borrow().iter().cloned()),
                        _ => {
                            return Err(Signal::Fatal(
                                "brasa: `flatten` requires a `Vector<Vector<...>>`".to_string(),
                            ));
                        }
                    }
                }
                Ok(Value::vector(flat))
            }
            // Structural equality, first occurrence kept, insertion
            // order preserved — the `Set` constructor's dedup rule.
            ("uniq", []) => {
                let snapshot = items.borrow().clone();
                let mut unique: Vec<Value> = Vec::new();
                for item in snapshot {
                    if !unique.iter().any(|seen| value_eq(seen, &item)) {
                        unique.push(item);
                    }
                }
                Ok(Value::vector(unique))
            }
            _ => Err(self.builtin_error(name)),
        }
    }

    /// `sort` in natural ascending order: the elements must satisfy the
    /// same orderable rule as `sortBy` keys, NaN panic included
    /// (BRS-35, `docs/spec/05-stdlib.md`).
    fn sort_natural(&mut self, items: Vec<Value>) -> EvalResult {
        for item in &items {
            match item {
                Value::Float(v) if v.is_nan() => {
                    return Err(self.panic(
                        PanicKind::AssertionFailed,
                        "cannot sort a NaN element (floats with NaN do not order)",
                    ));
                }
                Value::Int(_) | Value::Float(_) | Value::Str(_) | Value::Char(_) => {}
                _ => {
                    return Err(Signal::Fatal(
                        "brasa: `sort` elements must be ints, floats, strings, or chars"
                            .to_string(),
                    ));
                }
            }
        }

        let mut sorted = items;
        sorted.sort_by(|a, b| value_cmp(a, b).unwrap_or(Ordering::Equal));
        Ok(Value::vector(sorted))
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
                entries
                    .borrow_mut()
                    .insert(key.clone(), value.clone(), value_eq);
                Ok(Value::Unit)
            }
            ("remove", [key]) => Ok(entries
                .borrow_mut()
                .remove(key, value_eq)
                .map(Value::some)
                .unwrap_or(Value::NONE)),
            ("has?", [key]) => Ok(Value::Bool(entries.borrow().contains_key(key, value_eq))),
            ("get", [key]) => Ok(entries
                .borrow()
                .get(key, value_eq)
                .map(|v| Value::some(v.clone()))
                .unwrap_or(Value::NONE)),
            ("entries", []) => Ok(Value::vector(
                entries
                    .borrow()
                    .iter()
                    .map(|(k, v)| Value::Tuple(Rc::from(vec![k.clone(), v.clone()])))
                    .collect(),
            )),
            // A NEW map: the receiver's entries, then the argument's,
            // with the argument winning on duplicate keys; neither
            // operand is modified.
            ("merge", [Value::Map(other)]) => {
                let mut merged = entries.borrow().clone();
                for (key, value) in other.borrow().iter() {
                    merged.insert(key.clone(), value.clone(), value_eq);
                }
                Ok(Value::map(merged))
            }
            ("each", [f]) => {
                let snapshot = entries.borrow().entries().to_vec();
                for (key, value) in snapshot {
                    self.call_value(f.clone(), vec![key, value])?;
                }
                Ok(Value::Unit)
            }
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
                items.borrow_mut().add(value.clone(), value_eq);
                Ok(Value::Unit)
            }
            ("remove", [value]) => Ok(Value::Bool(items.borrow_mut().remove(value, value_eq))),
            ("has?", [value]) => Ok(Value::Bool(items.borrow().contains(value, value_eq))),
            // The algebra members return NEW sets in the receiver's
            // insertion order (`union` appends the argument's unseen
            // elements in its order); neither operand is modified.
            ("union", [Value::Set(other)]) => {
                let mut result = items.borrow().clone();
                for value in other.borrow().iter() {
                    result.add(value.clone(), value_eq);
                }
                Ok(Value::set(result))
            }
            ("intersect" | "diff", [Value::Set(other)]) => {
                let other = other.borrow();
                let keep_present = name == "intersect";
                let result: Vec<Value> = items
                    .borrow()
                    .iter()
                    .filter(|v| other.contains(v, value_eq) == keep_present)
                    .cloned()
                    .collect();
                Ok(Value::set(OrderedSet::from_distinct_items(result)))
            }
            _ => Err(self.builtin_error(name)),
        }
    }

    // --- std::proc + std::env (BRS-32, `docs/spec/05-stdlib.md`) -----

    /// The `std::proc` runners: `run`/`tryRun` take an argv vector (the
    /// primary form) or a whitespace-split string (sugar); `shell` runs
    /// the line via `/bin/sh -c`. Every runner accepts an optional
    /// trailing stdin string piped to the child; without it the child
    /// reads an empty stdin. `run`/`shell` throw `proc.NonZeroExit` on
    /// a non-zero exit; every runner throws `proc.SpawnError` when the
    /// child cannot start.
    pub(crate) fn proc_call(&mut self, name: &str, args: Vec<Value>) -> EvalResult {
        if !matches!(name, "run" | "tryRun" | "shell") {
            return Err(self.fatal(format!("brasa: unknown member `{name}` on module `proc`")));
        }

        let invalid = || Signal::Fatal(format!("brasa: invalid argument(s) to `proc.{name}`"));
        let (cmd, stdin) = match args.as_slice() {
            [cmd] => (cmd, None),
            [cmd, Value::Str(text)] => (cmd, Some(text.clone())),
            _ => return Err(invalid()),
        };

        let (argv, shown) = match (name, cmd) {
            ("shell", Value::Str(line)) => (shell_argv(line), line.to_string()),
            ("shell", _) => return Err(invalid()),
            (_, Value::Str(line)) => {
                let argv: Vec<String> = line.split_whitespace().map(str::to_string).collect();
                let shown = argv.join(" ");
                (argv, shown)
            }
            (_, Value::Vector(items)) => {
                let mut argv = Vec::with_capacity(items.borrow().len());
                for item in items.borrow().iter() {
                    match item {
                        Value::Str(s) => argv.push(s.to_string()),
                        _ => return Err(invalid()),
                    }
                }
                let shown = argv.join(" ");
                (argv, shown)
            }
            _ => return Err(invalid()),
        };

        let output = run_command(&argv, stdin.as_deref(), &self.env_overlay)
            .map_err(|message| self.native_error(PROC_SPAWN_ERROR, message))?;

        if name != "tryRun" && output.code != 0 {
            let message = non_zero_exit_message(&shown, &output);
            return Err(self.native_error(PROC_NON_ZERO_EXIT, message));
        }

        Ok(Value::ProcOutput(Rc::new(OutputValue {
            stdout: Rc::from(output.stdout),
            stderr: Rc::from(output.stderr),
            code: output.code,
        })))
    }

    /// The `std::env` members: the process environment merged with the
    /// `env.set` overlay (`docs/spec/05-stdlib.md`, BRS-32).
    pub(crate) fn env_call(&mut self, name: &str, args: Vec<Value>) -> EvalResult {
        match (name, args.as_slice()) {
            // A chosen exit is not an error: it unwinds past every
            // handler and the CLI prints nothing
            // (`docs/spec/05-stdlib.md`).
            ("exit", [Value::Int(code)]) => {
                let code = *code;
                if !(0..=255).contains(&code) {
                    return Err(self.panic(
                        PanicKind::AssertionFailed,
                        format!("`env.exit` takes a status of 0 to 255, got {code}"),
                    ));
                }
                Err(Signal::Exit(code as i32))
            }
            ("get", [Value::Str(key)]) => {
                let value = self
                    .env_overlay
                    .get(key.as_ref())
                    .cloned()
                    .or_else(|| env_lookup(key));
                Ok(match value {
                    Some(value) => Value::some(Value::str(value)),
                    None => Value::NONE,
                })
            }
            ("set", [Value::Str(key), Value::Str(value)]) => {
                if !valid_env_name(key) {
                    return Err(self.fatal(format!(
                        "brasa: invalid environment variable name {:?} in `env.set`",
                        key.as_ref()
                    )));
                }
                self.env_overlay.insert(key.to_string(), value.to_string());
                Ok(Value::Unit)
            }
            ("vars", []) => {
                let entries: Vec<(Value, Value)> = merged_env(&self.env_overlay)
                    .into_iter()
                    .map(|(key, value)| (Value::str(key), Value::str(value)))
                    .collect();
                Ok(Value::map(OrderedMap::from_distinct_entries(entries)))
            }
            ("args", []) => Ok(Value::vector(
                self.script_args.iter().map(Value::str).collect(),
            )),
            ("cwd", []) => self.fs_str(fs_glue::cwd()),
            ("cd", [Value::Str(path)]) => self.fs_unit(fs_glue::cd(path)),
            ("get" | "set" | "vars" | "args" | "cwd" | "cd", _) => {
                Err(self.fatal(format!("brasa: invalid argument(s) to `env.{name}`")))
            }
            _ => Err(self.fatal(format!("brasa: unknown member `{name}` on module `env`"))),
        }
    }

    /// The `std::fs` members plus path helpers (BRS-33,
    /// `docs/spec/05-stdlib.md`); all OS behavior lives in the shared
    /// [`fs_glue`], only value construction happens here.
    pub(crate) fn fs_call(&mut self, name: &str, args: Vec<Value>) -> EvalResult {
        match (name, args.as_slice()) {
            ("read", [Value::Str(path)]) => self.fs_str(fs_glue::read(path)),
            ("write", [Value::Str(path), Value::Str(contents)]) => {
                self.fs_unit(fs_glue::write(path, contents))
            }
            ("append", [Value::Str(path), Value::Str(contents)]) => {
                self.fs_unit(fs_glue::append(path, contents))
            }
            ("exists?", [Value::Str(path)]) => Ok(Value::Bool(fs_glue::exists(path))),
            ("isFile?", [Value::Str(path)]) => Ok(Value::Bool(fs_glue::is_file(path))),
            ("isDir?", [Value::Str(path)]) => Ok(Value::Bool(fs_glue::is_dir(path))),
            // The one predicate that must NOT follow the link: it
            // answers about the path, not about its target.
            ("isSymlink?", [Value::Str(path)]) => Ok(Value::Bool(fs_glue::is_symlink(path))),
            ("ls", [Value::Str(path)]) => self.fs_strings(fs_glue::ls(path)),
            ("glob", [Value::Str(pattern)]) => self.fs_strings(fs_glue::glob(pattern)),
            ("walk", [Value::Str(path)]) => self.fs_strings(fs_glue::walk(path, &[])),
            ("walk", [Value::Str(path), Value::Vector(prune)]) => {
                let names = self.prune_names(prune, "walk")?;
                self.fs_strings(fs_glue::walk(path, &names))
            }
            ("tryWalk", [Value::Str(path)]) => self.fs_walk(fs_glue::try_walk(path, &[])),
            ("tryWalk", [Value::Str(path), Value::Vector(prune)]) => {
                let names = self.prune_names(prune, "tryWalk")?;
                self.fs_walk(fs_glue::try_walk(path, &names))
            }
            ("mkdir", [Value::Str(path)]) => self.fs_unit(fs_glue::mkdir(path)),
            ("mkdirAll", [Value::Str(path)]) => self.fs_unit(fs_glue::mkdir_all(path)),
            ("rm", [Value::Str(path)]) => self.fs_unit(fs_glue::rm(path)),
            ("rmAll", [Value::Str(path)]) => self.fs_unit(fs_glue::rm_all(path)),
            ("cp", [Value::Str(from), Value::Str(to)]) => self.fs_unit(fs_glue::cp(from, to)),
            ("mv", [Value::Str(from), Value::Str(to)]) => self.fs_unit(fs_glue::mv(from, to)),
            ("join", [Value::Str(base), Value::Str(part)]) => {
                Ok(Value::str(fs_glue::join(base, part)))
            }
            ("base", [Value::Str(path)]) => Ok(Value::str(fs_glue::base(path))),
            ("dir", [Value::Str(path)]) => Ok(Value::str(fs_glue::dir(path))),
            ("ext", [Value::Str(path)]) => Ok(Value::str(fs_glue::ext(path))),
            ("abs", [Value::Str(path)]) => self.fs_str(fs_glue::abs(path)),
            ("resolve", [Value::Str(path)]) => self.fs_str(fs_glue::resolve(path)),
            (
                "read" | "write" | "append" | "exists?" | "isFile?" | "isDir?" | "ls" | "glob"
                | "walk" | "tryWalk" | "mkdir" | "mkdirAll" | "rm" | "rmAll" | "cp" | "mv" | "join"
                | "base" | "dir" | "ext" | "abs",
                _,
            ) => Err(self.fatal(format!("brasa: invalid argument(s) to `fs.{name}`"))),
            _ => Err(self.fatal(format!("brasa: unknown member `{name}` on module `fs`"))),
        }
    }

    /// The `std::json` members (BRS-34, `docs/spec/05-stdlib.md`); all
    /// JSON behavior lives in the shared [`json_glue`], only value
    /// construction happens here.
    pub(crate) fn json_call(&mut self, name: &str, args: Vec<Value>) -> EvalResult {
        match (name, args.as_slice()) {
            ("parse", [Value::Str(text)]) => match json_glue::parse(text) {
                Ok(tree) => Ok(Value::Json(tree)),
                Err(err) => Err(self.native_error(err.name, err.message)),
            },
            ("stringify", [Value::Json(tree)]) => Ok(Value::str(json_glue::stringify(tree))),
            ("parse" | "stringify", _) => {
                Err(self.fatal(format!("brasa: invalid argument(s) to `json.{name}`")))
            }
            _ => Err(self.fatal(format!("brasa: unknown member `{name}` on module `json`"))),
        }
    }

    /// The `std::io` members (BRS-34, `docs/spec/05-stdlib.md`):
    /// `puts`/`print` mirror the prelude printers, `eprint` writes to
    /// the run's error stream, and the readers consume the run's input
    /// stream through the shared [`io_glue`].
    pub(crate) fn io_call(&mut self, name: &str, args: Vec<Value>) -> EvalResult {
        match (name, args.as_slice()) {
            ("puts" | "print" | "eprint", [value]) => {
                let value = value.clone();
                let text = self.display(&value)?;
                self.write_io(name, &text)
            }
            ("readLine", []) => Ok(match io_glue::read_line(self.input) {
                Some(line) => Value::some(Value::str(line)),
                None => Value::NONE,
            }),
            ("readAll", []) => Ok(Value::str(io_glue::read_all(self.input))),
            ("puts" | "print" | "eprint" | "readLine" | "readAll", _) => {
                Err(self.fatal(format!("brasa: invalid argument(s) to `io.{name}`")))
            }
            _ => Err(self.fatal(format!("brasa: unknown member `{name}` on module `io`"))),
        }
    }

    /// The `std::time` members (BRS-35, `docs/spec/05-stdlib.md`); all
    /// clock and formatting behavior lives in the shared [`time_glue`].
    /// A negative `sleep` duration panics with `panics.AssertionFailed`
    /// (the sortBy-NaN precedent: a programmer error, not a recoverable
    /// scripting error).
    pub(crate) fn time_call(&mut self, name: &str, args: Vec<Value>) -> EvalResult {
        match (name, args.as_slice()) {
            ("now", []) => Ok(Value::Float(time_glue::now_seconds())),
            ("nowMillis", []) => Ok(Value::Int(time_glue::now_millis())),
            ("sleep", [Value::Int(ms)]) => {
                if *ms < 0 {
                    return Err(self.panic(
                        PanicKind::AssertionFailed,
                        format!("cannot sleep a negative duration ({ms} ms)"),
                    ));
                }
                time_glue::sleep_ms(*ms as u64);
                Ok(Value::Unit)
            }
            ("iso", [Value::Int(millis)]) => Ok(Value::str(time_glue::iso_utc(*millis))),
            ("now" | "nowMillis" | "sleep" | "iso", _) => {
                Err(self.fatal(format!("brasa: invalid argument(s) to `time.{name}`")))
            }
            _ => Err(self.fatal(format!("brasa: unknown member `{name}` on module `time`"))),
        }
    }

    /// The `std::rand` members (BRS-35, `docs/spec/05-stdlib.md`),
    /// backed by the shared per-run PRNG ([`crate::rand_glue`]).
    /// Picking from an empty range or vector panics with
    /// `panics.AssertionFailed`; `shuffle` returns a NEW vector.
    pub(crate) fn rand_call(&mut self, name: &str, args: Vec<Value>) -> EvalResult {
        match (name, args.as_slice()) {
            ("seed", [Value::Int(n)]) => {
                self.rng = crate::rand_glue::Rng::seeded(*n as u64);
                Ok(Value::Unit)
            }
            ("int", [Value::Range { lo, hi, inclusive }]) => {
                match self.rng.int_in(*lo, *hi, *inclusive) {
                    Some(value) => Ok(Value::Int(value)),
                    None => Err(self.panic(
                        PanicKind::AssertionFailed,
                        "cannot pick from an empty range",
                    )),
                }
            }
            ("float", []) => Ok(Value::Float(self.rng.float())),
            ("choice", [Value::Vector(items)]) => {
                let items = items.borrow();
                if items.is_empty() {
                    return Err(self.panic(
                        PanicKind::AssertionFailed,
                        "cannot pick from an empty vector",
                    ));
                }
                let index = self.rng.below(items.len() as u64) as usize;
                Ok(items[index].clone())
            }
            ("shuffle", [Value::Vector(items)]) => {
                let mut shuffled = items.borrow().clone();
                self.rng.shuffle(&mut shuffled);
                Ok(Value::vector(shuffled))
            }
            ("seed" | "int" | "float" | "choice" | "shuffle", _) => {
                Err(self.fatal(format!("brasa: invalid argument(s) to `rand.{name}`")))
            }
            _ => Err(self.fatal(format!("brasa: unknown member `{name}` on module `rand`"))),
        }
    }

    /// One printer write: `puts` appends a newline, `eprint` targets
    /// stderr. A closed read end is a silent exit on every stream,
    /// like the prelude printers.
    fn write_io(&mut self, name: &str, text: &str) -> EvalResult {
        let result = match name {
            "puts" => writeln!(self.out, "{text}"),
            "print" => write!(self.out, "{text}"),
            _ => write!(self.err, "{text}"),
        };

        match result {
            Ok(()) => Ok(Value::Unit),
            Err(err) if err.kind() == std::io::ErrorKind::BrokenPipe => Err(Signal::BrokenPipe),
            Err(err) => Err(self.fatal(format!("brasa: failed to write output: {err}"))),
        }
    }

    fn fs_signal(&self, err: fs_glue::FsError) -> Signal {
        self.native_error(err.name, err.message)
    }

    fn fs_str(&mut self, result: fs_glue::FsResult<String>) -> EvalResult {
        result.map(Value::str).map_err(|err| self.fs_signal(err))
    }

    fn fs_unit(&mut self, result: fs_glue::FsResult<()>) -> EvalResult {
        result
            .map(|()| Value::Unit)
            .map_err(|err| self.fs_signal(err))
    }

    fn fs_strings(&mut self, result: fs_glue::FsResult<Vec<String>>) -> EvalResult {
        result
            .map(|items| Value::vector(items.into_iter().map(Value::str).collect()))
            .map_err(|err| self.fs_signal(err))
    }

    /// The directory names a `walk`/`tryWalk` prune argument carries.
    fn prune_names(
        &mut self,
        prune: &std::rc::Rc<std::cell::RefCell<Vec<Value>>>,
        member: &str,
    ) -> EvalResult<Vec<String>> {
        let items = prune.borrow().clone();

        let mut names = Vec::with_capacity(items.len());
        for item in &items {
            match item {
                Value::Str(name) => names.push(name.to_string()),
                _ => return Err(self.builtin_error(member)),
            }
        }

        Ok(names)
    }

    /// Builds the `Walk` record (BRS-66) from what the traversal
    /// reached and what it could not read.
    fn fs_walk(&mut self, result: fs_glue::FsResult<(Vec<String>, Vec<String>)>) -> EvalResult {
        result
            .map(|(paths, unreadable)| {
                Value::Walk(std::rc::Rc::new(WalkValue {
                    paths: Value::vector(paths.into_iter().map(Value::str).collect()),
                    unreadable: Value::vector(unreadable.into_iter().map(Value::str).collect()),
                }))
            })
            .map_err(|err| self.fs_signal(err))
    }
}

/// The `Json` accessors (BRS-34, `docs/spec/05-stdlib.md`), pure over
/// the shared tree; `None` means the name is not a `Json` builtin (the
/// caller reports it).
fn json_builtin(tree: &json_glue::JsonValue, name: &str, args: &[Value]) -> Option<Value> {
    if !args.is_empty() {
        return None;
    }

    let some_or_none = |value: Option<Value>| value.map(Value::some).unwrap_or(Value::NONE);

    Some(match name {
        "asString" => some_or_none(json_glue::as_string(tree).map(Value::str)),
        "asInt" => some_or_none(json_glue::as_int(tree).map(Value::Int)),
        "asFloat" => some_or_none(json_glue::as_float(tree).map(Value::Float)),
        "asBool" => some_or_none(json_glue::as_bool(tree).map(Value::Bool)),
        "asArray" => some_or_none(
            json_glue::as_array(tree)
                .map(|items| Value::vector(items.into_iter().map(Value::Json).collect())),
        ),
        "asObject" => some_or_none(json_glue::as_object(tree).map(|members| {
            let entries = members
                .into_iter()
                .map(|(key, member)| (Value::str(key), Value::Json(member)))
                .collect();
            Value::map(OrderedMap::from_distinct_entries(entries))
        })),
        "null?" => Value::Bool(json_glue::is_null(tree)),
        _ => return None,
    })
}

/// The `Json` accessors on an `Option<Json>` receiver: `Some` unwraps
/// and delegates, `None` propagates — except `null?`, which is `false`
/// (an absent member is not an explicit JSON `null`).
fn json_option_builtin(inner: Option<&Value>, name: &str, args: &[Value]) -> Option<Value> {
    match inner {
        Some(Value::Json(tree)) => json_builtin(tree, name, args),
        None if args.is_empty() => match name {
            "null?" => Some(Value::Bool(false)),
            "asString" | "asInt" | "asFloat" | "asBool" | "asArray" | "asObject" => {
                Some(Value::NONE)
            }
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    //! `Set` methods are exercised directly: the M1 frontend has no
    //! surface constructor for sets yet (`Set([1, 2, 3])` is rejected by
    //! the resolver), so no golden program can reach them.

    use brasa_hir::Hir;
    use brasa_resolver::Resolutions;
    use brasa_typeck::TypeTables;

    use crate::interp::Interp;
    use crate::io_glue::Streams;
    use crate::table::OrderedSet;
    use crate::value::{Value, value_eq};

    fn set_of(items: Vec<Value>) -> Value {
        Value::set(OrderedSet::from_distinct_items(items))
    }

    #[test]
    fn set_methods_add_remove_query_and_count() {
        let hir = Hir::new();
        let res = Resolutions::default();
        let types = TypeTables::default();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut input = std::io::empty();
        let streams = Streams {
            out: &mut out,
            err: &mut err,
            input: &mut input,
        };
        let mut interp = Interp::new(&hir, &res, &types, streams, 16, &[]);

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
        let mut err = Vec::new();
        let mut input = std::io::empty();
        let streams = Streams {
            out: &mut out,
            err: &mut err,
            input: &mut input,
        };
        let mut interp = Interp::new(&hir, &res, &types, streams, 16, &[]);

        let set = set_of(vec![Value::Int(2), Value::Int(1)]);
        let text = interp.display(&set).expect("display succeeds");
        assert_eq!(text, "Set([2, 1])");
    }
}
