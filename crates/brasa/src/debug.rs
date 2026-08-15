//! `brasa debug` (BRS-118): one question per invocation.
//!
//! Deliberately ahead of the DAP adapter and any TUI. It runs in CI, so
//! a debugger that silently breaks gets caught; an agent can drive it,
//! which replaces the `puts`-and-rerun loop with one command; and it
//! keeps the [`brasa_vm::debug`] API honest before a UI becomes its
//! only caller and its convenience leaks back into the design.
//!
//! No stdin REPL, no interactive prompt, no stepping session held open
//! across commands. A stateful session is the DAP adapter's job and it
//! has a protocol for it.
//!
//! # One report, two renderings
//!
//! Everything this prints goes through [`Report`]. `--json` serialises
//! it and the plain form renders the same value — never a second code
//! path, so the two cannot drift into disagreeing about what happened.

use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use brasa_source::{FileId, SourceMap};
use brasa_vm::debug::{FrameView, Session, Stop};

/// A breakpoint that was never reached.
///
/// Deliberately below the sysexits block the rest of the CLI uses
/// (64+), so it cannot be mistaken for one of them. For a caller this
/// is a different outcome from a clean run: the program ended without
/// the question ever being asked, and reporting 0 would say the
/// opposite.
pub const EXIT_NEVER_HIT: u8 = 3;

#[derive(clap::Args)]
pub struct DebugArgs {
    /// Open the interactive debugger instead of answering one
    /// question. `brasa debug tui <script>`.
    #[command(subcommand)]
    pub mode: Option<Mode>,

    /// Script to debug. Omitted when a mode names its own.
    pub script: Option<PathBuf>,

    /// Where to stop, as `file:line`. Repeatable.
    #[arg(long = "break", value_name = "FILE:LINE")]
    pub breaks: Vec<String>,

    /// Resume this many more times after the first stop, so a
    /// breakpoint inside a loop can be inspected on a later iteration.
    #[arg(long = "continue", default_value_t = 0, value_name = "N")]
    pub continue_hits: u32,

    /// What to print when a breakpoint is hit.
    #[arg(long, value_enum, default_value_t = Dump::Frames)]
    pub dump: Dump,

    /// Machine-readable output — the contract for an agent.
    #[arg(long)]
    pub json: bool,
}

/// A mode of `brasa debug` that is not "answer one question".
#[derive(clap::Subcommand)]
pub enum Mode {
    /// The interactive debugger: source, breakpoints, stepping,
    /// frames, locals, output and the heap, without leaving it.
    Tui {
        /// Script to debug.
        script: PathBuf,
    },
}

#[derive(clap::ValueEnum, Clone, Copy, PartialEq, Eq)]
pub enum Dump {
    /// The call stack: function, position, and source line.
    Frames,
    /// The innermost frame's slots and their values.
    Locals,
    /// The heap: live objects by kind, and what the collector has done
    /// (BRS-120). The one view an editor's panels cannot show.
    Heap,
}

/// What one invocation found, before any rendering decision.
struct Report {
    stopped: bool,
    heap: Option<HeapReport>,
    /// Where it stopped, as `file:line`, when it stopped.
    at: Option<String>,
    function: Option<String>,
    frames: Vec<ReportFrame>,
    locals: Vec<ReportLocal>,
}

struct ReportFrame {
    name: String,
    at: String,
}

struct HeapReport {
    by_kind: Vec<(String, usize)>,
    live_slots: usize,
    free_slots: usize,
    live_bytes: usize,
    peak_bytes: usize,
    allocations: u64,
    collections: u64,
}

struct ReportLocal {
    slot: usize,
    value: Option<String>,
    children: Vec<(String, String)>,
}

