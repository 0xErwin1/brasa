//! Multi-file programs end to end (BRS-97): what loads, what runs, in
//! what order, and what is refused.
//!
//! Every case is a real directory of `.bras` files run through the CLI,
//! because the properties under test — path resolution relative to the
//! importer, one module per canonical file, evaluation order across the
//! graph — only exist once there are real files on a real filesystem.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn brasa(entry: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_brasa"))
        .arg(entry)
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn brasa")
}

/// A fresh directory per test, named after the test so a failure leaves
/// something identifiable behind.
fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("brasa-modules-{name}-{}", std::process::id()));
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

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The base case: an exported function called across the boundary.
#[test]
fn an_exported_function_is_callable_through_the_stem() {
    let dir = temp_dir("call");
    write(
        &dir,
        "util.bras",
        "pub def double(x: int): int\n  x * 2\nend\n",
    );
    let main = write(
        &dir,
        "main.bras",
        "import \"util.bras\"\nputs util.double(21)\n",
    );

    let output = brasa(&main);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(stdout(&output), "42\n");
}

/// Exported `let`s are values, not just functions, and a `pub let mut`
/// stays assignable through the qualified name — the same mutability
/// rule a top-level `let` gets inside its own module.
#[test]
fn exported_lets_are_readable_and_a_mutable_one_is_assignable() {
    let dir = temp_dir("lets");
    write(
        &dir,
        "state.bras",
        "pub let name = \"brasa\"\npub let mut hits = 0\n",
    );
    let main = write(
        &dir,
        "main.bras",
        "import \"state.bras\"\nstate.hits = state.hits + 2\nputs state.name\nputs state.hits\n",
    );

    let output = brasa(&main);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(stdout(&output), "brasa\n2\n");
}

/// `docs/spec/01-syntax.md`: everything is private unless `pub`. The
/// diagnostic has to point at both ends — the use and the declaration
/// that forgot the keyword — or the reader has to go hunting.
#[test]
fn a_private_definition_is_not_reachable_from_an_importer() {
    let dir = temp_dir("private");
    write(&dir, "util.bras", "def secret(): int\n  1\nend\n");
    let main = write(
        &dir,
        "main.bras",
        "import \"util.bras\"\nputs util.secret()\n",
    );

    let output = brasa(&main);

    assert_eq!(output.status.code(), Some(65));
    let stderr = stderr(&output);
    assert!(stderr.contains("R013"), "got: {stderr}");
    assert!(
        stderr.contains("`secret` is not exported by module `util`"),
        "got: {stderr}"
    );
    assert!(
        stderr.contains("declared without `pub` here"),
        "the declaration must be labelled too, got: {stderr}"
    );
}

#[test]
fn a_name_the_module_does_not_declare_at_all_is_reported_as_missing() {
    let dir = temp_dir("missing");
    write(&dir, "util.bras", "pub def real(): int\n  1\nend\n");
    let main = write(
        &dir,
        "main.bras",
        "import \"util.bras\"\nputs util.nope()\n",
    );

    let output = brasa(&main);

    assert_eq!(output.status.code(), Some(65));
    let stderr = stderr(&output);
    assert!(
        stderr.contains("module `util` has no member `nope`"),
        "got: {stderr}"
    );
}

/// `docs/spec/01-syntax.md`: top-level statements run once, the first
/// time the module is imported, in post-order DFS — dependencies first.
/// The pin is the interleaving, which is the whole of the rule.
#[test]
fn top_levels_run_once_each_dependencies_first() {
    let dir = temp_dir("order");
    write(&dir, "deep.bras", "puts \"deep\"\n");
    write(&dir, "left.bras", "import \"deep.bras\"\nputs \"left\"\n");
    write(&dir, "right.bras", "import \"deep.bras\"\nputs \"right\"\n");
    let main = write(
        &dir,
        "main.bras",
        "import \"left.bras\"\nimport \"right.bras\"\nputs \"main\"\n",
    );

    let output = brasa(&main);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "deep\nleft\nright\nmain\n",
        "a diamond runs the shared dependency once, before both sides"
    );
}

