//! Error-set inference for Brasa (BRS-22): the interprocedural fixpoint
//! over the call graph.
//!
//! Per `docs/spec/04-errors.md` ("Error-set inference"), the error-set
//! of a function is "the set of types it can throw: its own `throw`s ∪
//! the error-sets of what it calls, minus what it catches. It is an
//! interprocedural fixpoint analysis (recursion converges because the
//! sets only grow and are finite)." The set is derived metadata, not
//! part of the written signature, and it travels between modules.
//!
//! The crate COMPUTES the sets, exposes them as side tables
//! (`docs/spec/00-vision.md`, the `error_sets: Map<FuncId, ErrorSet>`
//! row), and then runs the checks that consume them (BRS-23):
//! unreachable arms, `catch!` exhaustiveness, `throws` verification,
//! and the rendering contract (a `toString` override's set must be
//! empty) — see [`check`] for the rules and recorded decisions.
//! The pass runs after type checking because tagging a thrown value
//! needs its type.
//!
//! Panics never appear in an error-set: they are a separate channel
//! (`docs/spec/04-errors.md`, "Panics vs errors"), so indexing,
//! division, and arithmetic contribute nothing here.
//!
//! Top-level code (`Item::Stmt` blocks and `TopLet` initializers) is
//! analyzed as one pseudo-body during the post-convergence checking
//! pass, so its `catch`/`catch!` expressions get the same
//! E001/E002/E003 checks as any function body. The top level declares
//! no `throws` contract, so its set may be non-empty without any
//! diagnostic: an uncaught top-level throw ends the script at runtime
//! (exit 70), and the set itself is discarded. The returned sets are
//! keyed by [`DefRef`] only, plus one set per lambda literal reachable
//! from a function or method body.

pub mod dump;

mod check;
mod collect;

use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap};

use brasa_diagnostics::Diagnostic;
use brasa_hir::{ExprId, Hir, Item, ItemId};
use brasa_resolver::{DefRef, Resolutions};
use brasa_typeck::TypeTables;

use collect::Collector;

/// A throwable primitive type. `throw` accepts any value
/// (`docs/spec/04-errors.md`, "Throwing"), so primitives are legitimate
/// error tags alongside nominal structs and enums.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Primitive {
    Int,
    Float,
    Bool,
    String,
    Char,
    Unit,
}

impl Primitive {
    pub fn name(self) -> &'static str {
        match self {
            Primitive::Int => "int",
            Primitive::Float => "float",
            Primitive::Bool => "bool",
            Primitive::String => "string",
            Primitive::Char => "char",
            Primitive::Unit => "unit",
        }
    }
}

/// One element of an error-set. Matching is nominal
/// (`docs/spec/04-errors.md`, "`catch` distinguishes by the declared
/// type of the thrown value"), so a tag is an identity, never a shape.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ErrorTag {
    /// A user-declared struct or enum, by its defining item.
    Item(ItemId),
    /// A thrown primitive value (`throw "boom"`).
    Primitive(Primitive),
    /// A stdlib-native error, by its canonical qualified name from
    /// `brasa_resolver::NATIVE_ERRORS` (`string.ParseError`, the
    /// `proc` and `fs` errors, `json.ParseError`). Nominal like the
    /// other tags: the name IS the identity.
    Opaque(&'static str),
}

/// Variant rank for the manual [`Ord`]: items first, then primitives,
/// then opaque names — the order the dump prints tags in.
fn tag_rank(tag: &ErrorTag) -> u8 {
    match tag {
        ErrorTag::Item(_) => 0,
        ErrorTag::Primitive(_) => 1,
        ErrorTag::Opaque(_) => 2,
    }
}

impl Ord for ErrorTag {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (ErrorTag::Item(a), ErrorTag::Item(b)) => a.index().cmp(&b.index()),
            (ErrorTag::Primitive(a), ErrorTag::Primitive(b)) => a.cmp(b),
            (ErrorTag::Opaque(a), ErrorTag::Opaque(b)) => a.cmp(b),
            _ => tag_rank(self).cmp(&tag_rank(other)),
        }
    }
}

impl PartialOrd for ErrorTag {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// The inferred error-set of one function, method, or lambda: a set of
/// nominal tags plus an `open` flag.
///
/// `open` marks a set with unknowable contributions: an indirect call
/// (through a local, parameter, field, or generic receiver) or a
/// `throw` of an expression whose type the checker deferred
/// (`Unknown`). Downstream checks (BRS-23) must skip precision claims —
/// unreachable-arm and exhaustiveness reasoning — on open sets. The
/// tags in an open set are still sound lower bounds: they CAN be
/// thrown; openness only says the list may be incomplete.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ErrorSet {
    pub tags: BTreeSet<ErrorTag>,
    pub open: bool,
}

