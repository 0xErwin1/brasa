//! Brasa CLI: run a `.brs` script, or drop into tooling subcommands.
//!
//! Exit codes follow sysexits: 64 usage, 65 bad input, 70 runtime failure.

use std::io::{BufWriter, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

use brasa_diagnostics::{Diagnostic, Severity};
use brasa_source::SourceMap;

#[derive(Parser)]
#[command(name = "brasa", version, about = "The Brasa programming language")]
struct Cli {
    /// Script to execute.
    script: PathBuf,

    /// Print the parsed AST to stdout instead of executing the script.
    #[arg(long)]
    dump_ast: bool,

    /// Print the lowered HIR to stdout instead of executing the script.
    #[arg(long)]
    dump_hir: bool,

    /// Arguments passed through to the script as `args()`.
    #[arg(trailing_var_arg = true)]
    args: Vec<String>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    match std::fs::metadata(&cli.script) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => {
            eprintln!("brasa: {} is not a regular file", cli.script.display());
            return ExitCode::from(65);
        }
        Err(err) => {
            eprintln!("brasa: cannot read {}: {err}", cli.script.display());
            return ExitCode::from(65);
        }
    }

    let source = match std::fs::read_to_string(&cli.script) {
        Ok(source) => source,
        Err(err) => {
            eprintln!("brasa: cannot read {}: {err}", cli.script.display());
            return ExitCode::from(65);
        }
    };

    let mut sources = brasa_source::SourceMap::new();
    let file = sources.add_file(cli.script.clone(), source.clone());

    let result = brasa_parser::parse(&source, file);
    let has_errors = result
        .diagnostics
        .iter()
        .any(|diag| diag.severity == Severity::Error);

    let color = std::io::stderr().is_terminal();
    let mut stderr = BufWriter::new(std::io::stderr());
    for diagnostic in &result.diagnostics {
        if let Err(err) =
            brasa_diagnostics::render::render(diagnostic, &sources, &mut stderr, color)
        {
            eprintln!("brasa: failed to render diagnostic: {err}");
            return ExitCode::from(70);
        }
    }
    if let Err(err) = stderr.flush() {
        eprintln!("brasa: failed to flush diagnostics: {err}");
        return ExitCode::from(70);
    }

    if has_errors {
        return ExitCode::from(65);
    }

    if cli.dump_ast {
        println!("{}", brasa_parser::dump::dump(&result.ast, &result.roots));
        return ExitCode::from(0);
    }

    if cli.dump_hir {
        let lowered = brasa_hir::lower(&result.ast, &result.roots);
        match render_diagnostics(&lowered.diagnostics, &sources, color) {
            Ok(false) => {}
            Ok(true) => return ExitCode::from(65),
            Err(code) => return code,
        }

        println!("{}", brasa_hir::dump::dump(&lowered.hir, &lowered.roots));
        return ExitCode::from(0);
    }

    let lowered = brasa_hir::lower(&result.ast, &result.roots);
    match render_diagnostics(&lowered.diagnostics, &sources, color) {
        Ok(false) => {}
        Ok(true) => return ExitCode::from(65),
        Err(code) => return code,
    }

    let resolved = brasa_resolver::resolve(&lowered.hir, &lowered.roots);
    match render_diagnostics(&resolved.diagnostics, &sources, color) {
        Ok(false) => {}
        Ok(true) => return ExitCode::from(65),
        Err(code) => return code,
    }

    let checked = brasa_typeck::check(
        &lowered.hir,
        &lowered.roots,
        &resolved.resolutions,
        &lowered.sugar_origins,
    );
    match render_diagnostics(&checked.diagnostics, &sources, color) {
        Ok(false) => {}
        Ok(true) => return ExitCode::from(65),
        Err(code) => return code,
    }

    // Pipeline continues here as milestones complete: run (M1
    // tree-walker, M3 VM).
    eprintln!("check OK (execution lands with the M1 tree-walker)");

    ExitCode::from(70)
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
