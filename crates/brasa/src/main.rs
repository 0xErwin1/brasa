//! Brasa CLI: run a `.brs` script, or drop into tooling subcommands.
//!
//! Exit codes follow sysexits: 64 usage, 65 bad input, 70 runtime failure.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

#[derive(Parser)]
#[command(name = "brasa", version, about = "The Brasa programming language")]
struct Cli {
    /// Script to execute.
    script: PathBuf,

    /// Arguments passed through to the script as `args()`.
    #[arg(trailing_var_arg = true)]
    args: Vec<String>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let source = match std::fs::read_to_string(&cli.script) {
        Ok(source) => source,
        Err(err) => {
            eprintln!("brasa: cannot read {}: {err}", cli.script.display());
            return ExitCode::from(65);
        }
    };

    // Pipeline lands here as milestones complete: parse (M0), check (M1),
    // run (M1 tree-walker, M3 VM).
    let _ = source;
    eprintln!("brasa: execution not implemented yet (M0 in progress)");

    ExitCode::from(70)
}
