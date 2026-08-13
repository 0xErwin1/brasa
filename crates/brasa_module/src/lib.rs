//! The module graph: turning one entry file into every module a program
//! reaches.
//!
//! A Brasa program is a set of files, not one file
//! (`docs/spec/01-syntax.md`, Modules). This crate sits ahead of the
//! rest of the pipeline: it walks `import "path"` items from the entry
//! file, reads and parses each reachable file, lowers them all into one
//! shared [`brasa_hir::Hir`], and hands the later phases a module list
//! instead of a single root list.
//!
//! Two decisions here are load-bearing for everything downstream:
//!
//! - **One arena, many modules.** Every file lowers into the same
//!   [`brasa_hir::Hir`], so `ItemId`/`ExprId` stay globally unique and
//!   the resolution, type, error-set and codegen tables keyed by node ID
//!   need no module component. Modules are just disjoint slices of root
//!   IDs.
//! - **Post-order DFS is the module order.** [`Program::modules`] lists
//!   dependencies before their importers, which is exactly the order the
//!   spec requires top-level statements to run in. Downstream phases
//!   consume that order directly rather than recomputing it.
//!
//! Identity is the canonical path: `./util.bras`, `util.bras`, and a
//! symlink to it are one module, loaded once. A file reached twice is
//! reused; a file reached while it is still being loaded is an import
//! cycle, which is a compile error because top-level `let`s evaluate on
//! import and a cycle has no sound order.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use brasa_diagnostics::{Diagnostic, Severity, codes};
use brasa_hir::{Hir, ImportPath, Item, ItemId, Lowerer, SugarOrigin};
use brasa_source::{FileId, SourceMap, Span};

/// One loaded file.
#[derive(Debug)]
pub struct Module {
    /// The canonical path this module was loaded from; the identity the
    /// loader deduplicates on.
    pub path: PathBuf,
    pub file: FileId,
    /// The name this module binds in an importer's scope: the file stem
    /// (`docs/spec/01-syntax.md`).
    pub name: String,
    /// Top-level HIR items in source order.
    pub roots: Vec<ItemId>,
    /// Where each file-import item in `roots` points, as an index into
    /// [`Program::modules`]. A missing entry means the import failed to
    /// load and was already reported; the later phases treat its
    /// binding as opaque rather than cascading.
    pub imports: HashMap<ItemId, usize>,
}

/// Every module one entry file reaches, lowered into one HIR.
pub struct Program {
    pub hir: Hir,
    /// Post-order DFS from the entry file: a module always follows the
    /// modules it imports.
    pub modules: Vec<Module>,
    /// The file the user invoked. Its `main` is the program's entry
    /// point; an imported module's `main` is never called
    /// (`docs/spec/01-syntax.md`).
    pub entry: usize,
    pub sugar_origins: HashMap<brasa_hir::ExprId, SugarOrigin>,
    pub diagnostics: Vec<Diagnostic>,
}

impl Program {
    pub fn module(&self, ix: usize) -> &Module {
        &self.modules[ix]
    }

    /// Every module's roots concatenated in module order — the shape the
    /// phases that do not care about module boundaries want.
    pub fn all_roots(&self) -> Vec<ItemId> {
        self.modules
            .iter()
            .flat_map(|module| module.roots.iter().copied())
            .collect()
    }
}

/// How deep a chain of imports the loader follows. A cycle is caught by
/// identity rather than by depth, so reaching this means a genuinely
/// unbounded generated graph; the limit exists so it fails with a
/// diagnostic instead of a host stack overflow.
const MAX_IMPORT_DEPTH: usize = 128;

/// Loads `entry` and everything it imports.
///
/// `entry` is expected to be readable — the caller checked it, and its
/// own read failure is the caller's to report. Every failure *inside*
/// the graph (an unreadable import, a cycle, a parse error in an
/// imported file) lands in [`Program::diagnostics`] and still produces a
/// module list, so one run reports every problem it can see.
pub fn load(entry: &Path, sources: &mut SourceMap) -> Program {
    let mut loader = Loader {
        sources,
        lowerer: Lowerer::new(),
        modules: Vec::new(),
        loaded: HashMap::new(),
        stack: Vec::new(),
        diagnostics: Vec::new(),
    };

    let canonical = canonicalize(entry);
    let entry_ix = loader
        .load_file(&canonical, None, 0)
        .unwrap_or_else(|| loader.push_empty(canonical));

    let (hir, sugar_origins, lower_diagnostics) = loader.lowerer.finish();

    let mut diagnostics = loader.diagnostics;
    diagnostics.extend(lower_diagnostics);

    Program {
        hir,
        modules: loader.modules,
        entry: entry_ix,
        sugar_origins,
        diagnostics,
    }
}