pub fn run(args: &DebugArgs) -> ExitCode {
    if let Some(Mode::Tui { script }) = &args.mode {
        return crate::debug_tui::run(script);
    }

    let Some(script) = &args.script else {
        eprintln!("brasa: debug needs a script, or a mode such as `tui`");
        return ExitCode::from(64);
    };

    let mut sources = SourceMap::new();
    let program = brasa_module::load(script, &mut sources);

    let module = match crate::compile_for_debug(&program, &sources) {
        Ok(module) => module,
        Err(code) => return code,
    };

    let mut err = std::io::stderr();
    let mut input = BufReader::new(std::io::stdin());
    let mut out = std::io::stdout();

    let streams = brasa_runtime::Streams {
        out: &mut out,
        err: &mut err,
        input: &mut input,
    };

    let mut session = Session::new(&module, streams, &[]);

    // Resolve every breakpoint before running anything. A position that
    // matches no instruction is a usage error, not a run that quietly
    // never stops — the difference matters most to whoever typed the
    // line number.
    for spec in &args.breaks {
        match resolve(&session, &sources, spec) {
            Ok((func, ip)) => {
                session.set_breakpoint(func, ip);
            }
            Err(message) => {
                eprintln!("brasa: {message}");
                return ExitCode::from(64);
            }
        }
    }

    let mut stop = session.resume();
    for _ in 0..args.continue_hits {
        if matches!(stop, Stop::Finished(_)) {
            break;
        }
        stop = session.resume();
    }

    let report = build_report(&session, &sources, &stop);
    let text = if args.json {
        render_json(&report, args.dump)
    } else {
        render_text(&report, args.dump)
    };
    println!("{text}");

    if report.stopped {
        ExitCode::from(0)
    } else {
        ExitCode::from(EXIT_NEVER_HIT)
    }
}

/// `file:line` against the loaded sources.
///
/// The line is 1-based, as every editor and every diagnostic in this
/// toolchain spells it.
fn resolve(
    session: &Session<'_>,
    sources: &SourceMap,
    spec: &str,
) -> Result<(brasa_bytecode::FuncId, usize), String> {
    let (path, line) = spec
        .rsplit_once(':')
        .ok_or_else(|| format!("`{spec}` is not a `file:line` position"))?;

    let line: usize = line
        .parse()
        .map_err(|_| format!("`{line}` is not a line number in `{spec}`"))?;
    if line == 0 {
        return Err(format!("line numbers start at 1, got `{spec}`"));
    }

    let file = file_of(sources, Path::new(path))
        .ok_or_else(|| format!("`{path}` is not a file this program loads"))?;

    let source = sources.get(&file);
    let start = source
        .line_starts
        .get(line - 1)
        .ok_or_else(|| format!("`{path}` has no line {line}"))?;

    // A line whose first byte is indentation resolves through the first
    // instruction anywhere on it, so `--break` on a normally-indented
    // statement works without the caller counting columns. The scan
    // lives in the substrate so this and the DAP adapter cannot drift.
    let end = source
        .line_starts
        .get(line)
        .map(|next| next.0)
        .unwrap_or_else(|| source.len_bytes());

    session
        .resolve_range(file, start.0, end)
        .ok_or_else(|| format!("no code at `{spec}`"))
}

fn file_of(sources: &SourceMap, path: &Path) -> Option<FileId> {
    let canonical = std::fs::canonicalize(path);
    let canonical = canonical.as_deref().unwrap_or(path);

    sources
        .lookup_by_path(canonical)
        .or_else(|| sources.lookup_by_path(path))
}

fn build_report(session: &Session<'_>, sources: &SourceMap, stop: &Stop) -> Report {
    let Stop::Paused { span, .. } = stop else {
        return Report {
            stopped: false,
            heap: None,
            at: None,
            function: None,
            frames: Vec::new(),
            locals: Vec::new(),
        };
    };

    let frames = session.frames();
    let innermost = frames.last();

    let heap = session.heap();

    Report {
        stopped: true,
        heap: Some(HeapReport {
            by_kind: heap.by_kind.clone(),
            live_slots: heap.live_slots,
            free_slots: heap.free_slots,
            live_bytes: heap.live_bytes,
            peak_bytes: heap.peak_bytes,
            allocations: heap.allocations,
            collections: heap.collections,
        }),
        at: Some(position(sources, *span)),
        function: innermost.map(|frame| frame.name.clone()),
        frames: frames
            .iter()
            .map(|frame| ReportFrame {
                name: frame.name.clone(),
                at: position(sources, frame.span),
            })
            .collect(),
        locals: innermost.map(locals_of).unwrap_or_default(),
    }
}

