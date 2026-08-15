//! `brasa bundle`: turning a program into one file that runs on a
//! machine that has never seen Brasa.
//!
//! A bundled tool is this exact binary with the program's source
//! appended, followed by a fixed-size trailer. The trailer is what
//! makes the cold-start check cheap: finding the payload is one seek to
//! the last [`brasa_module::bundle::TRAILER_LEN`] bytes of the
//! executable, never a scan, so an unbundled `brasa` pays one open, one
//! seek and one 16-byte read before it does anything else.
//!
//! What is embedded is source, not bytecode: the compiled module is
//! in-memory only (spec: 07 — Diseño del bytecode), and compiling the front
//! half of the pipeline at startup is invisible against process spawn.

use std::io::{IsTerminal, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use brasa_module::bundle::{Bundle, TRAILER_LEN};
use brasa_source::SourceMap;

use crate::{Compiled, Dumps, execute, reject_entry};

#[derive(clap::Args)]
pub struct BundleArgs {
    /// Script to bundle. Every module it imports is embedded with it.
    /// Without one, `build.entry` from the nearest `brasa.toml`.
    pub script: Option<PathBuf>,

    /// Where to write the self-contained executable. Without one,
    /// `<out_dir>/<project.name or the entry's stem>` from the nearest
    /// `brasa.toml`.
    #[arg(short, long)]
    pub output: Option<PathBuf>,
}

/// The payload this executable carries, if it carries one.
///
/// `Ok(None)` is the ordinary case and the one that has to stay cheap:
/// it costs one open, one seek to the end and one 16-byte read. An
/// executable whose trailer says there is a payload but whose bytes do
/// not deliver one is an error rather than a fall-through, because
/// falling through would silently reinterpret a bundled tool's
/// arguments as this CLI's.
pub fn embedded() -> Result<Option<Vec<u8>>, String> {
    let Ok(exe) = std::env::current_exe() else {
        return Ok(None);
    };

    let Ok(mut file) = std::fs::File::open(&exe) else {
        return Ok(None);
    };

    let trailer_at = match file.seek(SeekFrom::End(-(TRAILER_LEN as i64))) {
        Ok(offset) => offset,
        // Shorter than a trailer, or not seekable: not a bundle.
        Err(_) => return Ok(None),
    };

    let mut trailer = [0u8; TRAILER_LEN];
    if file.read_exact(&mut trailer).is_err() {
        return Ok(None);
    }

    let Some(len) = brasa_module::bundle::payload_len(&trailer) else {
        return Ok(None);
    };

    if len > trailer_at {
        return Err(format!(
            "this executable claims to carry a {len}-byte program but is only {trailer_at} bytes long before its trailer"
        ));
    }

    let mut payload = vec![0u8; len as usize];
    file.seek(SeekFrom::Start(trailer_at - len))
        .and_then(|_| file.read_exact(&mut payload))
        .map_err(|err| format!("cannot read the embedded program: {err}"))?;

    Ok(Some(payload))
}

/// Runs an embedded program. Every argument belongs to it: a bundled
/// tool is its own command, so nothing here is interpreted as a `brasa`
/// flag or subcommand.
pub fn run(payload: &[u8]) -> ExitCode {
    let bundle = match Bundle::decode(payload) {
        Ok(bundle) => bundle,
        Err(err) => {
            eprintln!("brasa: {err}");
            return ExitCode::from(70);
        }
    };

    let color = std::io::stderr().is_terminal();

    let mut sources = SourceMap::new();
    let program = brasa_module::bundle::load(&bundle, &mut sources);

    let args: Vec<String> = std::env::args().skip(1).collect();

    match crate::compile_program(&program, &sources, color, false, Dumps::default()) {
        Compiled::Stopped(code) => ExitCode::from(code),
        Compiled::Module(compiled) => execute(&compiled.module, &args),
    }
}

/// `brasa bundle script.bras -o tool`. The manifest only fills in what
/// the command line omitted: without a script, `build.entry`; without
/// `-o`, `<out_dir>/<project.name or the entry's stem>`.
pub fn write(args: &BundleArgs) -> ExitCode {
    let found = match crate::manifest::discover() {
        Ok(found) => found,
        Err(code) => return code,
    };

    let script = match args
        .script
        .clone()
        .or_else(|| found.as_ref().and_then(crate::manifest::Manifest::entry))
    {
        Some(script) => script,
        None => {
            eprintln!(
                "brasa: no script to bundle, and no `brasa.toml` with a `build.entry` was found"
            );
            eprintln!("usage: brasa bundle <script.bras> -o <tool>");
            return ExitCode::from(64);
        }
    };

    let output = match args
        .output
        .clone()
        .or_else(|| found.as_ref().map(|m| m.bundle_output(&script)))
    {
        Some(output) => output,
        None => {
            eprintln!("brasa: no output path, and no `brasa.toml` to take one from");
            eprintln!("usage: brasa bundle <script.bras> -o <tool>");
            return ExitCode::from(64);
        }
    };

    if names_source_file(&output) {
        return refuse_flattening();
    }

    if let Some(code) = reject_entry(&script) {
        return code;
    }

    // A manifest default lands in a directory that may not exist yet;
    // an explicit `-o` keeps today's behavior of writing exactly where
    // it was told.
    if args.output.is_none()
        && let Some(parent) = output.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        eprintln!("brasa: cannot create {}: {err}", parent.display());
        return ExitCode::from(70);
    }

    let color = std::io::stderr().is_terminal();

    let mut sources = SourceMap::new();
    let options = crate::load_options(found.as_ref());
    let program = brasa_module::load_with(&script, &mut sources, &options);

    // The program is compiled all the way to bytecode and the result
    // thrown away. A bundle that does not compile would only fail on
    // the machine it was delivered to, which is the worst place to
    // discover it.
    if let Compiled::Stopped(code) =
        crate::compile_program(&program, &sources, color, false, Dumps::default())
    {
        return ExitCode::from(code);
    }

    let payload = match Bundle::capture(&program, &sources).and_then(|bundle| bundle.encode()) {
        Ok(payload) => payload,
        Err(err) => {
            eprintln!("brasa: {err}");
            return ExitCode::from(65);
        }
    };

    match emit(&output, &payload) {
        Ok(()) => ExitCode::from(0),
        Err(err) => {
            eprintln!("brasa: cannot write {}: {err}", output.display());
            ExitCode::from(70)
        }
    }
}

