//! Brasa CLI: run a `.bras` script, or drop into tooling subcommands
//! (`brasa fmt`, `brasa test`, `brasa bundle`).
//!
//! This binary is also the bundle runtime: a bundled tool is a copy of
//! it with a program appended, so the first thing `main` does is ask
//! whether it is carrying one (see [`bundle`]).
//!
//! Exit codes follow sysexits: 64 usage, 65 bad input, 70 runtime failure.

mod bundle;
mod debug;
mod debug_tui;
mod fmt;
mod manifest;

use std::collections::HashMap;
use std::io::{BufWriter, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

use brasa_diagnostics::{Diagnostic, Severity};
use brasa_hir::ItemId;
use brasa_resolver::ModuleView;
use brasa_source::SourceMap;

#[derive(Parser)]
#[command(
    name = "brasa",
    version,
    about = "The Brasa programming language",
    args_conflicts_with_subcommands = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Subcommand>,

    /// Script to execute.
    script: Option<PathBuf>,

    /// Print the parsed AST to stdout instead of executing the script.
    /// Covers the named file only, not the modules it imports.
    #[arg(long)]
    dump_ast: bool,

    /// Print the lowered HIR to stdout instead of executing the script.
    #[arg(long)]
    dump_hir: bool,

    /// Stop after the static checks (through error-set analysis)
    /// without executing the script; prints nothing on success.
    #[arg(long)]
    check: bool,

    /// Print the inferred error-sets to stdout instead of executing the
    /// script (spec: 04 — Sistema de errores, error-set inference).
    #[arg(long)]
    dump_error_sets: bool,

    /// Print the compiled bytecode disassembly to stdout instead of
    /// executing the script (spec: 07 — Diseño del bytecode).
    #[arg(long)]
    dump_bytecode: bool,

    /// Arguments passed through to the script as `args()`.
    ///
    /// `allow_hyphen_values` is what makes `std::cli` usable at all:
    /// without it clap claims `brasa script.bras --top 5`'s `--top` as
    /// its own and the script can never see a flag.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

#[derive(clap::Subcommand)]
enum Subcommand {
    /// Format Brasa source files.
    Fmt(fmt::FmtArgs),
    /// Run a script's `test` items.
    Test(TestArgs),
    /// Pack a script and everything it imports into one executable.
    Bundle(bundle::BundleArgs),
    /// Debug a script non-interactively (BRS-118).
    ///
    /// One question per invocation: stop at a `file:line`, print the
    /// frames or the locals, exit. `--json` is the contract for an
    /// agent; the plain form renders the same data.
    Debug(debug::DebugArgs),
    /// Profile a script: where the time goes (BRS-121).
    Profile(ProfileArgs),
    /// Serve the Debug Adapter Protocol over stdin/stdout (BRS-119).
    ///
    /// Breakpoints and stepping in VS Code and nvim-dap. Not meant to
    /// be run by hand — an editor starts it.
    Dap,
    /// Run the language server over stdin/stdout (BRS-92).
    ///
    /// Speaks LSP to an editor: diagnostics as you type, and hover
    /// showing the inferred type and error-set. Not meant to be run by
    /// hand — an editor starts it.
    Lsp,
}

#[derive(clap::Args)]
struct ProfileArgs {
    /// Script to profile.
    script: PathBuf,

    /// Sampling interval in microseconds.
    #[arg(long, default_value_t = 500, value_name = "US")]
    interval: u64,

    /// Print collapsed stacks for flamegraph tooling instead of the
    /// report — an instrument, not a viewer.
    #[arg(long)]
    collapsed: bool,
}

#[derive(clap::Args)]
struct TestArgs {
    /// Script whose tests to run. Without one, `[test].globs` from the
    /// nearest `brasa.toml` says which scripts to run, falling back to
    /// its `build.entry`.
    script: Option<PathBuf>,
}

fn main() -> ExitCode {
    // Before anything reads argv: a bundled tool's arguments belong to
    // the program it carries, not to this CLI.
    match bundle::embedded() {
        Ok(Some(payload)) => return bundle::run(&payload),
        Ok(None) => {}
        Err(message) => {
            eprintln!("brasa: {message}");
            return ExitCode::from(70);
        }
    }

    // Everything after the script path belongs to the SCRIPT, `--help`
    // included. Without this split clap claims `brasa tool.bras --help`
    // and a script can never own the one flag every script has — the
    // same way it claimed `--top` before `allow_hyphen_values`.
    let (mine, theirs) = split_at_script(std::env::args());

    let mut cli = Cli::parse_from(mine);
    cli.args = theirs;

    match &cli.command {
        Some(Subcommand::Fmt(args)) => return fmt::run(args),
        Some(Subcommand::Test(args)) => return run_tests(args.script.as_ref()),
        Some(Subcommand::Bundle(args)) => return bundle::write(args),
        Some(Subcommand::Debug(args)) => return debug::run(args),
        Some(Subcommand::Profile(args)) => return run_profile(args),
        Some(Subcommand::Dap) => return run_dap(),
        Some(Subcommand::Lsp) => return run_lsp(),
        None => {}
    }

    // The manifest only fills in what the command line omitted: an
    // explicit script runs exactly as it would with no project around
    // it, aliases aside.
    let found = match manifest::discover() {
        Ok(found) => found,
        Err(code) => return code,
    };

    let entry = found.as_ref().and_then(manifest::Manifest::entry);
    let Some(script) = cli.script.clone().or(entry) else {
        eprintln!("brasa: no script, no subcommand, and no `brasa.toml` with a `build.entry`");
        eprintln!("usage: brasa <script.bras> [args...]   or   brasa fmt [paths...]");
        return ExitCode::from(64);
    };

    let options = load_options(found.as_ref());
    run_script(&cli, &script, &options)
}

/// What the loader gets: the manifest's contribution, or nothing.
fn load_options(found: Option<&manifest::Manifest>) -> brasa_module::LoadOptions {
    found
        .map(manifest::Manifest::load_options)
        .unwrap_or_default()
}

/// Compiles a loaded program to a module for a debug session.
///
/// Gated exactly like a normal run: a script that does not compile
/// cannot be debugged, and reporting its diagnostics is more useful
/// than stopping at a breakpoint in code the compiler rejected.
fn compile_for_debug(
    program: &brasa_module::Program,
    sources: &SourceMap,
) -> Result<brasa_bytecode::Module, ExitCode> {
    match compile_program(program, sources, true, false, Dumps::default()) {
        Compiled::Module(result) => Ok(result.module),
        Compiled::Stopped(code) => Err(ExitCode::from(code)),
    }
}

/// Every phase's diagnostics, ungated, plus the module when it compiled.
fn analyze_for_tui(
    program: &brasa_module::Program,
    sources: &SourceMap,
) -> (Vec<Diagnostic>, Option<brasa_bytecode::Module>) {
    let _ = sources;

    let mut diagnostics = program.diagnostics.clone();
    let roots = program.all_roots();

    let import_maps: Vec<HashMap<ItemId, usize>> = program
        .modules
        .iter()
        .map(|module| module.imports.clone())
        .collect();
    let views: Vec<ModuleView<'_>> = program
        .modules
        .iter()
        .zip(&import_maps)
        .map(|(module, imports)| ModuleView {
            name: &module.name,
            roots: &module.roots,
            imports,
        })
        .collect();

    let resolved = brasa_resolver::resolve_program(&program.hir, &views);
    diagnostics.extend(resolved.diagnostics.clone());

    let checked = brasa_typeck::check(
        &program.hir,
        &roots,
        &resolved.resolutions,
        &program.sugar_origins,
    );
    diagnostics.extend(checked.diagnostics.clone());

    let inferred =
        brasa_errorset::infer(&program.hir, &roots, &resolved.resolutions, &checked.types);
    diagnostics.extend(inferred.diagnostics.clone());

    let failed = diagnostics
        .iter()
        .any(|d| d.severity == brasa_diagnostics::Severity::Error);
    if failed {
        return (diagnostics, None);
    }

    let entry = &program.module(program.entry).roots;
    let compiled = brasa_codegen::compile_program(
        &program.hir,
        &roots,
        entry,
        &resolved.resolutions,
        &checked.types,
    );
    diagnostics.extend(compiled.diagnostics.clone());

    let clean = !compiled
        .diagnostics
        .iter()
        .any(|d| d.severity == brasa_diagnostics::Severity::Error);

    (diagnostics, clean.then_some(compiled.module))
}

/// Profiles a script and prints where its time went.
///
/// The program's own output goes to stdout as usual; the report goes to
/// stderr, so a profiled run still pipes like an unprofiled one.
fn run_profile(args: &ProfileArgs) -> ExitCode {
    let mut sources = SourceMap::new();
    let program = brasa_module::load(&args.script, &mut sources);

    let module = match compile_for_debug(&program, &sources) {
        Ok(module) => module,
        Err(code) => return code,
    };

    let mut out = std::io::stdout();
    let (outcome, profile) = brasa_vm::profile(
        &module,
        &mut out,
        &[],
        std::time::Duration::from_micros(args.interval),
    );

    let text = if args.collapsed {
        profile.collapsed()
    } else {
        profile.report()
    };
    eprintln!("{text}");

    match outcome {
        brasa_runtime::Outcome::Success => ExitCode::from(0),
        _ => ExitCode::from(70),
    }
}

/// Serves the debug adapter until the editor disconnects.
fn run_dap() -> ExitCode {
    match brasa_dap::run() {
        Ok(()) => ExitCode::from(0),
        Err(err) => {
            eprintln!("brasa: debug adapter stopped: {err}");
            ExitCode::from(70)
        }
    }
}

/// Serves the language server until the editor disconnects.
///
/// A transport failure is exit 70 like any other host failure the CLI
/// cannot continue past; there is no diagnostic to render, because the
/// channel a diagnostic would travel on is the thing that broke.
fn run_lsp() -> ExitCode {
    match brasa_lsp::run() {
        Ok(()) => ExitCode::from(0),
        Err(err) => {
            eprintln!("brasa: language server stopped: {err}");
            ExitCode::from(70)
        }
    }
}

/// What the flags asked for, so the pipeline can stop where they say.
#[derive(Default, Clone, Copy)]
struct Dumps {
    hir: bool,
    error_sets: bool,
    bytecode: bool,
    check_only: bool,
}

/// A finished front-to-back compilation, or the reason there is nothing
/// to run: a dump was printed, or something was rejected.
enum Compiled {
    /// Boxed because the two variants are wildly different sizes and
    /// this is returned once per run, so the indirection costs nothing
    /// against carrying a whole module's worth of stack for the
    /// `Stopped` case too.
    Module(Box<brasa_codegen::CompileResult>),
    /// The raw sysexits number rather than an [`ExitCode`], because an
    /// aggregated `brasa test` run compares codes across files to pick
    /// the worst one, and `ExitCode` is deliberately opaque.
    Stopped(u8),
}

/// Splits argv into what this CLI parses and what the script receives.
///
/// The boundary is the first argument that is not an option and is not
/// a subcommand name: that is the script path, and it is the last thing
/// this CLI has an opinion about. A subcommand keeps the whole line,
/// since `brasa fmt --check` is genuinely ours.
fn split_at_script(argv: impl Iterator<Item = String>) -> (Vec<String>, Vec<String>) {
    const SUBCOMMANDS: &[&str] = &[
        "fmt", "test", "bundle", "lsp", "dap", "debug", "profile", "help",
    ];

    let argv: Vec<String> = argv.collect();

    let boundary = argv
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, arg)| !arg.starts_with('-'));

    match boundary {
        Some((at, name)) if !SUBCOMMANDS.contains(&name.as_str()) => {
            (argv[..=at].to_vec(), argv[at + 1..].to_vec())
        }
        _ => (argv, Vec::new()),
    }
}

