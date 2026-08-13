//! Brasa CLI: run a `.bras` script, or drop into tooling subcommands
//! (`brasa fmt`, `brasa test`, `brasa bundle`).
//!
//! This binary is also the bundle runtime: a bundled tool is a copy of
//! it with a program appended, so the first thing `main` does is ask
//! whether it is carrying one (see [`bundle`]).
//!
//! Exit codes follow sysexits: 64 usage, 65 bad input, 70 runtime failure.

mod bundle;
mod fmt;

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
    /// script (`docs/spec/04-errors.md`, error-set inference).
    #[arg(long)]
    dump_error_sets: bool,

    /// Print the compiled bytecode disassembly to stdout instead of
    /// executing the script (`docs/spec/07-bytecode.md`).
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
}

#[derive(clap::Args)]
struct TestArgs {
    /// Script whose tests to run.
    script: PathBuf,
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
        Some(Subcommand::Test(args)) => return run_tests(&args.script),
        Some(Subcommand::Bundle(args)) => return bundle::write(args),
        None => {}
    }

    let Some(script) = cli.script.clone() else {
        eprintln!("brasa: no script and no subcommand");
        eprintln!("usage: brasa <script.bras> [args...]   or   brasa fmt [paths...]");
        return ExitCode::from(64);
    };

    run_script(&cli, &script)
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
    Stopped(ExitCode),
}

/// Splits argv into what this CLI parses and what the script receives.
///
/// The boundary is the first argument that is not an option and is not
/// a subcommand name: that is the script path, and it is the last thing
/// this CLI has an opinion about. A subcommand keeps the whole line,
/// since `brasa fmt --check` is genuinely ours.
fn split_at_script(argv: impl Iterator<Item = String>) -> (Vec<String>, Vec<String>) {
    const SUBCOMMANDS: &[&str] = &["fmt", "test", "bundle", "help"];

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

fn run_script(cli: &Cli, script: &PathBuf) -> ExitCode {
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

    match compile(script, color, false, dumps) {
        Compiled::Stopped(code) => code,
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
fn compile(script: &PathBuf, color: bool, with_tests: bool, dumps: Dumps) -> Compiled {
    if let Some(code) = reject_entry(script) {
        return Compiled::Stopped(code);
    }

    let mut sources = SourceMap::new();
    let program = brasa_module::load(script, &mut sources);

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
        return Compiled::Stopped(ExitCode::from(0));
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
        return Compiled::Stopped(ExitCode::from(0));
    }

    // Code generation runs even under `--check`: the limits it reports
    // are properties of the program, so a program that cannot be
    // compiled must be rejected here rather than at run time
    // (`docs/spec/06-diagnostics.md`, code generation).
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
        return Compiled::Stopped(ExitCode::from(0));
    }

    if dumps.check_only {
        return Compiled::Stopped(ExitCode::from(0));
    }

    Compiled::Module(Box::new(compiled))
}

/// Renders one phase's diagnostics and reports the exit code to stop
/// with, when it has to stop.
fn reject(diagnostics: &[Diagnostic], sources: &SourceMap, color: bool) -> Option<ExitCode> {
    match render_diagnostics(diagnostics, sources, color) {
        Ok(false) => None,
        Ok(true) => Some(ExitCode::from(65)),
        Err(code) => Some(code),
    }
}

/// `brasa test script.bras`: compiles the script WITH its `test` items
/// and runs each one, reporting a line per test and exiting non-zero if
/// any failed.
fn run_tests(script: &PathBuf) -> ExitCode {
    let color = std::io::stderr().is_terminal();

    let compiled = match compile(script, color, true, Dumps::default()) {
        Compiled::Stopped(code) => return code,
        Compiled::Module(compiled) => compiled,
    };

    if compiled.module.tests.is_empty() {
        println!("no tests");
        return ExitCode::from(0);
    }

    let mut stdout = std::io::stdout();
    let (setup, results) = brasa_vm::run_tests(&compiled.module, &mut stdout, &[]);

    // A failed setup means no test ran: the module never finished
    // initializing, so every result would be about that instead.
    if let Some(message) = failure_message(&setup) {
        eprintln!("{message}");
        eprintln!("brasa: the script's top level failed, so no test ran");
        return ExitCode::from(70);
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
        return ExitCode::from(1);
    }
    ExitCode::from(0)
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
        Err(code) => return code,
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
) -> Result<bool, ExitCode> {
    let mut stderr = BufWriter::new(std::io::stderr());
    for diagnostic in diagnostics {
        if let Err(err) = brasa_diagnostics::render::render(diagnostic, sources, &mut stderr, color)
        {
            eprintln!("brasa: failed to render diagnostic: {err}");
            return Err(ExitCode::from(70));
        }
    }
    if let Err(err) = stderr.flush() {
        eprintln!("brasa: failed to flush diagnostics: {err}");
        return Err(ExitCode::from(70));
    }

    Ok(diagnostics
        .iter()
        .any(|diag| diag.severity == Severity::Error))
}
