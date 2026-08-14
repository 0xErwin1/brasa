//! The Debug Adapter Protocol adapter (BRS-119).
//!
//! Thin by construction: the protocol is a serialization concern, and
//! every capability advertised here already exists in
//! [`brasa_vm::debug`] (BRS-117). Nothing in this crate decides
//! debugger behaviour — it decides how to spell it.
//!
//! # What it does not do
//!
//! No expression evaluator. `evaluate` answers a plain variable read
//! and nothing else: running arbitrary Brasa in a paused frame needs
//! the checker over a partial program plus a re-entrant VM, which is a
//! much larger promise than a first version should make.
//!
//! # Why the module lives on the stack
//!
//! A session borrows the module for as long as it runs, so an adapter
//! that stored both would be self-referential. Instead the loop is in
//! two phases: read messages until `launch` names a program, compile
//! it, then run the rest of the conversation with the module borrowed
//! from the enclosing frame. No `unsafe`, no `Rc`, and the borrow
//! checker enforces the lifetime the protocol already implies.

pub mod wire;

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use brasa_bytecode::Module;
use brasa_source::SourceMap;
use brasa_vm::debug::{Session, Stop};
use serde_json::{Value, json};

use wire::Connection;

/// Serves one debug session over stdio until the client disconnects.
pub fn run() -> std::io::Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();

    let mut conn = Connection::new(BufReader::new(stdin.lock()), stdout.lock());
    serve(&mut conn)
}

/// The two-phase conversation, over any transport (the tests use a
/// pipe pair rather than the process's stdio).
pub fn serve<R: BufRead, W: Write>(conn: &mut Connection<R, W>) -> std::io::Result<()> {
    let Some(program) = configure(conn)? else {
        return Ok(());
    };

    let mut sources = SourceMap::new();
    let loaded = brasa_module::load(&program, &mut sources);

    match compile(&loaded) {
        Some(module) => debug_loop(conn, &module, &sources),
        None => {
            // A program that does not compile has no session. Say so as
            // an event the client shows, then end: pretending to launch
            // and stopping nowhere would look like a debugger bug.
            conn.event(
                "output",
                json!({
                    "category": "stderr",
                    "output": format!("brasa: `{}` did not compile\n", program.display()),
                }),
            )?;
            conn.event("terminated", json!({}))
        }
    }
}

/// Phase one: `initialize` and `launch`. Returns the program to debug,
/// or `None` if the client left first.
fn configure<R: BufRead, W: Write>(
    conn: &mut Connection<R, W>,
) -> std::io::Result<Option<PathBuf>> {
    while let Some(request) = conn.read()? {
        match request.command.as_str() {
            "initialize" => {
                conn.respond(&request, capabilities())?;
                // The client may configure breakpoints from here on.
                conn.event("initialized", json!({}))?;
            }
            "launch" | "attach" => {
                let program = request.arguments["program"].as_str().map(PathBuf::from);

                match program {
                    Some(program) => {
                        conn.respond(&request, json!({}))?;
                        return Ok(Some(program));
                    }
                    None => conn.respond_error(&request, "launch needs a `program` path")?,
                }
            }
            "disconnect" | "terminate" => {
                conn.respond(&request, json!({}))?;
                return Ok(None);
            }
            _ => conn.respond(&request, json!({}))?,
        }
    }

    Ok(None)
}

/// What this adapter can do. Every flag here is backed by BRS-117; an
/// adapter that advertised more would be promising the editor
/// something no layer implements.
fn capabilities() -> Value {
    json!({
        "supportsConfigurationDoneRequest": true,
        "supportsStepBack": false,
        "supportsRestartRequest": false,
        "supportsSetVariable": false,
        // No conditional breakpoints: a condition is an expression, and
        // there is no evaluator (see the module docs).
        "supportsConditionalBreakpoints": false,
        "supportsEvaluateForHovers": true,
    })
}