impl ErrorSet {
    pub(crate) fn open() -> ErrorSet {
        ErrorSet {
            tags: BTreeSet::new(),
            open: true,
        }
    }

    pub(crate) fn union_with(&mut self, other: &ErrorSet) {
        self.tags.extend(other.tags.iter().cloned());
        self.open |= other.open;
    }
}

/// The output of error-set inference: one set per function/method
/// [`DefRef`], one per lambda literal (keyed by its `Expr::Lambda`
/// node), and the diagnostics of the consuming checks (see [`check`]).
pub struct ErrorSetResult {
    pub sets: HashMap<DefRef, ErrorSet>,
    pub lambda_sets: HashMap<ExprId, ErrorSet>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Infers the error-set of every function, struct method, and lambda
/// reachable from `roots`.
///
/// The fixpoint is a naive iterate-until-stable pass: every body is
/// recollected against the previous iteration's sets until nothing
/// changes. Every per-body collection rule is monotone — tags are only
/// added, openness only turns on, and a `catch` subtracts a fixed tag
/// list from a growing subject set — so the sets only grow and the
/// finite tag universe (declared items + six primitives) guarantees
/// convergence (`docs/spec/04-errors.md`).
///
/// After convergence one extra checking pass recollects every body
/// against the final sets (its output is identical by fixpoint) with
/// diagnostics enabled: each `catch` is checked against its subject's
/// transient contribution set, each definition's declared `throws`
/// contract against its converged set, and each `toString` override
/// against the requirement that its converged set be empty.
pub fn infer(
    hir: &Hir,
    roots: &[ItemId],
    resolutions: &Resolutions,
    types: &TypeTables,
) -> ErrorSetResult {
    let defs = collect_defs(hir, roots);

    let mut sets: HashMap<DefRef, ErrorSet> = defs
        .iter()
        .map(|&(def, _)| (def, ErrorSet::default()))
        .collect();
    let mut lambda_sets: HashMap<ExprId, ErrorSet> = HashMap::new();

    loop {
        let mut next_sets = HashMap::with_capacity(sets.len());
        let mut next_lambda_sets = HashMap::with_capacity(lambda_sets.len());

        for &(def, body) in &defs {
            let mut collector = Collector {
                hir,
                res: resolutions,
                types,
                sets: &sets,
                lambda_sets: &mut next_lambda_sets,
                diagnostics: None,
            };
            next_sets.insert(def, collector.block(body));
        }

        if next_sets == sets && next_lambda_sets == lambda_sets {
            break;
        }
        sets = next_sets;
        lambda_sets = next_lambda_sets;
    }

    let mut diagnostics = Vec::new();
    let mut checked_lambda_sets = HashMap::with_capacity(lambda_sets.len());
    for &(def, body) in &defs {
        let mut collector = Collector {
            hir,
            res: resolutions,
            types,
            sets: &sets,
            lambda_sets: &mut checked_lambda_sets,
            diagnostics: Some(&mut diagnostics),
        };
        collector.block(body);

        check::throws_contract(hir, resolutions, &sets, def, &mut diagnostics);
        check::render_contract(hir, &sets, def, &mut diagnostics);
    }

    // The top-level pseudo-body runs only here, after convergence: no
    // function can call into top-level code, so its set feeds nothing
    // back into the fixpoint, and it exists only so top-level catches
    // are checked — the set itself has no contract to verify.
    let mut collector = Collector {
        hir,
        res: resolutions,
        types,
        sets: &sets,
        lambda_sets: &mut checked_lambda_sets,
        diagnostics: Some(&mut diagnostics),
    };
    collector.top_level(roots);

    ErrorSetResult {
        sets,
        lambda_sets,
        diagnostics,
    }
}

/// Every function and struct-method body under `roots`, in declaration
/// order (deterministic for the dump). Top-level statements and
/// `TopLet` initializers are not defs: they form the pseudo-body that
/// `Collector::top_level` walks during the checking pass.
fn collect_defs<'a>(hir: &'a Hir, roots: &[ItemId]) -> Vec<(DefRef, &'a brasa_hir::Block)> {
    let mut defs = Vec::new();

    for &item in roots {
        match hir.item(item) {
            Item::FuncDef(func) => defs.push((DefRef::Item(item), &func.body)),
            Item::StructDef(def) => {
                for (index, method) in def.methods.iter().enumerate() {
                    defs.push((DefRef::Method { owner: item, index }, &method.body));
                }
            }
            _ => {}
        }
    }

    defs
}
