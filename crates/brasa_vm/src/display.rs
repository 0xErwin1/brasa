//! Derived `toString` rendering, ported from the walker
//! (`brasa_interp::interp`): structs as `Point { x: 1.0, y: 2.0 }`,
//! enums as `Circle(1.0)` or bare `Dot`, floats always with a decimal
//! point, recursion capped at depth 100. A user struct `toString`
//! override (recorded on the shape) replaces the derived rendering
//! everywhere, including nested positions.

use crate::value::Value;
use crate::vm::{Signal, Vm, VmResult};

/// Maximum nested `toString` depth, an insurance policy against cyclic
/// heap values during rendering.
const MAX_DISPLAY_DEPTH: usize = 100;

impl<'a> Vm<'a> {
    /// Renders a value the way `puts`, `print`, interpolation, and
    /// `.toString()` show it. A top-level string or char prints raw;
    /// inside containers strings print double-quoted (escaped) and
    /// chars single-quoted.
    pub(crate) fn display(&mut self, value: &Value) -> VmResult<String> {
        self.render(value, false, 0)
    }

    fn render(&mut self, value: &Value, quoted: bool, depth: usize) -> VmResult<String> {
        if depth > MAX_DISPLAY_DEPTH {
            return Err(Signal::Fatal(
                "brasa: toString recursion too deep (cyclic value?)".to_string(),
            ));
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
                let parts = self.render_all(items, depth)?;
                Ok(render_tuple(&parts))
            }
            Value::Vector(items) => {
                let items = self.heap.vector(*items).borrow().clone();
                let parts = self.render_all(&items, depth)?;
                Ok(format!("[{}]", parts.join(", ")))
            }
            Value::Set(items) => {
                let items = self.heap.set(*items).borrow().clone();
                let parts = self.render_all(&items, depth)?;
                Ok(format!("Set([{}])", parts.join(", ")))
            }
            Value::Map(entries) => {
                let entries = self.heap.map(*entries).borrow().clone();
                if entries.is_empty() {
                    return Ok("{}".to_string());
                }
                let mut parts = Vec::with_capacity(entries.len());
                for (key, value) in &entries {
                    let key = self.render(key, true, depth + 1)?;
                    let value = self.render(value, true, depth + 1)?;
                    parts.push(format!("{key}: {value}"));
                }
                Ok(format!("{{ {} }}", parts.join(", ")))
            }
            Value::Option(inner) => match inner {
                Some(inner) => {
                    let inner = self.render(inner, true, depth + 1)?;
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

                let fields = self.heap.struct_value(*s).fields.borrow().clone();
                if fields.is_empty() {
                    return Ok(format!("{} {{}}", shape.name));
                }
                let mut parts = Vec::with_capacity(fields.len());
                for (name, field) in shape.fields.iter().zip(fields.iter()) {
                    let field = self.render(field, true, depth + 1)?;
                    parts.push(format!("{name}: {field}"));
                }
                Ok(format!("{} {{ {} }}", shape.name, parts.join(", ")))
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
                let parts = self.render_all(&fields, depth)?;
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
                let stdout = self.render(&Value::Str(output.stdout.clone()), true, depth + 1)?;
                let stderr = self.render(&Value::Str(output.stderr.clone()), true, depth + 1)?;
                Ok(format!(
                    "Output {{ stdout: {stdout}, stderr: {stderr}, code: {} }}",
                    output.code
                ))
            }
            // A `Json` value renders as its compact serialization —
            // the same text `json.stringify` yields, in every position
            // (JSON is its own quoting) — BRS-34,
            // `docs/spec/05-stdlib.md`.
            Value::Json(tree) => Ok(brasa_interp::json_glue::stringify(tree)),
            Value::Caught(_) | Value::Iter(_) => {
                unreachable!("internal values never render")
            }
        }
    }

    fn render_all(&mut self, values: &[Value], depth: usize) -> VmResult<Vec<String>> {
        let mut parts = Vec::with_capacity(values.len());
        for value in values {
            parts.push(self.render(value, true, depth + 1)?);
        }
        Ok(parts)
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