fn run_script(cli: &Cli, script: &PathBuf, options: &brasa_module::LoadOptions) -> ExitCode {
    let color = std::io::stderr().is_terminal();

    // `--dump-ast` is a single-file view: an AST belongs to one parsed
    // file, and the module loader drops each one as soon as it lowers.
    if cli.dump_ast {
        return dump_ast(script, color);
    }

    let dumps = Dumps {
        hir: cli.dump_hir,
        error_sets: cli.dump_error_sets,
        bytecode: cli.dump_bytecode,
        check_only: cli.check,
    };

    match compile(script, color, false, dumps, options) {
        Compiled::Stopped(code) => ExitCode::from(code),
        Compiled::Module(compiled) => execute(&compiled.module, &cli.args),
    }
}

/// Rejects an entry path that is not a readable regular file, before
/// the loader is asked to treat it as one.
fn reject_entry(script: &PathBuf) -> Option<ExitCode> {
    match std::fs::metadata(script) {
        Ok(metadata) if metadata.is_file() => None,
        Ok(_) => {
            eprintln!("brasa: {} is not a regular file", script.display());
            Some(ExitCode::from(65))
        }
        Err(err) => {
            eprintln!("brasa: cannot read {}: {err}", script.display());
            Some(ExitCode::from(65))
        }
    }
}

