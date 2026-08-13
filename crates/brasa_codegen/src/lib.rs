//! HIR→bytecode code generation for Brasa (BRS-27, M3).
//!
//! Consumes the checked core HIR — the lowered [`brasa_hir::Hir`], its
//! roots, the resolver's [`brasa_resolver::Resolutions`], and the
//! checker's [`brasa_typeck::TypeTables`] — and produces a
//! [`brasa_bytecode::Module`] per the normative design in
//! `docs/spec/07-bytecode.md`. The observable-behavior oracle is the
//! conformance corpus (`brasa_vm/tests/conformance.rs`): where compiled
//! code and a pinned expectation disagree, this crate has a bug. That
//! role belonged to the reference tree-walker until BRS-108.
//!
//! Decisions this unit fixes (the spec fixes primitives, not strategy):
//!
//! - **Local slots**: every [`brasa_resolver::LocalId`] bound in a
//!   function maps to a dense frame slot — parameters first (`self` is
//!   slot 0 in methods), then capture slots, then the remaining locals
//!   in first-encounter order. Distinct `LocalId`s always get distinct
//!   slots, so shadowing needs no runtime support.
//! - **Capture order contract**: a closure captures, in this exact
//!   order, the enclosing `self` (when the lambda uses it) followed by
//!   the lambda's free `LocalId`s in ascending `LocalId` order. The
//!   free set is every local referenced anywhere in the lambda body
//!   (nested lambdas included) minus every local bound inside it.
//!   [`brasa_bytecode::Op::MakeClosure`] takes the values in that
//!   order; the VM copies them into the frame's capture slots in the
//!   same order.
//! - **Captures share bindings, not values**: a closure captures the
//!   lexical binding (`docs/spec/01-syntax.md`), so rebinding the name
//!   is observable from both sides. A binding both captured and
//!   rebound is boxed into a heap cell that the capture slot and the
//!   binding scope's own slot point at; reads and writes go through
//!   [`brasa_bytecode::Op::LoadBinding`] /
//!   [`brasa_bytecode::Op::StoreBinding`], and each execution of the
//!   binding site makes a fresh cell.
//!
//!   The cell is skipped where it cannot be observed. If NOTHING ever
//!   rebinds a captured binding, its cell would hold one value for its
//!   whole life, so copying that value into the closure is
//!   indistinguishable from sharing a cell — the analysis
//!   (`crate::bindings`) boxes only the intersection of "captured" and
//!   "rebound". This is a representation choice with no semantics
//!   attached: the uniform rule is the one in the spec, and this
//!   paragraph is the only place the exception exists.
//! - **`match` compilation**: straightforward left-to-right arm testing
//!   over the spec's decision-tree primitives (`dup`,
//!   `jump_if_variant_ne`, `jump_if_none`, `unwrap_some`,
//!   `tuple_field`/`enum_field`, `eq` + `jump_if_false`). Arms are few
//!   and shapes shallow in real programs, so a column-reordering
//!   decision tree buys nothing yet; the primitives leave that door
//!   open without a format change.
//! - **Returns** compile to a direct `ret` at each `return` site (no
//!   shared epilogue); functions without a declared return type emit
//!   `load_unit` before `ret`.
//! - **Statically-detected fatals** (an unavailable module member, a
//!   builtin used as a value, `break` outside a loop) compile to the
//!   internal `<fatal>` registry builtin; runtime match fall-through
//!   and a `for` pattern mismatch compile to `<assert-failed>`
//!   (`panics.AssertionFailed`), keeping the instruction set unchanged
//!   (`brasa_bytecode::builtin`).
//!
//!   Every one of these is unreachable in a checked program: the
//!   frontend rejects the condition first, with `T032`/`T033` for the
//!   three that used to reach run time (BRS-109). They are kept as
//!   defence in depth — a frontend gap should surface as a clean fatal
//!   rather than as miscompiled code.

mod bindings;
mod captures;
mod catch;
mod context;
mod depth;
mod expr;
mod func;
mod item;
mod limits;
mod pattern;
mod stmt;

use brasa_bytecode::Module;
use brasa_diagnostics::Diagnostic;
use brasa_hir::{Hir, ItemId};
use brasa_resolver::Resolutions;
use brasa_typeck::TypeTables;

/// The outcome of one compilation: the module, and every bytecode limit
/// the program broke (`crate::limits`).
///
/// `diagnostics` is empty for every program that fits the instruction
/// set. When it is not, `module` is empty and must not be executed: the
/// limits are checked while lowering, and lowering clamps what it cannot
/// encode so one run can report them all.
pub struct CompileResult {
    pub module: Module,
    pub diagnostics: Vec<Diagnostic>,
}

/// Compiles the checked module rooted at `roots` into a bytecode
/// [`Module`]: `functions[0]` is the synthetic `<toplevel>` (top-level
/// statements and top-`let` initializers in source order), followed by
/// declared functions, struct methods, and lambdas in compile order.
pub fn compile(
    hir: &Hir,
    roots: &[ItemId],
    resolutions: &Resolutions,
    types: &TypeTables,
) -> CompileResult {
    compile_program(hir, roots, roots, resolutions, types)
}

/// Compiles a whole module graph into one bytecode [`Module`].
///
/// `roots` is every module's items concatenated in the loader's
/// post-order, which is also the order `<toplevel>` runs them in
/// (`docs/spec/01-syntax.md`: a module's top level runs the first time
/// it is imported, dependencies first). `entry_roots` is the executed
/// file's slice of that list: only its `main` becomes
/// [`Module::entry`], because an imported module's `main` is never
/// invoked.
pub fn compile_program(
    hir: &Hir,
    roots: &[ItemId],
    entry_roots: &[ItemId],
    resolutions: &Resolutions,
    types: &TypeTables,
) -> CompileResult {
    compile_inner(hir, roots, entry_roots, resolutions, types, false)
}

/// Compiles a program together with its `test` items, for `brasa test`.
///
/// Tests come from the ENTRY module only. A test belongs to the file it
/// is written in, and running an imported library's tests as a side
/// effect of importing it would make `brasa test` mean something
/// different depending on what the file happens to depend on.
pub fn compile_tests(
    hir: &Hir,
    roots: &[ItemId],
    entry_roots: &[ItemId],
    resolutions: &Resolutions,
    types: &TypeTables,
) -> CompileResult {
    compile_inner(hir, roots, entry_roots, resolutions, types, true)
}

fn compile_inner(
    hir: &Hir,
    roots: &[ItemId],
    entry_roots: &[ItemId],
    resolutions: &Resolutions,
    types: &TypeTables,
    with_tests: bool,
) -> CompileResult {
    let mut cx = context::Cx::new(hir, resolutions, types);

    cx.collect(roots);
    if with_tests {
        cx.collect_tests(entry_roots);
    }

    // Bodies are lowered against the shapes and slot maps `collect`
    // assigned, so a shape that already broke a limit would have every
    // body reporting the same cause again, at worse spans.
    if cx.diagnostics.is_empty() {
        item::compile_toplevel(&mut cx, roots);
        item::compile_items(&mut cx, roots);
        item::compile_tests(&mut cx);
        cx.entry = item::find_entry(&cx, entry_roots);
    }

    cx.finish()
}