fn compile(program: &brasa_module::Program) -> Option<Module> {
    if program
        .diagnostics
        .iter()
        .any(|d| d.severity == brasa_diagnostics::Severity::Error)
    {
        return None;
    }

    let roots = program.all_roots();

    let import_maps: Vec<_> = program
        .modules
        .iter()
        .map(|module| module.imports.clone())
        .collect();
    let views: Vec<_> = program
        .modules
        .iter()
        .zip(&import_maps)
        .map(|(module, imports)| brasa_resolver::ModuleView {
            name: &module.name,
            roots: &module.roots,
            imports,
        })
        .collect();

    let resolved = brasa_resolver::resolve_program(&program.hir, &views);
    let checked = brasa_typeck::check(
        &program.hir,
        &roots,
        &resolved.resolutions,
        &program.sugar_origins,
    );
    let inferred =
        brasa_errorset::infer(&program.hir, &roots, &resolved.resolutions, &checked.types);

    let dirty = |diagnostics: &[brasa_diagnostics::Diagnostic]| {
        diagnostics
            .iter()
            .any(|d| d.severity == brasa_diagnostics::Severity::Error)
    };
    if dirty(&resolved.diagnostics) || dirty(&checked.diagnostics) || dirty(&inferred.diagnostics) {
        return None;
    }

    let entry = &program.module(program.entry).roots;
    let compiled = brasa_codegen::compile_program(
        &program.hir,
        &roots,
        entry,
        &resolved.resolutions,
        &checked.types,
    );

    (!dirty(&compiled.diagnostics)).then_some(compiled.module)
}

/// Phase two: the session, with the module borrowed from the caller.
fn debug_loop<R: BufRead, W: Write>(
    conn: &mut Connection<R, W>,
    module: &Module,
    sources: &SourceMap,
) -> std::io::Result<()> {
    let mut err = std::io::stderr();
    let mut input = std::io::empty();
    let mut out = Vec::new();

    let streams = brasa_runtime::Streams {
        out: &mut out,
        err: &mut err,
        input: &mut input,
    };

    let mut session = Session::new(module, streams, &[]);
    let mut running = false;

    while let Some(request) = conn.read()? {
        match request.command.as_str() {
            "setBreakpoints" => {
                let body = set_breakpoints(&mut session, sources, &request.arguments);
                conn.respond(&request, body)?;
            }
            // DAP is thread-oriented; a single-threaded VM answers with
            // one synthetic thread so the client has something to attach
            // its stack view to.
            "threads" => conn.respond(
                &request,
                json!({ "threads": [{ "id": 1, "name": "brasa" }] }),
            )?,
            "configurationDone" => {
                conn.respond(&request, json!({}))?;
                running = true;
                let stop = session.resume();
                report(conn, sources, &session, &stop)?;
            }
            "continue" | "next" | "stepIn" | "stepOut" => {
                conn.respond(&request, json!({ "allThreadsContinued": true }))?;

                if !running {
                    running = true;
                }

                let stop = match request.command.as_str() {
                    "continue" => session.resume(),
                    "next" => session.step_over(),
                    "stepIn" => session.step_in(),
                    _ => session.step_out(),
                };
                report(conn, sources, &session, &stop)?;
            }
            "stackTrace" => conn.respond(&request, stack_trace(&session, sources))?,
            "scopes" => conn.respond(&request, scopes(&request.arguments))?,
            "variables" => conn.respond(&request, variables(&session, &request.arguments))?,
            "evaluate" => match evaluate(&session, &request.arguments) {
                Some(body) => conn.respond(&request, body)?,
                None => conn.respond_error(&request, "only a plain variable read is supported")?,
            },
            "disconnect" | "terminate" => {
                conn.respond(&request, json!({}))?;
                return Ok(());
            }
            _ => conn.respond(&request, json!({}))?,
        }
    }

    Ok(())
}

/// Tells the client where the run stopped, or that it ended.
fn report<R: BufRead, W: Write>(
    conn: &mut Connection<R, W>,
    sources: &SourceMap,
    session: &Session<'_>,
    stop: &Stop,
) -> std::io::Result<()> {
    match stop {
        Stop::Paused { .. } => {
            let reason = if session.frames().is_empty() {
                "pause"
            } else {
                "breakpoint"
            };

            conn.event(
                "stopped",
                json!({
                    "reason": reason,
                    "threadId": 1,
                    "allThreadsStopped": true,
                }),
            )
        }
        Stop::Finished(_) => {
            let _ = sources;
            conn.event("terminated", json!({}))
        }
    }
}