/// The whole pipeline over one entry file, stopping wherever `dumps`
/// says to. `with_tests` compiles the entry module's `test` items too.
fn compile(
    script: &PathBuf,
    color: bool,
    with_tests: bool,
    dumps: Dumps,
    options: &brasa_module::LoadOptions,
) -> Compiled {
    if reject_entry(script).is_some() {
        return Compiled::Stopped(65);
    }

    let mut sources = SourceMap::new();
    let program = brasa_module::load_with(script, &mut sources, options);

    compile_program(&program, &sources, color, with_tests, dumps)
}

/// Everything after the module graph exists. Split out because a
/// bundled program arrives already loaded, from bytes rather than from
/// a path, and must then take exactly this path to bytecode.
fn compile_program(
    program: &brasa_module::Program,
    sources: &SourceMap,
    color: bool,
    with_tests: bool,
    dumps: Dumps,
) -> Compiled {
    if let Some(code) = reject(&program.diagnostics, sources, color) {
        return Compiled::Stopped(code);
    }

    let roots = program.all_roots();
    let entry_roots = &program.module(program.entry).roots;

    if dumps.hir {
        println!("{}", brasa_hir::dump::dump(&program.hir, &roots));
        return Compiled::Stopped(0);
    }

    // The loader's post-order list is what the resolver walks; the
    // per-module import maps have to outlive the views that borrow them.
    let import_maps: Vec<HashMap<ItemId, usize>> = program
        .modules
        .iter()
        .map(|module| module.imports.clone())
        .collect();
    let views: Vec<ModuleView<'_>> = program
        .modules
        .iter()
        .zip(&import_maps)
        .map(|(module, imports)| ModuleView {
            name: &module.name,
            roots: &module.roots,
            imports,
        })
        .collect();

    let resolved = brasa_resolver::resolve_program(&program.hir, &views);
    if let Some(code) = reject(&resolved.diagnostics, sources, color) {
        return Compiled::Stopped(code);
    }

    let checked = brasa_typeck::check(
        &program.hir,
        &roots,
        &resolved.resolutions,
        &program.sugar_origins,
    );
    if let Some(code) = reject(&checked.diagnostics, sources, color) {
        return Compiled::Stopped(code);
    }

    let inferred =
        brasa_errorset::infer(&program.hir, &roots, &resolved.resolutions, &checked.types);
    if let Some(code) = reject(&inferred.diagnostics, sources, color) {
        return Compiled::Stopped(code);
    }

    if dumps.error_sets {
        println!("{}", brasa_errorset::dump::dump(&program.hir, &inferred));
        return Compiled::Stopped(0);
    }

    // Code generation runs even under `--check`: the limits it reports
    // are properties of the program, so a program that cannot be
    // compiled must be rejected here rather than at run time
    // (spec: 06 — Diagnósticos, code generation).
    let generate = if with_tests {
        brasa_codegen::compile_tests
    } else {
        brasa_codegen::compile_program
    };
    let compiled = generate(
        &program.hir,
        &roots,
        entry_roots,
        &resolved.resolutions,
        &checked.types,
    );
    if let Some(code) = reject(&compiled.diagnostics, sources, color) {
        return Compiled::Stopped(code);
    }

    if dumps.bytecode {
        println!("{}", brasa_bytecode::dump::dump(&compiled.module));
        return Compiled::Stopped(0);
    }

    if dumps.check_only {
        return Compiled::Stopped(0);
    }

    Compiled::Module(Box::new(compiled))
}