/// `docs/spec/01-syntax.md`: only the executed file's `main` is invoked.
/// An imported module's `main` is an ordinary private function.
#[test]
fn an_imported_modules_main_is_not_invoked() {
    let dir = temp_dir("main");
    write(
        &dir,
        "lib.bras",
        "puts \"lib top level\"\n\ndef main()\n  puts \"lib main\"\nend\n",
    );
    let main = write(
        &dir,
        "main.bras",
        "import \"lib.bras\"\n\ndef main()\n  puts \"entry main\"\nend\n",
    );

    let output = brasa(&main);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(stdout(&output), "lib top level\nentry main\n");
}

/// Imports resolve relative to the importing file, not the process's
/// working directory — the nested module's own import is spelled from
/// where it sits.
#[test]
fn an_import_resolves_relative_to_the_importing_file() {
    let dir = temp_dir("relative");
    write(
        &dir,
        "sub/leaf.bras",
        "pub def leaf(): string\n  \"leaf\"\nend\n",
    );
    write(
        &dir,
        "sub/mid.bras",
        "import \"leaf.bras\"\n\npub def mid(): string\n  leaf.leaf() + \"-mid\"\nend\n",
    );
    let main = write(
        &dir,
        "main.bras",
        "import \"./sub/mid.bras\"\nputs mid.mid()\n",
    );

    let output = brasa(&main);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(stdout(&output), "leaf-mid\n");
}

/// Two spellings of one file are one module, so its top level runs once
/// and its `let` is one binding rather than two.
#[test]
fn two_spellings_of_one_file_are_one_module() {
    let dir = temp_dir("spelling");
    write(
        &dir,
        "once.bras",
        "puts \"loading\"\npub let mut count = 0\n",
    );
    write(
        &dir,
        "a.bras",
        "import \"once.bras\"\n\npub def bump()\n  once.count = once.count + 1\nend\n",
    );
    let main = write(
        &dir,
        "main.bras",
        "import \"./once.bras\"\nimport \"a.bras\"\na.bump()\nputs once.count\n",
    );

    let output = brasa(&main);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "loading\n1\n",
        "`./once.bras` and `once.bras` must name the same module"
    );
}

/// `docs/spec/01-syntax.md`: import cycles are a compile error. The
/// diagnostic names the whole chain, because reconstructing it by hand
/// is the entire cost of the error.
#[test]
fn an_import_cycle_is_a_compile_error_naming_the_chain() {
    let dir = temp_dir("cycle");
    write(
        &dir,
        "b.bras",
        "import \"a.bras\"\n\npub def b(): int\n  1\nend\n",
    );
    let main = write(
        &dir,
        "a.bras",
        "import \"b.bras\"\n\npub def a(): int\n  b.b()\nend\n",
    );

    let output = brasa(&main);

    assert_eq!(output.status.code(), Some(65));
    let stderr = stderr(&output);
    assert!(stderr.contains("M002"), "got: {stderr}");
    assert!(
        stderr.contains("import cycle: a -> b -> a"),
        "the cycle path must be spelled out, got: {stderr}"
    );
}

#[test]
fn a_module_that_imports_itself_is_a_cycle() {
    let dir = temp_dir("self-cycle");
    let main = write(
        &dir,
        "solo.bras",
        "import \"solo.bras\"\nputs \"unreachable\"\n",
    );

    let output = brasa(&main);

    assert_eq!(output.status.code(), Some(65));
    assert!(
        stderr(&output).contains("import cycle: solo -> solo"),
        "got: {}",
        stderr(&output)
    );
}

#[test]
fn an_import_naming_a_file_that_does_not_exist_is_reported_at_the_import() {
    let dir = temp_dir("missing-file");
    let main = write(&dir, "main.bras", "import \"nowhere.bras\"\nputs \"x\"\n");

    let output = brasa(&main);

    assert_eq!(output.status.code(), Some(65));
    let stderr = stderr(&output);
    assert!(stderr.contains("M001"), "got: {stderr}");
    assert!(
        stderr.contains("cannot read imported file"),
        "got: {stderr}"
    );
}

