//! Derived `toString` rendering: structs as `Point { x: 1.0, y: 2.0 }`,
//! enums as `Circle(1.0)` or bare `Dot`, floats always with a decimal
//! point, and cyclic values rendered as [`CYCLE_MARKER`]. A user struct
//! `toString` override (recorded on the shape) replaces the derived
//! rendering everywhere, including nested positions.

use crate::heap::GcRef;
use crate::value::Value;
use crate::vm::{Signal, Vm, VmResult};

/// Maximum nesting `toString` renders, a bound on the host stack.
///
/// A back-edge is normally caught first and exactly, by
/// [`Vm::render_cell`]. The exception is a cycle whose period exceeds
/// this limit: the path detector needs to come all the way around
/// before it sees a repeat, so the nesting check trips first. The
/// message therefore reports the nesting it measured and does not claim
/// to know whether a cycle caused it.
const MAX_DISPLAY_DEPTH: usize = 10_000;

/// What `toString` renders in place of a value already being rendered
/// further up the current path. Reads as a marker rather than as data,
/// like the other non-representable renderings (`<lambda>`,
/// `<bound method>`).
const CYCLE_MARKER: &str = "<cycle>";

impl<'a> Vm<'a> {
    /// Renders a value the way `puts`, `print`, interpolation, and
    /// `.toString()` show it. A top-level string or char prints raw;
    /// inside containers strings print double-quoted (escaped) and
    /// chars single-quoted.
    pub(crate) fn display(&mut self, value: &Value) -> VmResult<String> {
        self.render(value, false, 0, &mut Vec::new())
    }

    /// Renders one arena cell (`docs/spec/07-bytecode.md`: every
    /// reference cycle passes through a Vector, Map, Set, or Struct),
    /// emitting [`CYCLE_MARKER`] instead of recursing when the cell is
    /// already being rendered further up the current path. The path is
    /// popped on the way out, so a value that merely appears twice as a
    /// sibling still renders in full.
    ///
    /// `value` is the cell's own value, rooted for the duration: a
    /// nested `toString` override reenters compiled code, which can
    /// collect (BRS-62), and `path` holds bare [`GcRef`]s. Rooting here
    /// is what keeps every entry in `path` naming the cell it was
    /// recorded for instead of a swept slot some later allocation
    /// recycled. The arena-backed kinds are the only ones that reach
    /// this function, and the only ones a sweep can touch, so nothing
    /// is gained by rooting scalars on the way past.
    ///
    /// This one root is unpinned, unlike the other three in this file:
    /// observing it would need the recycled slot to be occupied by a
    /// cell that is itself rendered while the stale entry is still on
    /// the path, and no stable program arranges that. It stays because
    /// a stale reference in `path` is a wrong answer, not a slow one.
    fn render_cell(
        &mut self,
        value: &Value,
        cell: GcRef,
        path: &mut Vec<GcRef>,
        render: impl FnOnce(&mut Self, &mut Vec<GcRef>) -> VmResult<String>,
    ) -> VmResult<String> {
        if path.contains(&cell) {
            return Ok(CYCLE_MARKER.to_string());
        }

        path.push(cell);
        let rooted = [value.clone()];
        let rendered = self.with_rooted(&rooted, |this| render(this, path));
        path.pop();

        rendered
    }

