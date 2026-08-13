//! `brasa bundle` end to end (BRS-111): a program delivered as one
//! file to a machine that has never seen Brasa.
//!
//! The claim under test is not "a file was produced" but "the produced
//! file is independent of everything that made it": the source tree it
//! came from, the search path that resolved its imports, and the
//! directory it now sits in. So every case here bundles through the
//! real CLI, then runs the result as a command of its own and compares
//! it byte for byte with running the same program from source.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// `BRASA_PATH` is cleared on every spawn: a developer with one set
/// would otherwise be exercising a different search path from CI, and
/// the independence claims below would mean nothing.
fn brasa(args: &[&std::ffi::OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_brasa"))
        .args(args)
        .env_remove("BRASA_PATH")
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn brasa")
}

fn run_source(entry: &Path, args: &[&str]) -> Output {
    let mut argv: Vec<&std::ffi::OsStr> = vec![entry.as_ref()];
    argv.extend(args.iter().copied().map(std::ffi::OsStr::new));

    brasa(&argv)
}

fn run_tool(tool: &Path, args: &[&str]) -> Output {
    Command::new(tool)
        .args(args)
        .env_remove("BRASA_PATH")
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn the bundled tool")
}

fn bundle(entry: &Path, output: &Path) -> Output {
    brasa(&[
        "bundle".as_ref(),
        entry.as_ref(),
        "-o".as_ref(),
        output.as_ref(),
    ])
}

/// A fresh directory per test, named after the test so a failure leaves
/// something identifiable behind.
fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("brasa-bundle-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("failed to create temp dir");
    dir
}

fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("failed to create parent directory");
    }
    std::fs::write(&path, text).expect("failed to write fixture");
    path
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The property the whole feature rests on: the tool is the program.
fn assert_same_run(from_source: &Output, from_tool: &Output) {
    assert_eq!(
        from_tool.status.code(),
        from_source.status.code(),
        "exit codes differ; bundled stderr: {}",
        stderr(from_tool)
    );
    assert_eq!(
        from_tool.stdout,
        from_source.stdout,
        "stdout differs; bundled stderr: {}",
        stderr(from_tool)
    );
    assert_eq!(from_tool.stderr, from_source.stderr, "stderr differs");
}

#[test]
fn a_single_file_program_bundles_and_runs_identically() {
    let dir = temp_dir("single");
    let main = write(
        &dir,
        "main.bras",
        "def main()\n  puts \"hello\"\n  puts 21 * 2\nend\n",
    );
    let tool = dir.join("tool");

    let bundled = bundle(&main, &tool);
    assert_eq!(
        bundled.status.code(),
        Some(0),
        "stderr: {}",
        stderr(&bundled)
    );

    assert_same_run(&run_source(&main, &[]), &run_tool(&tool, &[]));
}

/// A multi-file graph has to come back with its evaluation order
/// intact: top-level statements run when a module is first imported,
/// so a wrong module order shows up as reordered output rather than as
/// a crash.
#[test]
fn a_multi_file_program_bundles_with_its_module_order_intact() {
    let dir = temp_dir("multi");
    write(
        &dir,
        "deep.bras",
        "puts \"deep loaded\"\npub def triple(x: int): int\n  x * 3\nend\n",
    );
    write(
        &dir,
        "util.bras",
        "import \"deep.bras\"\nputs \"util loaded\"\npub def double(x: int): int\n  deep.triple(x) * 2\nend\n",
    );
    let main = write(
        &dir,
        "main.bras",
        "import \"util.bras\"\nimport \"deep.bras\"\nputs \"main loaded\"\nputs util.double(7)\nputs deep.triple(7)\n",
    );
    let tool = dir.join("tool");

    let bundled = bundle(&main, &tool);
    assert_eq!(
        bundled.status.code(),
        Some(0),
        "stderr: {}",
        stderr(&bundled)
    );

    let from_source = run_source(&main, &[]);
    assert_eq!(
        String::from_utf8_lossy(&from_source.stdout),
        "deep loaded\nutil loaded\nmain loaded\n42\n21\n"
    );

    assert_same_run(&from_source, &run_tool(&tool, &[]));
}