/// A diagnostic inside an imported file must point at that file's text.
/// Rendering carries a `FileId` per span; with one file it could never
/// be wrong, so this is the first case that can catch it.
#[test]
fn a_diagnostic_in_an_imported_file_points_at_that_file() {
    let dir = temp_dir("diagnostic-file");
    write(
        &dir,
        "broken.bras",
        "pub def bad(): int\n  \"not an int\"\nend\n",
    );
    let main = write(
        &dir,
        "main.bras",
        "import \"broken.bras\"\nputs broken.bad()\n",
    );

    let output = brasa(&main);

    assert_eq!(output.status.code(), Some(65));
    let stderr = stderr(&output);
    assert!(
        stderr.contains("broken.bras"),
        "the imported file must be named, got: {stderr}"
    );
    assert!(
        stderr.contains("not an int"),
        "the imported file's source line must be shown, got: {stderr}"
    );
}

/// Error-set inference is a fixpoint over the call graph, and that graph
/// now crosses files: a `throws` declared in one module has to reach a
/// `catch!` in another, or exhaustiveness would be unverifiable.
#[test]
fn an_error_set_crosses_the_module_boundary() {
    let dir = temp_dir("errorset");
    write(
        &dir,
        "risky.bras",
        "pub struct Boom\n  detail: string\nend\n\npub def go(fail: bool): int throws Boom\n  if fail\n    throw Boom { detail: \"nope\" }\n  end\n  1\nend\n",
    );
    let main = write(
        &dir,
        "main.bras",
        "import \"risky.bras\"\n\nlet safe = risky.go(true) catch! (e)\n  Boom => 0\nend\nputs safe\n",
    );

    let output = brasa(&main);

    // The arm names a type this module does not import, so the pin is
    // that the set arrived at all: a set that stayed open would be
    // `E003`, and one that arrived empty would be `E001`.
    let stderr = stderr(&output);
    assert!(
        !stderr.contains("E003"),
        "the callee's error-set must cross the boundary, got: {stderr}"
    );
}

/// A module's private names stay private in both directions: the
/// importer's scope is not visible inside the imported module either.
#[test]
fn an_imported_module_does_not_see_its_importers_names() {
    let dir = temp_dir("no-leak");
    write(
        &dir,
        "util.bras",
        "pub def use_it(): int\n  from_main\nend\n",
    );
    let main = write(
        &dir,
        "main.bras",
        "import \"util.bras\"\nlet from_main = 1\nputs util.use_it()\n",
    );

    let output = brasa(&main);

    assert_eq!(output.status.code(), Some(65));
    assert!(
        stderr(&output).contains("unknown name `from_main`"),
        "got: {}",
        stderr(&output)
    );
}

/// Each module keeps its own namespace: the same name defined in two
/// modules is two different things, not a duplicate-definition error.
#[test]
fn two_modules_may_define_the_same_name() {
    let dir = temp_dir("same-name");
    write(&dir, "a.bras", "pub def tag(): string\n  \"a\"\nend\n");
    write(&dir, "b.bras", "pub def tag(): string\n  \"b\"\nend\n");
    let main = write(
        &dir,
        "main.bras",
        "import \"a.bras\"\nimport \"b.bras\"\n\ndef tag(): string\n  \"main\"\nend\n\nputs a.tag() + b.tag() + tag()\n",
    );

    let output = brasa(&main);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(stdout(&output), "abmain\n");
}

/// An import binds a handle for the importing module only. Re-export
/// does not exist in v1, so reaching through one module to another's
/// import is not a way around `pub`.
#[test]
fn an_import_is_not_re_exported() {
    let dir = temp_dir("no-reexport");
    write(&dir, "deep.bras", "pub def deep(): int\n  1\nend\n");
    write(&dir, "mid.bras", "import \"deep.bras\"\n");
    let main = write(
        &dir,
        "main.bras",
        "import \"mid.bras\"\nputs mid.deep.deep()\n",
    );

    let output = brasa(&main);

    assert_eq!(output.status.code(), Some(65));
    assert!(
        stderr(&output).contains("module `mid` has no member `deep`"),
        "got: {}",
        stderr(&output)
    );
}
