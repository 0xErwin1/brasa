//! `brasa fmt`: the formatter's command-line front end (BRS-91).
//!
//! Formatting itself lives in `brasa_fmt`; this module only decides
//! which files to hand it, where the result goes, and what to exit with.

use std::io::{BufWriter, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use brasa_source::SourceMap;

#[derive(clap::Args)]
pub struct FmtArgs {
    /// Files and directories to format. A directory is walked for
    /// `.bras` files; the default is the current directory.
    paths: Vec<PathBuf>,

    /// Report which files are not formatted instead of rewriting them,
    /// and exit 1 if any is. Nothing is written.
    #[arg(long)]
    check: bool,

    /// Write the formatted source to stdout instead of back to the file.
    #[arg(long, conflicts_with = "check")]
    stdout: bool,
}

pub fn run(args: &FmtArgs) -> ExitCode {
    let roots: Vec<PathBuf> = if args.paths.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        args.paths.clone()
    };

    let mut files = Vec::new();
    for root in &roots {
        if let Err(code) = collect(root, &mut files) {
            return code;
        }
    }

    let color = std::io::stderr().is_terminal();
    let mut unformatted = 0;
    let mut failed = false;

    for path in files {
        match format_one(&path, args, color) {
            Outcome::Formatted => {}
            Outcome::WouldChange => unformatted += 1,
            Outcome::Failed(code) => {
                if code != ExitCode::from(65) {
                    return code;
                }
                failed = true;
            }
        }
    }

    if failed {
        return ExitCode::from(65);
    }

    if unformatted > 0 {
        return ExitCode::from(1);
    }

    ExitCode::SUCCESS
}

enum Outcome {
    Formatted,
    WouldChange,
    Failed(ExitCode),
}

fn format_one(path: &Path, args: &FmtArgs, color: bool) -> Outcome {
    let source = match std::fs::read_to_string(path) {
        Ok(source) => source,
        Err(err) => {
            eprintln!("brasa: cannot read {}: {err}", path.display());
            return Outcome::Failed(ExitCode::from(65));
        }
    };

    let mut sources = SourceMap::new();
    let file = sources.add_file(path.to_path_buf(), source.clone());

    let formatted = match brasa_fmt::format(&source, file) {
        Ok(formatted) => formatted,
        Err(brasa_fmt::FormatError::Parse(diagnostics)) => {
            let mut stderr = BufWriter::new(std::io::stderr());
            for diagnostic in &diagnostics {
                if brasa_diagnostics::render::render(diagnostic, &sources, &mut stderr, color)
                    .is_err()
                {
                    eprintln!("brasa: failed to render diagnostic");
                    return Outcome::Failed(ExitCode::from(70));
                }
            }
            let _ = stderr.flush();
            return Outcome::Failed(ExitCode::from(65));
        }
        // Never the file's fault: the formatter checked its own output
        // and refused it. Reported loudly, and the file is left alone.
        Err(brasa_fmt::FormatError::Unstable(reason)) => {
            eprintln!(
                "brasa: internal formatter error on {}: {reason}",
                path.display()
            );
            return Outcome::Failed(ExitCode::from(70));
        }
    };

    if args.stdout {
        print!("{formatted}");
        return Outcome::Formatted;
    }

    if formatted == source {
        return Outcome::Formatted;
    }

    if args.check {
        println!("{}", path.display());
        return Outcome::WouldChange;
    }

    match std::fs::write(path, formatted) {
        Ok(()) => Outcome::Formatted,
        Err(err) => {
            eprintln!("brasa: cannot write {}: {err}", path.display());
            Outcome::Failed(ExitCode::from(70))
        }
    }
}

/// Every `.bras` file under `root`, or `root` itself when it is a file.
///
/// A named file is taken whatever its extension, since naming it is the
/// request; a walked directory only yields `.bras`. Hidden entries and
/// symlinked directories are skipped: a formatter rewrites what it
/// walks, so following a link out of the tree the user named is a
/// mistake that is hard to undo.
fn collect(root: &Path, found: &mut Vec<PathBuf>) -> Result<(), ExitCode> {
    let metadata = match std::fs::metadata(root) {
        Ok(metadata) => metadata,
        Err(err) => {
            eprintln!("brasa: cannot read {}: {err}", root.display());
            return Err(ExitCode::from(65));
        }
    };

    if metadata.is_file() {
        found.push(root.to_path_buf());
        return Ok(());
    }

    if !metadata.is_dir() {
        eprintln!("brasa: {} is not a file or a directory", root.display());
        return Err(ExitCode::from(65));
    }

    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(err) => {
            eprintln!("brasa: cannot read {}: {err}", root.display());
            return Err(ExitCode::from(65));
        }
    };

    let mut children = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                eprintln!("brasa: cannot read an entry of {}: {err}", root.display());
                return Err(ExitCode::from(65));
            }
        };

        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }

        let path = entry.path();
        let is_symlink = path.symlink_metadata().is_ok_and(|m| m.is_symlink());

        if path.is_dir() {
            if is_symlink {
                continue;
            }
            children.push(path);
        } else if path.extension().is_some_and(|ext| ext == "bras") {
            children.push(path);
        }
    }

    children.sort();
    for child in children {
        if child.is_dir() {
            collect(&child, found)?;
        } else {
            found.push(child);
        }
    }

    Ok(())
}