fn set_breakpoints(session: &mut Session<'_>, sources: &SourceMap, arguments: &Value) -> Value {
    let path = arguments["source"]["path"].as_str().unwrap_or_default();
    let file = file_of(sources, Path::new(path));

    // A `setBreakpoints` is authoritative for its file: the client
    // sends the full set every time, so anything previously set there
    // and not re-sent has been removed in the editor.
    for (func, ip) in session.breakpoints() {
        session.clear_breakpoint(func, ip);
    }

    let mut verified = Vec::new();

    for entry in arguments["breakpoints"].as_array().unwrap_or(&Vec::new()) {
        let line = entry["line"].as_u64().unwrap_or(0) as usize;

        let resolved = file
            .and_then(|file| line_range(sources, file, line).map(|range| (file, range)))
            .and_then(|(file, (start, end))| session.resolve_range(file, start, end));

        match resolved {
            Some((func, ip)) => {
                session.set_breakpoint(func, ip);
                verified.push(json!({ "verified": true, "line": line }));
            }
            // Unverified rather than an error: a line with no code is an
            // ordinary thing to click on, and the editor greys the
            // marker instead of showing a failure.
            None => verified.push(json!({ "verified": false, "line": line })),
        }
    }

    json!({ "breakpoints": verified })
}

/// The byte range a 1-based line covers.
fn line_range(sources: &SourceMap, file: brasa_source::FileId, line: usize) -> Option<(u32, u32)> {
    if line == 0 {
        return None;
    }

    let source = sources.get(&file);
    let start = source.line_starts.get(line - 1)?.0;
    let end = source
        .line_starts
        .get(line)
        .map(|next| next.0)
        .unwrap_or_else(|| source.len_bytes());

    Some((start, end))
}

fn file_of(sources: &SourceMap, path: &Path) -> Option<brasa_source::FileId> {
    let canonical = std::fs::canonicalize(path);
    let canonical = canonical.as_deref().unwrap_or(path);

    sources
        .lookup_by_path(canonical)
        .or_else(|| sources.lookup_by_path(path))
}

fn stack_trace(session: &Session<'_>, sources: &SourceMap) -> Value {
    let frames = session.frames();

    // DAP wants innermost first; the substrate reports outermost first.
    let rendered: Vec<Value> = frames
        .iter()
        .enumerate()
        .rev()
        .map(|(ix, frame)| {
            let (line, column) = sources.display_line_col(&frame.span.file, frame.span.start);
            let path = sources.get(&frame.span.file).path.clone();

            json!({
                "id": ix,
                "name": frame.name,
                "line": line,
                "column": column,
                "source": {
                    "name": path.file_name().map(|n| n.to_string_lossy().into_owned()),
                    "path": path,
                },
            })
        })
        .collect();

    json!({ "totalFrames": rendered.len(), "stackFrames": rendered })
}

/// One scope per frame, `Locals`. The reference encodes the frame so
/// `variables` can find it without any state of its own.
fn scopes(arguments: &Value) -> Value {
    let frame = arguments["frameId"].as_u64().unwrap_or(0);

    json!({
        "scopes": [{
            "name": "Locals",
            "variablesReference": frame + 1,
            "expensive": false,
        }],
    })
}

fn variables(session: &Session<'_>, arguments: &Value) -> Value {
    let reference = arguments["variablesReference"].as_u64().unwrap_or(0);
    if reference == 0 {
        return json!({ "variables": [] });
    }

    let frames = session.frames();
    let Some(frame) = frames.get((reference - 1) as usize) else {
        return json!({ "variables": [] });
    };

    let variables: Vec<Value> = frame
        .locals
        .iter()
        .enumerate()
        .map(|(slot, view)| {
            let value = view
                .as_ref()
                .map(|v| v.summary.clone())
                .unwrap_or_else(|| "<unset>".to_string());

            json!({
                "name": format!("slot {slot}"),
                "value": value,
                // Zero means "no children to expand". Reads stop after
                // one level (BRS-117), and the children are already in
                // the summary line's own view.
                "variablesReference": 0,
            })
        })
        .collect();

    json!({ "variables": variables })
}

/// `evaluate` for a plain variable read only.
///
/// A slot name is what this adapter names variables, so an expression
/// of any other shape is refused rather than guessed at — a debugger
/// that silently answers the wrong question is worse than one that
/// says it cannot.
fn evaluate(session: &Session<'_>, arguments: &Value) -> Option<Value> {
    let expression = arguments["expression"].as_str()?.trim();
    let slot: usize = expression.strip_prefix("slot ")?.trim().parse().ok()?;

    let frames = session.frames();
    let frame = match arguments["frameId"].as_u64() {
        Some(id) => frames.get(id as usize)?,
        None => frames.last()?,
    };

    let view = frame.locals.get(slot)?.as_ref()?;

    Some(json!({ "result": view.summary, "variablesReference": 0 }))
}