/// The whole point of the feature. A `::` import resolves against
/// `BRASA_PATH` and a `lib` directory beside the executed file, so a
/// bundle that re-ran resolution on the target would be exactly as
/// undeliverable as the Python script this replaces. Here the search
/// path is empty, the `lib` directory is gone and the source tree is
/// deleted before the tool runs.
#[test]
fn a_search_path_import_survives_its_source_tree_being_deleted() {
    let root = temp_dir("search-path");
    let project = root.join("project");
    let elsewhere = root.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).expect("failed to create the destination directory");

    write(
        &project,
        "lib/text/slug.bras",
        "pub def slugify(name: string): string\n  name.toLower().replace(\" \", \"-\")\nend\n",
    );
    let main = write(
        &project,
        "main.bras",
        "import text::slug\nputs slug.slugify(\"Hello Bundled World\")\n",
    );
    let tool = elsewhere.join("slugify");

    let from_source = run_source(&main, &[]);
    assert_eq!(
        from_source.status.code(),
        Some(0),
        "stderr: {}",
        stderr(&from_source)
    );

    let bundled = bundle(&main, &tool);
    assert_eq!(
        bundled.status.code(),
        Some(0),
        "stderr: {}",
        stderr(&bundled)
    );

    std::fs::remove_dir_all(&project).expect("failed to delete the source tree");
    assert!(!project.exists());

    assert_same_run(&from_source, &run_tool(&tool, &[]));
}

/// The bundle carries a resolved graph, not a request to resolve.
/// A `BRASA_PATH` pointing at a different `text::slug` must not reach
/// the delivered tool.
#[test]
fn a_hostile_search_path_cannot_reach_a_bundled_tool() {
    let root = temp_dir("hostile");
    let project = root.join("project");
    let decoy = root.join("decoy");

    write(
        &project,
        "lib/text/slug.bras",
        "pub def slugify(name: string): string\n  name.toLower()\nend\n",
    );
    write(
        &decoy,
        "text/slug.bras",
        "pub def slugify(name: string): string\n  \"decoy\"\nend\n",
    );
    let main = write(
        &project,
        "main.bras",
        "import text::slug\nputs slug.slugify(\"REAL\")\n",
    );
    let tool = root.join("tool");

    let bundled = bundle(&main, &tool);
    assert_eq!(
        bundled.status.code(),
        Some(0),
        "stderr: {}",
        stderr(&bundled)
    );

    let output = Command::new(&tool)
        .env("BRASA_PATH", &decoy)
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn the bundled tool");

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "real\n");
}

/// A bundled tool is its own command: everything on its command line
/// belongs to the program, not to the `brasa` CLI.
#[test]
fn arguments_reach_the_bundled_program_unchanged() {
    let dir = temp_dir("args");
    let main = write(
        &dir,
        "main.bras",
        "import std::env\ndef main()\n  for arg in env.args()\n    puts arg\n  end\nend\n",
    );
    let tool = dir.join("tool");

    let bundled = bundle(&main, &tool);
    assert_eq!(
        bundled.status.code(),
        Some(0),
        "stderr: {}",
        stderr(&bundled)
    );

    let args = ["--check", "one", "two"];
    let from_tool = run_tool(&tool, &args);

    assert_eq!(
        from_tool.status.code(),
        Some(0),
        "stderr: {}",
        stderr(&from_tool)
    );
    assert_eq!(
        String::from_utf8_lossy(&from_tool.stdout),
        "--check\none\ntwo\n"
    );
}