/// Renders one phase's diagnostics and reports the exit code to stop
/// with, when it has to stop.
fn reject(diagnostics: &[Diagnostic], sources: &SourceMap, color: bool) -> Option<u8> {
    match render_diagnostics(diagnostics, sources, color) {
        Ok(false) => None,
        Ok(true) => Some(65),
        Err(code) => Some(code),
    }
}

/// `brasa test`: which scripts to test, then each one's `test` items.
///
/// A named script runs exactly as it always has. Without one, the
/// nearest manifest decides: `[test].globs` expand relative to its
/// directory and run in sorted order, a `== path` header per file;
/// with no globs, `build.entry` is the one script tested.
fn run_tests(script: Option<&PathBuf>) -> ExitCode {
    let color = std::io::stderr().is_terminal();

    let found = match manifest::discover() {
        Ok(found) => found,
        Err(code) => return code,
    };
    let options = load_options(found.as_ref());

    if let Some(script) = script {
        return ExitCode::from(test_one(script, color, &options));
    }

    let Some(found) = found else {
        eprintln!("brasa: no script to test, and no `brasa.toml` was found");
        eprintln!("usage: brasa test <script.bras>");
        return ExitCode::from(64);
    };

    if let Some(globs) = found.test_globs() {
        let scripts = match expand_test_globs(&found, globs) {
            Ok(scripts) => scripts,
            Err(code) => return code,
        };

        if scripts.is_empty() {
            println!("no tests");
            return ExitCode::from(0);
        }

        // The aggregate exit is the numerically largest per-file code,
        // which is exactly this CLI's severity order: a host failure
        // (70) outranks a rejected script (65) outranks a failed test
        // (1) outranks a clean run (0).
        let mut worst = 0u8;
        for script in scripts {
            println!("== {}", script.display());
            worst = worst.max(test_one(&script, color, &options));
        }

        return ExitCode::from(worst);
    }

    let Some(entry) = found.entry() else {
        eprintln!(
            "brasa: no script to test, and the manifest defines neither `[test].globs` nor `build.entry`"
        );
        return ExitCode::from(64);
    };

    ExitCode::from(test_one(&entry, color, &options))
}