fn locals_of(frame: &FrameView) -> Vec<ReportLocal> {
    frame
        .locals
        .iter()
        .enumerate()
        .map(|(slot, view)| ReportLocal {
            slot,
            value: view.as_ref().map(|v| v.summary.clone()),
            children: view
                .as_ref()
                .map(|v| v.children.clone())
                .unwrap_or_default(),
        })
        .collect()
}

fn position(sources: &SourceMap, span: brasa_source::Span) -> String {
    let (line, column) = sources.display_line_col(&span.file, span.start);
    let path = sources.get(&span.file).path.display();

    format!("{path}:{line}:{column}")
}

fn render_json(report: &Report, dump: Dump) -> String {
    let body = match dump {
        Dump::Frames => serde_json::json!(
            report
                .frames
                .iter()
                .map(|frame| serde_json::json!({ "function": frame.name, "at": frame.at }))
                .collect::<Vec<_>>()
        ),
        Dump::Heap => match &report.heap {
            Some(heap) => serde_json::json!({
                "liveSlots": heap.live_slots,
                "freeSlots": heap.free_slots,
                "liveBytes": heap.live_bytes,
                "peakBytes": heap.peak_bytes,
                "allocations": heap.allocations,
                "collections": heap.collections,
                "byKind": heap
                    .by_kind
                    .iter()
                    .map(|(kind, count)| serde_json::json!({ "kind": kind, "count": count }))
                    .collect::<Vec<_>>(),
            }),
            None => serde_json::Value::Null,
        },
        Dump::Locals => serde_json::json!(
            report
                .locals
                .iter()
                .map(|local| serde_json::json!({
                    "slot": local.slot,
                    "value": local.value,
                    "children": local
                        .children
                        .iter()
                        .map(|(name, value)| serde_json::json!({ "name": name, "value": value }))
                        .collect::<Vec<_>>(),
                }))
                .collect::<Vec<_>>()
        ),
    };

    let mut value = serde_json::json!({
        "stopped": report.stopped,
        "at": report.at,
        "function": report.function,
    });

    let key = match dump {
        Dump::Frames => "frames",
        Dump::Locals => "locals",
        Dump::Heap => "heap",
    };
    value
        .as_object_mut()
        .expect("the report is an object")
        .insert(key.to_string(), body);

    serde_json::to_string_pretty(&value).expect("a report always serialises")
}

fn render_text(report: &Report, dump: Dump) -> String {
    if !report.stopped {
        return "no breakpoint was hit; the program ran to completion".to_string();
    }

    let at = report.at.as_deref().unwrap_or("<unknown>");
    let function = report.function.as_deref().unwrap_or("<unknown>");
    let mut out = format!("stopped in `{function}` at {at}\n");

    match dump {
        Dump::Frames => {
            for frame in report.frames.iter().rev() {
                out.push_str(&format!("  {} at {}\n", frame.name, frame.at));
            }
        }
        Dump::Heap => {
            if let Some(heap) = &report.heap {
                out.push_str(&format!(
                    "  {} live slots, {} free — {} bytes live, {} peak\n  {} allocations over {} collections\n",
                    heap.live_slots,
                    heap.free_slots,
                    heap.live_bytes,
                    heap.peak_bytes,
                    heap.allocations,
                    heap.collections,
                ));

                for (kind, count) in &heap.by_kind {
                    out.push_str(&format!("    {count:>5}  {kind}\n"));
                }
            }
        }
        Dump::Locals => {
            for local in &report.locals {
                let value = local.value.as_deref().unwrap_or("<unset>");
                out.push_str(&format!("  slot {}: {}\n", local.slot, value));

                for (name, child) in &local.children {
                    out.push_str(&format!("    {name}: {child}\n"));
                }
            }
        }
    }

    out.trim_end().to_string()
}
