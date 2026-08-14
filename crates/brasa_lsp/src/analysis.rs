//! One analysis of one module graph, and the questions an editor asks
//! of it.
//!
//! # Why this is not the CLI's `compile_program`
//!
//! The CLI gates each phase on the previous one being clean: an
//! unresolved tree is not type-checked, because doing so only produces
//! cascades nobody asked for (`docs/spec/06-diagnostics.md`). An editor
//! wants the opposite trade. The file under the cursor is almost always
//! broken somewhere, and the type three functions away from the break is
//! still useful and still correct.
//!
//! So every phase runs here unconditionally and every phase's
//! diagnostics are reported together. That is the shape
//! `crates/brasa/tests/partial.rs` established for BRS-114, which is
//! what settled that the phases tolerate holes — this is that finding
//! turned into a caller.
//!
//! # Why there is no incrementality
//!
//! Because the whole pipeline is affordable per keystroke: BRS-114
//! measured it over the largest bundled script and
//! `the_whole_pipeline_is_affordable_per_keystroke` defends the number.
//! A query system would be complexity bought against a cost nobody has
//! demonstrated, so every request re-analyses from the entry file.

use std::path::Path;

use brasa_diagnostics::Diagnostic;
use brasa_errorset::{ErrorSet, ErrorSetResult};
use brasa_hir::{Expr, ExprId, Item, ItemId};
use brasa_module::{Overlay, Program};
use brasa_resolver::{DefRef, ModuleView, Res, Resolutions};
use brasa_source::{FileId, SourceMap, Span};
use brasa_typeck::TypeTables;

/// Everything one analysis produced, kept together because one hover
/// answer reads across three of them.
pub struct Analysis {
    pub sources: SourceMap,
    pub program: Program,
    pub resolutions: Resolutions,
    pub types: TypeTables,
    pub error_sets: ErrorSetResult,
    /// Every phase's diagnostics, in phase order.
    pub diagnostics: Vec<Diagnostic>,
}

/// Runs every phase over `entry`'s module graph, with `overlay`'s
/// unsaved buffers layered over the files.
///
/// Never returns early. A phase that did would make the editor go
/// silent exactly when the user is mid-edit.
pub fn analyze(entry: &Path, overlay: &Overlay) -> Analysis {
    let mut sources = SourceMap::new();
    let program = brasa_module::load_partial(entry, &mut sources, overlay);

    let roots = program.all_roots();

    let import_maps: Vec<_> = program
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
    let checked = brasa_typeck::check(
        &program.hir,
        &roots,
        &resolved.resolutions,
        &program.sugar_origins,
    );
    let inferred =
        brasa_errorset::infer(&program.hir, &roots, &resolved.resolutions, &checked.types);

    let mut diagnostics = program.diagnostics.clone();
    diagnostics.extend(resolved.diagnostics);
    diagnostics.extend(checked.diagnostics);
    diagnostics.extend(inferred.diagnostics.clone());

    Analysis {
        sources,
        program,
        resolutions: resolved.resolutions,
        types: checked.types,
        error_sets: inferred,
        diagnostics,
    }
}

/// What a hover shows.
///
/// The two halves are not symmetric, and that is the error-set pass's
/// granularity rather than a simplification here. Sets are inferred per
/// FUNCTION — `ErrorSetResult::sets` is keyed by `DefRef` — so there is
/// no set to show for an arbitrary subexpression. A hover gets one
/// where the cursor is somewhere that HAS one: inside a function body,
/// or on a call to a function.
pub struct Hover {
    /// The rendered type, absent on a node the checker never typed.
    pub ty: Option<String>,
    /// An inferred error-set, rendered as a `throws` clause.
    pub throws: Option<String>,
    /// What the answer is about, for the client to underline.
    pub span: Span,
}

impl Analysis {
    /// The `FileId` this analysis knows `path` by, or `None` when the
    /// module graph never reached it.
    ///
    /// A file the entry does not import is not in the graph, and an
    /// editor asking about one gets nothing rather than an answer drawn
    /// from the wrong file.
    pub fn file_of(&self, path: &Path) -> Option<FileId> {
        let canonical = std::fs::canonicalize(path);
        let canonical = canonical.as_deref().unwrap_or(path);

        self.sources
            .lookup_by_path(canonical)
            .or_else(|| self.sources.lookup_by_path(path))
    }