/// Every file `[test].globs` names, deduplicated and sorted so a run is
/// deterministic whatever order the filesystem answers in.
fn expand_test_globs(
    found: &manifest::Manifest,
    globs: &[String],
) -> Result<Vec<PathBuf>, ExitCode> {
    let mut scripts = std::collections::BTreeSet::new();

    for pattern in globs {
        let anchored = found.dir().join(pattern).to_string_lossy().into_owned();

        let matched = match glob::glob(&anchored) {
            Ok(matched) => matched,
            Err(err) => {
                eprintln!("brasa: invalid test glob `{pattern}`: {err}");
                return Err(ExitCode::from(65));
            }
        };

        for entry in matched {
            match entry {
                Ok(path) if path.is_file() => {
                    scripts.insert(path);
                }
                Ok(_) => {}
                Err(err) => {
                    eprintln!("brasa: cannot read a match of `{pattern}`: {err}");
                    return Err(ExitCode::from(70));
                }
            }
        }
    }

    Ok(scripts.into_iter().collect())
}

/// One script's `test` items: compiled with them and run one at a time,
/// a line per test. Answers the raw sysexits number so an aggregated
/// run can compare codes across files.
fn test_one(script: &PathBuf, color: bool, options: &brasa_module::LoadOptions) -> u8 {
    let compiled = match compile(script, color, true, Dumps::default(), options) {
        Compiled::Stopped(code) => return code,
        Compiled::Module(compiled) => compiled,
    };

    if compiled.module.tests.is_empty() {
        println!("no tests");
        return 0;
    }

    let mut stdout = std::io::stdout();
    let (setup, results) = brasa_vm::run_tests(&compiled.module, &mut stdout, &[]);

    // A failed setup means no test ran: the module never finished
    // initializing, so every result would be about that instead.
    if let Some(message) = failure_message(&setup) {
        eprintln!("{message}");
        eprintln!("brasa: the script's top level failed, so no test ran");
        return 70;
    }

    let mut failed = 0;
    for (name, outcome) in &results {
        match failure_message(outcome) {
            None => println!("ok   {name}"),
            Some(message) => {
                failed += 1;
                println!("FAIL {name}");
                eprintln!("{message}");
            }
        }
    }

    println!("{} passed, {failed} failed", results.len() - failed);

    if failed > 0 {
        return 1;
    }
    0
}

