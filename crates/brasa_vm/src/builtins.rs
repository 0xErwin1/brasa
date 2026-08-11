//! Native builtin implementations, ported from the walker
//! (`brasa_interp::builtins`): every `brasa_bytecode::BUILTINS` entry
//! the code generator can emit, with the walker's exact messages and
//! failure classes. Method-style entries dispatch on the receiver's
//! runtime kind; higher-order entries (`map`, `filter`, `each`,
//! `sortBy`) call back into user code through the VM's bounded
//! reentrant loop.

use std::cmp::Ordering;
use std::rc::Rc;

use brasa_interp::proc_env::{
    env_lookup, merged_env, non_zero_exit_message, run_command, shell_argv, valid_env_name,
};

use crate::value::{OutputValue, Value, value_cmp, value_eq};
use crate::vm::{ASSERTION_FAILED, INTEGER_OVERFLOW, Signal, Vm, VmResult};

/// The canonical qualified name of the native `string` parse error
/// (mirrors `brasa_resolver::STRING_PARSE_ERROR`).
const STRING_PARSE_ERROR: &str = "string.ParseError";

/// The canonical qualified name of the native `string` regex error
/// (mirrors `brasa_resolver::STRING_REGEX_ERROR`).
const STRING_REGEX_ERROR: &str = "string.RegexError";

/// The canonical qualified name of the native `proc` non-zero-exit
/// error (mirrors `brasa_resolver::PROC_NON_ZERO_EXIT`).
const PROC_NON_ZERO_EXIT: &str = "proc.NonZeroExit";

/// The canonical qualified name of the native `proc` spawn error
/// (mirrors `brasa_resolver::PROC_SPAWN_ERROR`).
const PROC_SPAWN_ERROR: &str = "proc.SpawnError";

