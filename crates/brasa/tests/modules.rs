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
    // `BRASA_PATH` is cleared rather than inherited: a developer who has
    // one set would otherwise be running a different search path from
    // CI, and the search-path tests below would mean nothing.
    Command::new(env!("CARGO_BIN_EXE_brasa"))
        .arg(entry)
        .env_remove("BRASA_PATH")
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

// --- qualified type paths (BRS-115) ---------------------------------

/// An exported struct, named through the module in every type position
/// an annotation can occupy, and constructed through the same path.
#[test]
fn an_exported_struct_is_nameable_and_constructible_through_the_module() {
    let dir = temp_dir("qualified-struct");
    write(
        &dir,
        "geo.bras",
        "pub struct Point\n  x: int\n  y: int\nend\n\npub def origin(): Point\n  Point { x: 0, y: 0 }\nend\n",
    );
    let main = write(
        &dir,
        "main.bras",
        concat!(
            "import \"geo.bras\"\n\n",
            "let p: geo.Point = geo.Point { x: 3, y: 4 }\n",
            "let all: Vector<geo.Point> = [p, geo.origin()]\n\n",
            "def far(q: geo.Point): bool\n  q.x > 2\nend\n\n",
            "puts p.x\n",
            "puts all.len()\n",
            "puts far(p)\n",
        ),
    );

    let output = brasa(&main);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(stdout(&output), "3\n2\ntrue\n");
}

/// An exported enum's constructors, in expression and pattern position.
#[test]
fn an_exported_enums_constructors_work_qualified_in_both_positions() {
    let dir = temp_dir("qualified-enum");
    write(
        &dir,
        "paint.bras",
        "pub enum Color\n  Red\n  Blue(shade: int)\nend\n",
    );
    let main = write(
        &dir,
        "main.bras",
        concat!(
            "import \"paint.bras\"\n\n",
            "def name(c: paint.Color): string\n",
            "  match c\n",
            "    paint.Red => \"red\"\n",
            "    paint.Blue(n) => \"blue #{n}\"\n",
            "  end\n",
            "end\n\n",
            "puts name(paint.Red)\n",
            "puts name(paint.Blue(7))\n",
        ),
    );

    let output = brasa(&main);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(stdout(&output), "red\nblue 7\n");
}

/// The point of the whole feature for error handling: an importer can
/// name the error type it catches. BRS-97 could only pin that the SET
/// crossed the boundary; the arm itself could not be written.
#[test]
fn a_catch_arm_can_name_an_imported_error_type() {
    let dir = temp_dir("qualified-catch");
    write(
        &dir,
        "risky.bras",
        "pub struct Boom\n  detail: string\nend\n\npub def go(fail: bool): int throws Boom\n  if fail\n    throw Boom { detail: \"nope\" }\n  end\n  1\nend\n",
    );
    let main = write(
        &dir,
        "main.bras",
        concat!(
            "import \"risky.bras\"\n\n",
            "let caught = risky.go(true) catch! (e)\n  risky.Boom => 0\nend\n",
            "let clean = risky.go(false) catch! (e)\n  risky.Boom => 0\nend\n",
            "puts caught\n",
            "puts clean\n",
        ),
    );

    let output = brasa(&main);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "0\n1\n",
        "the arm must match the value's nominal tag `Boom`, not the path `risky.Boom`"
    );
}

/// `pub` gates the type namespace exactly as it gates the value one.
#[test]
fn a_private_type_is_not_nameable_from_an_importer() {
    let dir = temp_dir("qualified-private");
    write(&dir, "geo.bras", "struct Point\n  x: int\nend\n");
    let main = write(
        &dir,
        "main.bras",
        "import \"geo.bras\"\nlet p: geo.Point = geo.Point { x: 1 }\n",
    );

    let output = brasa(&main);

    assert_eq!(output.status.code(), Some(65));
    let stderr = stderr(&output);
    assert!(stderr.contains("R013"), "got: {stderr}");
    assert!(
        stderr.contains("`Point` is not exported by module `geo`"),
        "got: {stderr}"
    );
}

/// An unqualified name that an imported module DOES export should say
/// so: the fix is to qualify it, and the resolver knows which module.
#[test]
fn an_unqualified_name_is_told_which_module_exports_it() {
    let dir = temp_dir("qualified-hint");
    write(&dir, "geo.bras", "pub struct Point\n  x: int\nend\n");
    let main = write(
        &dir,
        "main.bras",
        "import \"geo.bras\"\ndef f(p: Point): int\n  p.x\nend\n",
    );

    let output = brasa(&main);

    assert_eq!(output.status.code(), Some(65));
    let stderr = stderr(&output);
    assert!(stderr.contains("R003"), "got: {stderr}");
    assert!(
        stderr.contains("module `geo` exports `Point`; write it as `geo.Point`"),
        "got: {stderr}"
    );
}

/// A dotted name is only a module path when its root is bound to one.
/// `panics.` is reserved and needs no import, and a `std::` module's
/// native errors keep their own meaning — so importing a file named
/// `fs.bras` does not silently reinterpret `fs.NotFound`.
#[test]
fn a_native_error_arm_is_not_shadowed_by_an_unrelated_import() {
    let dir = temp_dir("qualified-native");
    write(&dir, "helper.bras", "pub def noop(): int\n  1\nend\n");
    let main = write(
        &dir,
        "main.bras",
        concat!(
            "import std::fs\n",
            "import \"helper.bras\"\n\n",
            "let text = fs.read(\"definitely-missing-file\") catch (e)\n",
            "  fs.NotFound => \"missing\"\n",
            "end\n",
            "puts text\n",
            "puts helper.noop()\n",
        ),
    );

    let output = brasa(&main);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(stdout(&output), "missing\n1\n");
}

