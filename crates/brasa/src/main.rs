//! Brasa CLI: run a `.brs` script, or drop into tooling subcommands.
//!
//! Exit codes follow sysexits: 64 usage, 65 bad input, 70 runtime failure.

use std::io::{BufWriter, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

use brasa_diagnostics::Severity;

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

        let mut stderr = BufWriter::new(std::io::stderr());
        for diagnostic in &lowered.diagnostics {
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

        let lowering_failed = lowered
            .diagnostics
            .iter()
            .any(|diag| diag.severity == Severity::Error);
        if lowering_failed {
            return ExitCode::from(65);
        }

        println!("{}", brasa_hir::dump::dump(&lowered.hir, &lowered.roots));
        return ExitCode::from(0);
    }

    // Pipeline continues here as milestones complete: check (M1), run (M1
    // tree-walker, M3 VM).
    eprintln!("parsed OK (execution lands in M1)");

    ExitCode::from(70)
}