/// The canonical form of a path, or the path itself when the OS cannot
/// canonicalize it (it does not exist yet, or a component is denied).
/// A non-canonical key can only ever make the loader treat one file as
/// two; the read that follows reports the real problem.
fn canonicalize(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The name a file binds in an importer's scope: its stem
/// (`docs/spec/01-syntax.md`). A path with no usable stem keeps its
/// whole display form so diagnostics still name something.
fn module_name(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

struct Loader<'a> {
    sources: &'a mut SourceMap,
    lowerer: Lowerer,
    modules: Vec<Module>,
    /// Canonical path to the module it produced, so a file reached twice
    /// is loaded once.
    loaded: HashMap<PathBuf, usize>,
    /// The chain currently being loaded, innermost last; a path already
    /// on it is a cycle.
    stack: Vec<PathBuf>,
    diagnostics: Vec<Diagnostic>,
}

impl Loader<'_> {
    fn error(
        &mut self,
        code: &'static str,
        span: Span,
        message: String,
        label: &str,
    ) -> Diagnostic {
        Diagnostic::new(Severity::Error, message, code.to_string(), span)
            .with_label(span, label.to_string())
    }

    /// A module with no items, standing in for a file that could not be
    /// loaded at all. Only the entry file needs one: it must exist for
    /// [`Program::entry`] to point somewhere.
    fn push_empty(&mut self, path: PathBuf) -> usize {
        let name = module_name(&path);
        let file = self.sources.add_file(path.clone(), String::new());

        self.push(Module {
            path,
            file,
            name,
            roots: Vec::new(),
            imports: HashMap::new(),
        })
    }

    fn push(&mut self, module: Module) -> usize {
        let ix = self.modules.len();
        self.modules.push(module);
        ix
    }

    /// Loads one canonical path and everything it imports, returning its
    /// index. `at` is the import that asked for it, for diagnostics;
    /// `None` for the entry file. Returns `None` when the file could not
    /// be read or parsed.
    fn load_file(&mut self, path: &Path, at: Option<Span>, depth: usize) -> Option<usize> {
        if let Some(&ix) = self.loaded.get(path) {
            return Some(ix);
        }

        let source = match std::fs::read_to_string(path) {
            Ok(source) => source,
            Err(err) => {
                // The entry file's own read failure belongs to the
                // caller, which checked it before calling in.
                if let Some(span) = at {
                    let message = format!("cannot read imported file `{}`: {err}", path.display());
                    let diagnostic =
                        self.error(codes::M_UNREADABLE_IMPORT, span, message, "imported here");
                    self.diagnostics.push(diagnostic);
                }
                return None;
            }
        };

        let file = self.sources.add_file(path.to_path_buf(), source.clone());

        let parsed = brasa_parser::parse(&source, file);
        let parse_failed = parsed
            .diagnostics
            .iter()
            .any(|diag| diag.severity == Severity::Error);
        self.diagnostics.extend(parsed.diagnostics);

        // A file that did not parse has no trustworthy import list and
        // no trustworthy items; lowering it would only cascade.
        if parse_failed {
            return None;
        }

        let roots = self.lowerer.lower_file(&parsed.ast, &parsed.roots);

        self.stack.push(path.to_path_buf());
        let imports = self.load_imports(path, &roots, depth);
        self.stack.pop();

        let ix = self.push(Module {
            path: path.to_path_buf(),
            file,
            name: module_name(path),
            roots,
            imports,
        });
        self.loaded.insert(path.to_path_buf(), ix);

        Some(ix)
    }

    /// Follows every file import declared by one module.
    fn load_imports(
        &mut self,
        importer: &Path,
        roots: &[ItemId],
        depth: usize,
    ) -> HashMap<ItemId, usize> {
        let mut imports = HashMap::new();

        let file_imports: Vec<(ItemId, String, Span)> = roots
            .iter()
            .filter_map(|&root| {
                let Item::Import(import) = self.lowerer.hir().item(root) else {
                    return None;
                };
                let ImportPath::File(path) = &import.path else {
                    return None;
                };
                Some((root, path.clone(), self.lowerer.hir().span_of_item(root)))
            })
            .collect();

        for (root, raw, span) in file_imports {
            let Some(target) = self.resolve_import(importer, &raw, span, depth) else {
                continue;
            };

            if let Some(ix) = self.load_file(&target, Some(span), depth + 1) {
                imports.insert(root, ix);
            }
        }

        imports
    }

    /// Turns one `import "path"` into the canonical path it names, or
    /// reports why it cannot be followed.
    fn resolve_import(
        &mut self,
        importer: &Path,
        raw: &str,
        span: Span,
        depth: usize,
    ) -> Option<PathBuf> {
        if depth >= MAX_IMPORT_DEPTH {
            let message = format!(
                "import chain is deeper than {MAX_IMPORT_DEPTH} files and was not followed"
            );
            let diagnostic = self.error(
                codes::M_IMPORTS_TOO_DEEP,
                span,
                message,
                "too deeply imported",
            );
            self.diagnostics.push(diagnostic);
            return None;
        }

        // "relative to the importing file" (`docs/spec/01-syntax.md`).
        // An absolute path in an import is not spelled by the spec, and
        // `join` already takes it verbatim.
        let base = importer.parent().unwrap_or_else(|| Path::new("."));
        let target = canonicalize(&base.join(raw));

        if let Some(position) = self.stack.iter().position(|entry| entry == &target) {
            self.report_cycle(position, &target, span);
            return None;
        }

        Some(target)
    }

    /// Reports an import cycle, naming every file on it in the order the
    /// imports were followed. A bare "cycle detected" would leave the
    /// reader to reconstruct the chain by hand, and the chain is the
    /// whole content of the error.
    fn report_cycle(&mut self, start: usize, target: &Path, span: Span) {
        let mut chain: Vec<String> = self.stack[start..]
            .iter()
            .map(|path| module_name(path))
            .collect();
        chain.push(module_name(target));

        let message = format!("import cycle: {}", chain.join(" -> "));
        let diagnostic = self
            .error(codes::M_IMPORT_CYCLE, span, message, "closes the cycle")
            .with_note(
                "a module's top-level statements run when it is first imported, so a cycle has no order that initializes both sides"
                    .to_string(),
            );
        self.diagnostics.push(diagnostic);
    }
}