    /// The hover answer at `offset` in `file`.
    ///
    /// Three kinds of node can be under a cursor and each knows its
    /// own type: a checked expression, a local's binding site, and an
    /// item. They are gathered and the SMALLEST-spanned one wins,
    /// rather than one kind being tried before another.
    ///
    /// Preferring a kind was the first attempt and it was wrong. A
    /// binder's span is the whole `let` statement, not just the name,
    /// so checking binders first made `let n = parse("42")` answer
    /// about `n` no matter where in the line the cursor was — hovering
    /// the call gave the binding. Smallest-span is the only rule that
    /// does not need to know which kinds nest inside which.
    pub fn hover(&self, file: FileId, offset: u32) -> Option<Hover> {
        let hir = &self.program.hir;

        // Only nodes the checker actually typed are candidates.
        //
        // `Hir::exprs` walks the arena and over-approximates by design:
        // a node lowering allocated and then dropped is still in it. A
        // dropped node was never checked, so it is absent from
        // `expr_types` — filtering on that is what keeps a hover from
        // answering about a node no longer in any body, which could
        // otherwise win the contest outright.
        let expr = self.innermost_typed_expr(file, offset);

        // A call under the cursor is what makes a callee's set
        // reachable, since a set belongs to a definition rather than to
        // any expression. Failing that, the function the cursor is
        // inside is the one whose set is worth showing.
        let throws = expr
            .and_then(|id| self.throws_of_call(id))
            .or_else(|| self.throws_of_enclosing_function(file, offset))
            .map(|set| brasa_errorset::dump::render_throws_clause(hir, set));

        let mut candidates: Vec<(Span, Option<String>)> = Vec::new();

        if let Some(id) = expr {
            candidates.push((
                hir.span_of_expr(id),
                self.types.expr_types.get(&id).map(|ty| ty.display(hir)),
            ));
        }
        if let Some((span, ty)) = self.innermost_local(file, offset) {
            candidates.push((span, ty));
        }
        if let Some(id) = self.innermost_item(file, offset) {
            candidates.push((
                hir.span_of_item(id),
                self.types.item_types.get(&id).map(|ty| ty.display(hir)),
            ));
        }

        let (span, ty) = candidates
            .into_iter()
            .min_by_key(|(span, _)| span_len(*span))?;

        Some(Hover { ty, throws, span })
    }

    /// The smallest binding site covering `offset`, with its type.
    ///
    /// Binders are their own candidate because a binding site is not an
    /// expression: `let count = 21` has nothing under `count` for the
    /// expression search to find, and without this the answer would
    /// fall through to the enclosing function and read `() -> unit`.
    fn innermost_local(&self, file: FileId, offset: u32) -> Option<(Span, Option<String>)> {
        let hir = &self.program.hir;

        let (id, info) = self
            .resolutions
            .locals
            .iter()
            .enumerate()
            .filter(|(_, info)| covers(info.span, file, offset))
            .min_by_key(|(_, info)| span_len(info.span))?;

        let local = brasa_resolver::LocalId(id as u32);

        Some((
            info.span,
            self.types.local_types.get(&local).map(|ty| ty.display(hir)),
        ))
    }

    /// The smallest-spanned CHECKED expression covering `offset`.
    ///
    /// Smallest wins because spans nest: on `f(x)`, hovering `x` must
    /// answer about `x` and not about the call enclosing it.
    fn innermost_typed_expr(&self, file: FileId, offset: u32) -> Option<ExprId> {
        let hir = &self.program.hir;

        hir.exprs()
            .map(|(id, _)| id)
            .filter(|id| self.types.expr_types.contains_key(id))
            .filter(|&id| covers(hir.span_of_expr(id), file, offset))
            .min_by_key(|&id| span_len(hir.span_of_expr(id)))
    }

    /// The smallest-spanned item covering `offset`.
    fn innermost_item(&self, file: FileId, offset: u32) -> Option<ItemId> {
        let hir = &self.program.hir;

        hir.items()
            .map(|(id, _)| id)
            .filter(|&id| covers(hir.span_of_item(id), file, offset))
            .min_by_key(|&id| span_len(hir.span_of_item(id)))
    }

    /// The error-set of what this expression CALLS, when it is a call
    /// to something the resolver named.
    fn throws_of_call(&self, expr: ExprId) -> Option<&ErrorSet> {
        let hir = &self.program.hir;

        let Expr::Call { callee, .. } = hir.expr(expr) else {
            return None;
        };

        match self.resolutions.expr_res.get(callee) {
            Some(Res::Item(item)) => self.error_sets.sets.get(&DefRef::Item(*item)),
            _ => None,
        }
    }

    /// The error-set of the function body the cursor is inside.
    ///
    /// This is what makes hovering anywhere in a function answer with
    /// what that function throws — the question the ticket is named
    /// for, and the one per-function granularity answers best.
    fn throws_of_enclosing_function(&self, file: FileId, offset: u32) -> Option<&ErrorSet> {
        let hir = &self.program.hir;

        let item = hir
            .items()
            .filter(|(_, item)| matches!(item, Item::FuncDef(_)))
            .map(|(id, _)| id)
            .filter(|&id| covers(hir.span_of_item(id), file, offset))
            .min_by_key(|&id| span_len(hir.span_of_item(id)))?;

        self.error_sets.sets.get(&DefRef::Item(item))
    }
}

/// Whether `span` is in `file` and covers `offset`.
///
/// The end is inclusive so a caret resting just past the last byte of a
/// name still hovers it — which is exactly where an editor leaves it
/// after typing one.
fn covers(span: Span, file: FileId, offset: u32) -> bool {
    span.file == file && span.start.0 <= offset && offset <= span.end.0
}

fn span_len(span: Span) -> u32 {
    span.end.0.saturating_sub(span.start.0)
}