/// A program that fails to compile is refused at bundle time. The
/// alternative is discovering it on the machine it was delivered to.
#[test]
fn a_program_that_does_not_compile_is_not_bundled() {
    let dir = temp_dir("broken");
    let main = write(&dir, "main.bras", "puts unknownName\n");
    let tool = dir.join("tool");

    let bundled = bundle(&main, &tool);

    assert_eq!(bundled.status.code(), Some(65));
    assert!(!tool.exists(), "a broken program still produced a tool");
}

/// Flattening the module graph into one source file is a different
/// feature and is not implemented; the refusal has to say so rather
/// than emitting an executable under a `.bras` name.
#[test]
fn a_bras_output_path_is_refused_with_a_reason() {
    let dir = temp_dir("flatten");
    let main = write(&dir, "main.bras", "puts \"hi\"\n");
    let flattened = dir.join("tool.bras");

    let bundled = bundle(&main, &flattened);

    assert_eq!(bundled.status.code(), Some(64));
    let message = stderr(&bundled);
    assert!(
        message.contains("single `.bras` source file is not implemented"),
        "unexpected message: {message}"
    );
    assert!(
        message.contains("renaming pass"),
        "the refusal does not say why: {message}"
    );
    assert!(!flattened.exists(), "a refused bundle still wrote a file");
}

/// The trailer is absent from an unbundled binary, which is what keeps
/// the cold-start check to one seek and one 16-byte read: the last
/// bytes of `brasa` itself must not look like a bundle.
#[test]
fn an_unbundled_binary_carries_no_trailer() {
    let exe = PathBuf::from(env!("CARGO_BIN_EXE_brasa"));
    let bytes = std::fs::read(&exe).expect("failed to read the brasa binary");

    let trailer = &bytes[bytes.len() - 16..];

    assert_ne!(&trailer[..8], b"BRASABND");
}

/// The trailer sits at a fixed offset from the end, so the payload is
/// found by arithmetic rather than by searching for a marker.
#[test]
fn the_trailer_sits_at_a_fixed_offset_from_the_end() {
    let dir = temp_dir("trailer");
    let main = write(&dir, "main.bras", "puts \"hi\"\n");
    let tool = dir.join("tool");

    let bundled = bundle(&main, &tool);
    assert_eq!(
        bundled.status.code(),
        Some(0),
        "stderr: {}",
        stderr(&bundled)
    );

    let bytes = std::fs::read(&tool).expect("failed to read the bundled tool");
    let trailer = &bytes[bytes.len() - 16..];

    assert_eq!(&trailer[..8], b"BRASABND");

    let length = u64::from_le_bytes(trailer[8..].try_into().expect("eight length bytes")) as usize;
    let payload_at = bytes.len() - 16 - length;

    assert_eq!(&bytes[payload_at..payload_at + 8], b"BRASAPKG");
}

/// A tool whose payload was damaged in transit must say so, not fall
/// back to reading its own command line as a `brasa` invocation.
#[test]
fn a_damaged_payload_is_reported_rather_than_ignored() {
    let dir = temp_dir("damaged");
    let main = write(&dir, "main.bras", "puts \"hi\"\n");
    let tool = dir.join("tool");

    let bundled = bundle(&main, &tool);
    assert_eq!(
        bundled.status.code(),
        Some(0),
        "stderr: {}",
        stderr(&bundled)
    );

    let mut bytes = std::fs::read(&tool).expect("failed to read the bundled tool");
    let length =
        u64::from_le_bytes(bytes[bytes.len() - 8..].try_into().expect("eight bytes")) as usize;
    let payload_at = bytes.len() - 16 - length;
    bytes[payload_at] ^= 0xff;
    std::fs::write(&tool, &bytes).expect("failed to rewrite the bundled tool");

    let output = run_tool(&tool, &[]);

    assert_eq!(output.status.code(), Some(70));
    assert!(
        stderr(&output).contains("not a Brasa payload"),
        "unexpected message: {}",
        stderr(&output)
    );
}