    fn render(
        &mut self,
        value: &Value,
        quoted: bool,
        depth: usize,
        path: &mut Vec<GcRef>,
    ) -> VmResult<String> {
        if depth > MAX_DISPLAY_DEPTH {
            return Err(Signal::Fatal(format!(
                "brasa: toString nesting deeper than {MAX_DISPLAY_DEPTH} levels"
            )));
        }

        match value {
            Value::Int(v) => Ok(v.to_string()),
            Value::Float(v) => Ok(render_float(*v)),
            Value::Bool(v) => Ok(v.to_string()),
            Value::Unit => Ok("unit".to_string()),
            Value::Char(v) => {
                if quoted {
                    Ok(format!("'{}'", escape_char(*v)))
                } else {
                    Ok(v.to_string())
                }
            }
            Value::Str(v) => {
                if quoted {
                    Ok(format!("\"{}\"", escape_str(v)))
                } else {
                    Ok(v.to_string())
                }
            }
            Value::Range { lo, hi, inclusive } => {
                let op = if *inclusive { "..=" } else { ".." };
                Ok(format!("{lo}{op}{hi}"))
            }
            Value::Tuple(items) => {
                let parts = self.render_all(items, depth, path)?;
                Ok(render_tuple(&parts))
            }
            Value::Vector(cell) => {
                let (cell, items) = (*cell, self.heap.vector(*cell).borrow().clone());
                self.render_cell(value, cell, path, |this, path| {
                    let parts = this.render_all(&items, depth, path)?;
                    Ok(format!("[{}]", parts.join(", ")))
                })
            }
            Value::Set(cell) => {
                let (cell, items) = (*cell, self.heap.set(*cell).borrow().items().to_vec());
                self.render_cell(value, cell, path, |this, path| {
                    let parts = this.render_all(&items, depth, path)?;
                    Ok(format!("Set([{}])", parts.join(", ")))
                })
            }
            Value::Map(cell) => {
                let (cell, entries) = (*cell, self.heap.map(*cell).borrow().entries().to_vec());
                if entries.is_empty() {
                    return Ok("{}".to_string());
                }
                let rooted: Vec<Value> = entries
                    .iter()
                    .flat_map(|(key, value)| [key.clone(), value.clone()])
                    .collect();
                self.render_cell(value, cell, path, |this, path| {
                    this.with_rooted(&rooted, |this| {
                        let mut parts = Vec::with_capacity(entries.len());
                        for (key, value) in &entries {
                            let key = this.render(key, true, depth + 1, path)?;
                            let value = this.render(value, true, depth + 1, path)?;
                            parts.push(format!("{key}: {value}"));
                        }
                        Ok(format!("{{ {} }}", parts.join(", ")))
                    })
                })
            }
            Value::Option(inner) => match inner {
                Some(inner) => {
                    let inner = self.render(inner, true, depth + 1, path)?;
                    Ok(format!("Some({inner})"))
                }
                None => Ok("None".to_string()),
            },
            Value::Struct(s) => {
                let shape = self.module_struct(self.heap.struct_value(*s).shape);
                if let Some(to_string) = shape.to_string {
                    let text = self.call_function(to_string, vec![value.clone()])?;
                    return match text {
                        Value::Str(text) => Ok(text.to_string()),
                        _ => Err(Signal::Fatal(
                            "brasa: `toString` must return a string".to_string(),
                        )),
                    };
                }

                let (cell, fields) = (*s, self.heap.struct_value(*s).fields.borrow().clone());
                if fields.is_empty() {
                    return Ok(format!("{} {{}}", shape.name));
                }
                self.render_cell(value, cell, path, |this, path| {
                    this.with_rooted(&fields, |this| {
                        let mut parts = Vec::with_capacity(fields.len());
                        for (name, field) in shape.fields.iter().zip(fields.iter()) {
                            let field = this.render(field, true, depth + 1, path)?;
                            parts.push(format!("{name}: {field}"));
                        }
                        Ok(format!("{} {{ {} }}", shape.name, parts.join(", ")))
                    })
                })
            }
            Value::Enum(e) => {
                let variant_name = self
                    .module_enum(e.shape)
                    .variants
                    .get(e.variant)
                    .map(|v| v.name.clone())
                    .unwrap_or_else(|| "<variant>".to_string());
                if e.fields.is_empty() {
                    return Ok(variant_name);
                }
                let fields = e.fields.clone();
                let parts = self.render_all(&fields, depth, path)?;
                Ok(format!("{variant_name}({})", parts.join(", ")))
            }
            Value::Func(func) => Ok(format!("<function {}>", self.function(*func).name)),
            Value::Closure(_) => Ok("<lambda>".to_string()),
            Value::BoundMethod(_) | Value::BoundBuiltin(_) => Ok("<bound method>".to_string()),
            // Only the message: the uncaught-error path prepends the
            // nominal tag itself, producing `error: string.ParseError:
            // <message>` without duplication.
            Value::NativeError { message, .. } => Ok(message.to_string()),
            // The `Output` record renders like a struct
            // (`docs/spec/05-stdlib.md`, BRS-32).
            Value::ProcOutput(output) => {
                let stdout =
                    self.render(&Value::Str(output.stdout.clone()), true, depth + 1, path)?;
                let stderr =
                    self.render(&Value::Str(output.stderr.clone()), true, depth + 1, path)?;
                Ok(format!(
                    "Output {{ stdout: {stdout}, stderr: {stderr}, code: {} }}",
                    output.code
                ))
            }
            // The `Walk` record renders like a struct too (BRS-66). Its
            // fields are arena vectors, so the copies are rooted for
            // the nested renders exactly as a struct's fields are.
            Value::Walk(walk) => {
                let fields = [walk.paths.clone(), walk.unreadable.clone()];
                self.with_rooted(&fields, |this| {
                    let paths = this.render(&fields[0], true, depth + 1, path)?;
                    let unreadable = this.render(&fields[1], true, depth + 1, path)?;

                    Ok(format!(
                        "Walk {{ paths: {paths}, unreadable: {unreadable} }}"
                    ))
                })
            }
            // A `Json` value renders as its compact serialization —
            // the same text `json.stringify` yields, in every position
            // (JSON is its own quoting) — BRS-34,
            // `docs/spec/05-stdlib.md`.
            Value::Json(tree) => Ok(brasa_runtime::json_glue::stringify(tree)),
            Value::Caught(_) | Value::Iter(_) => {
                unreachable!("internal values never render")
            }
        }
    }

    fn render_all(
        &mut self,
        values: &[Value],
        depth: usize,
        path: &mut Vec<GcRef>,
    ) -> VmResult<Vec<String>> {
        // `values` was copied out of a container that a `toString`
        // override may empty while the later elements are still
        // pending, so the copy is rooted in its own right (BRS-62).
        self.with_rooted(values, |this| {
            let mut parts = Vec::with_capacity(values.len());
            for value in values {
                parts.push(this.render(value, true, depth + 1, path)?);
            }
            Ok(parts)
        })
    }
}

/// Renders tuple elements in source syntax. A one-element tuple keeps
/// its comma (`(1,)`): bare parentheses around a single expression mean
/// grouping, so without the comma the output would read back as a
/// scalar (`docs/spec/02-grammar.md`).
fn render_tuple(parts: &[String]) -> String {
    if let [only] = parts {
        return format!("({only},)");
    }
    format!("({})", parts.join(", "))
}

/// Floats always show the decimal point: `1.0`, never `1`. `NaN`,
/// `inf`, and exponent forms render as Rust prints them.
fn render_float(v: f64) -> String {
    let text = format!("{v}");
    if v.is_finite() && !text.contains('.') && !text.contains('e') {
        format!("{text}.0")
    } else {
        text
    }
}

fn escape_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            other => out.push(other),
        }
    }
    out
}

fn escape_char(c: char) -> String {
    match c {
        '\'' => "\\'".to_string(),
        '\\' => "\\\\".to_string(),
        '\n' => "\\n".to_string(),
        '\t' => "\\t".to_string(),
        '\r' => "\\r".to_string(),
        other => other.to_string(),
    }
}