impl Vm<'_> {
    /// Receiver-less builtins: the prelude printers, `std::math`
    /// members, and the internal failure raisers.
    pub(crate) fn free_builtin(&mut self, name: &str, args: Vec<Value>) -> VmResult {
        match name {
            "puts" | "print" => {
                let [value] = args.as_slice() else {
                    return Err(Signal::Fatal(
                        "brasa: `puts`/`print` take exactly 1 argument".to_string(),
                    ));
                };
                let text = self.display(value)?;
                let result = if name == "puts" {
                    writeln!(self.out, "{text}")
                } else {
                    write!(self.out, "{text}")
                };
                match result {
                    Ok(()) => Ok(Value::Unit),
                    // A closed read end (`brasa ... | head`) is not a
                    // program failure: exit silently like Unix tools.
                    Err(err) if err.kind() == std::io::ErrorKind::BrokenPipe => {
                        Err(Signal::BrokenPipe)
                    }
                    Err(err) => Err(Signal::Fatal(format!(
                        "brasa: failed to write output: {err}"
                    ))),
                }
            }
            "<fatal>" => match args.into_iter().next() {
                Some(Value::Str(message)) => Err(Signal::Fatal(message.to_string())),
                _ => unreachable!("<fatal> always receives a message string"),
            },
            "<assert-failed>" => match args.into_iter().next() {
                Some(Value::Str(detail)) => Err(self.panic(ASSERTION_FAILED, detail.to_string())),
                _ => unreachable!("<assert-failed> always receives a detail string"),
            },
            _ => {
                if let Some(member) = name.strip_prefix("math.") {
                    self.math_call(member, args)
                } else if let Some(member) = name.strip_prefix("proc.") {
                    self.proc_call(member, args)
                } else if let Some(member) = name.strip_prefix("env.") {
                    self.env_call(member, args)
                } else {
                    unreachable!("unknown free builtin `{name}`")
                }
            }
        }
    }

    /// The `std::proc` runners, ported from the walker's `proc_call`
    /// (BRS-32, `docs/spec/05-stdlib.md`): `run`/`tryRun` take an argv
    /// vector or a whitespace-split string, `shell` runs via
    /// `/bin/sh -c`; every runner accepts an optional trailing stdin
    /// string. `run`/`shell` throw `proc.NonZeroExit` on a non-zero
    /// exit; every runner throws `proc.SpawnError` when the child
    /// cannot start.
    fn proc_call(&mut self, name: &str, args: Vec<Value>) -> VmResult {
        if !matches!(name, "run" | "tryRun" | "shell") {
            return Err(Signal::Fatal(format!(
                "brasa: unknown member `{name}` on module `proc`"
            )));
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
                let items = self.heap.vector(*items).borrow().clone();
                let mut argv = Vec::with_capacity(items.len());
                for item in &items {
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
            .map_err(|message| native_error(PROC_SPAWN_ERROR, message))?;

        if name != "tryRun" && output.code != 0 {
            let message = non_zero_exit_message(&shown, &output);
            return Err(native_error(PROC_NON_ZERO_EXIT, message));
        }

        Ok(Value::ProcOutput(Rc::new(OutputValue {
            stdout: Rc::from(output.stdout),
            stderr: Rc::from(output.stderr),
            code: output.code,
        })))
    }

    /// The `std::env` members, ported from the walker's `env_call`
    /// (BRS-32, `docs/spec/05-stdlib.md`): the process environment
    /// merged with the `env.set` overlay.
    fn env_call(&mut self, name: &str, args: Vec<Value>) -> VmResult {
        match (name, args.as_slice()) {
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
                    return Err(Signal::Fatal(format!(
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
                Ok(self.heap.alloc_map(entries))
            }
            ("args", []) => {
                let args = self.script_args.iter().map(Value::str).collect();
                Ok(self.heap.alloc_vector(args))
            }
            ("get" | "set" | "vars" | "args", _) => Err(Signal::Fatal(format!(
                "brasa: invalid argument(s) to `env.{name}`"
            ))),
            _ => Err(Signal::Fatal(format!(
                "brasa: unknown member `{name}` on module `env`"
            ))),
        }
    }

    /// The `std::math` slice executable in M1: f64 semantics
    /// throughout; `abs`, `min`, and `max` also work on ints.
    fn math_call(&mut self, name: &str, args: Vec<Value>) -> VmResult {
        match (name, args.as_slice()) {
            ("sqrt", [Value::Float(v)]) => Ok(Value::Float(v.sqrt())),
            ("floor", [Value::Float(v)]) => Ok(Value::Float(v.floor())),
            ("ceil", [Value::Float(v)]) => Ok(Value::Float(v.ceil())),
            ("round", [Value::Float(v)]) => Ok(Value::Float(v.round())),
            ("pow", [Value::Float(a), Value::Float(b)]) => Ok(Value::Float(a.powf(*b))),
            ("abs", [Value::Float(v)]) => Ok(Value::Float(v.abs())),
            ("abs", [Value::Int(v)]) => v
                .checked_abs()
                .map(Value::Int)
                .ok_or_else(|| self.panic(INTEGER_OVERFLOW, "integer overflow in `math.abs`")),
            ("min", [Value::Int(a), Value::Int(b)]) => Ok(Value::Int((*a).min(*b))),
            ("max", [Value::Int(a), Value::Int(b)]) => Ok(Value::Int((*a).max(*b))),
            ("min", [Value::Float(a), Value::Float(b)]) => Ok(Value::Float(a.min(*b))),
            ("max", [Value::Float(a), Value::Float(b)]) => Ok(Value::Float(a.max(*b))),
            ("sqrt" | "floor" | "ceil" | "round" | "pow" | "abs" | "min" | "max", _) => Err(
                Signal::Fatal(format!("brasa: invalid argument(s) to `math.{name}`")),
            ),
            _ => Err(Signal::Fatal(format!(
                "brasa: unknown member `{name}` on module `math`"
            ))),
        }
    }

    /// Method-style builtins, dispatched on the receiver's runtime
    /// kind exactly like the walker's `call_builtin`.
    pub(crate) fn method_builtin(&mut self, name: &str, recv: Value, args: Vec<Value>) -> VmResult {
        // The universal derived `toString` applies to every type; a
        // struct's own method wins inside `display` via the shape.
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
                proc_output_builtin(&output, name, &args)
            }
            _ => Err(builtin_error(name)),
        }
    }

    fn int_builtin(&mut self, v: i64, name: &str, args: &[Value]) -> VmResult {
        match (name, args) {
            ("toFloat", []) => Ok(Value::Float(v as f64)),
            ("toString", []) => Ok(Value::str(v.to_string())),
            _ => Err(builtin_error(name)),
        }
    }

    fn float_builtin(&mut self, v: f64, name: &str, args: &[Value]) -> VmResult {
        match (name, args) {
            ("toInt", []) => Ok(Value::Int(v as i64)),
            ("toString", []) => {
                let text = self.display(&Value::Float(v))?;
                Ok(Value::str(text))
            }
            _ => Err(builtin_error(name)),
        }
    }

    /// Compiles `pattern` through the per-run cache; an invalid pattern
    /// throws the native `string.RegexError`. Mirrors the walker's
    /// `compile_regex` exactly, message included.
    fn compile_regex(&mut self, pattern: &str) -> Result<regex::Regex, Signal> {
        if let Some(re) = self.regex_cache.get(pattern) {
            return Ok(re.clone());
        }

        let re = regex::Regex::new(pattern)
            .map_err(|_| native_error(STRING_REGEX_ERROR, format!("invalid regex {pattern:?}")))?;
        self.regex_cache.insert(pattern.to_string(), re.clone());
        Ok(re)
    }

    fn string_builtin(&mut self, s: &str, name: &str, args: &[Value]) -> VmResult {
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
                Ok(self.heap.alloc_vector(parts))
            }
            ("lines", []) => Ok(self.heap.alloc_vector(s.lines().map(Value::str).collect())),
            ("chars", []) => Ok(self.heap.alloc_vector(s.chars().map(Value::Char).collect())),
            ("bytes", []) => Ok(self
                .heap
                .alloc_vector(s.bytes().map(|b| Value::Int(b as i64)).collect())),
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
                        Ok(Value::some(self.heap.alloc_vector(groups)))
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
                let matches = re.find_iter(s).map(|m| Value::str(m.as_str())).collect();
                Ok(self.heap.alloc_vector(matches))
            }
            ("find", [Value::Str(needle)]) => match s.find(needle.as_ref()) {
                Some(byte_index) => {
                    let char_index = s[..byte_index].chars().count() as i64;
                    Ok(Value::some(Value::Int(char_index)))
                }
                None => Ok(Value::NONE),
            },
            ("toInt", []) => s.parse::<i64>().map(Value::Int).map_err(|_| {
                native_error(STRING_PARSE_ERROR, format!("cannot parse {s:?} as int"))
            }),
            ("toFloat", []) => s.parse::<f64>().map(Value::Float).map_err(|_| {
                native_error(STRING_PARSE_ERROR, format!("cannot parse {s:?} as float"))
            }),
            _ => Err(builtin_error(name)),
        }
    }

    fn vector_builtin(&mut self, recv: &Value, name: &str, args: Vec<Value>) -> VmResult {
        let Value::Vector(items) = recv else {
            return Err(builtin_error(name));
        };
        let items = *items;

        match (name, args.as_slice()) {
            ("len", []) => Ok(Value::Int(self.heap.vector(items).borrow().len() as i64)),
            ("push", [value]) => {
                self.heap.vector(items).borrow_mut().push(value.clone());
                Ok(Value::Unit)
            }
            ("pop", []) => Ok(match self.heap.vector(items).borrow_mut().pop() {
                Some(value) => Value::some(value),
                None => Value::NONE,
            }),
            ("first", []) => Ok(self
                .heap
                .vector(items)
                .borrow()
                .first()
                .map(|v| Value::some(v.clone()))
                .unwrap_or(Value::NONE)),
            ("last", []) => Ok(self
                .heap
                .vector(items)
                .borrow()
                .last()
                .map(|v| Value::some(v.clone()))
                .unwrap_or(Value::NONE)),
            ("reverse", []) => {
                let mut reversed = self.heap.vector(items).borrow().clone();
                reversed.reverse();
                Ok(self.heap.alloc_vector(reversed))
            }
            ("contains?", [value]) => Ok(Value::Bool(
                self.heap
                    .vector(items)
                    .borrow()
                    .iter()
                    .any(|v| value_eq(&self.heap, v, value)),
            )),
            ("join", [Value::Str(sep)]) => {
                let items = self.heap.vector(items).borrow().clone();
                let mut parts = Vec::with_capacity(items.len());
                for item in &items {
                    match item {
                        Value::Str(s) => parts.push(s.to_string()),
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
                let snapshot = self.heap.vector(items).borrow().clone();
                let mut mapped = Vec::with_capacity(snapshot.len());
                for item in snapshot {
                    mapped.push(self.call_callable(f.clone(), vec![item])?);
                }
                Ok(self.heap.alloc_vector(mapped))
            }
            ("filter", [f]) => {
                let snapshot = self.heap.vector(items).borrow().clone();
                let mut kept = Vec::new();
                for item in snapshot {
                    match self.call_callable(f.clone(), vec![item.clone()])? {
                        Value::Bool(true) => kept.push(item),
                        Value::Bool(false) => {}
                        _ => {
                            return Err(Signal::Fatal(
                                "brasa: `filter` predicate must return a bool".to_string(),
                            ));
                        }
                    }
                }
                Ok(self.heap.alloc_vector(kept))
            }
            ("each", [f]) => {
                let snapshot = self.heap.vector(items).borrow().clone();
                for item in snapshot {
                    self.call_callable(f.clone(), vec![item])?;
                }
                Ok(Value::Unit)
            }
            ("sortBy", [f]) => {
                let snapshot = self.heap.vector(items).borrow().clone();
                self.sort_by(snapshot, f.clone())
            }
            _ => Err(builtin_error(name)),
        }
    }

    fn sort_by(&mut self, items: Vec<Value>, f: Value) -> VmResult {
        let mut keyed = Vec::with_capacity(items.len());
        for item in items {
            let key = self.call_callable(f.clone(), vec![item.clone()])?;
            match &key {
                Value::Float(v) if v.is_nan() => {
                    return Err(self.panic(
                        ASSERTION_FAILED,
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
        Ok(self
            .heap
            .alloc_vector(keyed.into_iter().map(|(_, v)| v).collect()))
    }

    fn map_builtin(&mut self, recv: &Value, name: &str, args: &[Value]) -> VmResult {
        let Value::Map(entries) = recv else {
            return Err(builtin_error(name));
        };
        let entries = *entries;

        match (name, args) {
            ("len", []) => Ok(Value::Int(self.heap.map(entries).borrow().len() as i64)),
            ("keys", []) => {
                let keys = self
                    .heap
                    .map(entries)
                    .borrow()
                    .iter()
                    .map(|(k, _)| k.clone())
                    .collect();
                Ok(self.heap.alloc_vector(keys))
            }
            ("values", []) => {
                let values = self
                    .heap
                    .map(entries)
                    .borrow()
                    .iter()
                    .map(|(_, v)| v.clone())
                    .collect();
                Ok(self.heap.alloc_vector(values))
            }
            ("insert", [key, value]) => {
                let mut entries = self.heap.map(entries).borrow_mut();
                match entries
                    .iter_mut()
                    .find(|(k, _)| value_eq(&self.heap, k, key))
                {
                    Some(entry) => entry.1 = value.clone(),
                    None => entries.push((key.clone(), value.clone())),
                }
                Ok(Value::Unit)
            }
            ("remove", [key]) => {
                let mut entries = self.heap.map(entries).borrow_mut();
                match entries
                    .iter()
                    .position(|(k, _)| value_eq(&self.heap, k, key))
                {
                    Some(index) => Ok(Value::some(entries.remove(index).1)),
                    None => Ok(Value::NONE),
                }
            }
            ("has?", [key]) => Ok(Value::Bool(
                self.heap
                    .map(entries)
                    .borrow()
                    .iter()
                    .any(|(k, _)| value_eq(&self.heap, k, key)),
            )),
            ("get", [key]) => Ok(self
                .heap
                .map(entries)
                .borrow()
                .iter()
                .find(|(k, _)| value_eq(&self.heap, k, key))
                .map(|(_, v)| Value::some(v.clone()))
                .unwrap_or(Value::NONE)),
            _ => Err(builtin_error(name)),
        }
    }

    fn set_builtin(&mut self, recv: &Value, name: &str, args: &[Value]) -> VmResult {
        let Value::Set(items) = recv else {
            return Err(builtin_error(name));
        };
        let items = *items;

        match (name, args) {
            ("len", []) => Ok(Value::Int(self.heap.set(items).borrow().len() as i64)),
            ("add", [value]) => {
                let mut items = self.heap.set(items).borrow_mut();
                if !items.iter().any(|v| value_eq(&self.heap, v, value)) {
                    items.push(value.clone());
                }
                Ok(Value::Unit)
            }
            ("remove", [value]) => {
                let mut items = self.heap.set(items).borrow_mut();
                match items.iter().position(|v| value_eq(&self.heap, v, value)) {
                    Some(index) => {
                        items.remove(index);
                        Ok(Value::Bool(true))
                    }
                    None => Ok(Value::Bool(false)),
                }
            }
            ("has?", [value]) => Ok(Value::Bool(
                self.heap
                    .set(items)
                    .borrow()
                    .iter()
                    .any(|v| value_eq(&self.heap, v, value)),
            )),
            _ => Err(builtin_error(name)),
        }
    }
}

/// The `Output` record's field accessors (BRS-32,
/// `docs/spec/05-stdlib.md`): receiver-only builtins that yield the
/// field value, matching the walker's `proc_output_builtin`.
fn proc_output_builtin(output: &OutputValue, name: &str, args: &[Value]) -> VmResult {
    match (name, args) {
        ("stdout", []) => Ok(Value::Str(output.stdout.clone())),
        ("stderr", []) => Ok(Value::Str(output.stderr.clone())),
        ("code", []) => Ok(Value::Int(output.code)),
        _ => Err(builtin_error(name)),
    }
}

fn builtin_error(name: &str) -> Signal {
    Signal::Fatal(format!("brasa: unknown builtin method `{name}`"))
}

/// Raises a stdlib-native error: an ordinary error signal carrying a
/// [`Value::NativeError`], caught by naming its qualified name or by
/// `_` like any thrown value.
fn native_error(name: &'static str, message: String) -> Signal {
    Signal::Error(Value::NativeError {
        name,
        message: Rc::from(message),
    })
}
