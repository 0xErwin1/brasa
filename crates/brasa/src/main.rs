//! Brasa CLI: run a `.bras` script, or drop into tooling subcommands.
//!
//! Exit codes follow sysexits: 64 usage, 65 bad input, 70 runtime failure.

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
    #[arg(trailing_var_arg = true)]
    args: Vec<String>,
}

#[derive(clap::Subcommand)]
enum Subcommand {
    /// Format Brasa source files.
    Fmt(fmt::FmtArgs),
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    if let Some(Subcommand::Fmt(args)) = &cli.command {
        return fmt::run(args);
    }

    let Some(script) = cli.script.clone() else {
        eprintln!("brasa: no script and no subcommand");
        eprintln!("usage: brasa <script.bras> [args...]   or   brasa fmt [paths...]");
        return ExitCode::from(64);
    };

    run_script(&cli, &script)
}

fn run_script(cli: &Cli, script: &PathBuf) -> ExitCode {
    match std::fs::metadata(script) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => {
            eprintln!("brasa: {} is not a regular file", script.display());
            return ExitCode::from(65);
        }
        Err(err) => {
            eprintln!("brasa: cannot read {}: {err}", script.display());
            return ExitCode::from(65);
        }
    }

    let color = std::io::stderr().is_terminal();

    // `--dump-ast` is a single-file view: an AST belongs to one parsed
    // file, and the module loader drops each one as soon as it lowers.
    if cli.dump_ast {
        return dump_ast(script, color);
    }

    let mut sources = SourceMap::new();
    let program = brasa_module::load(script, &mut sources);
    match render_diagnostics(&program.diagnostics, &sources, color) {
        Ok(false) => {}
        Ok(true) => return ExitCode::from(65),
        Err(code) => return code,
    }

    let roots = program.all_roots();
    let entry_roots = &program.module(program.entry).roots;

    if cli.dump_hir {
        println!("{}", brasa_hir::dump::dump(&program.hir, &roots));
        return ExitCode::from(0);
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
    match render_diagnostics(&resolved.diagnostics, &sources, color) {
        Ok(false) => {}
        Ok(true) => return ExitCode::from(65),
        Err(code) => return code,
    }

    let checked = brasa_typeck::check(
        &program.hir,
        &roots,
        &resolved.resolutions,
        &program.sugar_origins,
    );
    match render_diagnostics(&checked.diagnostics, &sources, color) {
        Ok(false) => {}
        Ok(true) => return ExitCode::from(65),
        Err(code) => return code,
    }

    let inferred =
        brasa_errorset::infer(&program.hir, &roots, &resolved.resolutions, &checked.types);
    match render_diagnostics(&inferred.diagnostics, &sources, color) {
        Ok(false) => {}
        Ok(true) => return ExitCode::from(65),
        Err(code) => return code,
    }

    if cli.dump_error_sets {
        println!("{}", brasa_errorset::dump::dump(&program.hir, &inferred));
        return ExitCode::from(0);
    }

    // Code generation runs even under `--check`: the limits it reports
    // are properties of the program, so a program that cannot be
    // compiled must be rejected here rather than at run time
    // (`docs/spec/06-diagnostics.md`, code generation).
    let compiled = brasa_codegen::compile_program(
        &program.hir,
        &roots,
        entry_roots,
        &resolved.resolutions,
        &checked.types,
    );
    match render_diagnostics(&compiled.diagnostics, &sources, color) {
        Ok(false) => {}
        Ok(true) => return ExitCode::from(65),
        Err(code) => return code,
    }

    if cli.dump_bytecode {
        println!("{}", brasa_bytecode::dump::dump(&compiled.module));
        return ExitCode::from(0);
    }

    if cli.check {
        return ExitCode::from(0);
    }

    execute(&compiled.module, &cli.args)
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