/// How a run ended, when it ended badly.
fn failure_message(outcome: &brasa_runtime::Outcome) -> Option<&str> {
    match outcome {
        brasa_runtime::Outcome::Error { message } | brasa_runtime::Outcome::Panic { message } => {
            Some(message)
        }
        brasa_runtime::Outcome::Exit { code } if *code != 0 => Some("exited non-zero"),
        _ => None,
    }
}

fn execute(module: &brasa_bytecode::Module, args: &[String]) -> ExitCode {
    let mut stdout = std::io::stdout();
    let outcome = brasa_vm::run(module, &mut stdout, args);
    let flushed = stdout.flush();

    // The outcome is reported before any flush handling: a script
    // failure must never be masked by an output-stream condition.
    // A failure is reported and answered here; everything else reduces
    // to the status to return once the stream turns out to be healthy.
    // Listing the variants rather than falling through a wildcard is
    // deliberate: a future outcome must not be able to take the
    // success path by default.
    let chosen = match outcome {
        brasa_runtime::Outcome::Error { message } | brasa_runtime::Outcome::Panic { message } => {
            eprintln!("{message}");
            return ExitCode::from(70);
        }
        brasa_runtime::Outcome::Exit { code } => code as u8,
        brasa_runtime::Outcome::Success | brasa_runtime::Outcome::BrokenPipe => 0,
    };

    match flushed {
        // A closed read end (`brasa ... | head`) is a silent success,
        // like standard Unix tools; any other flush failure is real,
        // and it outranks a chosen status because output that never
        // arrived is a failure the script does not know about.
        Err(err) if err.kind() != std::io::ErrorKind::BrokenPipe => {
            eprintln!("brasa: failed to flush output: {err}");
            ExitCode::from(70)
        }
        _ => ExitCode::from(chosen),
    }
}

fn dump_ast(script: &PathBuf, color: bool) -> ExitCode {
    let source = match std::fs::read_to_string(script) {
        Ok(source) => source,
        Err(err) => {
            eprintln!("brasa: cannot read {}: {err}", script.display());
            return ExitCode::from(65);
        }
    };

    let mut sources = SourceMap::new();
    let file = sources.add_file(script.clone(), source.clone());

    let result = brasa_parser::parse(&source, file);
    match render_diagnostics(&result.diagnostics, &sources, color) {
        Ok(false) => {}
        Ok(true) => return ExitCode::from(65),
        Err(code) => return ExitCode::from(code),
    }

    println!("{}", brasa_parser::dump::dump(&result.ast, &result.roots));
    ExitCode::from(0)
}

/// Renders every diagnostic to stderr and reports whether any was an
/// error; a rendering or flush failure yields the exit code to return.
fn render_diagnostics(
    diagnostics: &[Diagnostic],
    sources: &SourceMap,
    color: bool,
) -> Result<bool, u8> {
    let mut stderr = BufWriter::new(std::io::stderr());
    for diagnostic in diagnostics {
        if let Err(err) = brasa_diagnostics::render::render(diagnostic, sources, &mut stderr, color)
        {
            eprintln!("brasa: failed to render diagnostic: {err}");
            return Err(70);
        }
    }
    if let Err(err) = stderr.flush() {
        eprintln!("brasa: failed to flush diagnostics: {err}");
        return Err(70);
    }

    Ok(diagnostics
        .iter()
        .any(|diag| diag.severity == Severity::Error))
}