/// Field and method access is unaffected: the lexical class after the
/// dot is what tells a qualified path from a member access, so a
/// lowercase member on a value never takes the path branch.
#[test]
fn a_member_access_on_a_value_is_not_read_as_a_path() {
    let dir = temp_dir("qualified-member");
    write(
        &dir,
        "geo.bras",
        "pub struct Point\n  x: int\n\n  def doubled(self): int\n    self.x * 2\n  end\nend\n",
    );
    let main = write(
        &dir,
        "main.bras",
        "import \"geo.bras\"\nlet p = geo.Point { x: 5 }\nputs p.x\nputs p.doubled()\n",
    );

    let output = brasa(&main);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(stdout(&output), "5\n10\n");
}

// --- the module search path (BRS-102) --------------------------------

fn brasa_with_path(entry: &Path, search_path: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_brasa"))
        .arg(entry)
        .env("BRASA_PATH", search_path)
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn brasa")
}

/// A `lib` directory beside the executed file is on the search path with
/// no configuration at all, which is what makes vendoring work.
#[test]
fn a_lib_directory_beside_the_script_is_searched() {
    let dir = temp_dir("search-lib");
    write(
        &dir,
        "lib/text/slug.bras",
        "pub def slugify(s: string): string\n  s.trim().toLower().replace(\" \", \"-\")\nend\n",
    );
    let main = write(
        &dir,
        "main.bras",
        "import text::slug\nputs slug.slugify(\"Hola Mundo\")\n",
    );

    let output = brasa(&main);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(stdout(&output), "hola-mundo\n");
}

#[test]
fn brasa_path_entries_are_searched_in_order() {
    let dir = temp_dir("search-order");
    write(
        &dir,
        "first/pick/me.bras",
        "pub def who(): string\n  \"first\"\nend\n",
    );
    write(
        &dir,
        "second/pick/me.bras",
        "pub def who(): string\n  \"second\"\nend\n",
    );
    let main = write(&dir, "main.bras", "import pick::me\nputs me.who()\n");

    let path = format!(
        "{}:{}",
        dir.join("first").display(),
        dir.join("second").display()
    );
    let output = brasa_with_path(&main, &path);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(stdout(&output), "first\n", "the first root wins");
}

/// `std` is reserved: it names builtins and is never looked for on disk,
/// so a directory called `std` cannot shadow the standard library.
#[test]
fn a_std_directory_on_the_search_path_does_not_shadow_the_stdlib() {
    let dir = temp_dir("search-std");
    write(
        &dir,
        "lib/std/math.bras",
        "pub def abs(x: int): int\n  99\nend\n",
    );
    let main = write(&dir, "main.bras", "import std::math\nputs math.abs(-3)\n");

    let output = brasa(&main);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(stdout(&output), "3\n", "the real `math.abs` must be used");
}

/// The failure a user actually hits, and the one thing that makes it
/// diagnosable: which directories were searched.
#[test]
fn a_module_missing_from_the_search_path_names_the_roots_it_tried() {
    let dir = temp_dir("search-missing");
    let main = write(&dir, "main.bras", "import nowhere::at_all\nputs 1\n");

    let output = brasa(&main);

    assert_eq!(output.status.code(), Some(65));
    let stderr = stderr(&output);
    assert!(stderr.contains("M004"), "got: {stderr}");
    assert!(
        stderr.contains("cannot find module `nowhere::at_all` on the search path"),
        "got: {stderr}"
    );
    assert!(
        stderr.contains("nowhere/at_all.bras") && stderr.contains("lib"),
        "the roots searched must be named, got: {stderr}"
    );
}

/// A searched module is a module like any other: it may import its own
/// dependencies relatively, and identity still deduplicates.
#[test]
fn a_searched_module_may_import_relatively_and_stays_one_instance() {
    let dir = temp_dir("search-nested");
    write(
        &dir,
        "lib/pkg/helper.bras",
        "puts \"helper loaded\"\npub let tag = \"h\"\n",
    );
    write(
        &dir,
        "lib/pkg/main.bras",
        "import \"helper.bras\"\n\npub def label(): string\n  \"pkg-\" + helper.tag\nend\n",
    );
    let main = write(
        &dir,
        "main.bras",
        "import pkg::main\nimport pkg::helper\nputs main.label()\nputs helper.tag\n",
    );

    let output = brasa(&main);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "helper loaded\npkg-h\nh\n",
        "the helper is reached two ways and must load once"
    );
}

/// A `std::` path naming no std module is still the resolver's `R009`,
/// not a search-path miss: `std` is closed, so a typo there is a
/// different mistake from a module that is merely not installed.
#[test]
fn an_unknown_std_module_is_still_reported_as_such() {
    let dir = temp_dir("search-std-typo");
    let main = write(&dir, "main.bras", "import std::netz\nputs 1\n");

    let output = brasa(&main);

    assert_eq!(output.status.code(), Some(65));
    let stderr = stderr(&output);
    assert!(stderr.contains("R009"), "got: {stderr}");
    assert!(
        stderr.contains("unknown std module `netz`"),
        "got: {stderr}"
    );
}