/// Flattening the module graph into one `.bras` file is a different
/// feature, and the refusal says why rather than pretending the flag
/// was mistyped.
fn refuse_flattening() -> ExitCode {
    eprintln!("brasa: bundling into a single `.bras` source file is not implemented");
    eprintln!(
        "note: a module's file is its namespace, so two modules that both define `slugify` have nowhere to coexist in one file; flattening needs a renaming pass that does not exist yet"
    );
    eprintln!(
        "note: give `-o` a path without a `.bras` extension to get a self-contained executable"
    );

    ExitCode::from(64)
}

fn names_source_file(output: &Path) -> bool {
    output
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("bras"))
}

/// Writes the tool: this binary, then the payload, then the trailer.
///
/// Assembled beside the destination and renamed into place, so an
/// interrupted bundle never leaves a half-written executable under the
/// name the user is about to run.
fn emit(output: &Path, payload: &[u8]) -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    let staging = staging_path(output);

    let assembled =
        assemble(&exe, &staging, payload).and_then(|()| std::fs::rename(&staging, output));

    if assembled.is_err() {
        let _ = std::fs::remove_file(&staging);
    }

    assembled
}

fn assemble(exe: &Path, staging: &Path, payload: &[u8]) -> std::io::Result<()> {
    std::fs::copy(exe, staging)?;

    let mut file = std::fs::OpenOptions::new().append(true).open(staging)?;
    file.write_all(payload)?;
    file.write_all(&brasa_module::bundle::trailer(payload.len() as u64))?;
    file.flush()?;
    drop(file);

    make_executable(staging)
}

/// A sibling of the destination, so the rename that follows stays on one
/// filesystem and therefore stays atomic.
fn staging_path(output: &Path) -> PathBuf {
    let name = output
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();

    output.with_file_name(format!(".{name}.brasa-bundle-{}", std::process::id()))
}

#[cfg(unix)]
fn make_executable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> std::io::Result<()> {
    Ok(())
}
