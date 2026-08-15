//! The resolver walk: a declaration pass over the module followed by a
//! full body walk.
//!
//! Scoping rules implemented here (with the decisions the spec leaves
//! open, see `crate` docs):
//!
//! - Two namespaces (spec: 02 — Gramática formal): values (`IDENT`) and
//!   types/constructors (`TYPE_IDENT`) never collide.
//! - Items (`def`/`struct`/`enum`/`interface`) are visible module-wide,
//!   so forward references between them are fine. Top-level statements
//!   execute in order (spec: 01 — Sintaxis), so code in top-level
//!   position sees only top-level `let`s declared earlier; function and
//!   method bodies (which only run once everything is declared) see all
//!   of them.
//! - Shadowing is allowed in inner scopes only
//!   (spec: 01 — Sintaxis, spec: 03 — Sistema de tipos): re-binding a
//!   name in the *same* scope is a duplicate-definition error.
//! - `self` resolves only inside a function whose parameter list
//!   contains `self` (spec: 01 — Sintaxis); lambdas nested in such
//!   a method may use it too (they close over the method scope).
//! - Imports (spec: 01 — Sintaxis) bind the last `std::` segment or
//!   the file stem in the value namespace. For a file import, whose
//!   module `brasa_module` has already loaded, `stem.member` and
//!   `stem.Type` resolve here against that module's own scope, and only
//!   `pub` declarations are reachable. A `std::` module's members stay
//!   opaque: they are builtins the type checker knows.
//! - `catch` arm types (`CatchType`): bare names resolve in the type
//!   namespace (spec: 04 — Sistema de errores, arms match error types
//!   nominally); `panics.X` names are validated against the builtin
//!   closed panic union (BRS-24, no import needed — like the prelude);
//!   names in landed native-error namespaces (`string.`, `proc.`,
//!   `fs.`, `json.`) are validated against the closed native-error
//!   list (BRS-41); dotted names in other roots are skipped until
//!   their namespaces land. A `throws` list is resolved by the very
//!   same rules (`Resolver::resolve_throws_name`), so both halves of an
//!   error contract admit the same names — except `panics.X`, which is
//!   left for the error-set pass to reject as `E006`.

use std::collections::HashMap;
use std::path::Path;

use brasa_diagnostics::{Diagnostic, Severity, codes};
use brasa_hir::{
    ArmBody, CatchType, Constraint, EnumDef, Expr, ExprId, Field, FuncDef, GenericParam, Hir,
    IfNode, IfaceMember, Import, ImportPath, Item, ItemId, LambdaBody, Param, Pattern, PatternId,
    Stmt, StmtId, StructDef, Throws, ThrowsType, TypeExpr, TypeExprId,
};
use brasa_source::Span;

use crate::tables::{
    BinderKind, BuiltinType, BuiltinValue, CtorRes, DefRef, LocalId, LocalInfo, NATIVE_ERRORS,
    PANIC_UNION, Res, Resolutions, TypeRes, native_error_namespace_landed,
};

/// Which position a constructor name was written in. The builtin `Set`
/// constructor only exists in expression position; `Set(...)` in a
/// pattern is an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CtorPosition {
    Expr,
    Pattern,
}

/// What one name in a `throws` list turned out to denote. `None`
/// covers every name that denotes no type in scope: an unresolved one
/// (already reported), a `panics.` member (left for the error-set
/// pass), and a dotted name in a namespace that has not landed.
enum ThrowsName {
    Type(TypeRes),
    /// A member of the closed native-error list, by its canonical name.
    Native(&'static str),
    None,
}

/// The std modules that exist in v1 (spec: 05 — Stdlib de scripting).
///
/// Public because a name here is a promise that both backends can run
/// it: the parity suite reads this list and exercises every entry, so a
/// module cannot be accepted by the resolver with no runtime behind it.
pub const STD_MODULES: &[&str] = &[
    "cli", "env", "fs", "http", "io", "json", "math", "proc", "rand", "time",
];

pub(crate) fn builtin_value(name: &str) -> Option<BuiltinValue> {
    match name {
        "puts" => Some(BuiltinValue::Puts),
        "print" => Some(BuiltinValue::Print),
        "assert" => Some(BuiltinValue::Assert),
        "assertEq" => Some(BuiltinValue::AssertEq),
        "concurrent" => Some(BuiltinValue::Concurrent),
        _ => None,
    }
}

pub(crate) fn builtin_type(name: &str) -> Option<BuiltinType> {
    match name {
        "int" => Some(BuiltinType::Int),
        "float" => Some(BuiltinType::Float),
        "bool" => Some(BuiltinType::Bool),
        "string" => Some(BuiltinType::String),
        "char" => Some(BuiltinType::Char),
        "unit" => Some(BuiltinType::Unit),
        "Option" => Some(BuiltinType::Option),
        "Vector" => Some(BuiltinType::Vector),
        "Map" => Some(BuiltinType::Map),
        "Set" => Some(BuiltinType::Set),
        "Range" => Some(BuiltinType::Range),
        "Json" => Some(BuiltinType::Json),
        "Comparable" => Some(BuiltinType::Comparable),
        "Printable" => Some(BuiltinType::Printable),
        "Hashable" => Some(BuiltinType::Hashable),
        _ => None,
    }
}

/// The name an import binds in the value namespace: the last `std::`
/// segment, or the file stem for file imports
/// (spec: 01 — Sintaxis). `None` when the path is degenerate (which
/// the parser already reported).
pub(crate) fn import_binding_name(import: &Import) -> Option<&str> {
    match &import.path {
        ImportPath::Path(segments) => segments
            .last()
            .map(String::as_str)
            .filter(|s| !s.is_empty()),
        ImportPath::File(path) => Path::new(path).file_stem().and_then(|s| s.to_str()),
    }
}

fn err(code: &'static str, span: Span, message: String) -> Diagnostic {
    Diagnostic::new(Severity::Error, message, code.to_string(), span)
}

/// Like [`err`], with a label on the primary span so the renderer always
/// shows the offending source.
fn err_at(code: &'static str, span: Span, message: String, label: &str) -> Diagnostic {
    err(code, span, message).with_label(span, label.to_string())
}

struct ValueBinding {
    res: Res,
    span: Span,
    /// Position among the module's top-level `let`s, for the "only
    /// earlier top lets" visibility rule. `None` for every other kind of
    /// binding.
    top_let_order: Option<usize>,
    /// Whether an importer may name this through `stem.member`. Only
    /// module-level `pub` declarations are exported; locals carry
    /// `false` and are never reached through a module scope anyway
    /// (spec: 01 — Sintaxis: everything is private except `pub`).
    exported: bool,
}

struct TypeBinding {
    res: TypeRes,
    span: Span,
    /// Whether an importer may name this through `stem.Type`; see
    /// [`ValueBinding::exported`]. Generic-parameter frames are never
    /// reachable from outside, so they carry `false`.
    exported: bool,
}

/// One module's declarations, built by pass 1 and kept for the whole
/// run: resolving `util.slugify` in one module reads another module's
/// scope, so every scope has to outlive the module that built it.
#[derive(Default)]
struct ModuleScope<'h> {
    values: HashMap<&'h str, ValueBinding>,
    types: HashMap<&'h str, TypeBinding>,
    /// Every enum item declared here, in source order; the candidate
    /// pool for constructor resolution alongside `Some`/`None`.
    enums: Vec<ItemId>,
    /// For each root index, how many top-level `let`s precede it.
    top_lets_before: Vec<usize>,
}

/// One module as the resolver sees it. The module loader owns discovery
/// and cycle detection (`brasa_module`); this crate only needs to know
/// which items belong together and where each import points, so the view
/// is a plain borrow rather than a dependency on the loader.
pub struct ModuleView<'a> {
    /// The name this module binds in an importer's scope, for
    /// diagnostics.
    pub name: &'a str,
    pub roots: &'a [ItemId],
    /// File-import item to the index of the module it loaded. A `std::`
    /// import, or one whose file failed to load, has no entry: its
    /// binding stays an opaque module handle.
    pub imports: &'a HashMap<ItemId, usize>,
}

enum ValueLookup {
    Found(Res),
    /// A top-level `let` that exists but is not yet initialized at the
    /// referencing top-level position; carries its definition span.
    UseBeforeDef(Span),
    Missing,
}

pub(crate) struct Resolver<'h> {
    hir: &'h Hir,
    res: Resolutions,
    diagnostics: Vec<Diagnostic>,
    /// Local scopes only, innermost last. Module-level names live in
    /// [`Resolver::scopes`] and the prelude sits behind both as a
    /// hardcoded fallback rather than a real scope.
    value_scopes: Vec<HashMap<&'h str, ValueBinding>>,
    /// Generic-parameter (and interface `Self`) frames, innermost last.
    type_frames: Vec<HashMap<&'h str, TypeBinding>>,
    /// Every module's declarations, indexed like the [`ModuleView`] list
    /// the run was given.
    scopes: Vec<ModuleScope<'h>>,
    /// Module names, parallel to [`Resolver::scopes`], for diagnostics.
    module_names: Vec<String>,
    /// Import item to the module it loaded, flattened across every
    /// module: `ItemId`s are globally unique, so one map serves all.
    module_of_import: HashMap<ItemId, usize>,
    /// Which module's bodies are being resolved.
    current: usize,
    /// `Some(n)`: resolving code in top-level execution position, where
    /// only the first `n` top-level `let`s are initialized. `None`:
    /// resolving a function/method body, where all of them are visible.
    /// The watermark deliberately stays active inside lambdas written in
    /// top-level position: their bodies are resolved lexically.
    top_let_watermark: Option<usize>,
    /// Whether `self` is currently a valid expression.
    self_allowed: bool,
}

pub(crate) fn run(hir: &Hir, modules: &[ModuleView<'_>]) -> (Resolutions, Vec<Diagnostic>) {
    let mut resolver = Resolver {
        hir,
        res: Resolutions::default(),
        diagnostics: Vec::new(),
        value_scopes: vec![HashMap::new()],
        type_frames: Vec::new(),
        scopes: Vec::new(),
        module_names: modules.iter().map(|m| m.name.to_string()).collect(),
        module_of_import: modules
            .iter()
            .flat_map(|m| m.imports.iter().map(|(&item, &target)| (item, target)))
            .collect(),
        current: 0,
        top_let_watermark: None,
        self_allowed: false,
    };

    // Every module declares before any body resolves: a qualified name
    // reaches into another module's scope, and post-order DFS puts
    // dependencies first but says nothing about the reverse edges a
    // diagnostic may have to follow.
    for module in modules {
        let scope = resolver.collect_module(module.roots);
        resolver.scopes.push(scope);
    }

    for (ix, module) in modules.iter().enumerate() {
        resolver.current = ix;
        resolver.resolve_items(module.roots);
    }

    (resolver.res, resolver.diagnostics)
}

impl<'h> Resolver<'h> {
    fn error(&mut self, diag: Diagnostic) {
        self.diagnostics.push(diag);
    }

    fn duplicate_error(&mut self, name: &str, span: Span, prev_span: Span) {
        self.error(
            err(
                codes::R_DUPLICATE_DEFINITION,
                span,
                format!("duplicate definition of `{name}`"),
            )
            .with_label(span, "redefined here".to_string())
            .with_label(prev_span, "previously defined here".to_string()),
        );
    }

    // --- pass 1: module declarations -----------------------------------

    /// Declares every name one module defines, and records for each root
    /// index how many top-level `let`s precede it.
    fn collect_module(&mut self, roots: &[ItemId]) -> ModuleScope<'h> {
        let hir = self.hir;
        let mut scope = ModuleScope {
            top_lets_before: Vec::with_capacity(roots.len()),
            ..ModuleScope::default()
        };
        let mut top_let_count = 0usize;

        for &root in roots {
            scope.top_lets_before.push(top_let_count);
            let span = hir.span_of_item(root);

            match hir.item(root) {
                Item::Import(import) => {
                    self.check_import(import, span);
                    if let Some(name) = import_binding_name(import) {
                        // An import binds a handle for this module only:
                        // there is no re-export in v1, so an importer
                        // never reaches through it.
                        self.declare_module_value(
                            &mut scope,
                            name,
                            Res::Module(root),
                            span,
                            None,
                            false,
                        );
                    }
                }
                Item::FuncDef(func) => {
                    self.declare_module_value(
                        &mut scope,
                        &func.name,
                        Res::Item(root),
                        span,
                        None,
                        func.is_pub,
                    );
                }
                Item::TopLet(top_let) => {
                    self.declare_module_value(
                        &mut scope,
                        &top_let.let_stmt.name,
                        Res::Item(root),
                        span,
                        Some(top_let_count),
                        top_let.is_pub,
                    );
                    top_let_count += 1;
                }
                Item::StructDef(def) => {
                    self.declare_module_type(&mut scope, &def.name, root, span, def.is_pub);
                    self.check_struct_hygiene(def);
                }
                Item::EnumDef(def) => {
                    self.declare_module_type(&mut scope, &def.name, root, span, def.is_pub);
                    self.check_enum_hygiene(def);
                    scope.enums.push(root);
                }
                Item::InterfaceDef(def) => {
                    self.declare_module_type(&mut scope, &def.name, root, span, def.is_pub);
                    self.check_member_hygiene(&def.methods);
                }
                // A test binds no name: nothing can call it, and two
                // tests may share a title without colliding.
                Item::TestDef(_) | Item::Stmt(_) => {}
            }
        }

        scope
    }

    /// Enum definition hygiene (BRS-18): a repeated variant name within
    /// one enum is a duplicate-definition error, reported here at the
    /// declaration site — without this it would only surface as a
    /// self-ambiguity at every constructor use. Both labels point at the
    /// variant names themselves (spec: 06 — Diagnósticos). Fields
    /// within each variant are checked too.
    fn check_enum_hygiene(&mut self, def: &'h EnumDef) {
        let mut seen: HashMap<&'h str, Span> = HashMap::new();

        for variant in &def.variants {
            if let Some(&prev_span) = seen.get(variant.name.as_str()) {
                self.duplicate_error(&variant.name, variant.name_span, prev_span);
            } else {
                seen.insert(&variant.name, variant.name_span);
            }

            self.check_duplicate_fields(&variant.fields);
        }
    }

    /// Struct definition hygiene (BRS-57): fields and methods share one
    /// member namespace, so a method that repeats a field name — or an
    /// earlier method's name — is a duplicate-definition error just
    /// like a repeated field is. A struct body may interleave fields
    /// and methods, so the labels are ordered by position rather than
    /// by which list the member came from.
    fn check_struct_hygiene(&mut self, def: &'h StructDef) {
        let mut seen = self.check_duplicate_fields(&def.fields);

        // One namespace, so a method collides with an earlier field AND
        // with an earlier method. The first declaration keeps the slot,
        // matching `check_enum_hygiene`: a triple `a`/`a`/`a` blames
        // both later ones on the first rather than chaining.
        for method in &def.methods {
            let Some(&prev) = seen.get(method.name.as_str()) else {
                seen.insert(&method.name, method.name_span);
                continue;
            };

            let (prev_span, span) = if prev.start <= method.name_span.start {
                (prev, method.name_span)
            } else {
                (method.name_span, prev)
            };

            self.duplicate_error(&method.name, span, prev_span);
        }
    }

    /// Interface member hygiene, for a named `interface` and for an
    /// anonymous inline constraint alike: one member namespace, like a
    /// struct's. A repeated member is worse than dead code here — a
    /// second declaration at a different signature makes the interface
    /// unsatisfiable by construction, and the failure surfaces later as
    /// a satisfaction error blaming an innocent type.
    fn check_member_hygiene(&mut self, members: &'h [IfaceMember]) {
        let mut seen: HashMap<&'h str, Span> = HashMap::new();

        for member in members {
            if let Some(&prev_span) = seen.get(member.name.as_str()) {
                self.duplicate_error(&member.name, member.name_span, prev_span);
            } else {
                seen.insert(&member.name, member.name_span);
            }
        }
    }

    /// A repeated field name within one struct or enum variant is a
    /// duplicate-definition error; both labels point at the field names
    /// themselves (spec: 06 — Diagnósticos). Returns the span each
    /// name was first declared at.
    fn check_duplicate_fields(&mut self, fields: &'h [Field]) -> HashMap<&'h str, Span> {
        let mut seen: HashMap<&'h str, Span> = HashMap::new();

        for field in fields {
            if let Some(&prev_span) = seen.get(field.name.as_str()) {
                self.duplicate_error(&field.name, field.name_span, prev_span);
            } else {
                seen.insert(&field.name, field.name_span);
            }
        }

        seen
    }

    /// Validates a `::` import.
    ///
    /// Only the `std` root is this crate's business: its modules are a
    /// closed builtin list. Every other root names a file the module
    /// loader resolves on the search path, and the loader has already
    /// reported anything it could not find — re-rejecting it here would
    /// be a second diagnostic for one mistake, and rejecting it as an
    /// "unknown root" would be wrong outright.
    fn check_import(&mut self, import: &Import, span: Span) {
        let ImportPath::Path(segments) = &import.path else {
            return;
        };

        if segments.first().map(String::as_str) != Some("std") {
            return;
        }

        let module = segments[1..].join("::");
        if segments.len() != 2 || !STD_MODULES.contains(&module.as_str()) {
            self.error(
                err_at(
                    codes::R_UNKNOWN_STD_MODULE,
                    span,
                    format!("unknown std module `{module}`"),
                    "no such std module",
                )
                .with_note(format!("known std modules: {}", STD_MODULES.join(", "))),
            );
        }
    }

    /// First definition wins on duplicates so later references still
    /// resolve to something stable.
    fn declare_module_value(
        &mut self,
        scope: &mut ModuleScope<'h>,
        name: &'h str,
        res: Res,
        span: Span,
        top_let_order: Option<usize>,
        exported: bool,
    ) {
        if let Some(prev) = scope.values.get(name) {
            let prev_span = prev.span;
            self.duplicate_error(name, span, prev_span);
            return;
        }

        scope.values.insert(
            name,
            ValueBinding {
                res,
                span,
                top_let_order,
                exported,
            },
        );
    }

    fn declare_module_type(
        &mut self,
        scope: &mut ModuleScope<'h>,
        name: &'h str,
        item: ItemId,
        span: Span,
        exported: bool,
    ) {
        if let Some(prev) = scope.types.get(name) {
            let prev_span = prev.span;
            self.duplicate_error(name, span, prev_span);
            return;
        }

        scope.types.insert(
            name,
            TypeBinding {
                res: TypeRes::Item(item),
                span,
                exported,
            },
        );
    }

    // --- pass 2: bodies and signatures ---------------------------------

    fn resolve_items(&mut self, roots: &[ItemId]) {
        let hir = self.hir;
        let top_lets_before = std::mem::take(&mut self.scopes[self.current].top_lets_before);

        for (i, &root) in roots.iter().enumerate() {
            let span = hir.span_of_item(root);

            match hir.item(root) {
                Item::Import(_) => {}
                Item::FuncDef(func) => self.resolve_func(DefRef::Item(root), func, span),
                Item::StructDef(def) => {
                    self.push_generic_frame(DefRef::Item(root), &def.generics, span, false);

                    for field in &def.fields {
                        self.resolve_type(field.ty);
                    }
                    for (index, method) in def.methods.iter().enumerate() {
                        self.resolve_func(DefRef::Method { owner: root, index }, method, span);
                    }

                    self.type_frames.pop();
                }
                Item::EnumDef(def) => {
                    self.push_generic_frame(DefRef::Item(root), &def.generics, span, false);

                    for variant in &def.variants {
                        for field in &variant.fields {
                            self.resolve_type(field.ty);
                        }
                    }

                    self.type_frames.pop();
                }
                Item::InterfaceDef(def) => {
                    self.push_generic_frame(DefRef::Item(root), &def.generics, span, true);

                    for member in &def.methods {
                        self.resolve_iface_member(member);
                    }

                    self.type_frames.pop();
                }
                Item::TopLet(top_let) => {
                    self.top_let_watermark = Some(top_lets_before[i]);

                    if let Some(ty) = top_let.let_stmt.ty {
                        self.resolve_type(ty);
                    }
                    self.resolve_expr(top_let.let_stmt.value);

                    self.top_let_watermark = None;
                }
                // A test body runs after the whole module is
                // initialized, like a function body, so every top-level
                // `let` is visible to it and no watermark applies.
                Item::TestDef(def) => {
                    self.value_scopes.push(HashMap::new());
                    self.resolve_block(&def.body);
                    self.value_scopes.pop();
                }
                Item::Stmt(block) => {
                    self.top_let_watermark = Some(top_lets_before[i]);
                    self.resolve_block(block);
                    self.top_let_watermark = None;
                }
            }
        }
    }

    /// Declares `generics` as a new type frame owned by `owner` and
    /// resolves their constraints. `with_self` additionally binds `Self`
    /// (interface bodies, spec: 03 — Sistema de tipos). Diagnostics about a
    /// generic (duplicates, bad constraints) point at the parameter's
    /// name (spec: 06 — Diagnósticos); `span` — the owning item's —
    /// is only the `Self` binding's span, since `Self` has no name token
    /// of its own.
    fn push_generic_frame(
        &mut self,
        owner: DefRef,
        generics: &'h [GenericParam],
        span: Span,
        with_self: bool,
    ) {
        let mut frame: HashMap<&'h str, TypeBinding> = HashMap::new();

        for (index, generic) in generics.iter().enumerate() {
            if let Some(prev) = frame.get(generic.name.as_str()) {
                let prev_span = prev.span;
                self.duplicate_error(&generic.name, generic.name_span, prev_span);
                continue;
            }
            frame.insert(
                &generic.name,
                TypeBinding {
                    res: TypeRes::GenericParam { owner, index },
                    span: generic.name_span,
                    exported: false,
                },
            );
        }

        if with_self {
            frame.insert(
                "Self",
                TypeBinding {
                    res: TypeRes::SelfType,
                    span,
                    exported: false,
                },
            );
        }

        self.type_frames.push(frame);

        for (index, generic) in generics.iter().enumerate() {
            match &generic.constraint {
                None => {}
                Some(Constraint::Named(name)) => match self.lookup_type(name) {
                    Some(res) if self.is_interface(res) => {
                        self.res.constraint_res.insert((owner, index), res);
                    }
                    Some(_) => {
                        self.error(err_at(
                            codes::R_NOT_AN_INTERFACE,
                            generic.name_span,
                            format!("`{name}` is not an interface"),
                            "constraints must name an interface",
                        ));
                    }
                    None => {
                        self.error(err_at(
                            codes::R_UNKNOWN_TYPE,
                            generic.name_span,
                            format!("unknown type `{name}`"),
                            "not found in this scope",
                        ));
                    }
                },
                Some(Constraint::Inline(members)) => {
                    let mut self_frame = HashMap::new();
                    self_frame.insert(
                        "Self",
                        TypeBinding {
                            res: TypeRes::SelfType,
                            span,
                            exported: false,
                        },
                    );
                    self.type_frames.push(self_frame);

                    self.check_member_hygiene(members);
                    for member in members {
                        self.resolve_iface_member(member);
                    }

                    self.type_frames.pop();
                }
            }
        }
    }

    fn is_interface(&self, res: TypeRes) -> bool {
        match res {
            TypeRes::Item(item) => matches!(self.hir.item(item), Item::InterfaceDef(_)),
            TypeRes::Builtin(builtin) => builtin.is_interface(),
            TypeRes::GenericParam { .. } | TypeRes::SelfType => false,
        }
    }

    /// Resolves an interface member's signature. Its `throws` names are
    /// validated in the type namespace like a function's (`R003` on an
    /// unknown name) but recorded nowhere: enforcing the contract — a
    /// satisfying method must not throw more than the member declares —
    /// needs interface-satisfaction integration (typeck would have to
    /// record which method satisfied which member), deferred to M3+.
    fn resolve_iface_member(&mut self, member: &'h IfaceMember) {
        for param in &member.params {
            if let Param::Named { ty, .. } = param {
                self.resolve_type(*ty);
            }
        }
        if let Some(ret) = member.ret {
            self.resolve_type(ret);
        }

        if let Some(Throws::Types(types)) = &member.throws {
            for throws_type in types {
                self.resolve_throws_name(throws_type);
            }
        }
    }

    fn resolve_func(&mut self, owner: DefRef, func: &'h FuncDef, span: Span) {
        self.push_generic_frame(owner, &func.generics, span, false);

        if let Some(ret) = func.ret {
            self.resolve_type(ret);
        }
        self.resolve_throws(owner, func);

        let saved_self = self.self_allowed;
        self.self_allowed = func
            .params
            .iter()
            .any(|p| matches!(p, Param::SelfParam { .. }));

        self.value_scopes.push(HashMap::new());
        let mut param_locals = Vec::with_capacity(func.params.len());
        for param in &func.params {
            match param {
                Param::SelfParam { .. } => param_locals.push(None),
                Param::Named {
                    name,
                    name_span,
                    ty,
                } => {
                    self.resolve_type(*ty);
                    let local =
                        self.declare_local(name, false, *name_span, BinderKind::Param, Some(*ty));
                    param_locals.push(Some(local));
                }
            }
        }
        self.res.func_params.insert(owner, param_locals);

        self.resolve_block(&func.body);

        self.value_scopes.pop();
        self.self_allowed = saved_self;
        self.type_frames.pop();
    }

    /// Resolves a `throws Type | ...` declaration list, mirroring
    /// `catch` arm types name for name: every slot records its result
    /// positionally in `throws_types`, a native error goes to
    /// `throws_native_errors` instead, and anything that resolves to no
    /// type in scope records `None` so later slots stay aligned with
    /// the declared list. Runs inside the function's generic frame, so
    /// a `throws T` naming a generic parameter resolves (the error-set
    /// checker decides what it can do with it). `throws never` declares
    /// no names and records nothing.
    fn resolve_throws(&mut self, owner: DefRef, func: &'h FuncDef) {
        let Some(Throws::Types(types)) = &func.throws else {
            return;
        };

        let mut resolved = Vec::with_capacity(types.len());
        for (index, throws_type) in types.iter().enumerate() {
            match self.resolve_throws_name(throws_type) {
                ThrowsName::Type(res) => resolved.push(Some(res)),
                ThrowsName::Native(error) => {
                    self.res.throws_native_errors.insert((owner, index), error);
                    resolved.push(None);
                }
                ThrowsName::None => resolved.push(None),
            }
        }

        self.res.throws_types.insert(owner, resolved);
    }

    /// Classifies one `throws` name with the same rules a `catch` arm
    /// type uses, because the two halves of an error contract must
    /// admit the same names: `panics.X` is reserved and needs no import,
    /// a root bound to an imported file module names a type in that
    /// module, a root in a landed stdlib namespace names a native
    /// error, and a bare name resolves in the type namespace.
    ///
    /// A `panics.` name is deliberately left unreported here: it is not
    /// an unknown type but a category error the error-set pass names
    /// (`E006`), which is also where the reader is told why a panic
    /// cannot be declared.
    fn resolve_throws_name(&mut self, throws_type: &'h ThrowsType) -> ThrowsName {
        let name = &throws_type.name;
        let span = throws_type.span;

        if name.contains('.') {
            // `panics.` is reserved and needs no import, so it wins
            // over a file module that happens to be named `panics` —
            // the same precedence a `catch` arm applies, and the reason
            // this arrives before the module split rather than falling
            // through to the "namespace has not landed" case below.
            if name.starts_with("panics.") {
                return ThrowsName::None;
            }

            if let Some((stem, type_name)) = self.split_module_path(name) {
                return match self.qualified_type(stem, type_name, span) {
                    Some(res) => ThrowsName::Type(res),
                    None => ThrowsName::None,
                };
            }

            if native_error_namespace_landed(name) {
                return match self.lookup_native_error(name, span) {
                    Some(error) => ThrowsName::Native(error),
                    None => ThrowsName::None,
                };
            }

            return ThrowsName::None;
        }

        match self.lookup_type(name) {
            Some(res) => ThrowsName::Type(res),
            None => {
                self.error(err_at(
                    codes::R_UNKNOWN_TYPE,
                    span,
                    format!("unknown type `{name}`"),
                    "not found in this scope",
                ));
                ThrowsName::None
            }
        }
    }

    // --- scopes and bindings -------------------------------------------

    /// Declares a local in the innermost value scope. A clash in the
    /// *same* scope is a duplicate-definition error (shadowing is only
    /// allowed in inner scopes, spec: 03 — Sistema de tipos); the newer
    /// binding still wins afterwards to keep later references resolving.
    fn declare_local(
        &mut self,
        name: &'h str,
        mutable: bool,
        span: Span,
        kind: BinderKind,
        ty: Option<TypeExprId>,
    ) -> LocalId {
        let scope = self
            .value_scopes
            .last()
            .expect("scope stack is never empty");
        if let Some(prev) = scope.get(name) {
            let prev_span = prev.span;
            self.duplicate_error(name, span, prev_span);
        }

        let local = LocalId(self.res.locals.len() as u32);
        self.res.locals.push(LocalInfo {
            name: name.to_string(),
            mutable,
            span,
            kind,
            ty,
        });

        self.value_scopes
            .last_mut()
            .expect("scope stack is never empty")
            .insert(
                name,
                ValueBinding {
                    res: Res::Local(local),
                    span,
                    top_let_order: None,
                    exported: false,
                },
            );

        local
    }

    fn lookup_value(&self, name: &str) -> ValueLookup {
        for scope in self.value_scopes.iter().rev() {
            if let Some(binding) = scope.get(name) {
                return ValueLookup::Found(binding.res);
            }
        }

        if let Some(binding) = self.scopes[self.current].values.get(name) {
            if let (Some(order), Some(watermark)) = (binding.top_let_order, self.top_let_watermark)
                && order >= watermark
            {
                return ValueLookup::UseBeforeDef(binding.span);
            }
            return ValueLookup::Found(binding.res);
        }

        match builtin_value(name) {
            Some(builtin) => ValueLookup::Found(Res::Builtin(builtin)),
            None => ValueLookup::Missing,
        }
    }

    /// Resolves `stem.member` against the scope of the file module
    /// `stem` binds, recording the result on the member expression
    /// itself so the later phases treat it exactly like a direct
    /// reference to that item.
    ///
    /// Does nothing when the receiver is not a file-module handle: a
    /// `std::` module's members are builtins, and a member of a value is
    /// a field or method the type checker settles.
    fn resolve_module_member(&mut self, id: ExprId, recv: ExprId, name: &str) {
        let Some(Res::Module(import_item)) = self.res.expr_res.get(&recv).copied() else {
            return;
        };
        let Some(&target) = self.module_of_import.get(&import_item) else {
            return;
        };

        let module = &self.module_names[target];
        let span = self.hir.span_of_expr(id);

        match self.scopes[target].values.get(name) {
            Some(binding) if binding.exported => {
                let res = binding.res;
                self.res.expr_res.insert(id, res);
            }
            // An import binds a handle for its own module only. There
            // is no `pub import`, so pointing at the import and asking
            // for a keyword that does not exist would send the reader
            // after a fix they cannot make.
            Some(binding) if matches!(binding.res, Res::Module(_)) => {
                let declared = binding.span;
                self.error(
                    err_at(
                        codes::R_UNKNOWN_MODULE_MEMBER,
                        span,
                        format!("module `{module}` has no member `{name}`"),
                        "not found in that module",
                    )
                    .with_label(declared, format!("`{module}` imports `{name}` here"))
                    .with_note(
                        "an import is not re-exported: import the module directly instead"
                            .to_string(),
                    ),
                );
            }
            Some(binding) => {
                let declared = binding.span;
                self.error(
                    err_at(
                        codes::R_UNKNOWN_MODULE_MEMBER,
                        span,
                        format!("`{name}` is not exported by module `{module}`"),
                        "not exported",
                    )
                    .with_label(declared, "declared without `pub` here".to_string())
                    .with_note(format!(
                        "everything in a module is private unless declared `pub`; write `pub` before `{name}`'s definition to export it"
                    )),
                );
            }
            None => {
                self.error(err_at(
                    codes::R_UNKNOWN_MODULE_MEMBER,
                    span,
                    format!("module `{module}` has no member `{name}`"),
                    "not found in that module",
                ));
            }
        }
    }

    fn lookup_type(&self, name: &str) -> Option<TypeRes> {
        for frame in self.type_frames.iter().rev() {
            if let Some(binding) = frame.get(name) {
                return Some(binding.res);
            }
        }
        if let Some(binding) = self.scopes[self.current].types.get(name) {
            return Some(binding.res);
        }
        builtin_type(name).map(TypeRes::Builtin)
    }

    // --- statements ----------------------------------------------------

    fn resolve_block(&mut self, block: &'h [StmtId]) {
        self.value_scopes.push(HashMap::new());
        for &stmt in block {
            self.resolve_stmt(stmt);
        }
        self.value_scopes.pop();
    }

    fn resolve_stmt(&mut self, id: StmtId) {
        let hir = self.hir;

        match hir.stmt(id) {
            Stmt::Let(let_stmt) => {
                if let Some(ty) = let_stmt.ty {
                    self.resolve_type(ty);
                }
                // The initializer is resolved before the binding is
                // declared: `let x = x + 1` refers to the outer `x`.
                self.resolve_expr(let_stmt.value);

                let local = self.declare_local(
                    &let_stmt.name,
                    let_stmt.mutable,
                    hir.span_of_stmt(id),
                    BinderKind::Let,
                    let_stmt.ty,
                );
                self.res.stmt_locals.insert(id, local);
            }
            Stmt::Assign { target, value } => {
                self.resolve_expr(*target);
                self.resolve_expr(*value);
            }
            Stmt::Return(value) => {
                if let Some(value) = value {
                    self.resolve_expr(*value);
                }
            }
            Stmt::Break | Stmt::Continue => {}
            Stmt::Throw(value) => self.resolve_expr(*value),
            Stmt::If(node) => self.resolve_if(node),
            Stmt::While { cond, body } => {
                self.resolve_expr(*cond);
                self.resolve_block(body);
            }
            Stmt::For {
                pattern,
                iterable,
                body,
            } => {
                self.resolve_expr(*iterable);

                self.value_scopes.push(HashMap::new());
                self.resolve_pattern(*pattern);
                self.resolve_block(body);
                self.value_scopes.pop();
            }
            Stmt::Expr(value) => self.resolve_expr(*value),
        }
    }

    fn resolve_if(&mut self, node: &'h IfNode) {
        for (cond, body) in &node.branches {
            self.resolve_expr(*cond);
            self.resolve_block(body);
        }
        if let Some(else_) = &node.else_ {
            self.resolve_block(else_);
        }
    }

    // --- expressions ---------------------------------------------------

    fn resolve_expr(&mut self, id: ExprId) {
        let hir = self.hir;

        match hir.expr(id) {
            Expr::Int(_)
            | Expr::Float(_)
            | Expr::Bool(_)
            | Expr::Char(_)
            | Expr::Unit
            | Expr::Str(_) => {}
            Expr::Ident(name) => match self.lookup_value(name) {
                ValueLookup::Found(res) => {
                    self.res.expr_res.insert(id, res);
                }
                ValueLookup::UseBeforeDef(def_span) => {
                    let span = hir.span_of_expr(id);
                    self.error(
                        err_at(
                            codes::R_USE_BEFORE_DEF,
                            span,
                            format!("`{name}` is used before its definition"),
                            "used here",
                        )
                        .with_label(def_span, "defined here".to_string()),
                    );
                }
                ValueLookup::Missing => {
                    let span = hir.span_of_expr(id);
                    self.error(err_at(
                        codes::R_UNKNOWN_NAME,
                        span,
                        format!("unknown name `{name}`"),
                        "not found in this scope",
                    ));
                }
            },
            Expr::SelfExpr => {
                if self.self_allowed {
                    self.res.expr_res.insert(id, Res::SelfParam);
                } else {
                    let span = hir.span_of_expr(id);
                    self.error(
                        err_at(
                            codes::R_SELF_OUTSIDE_METHOD,
                            span,
                            "`self` outside a method".to_string(),
                            "not inside a method",
                        )
                        .with_note(
                            "`self` is only valid inside a method taking a `self` parameter"
                                .to_string(),
                        ),
                    );
                }
            }
            Expr::Call { callee, args } => {
                self.resolve_expr(*callee);
                for &arg in args {
                    self.resolve_expr(arg);
                }
            }
            // A member of an imported file module is a name in that
            // module's scope, so it resolves here. Every other member
            // name stays unresolved until the type checker knows the
            // receiver's type.
            Expr::Field { recv, name } => {
                self.resolve_expr(*recv);
                self.resolve_module_member(id, *recv, name);
            }
            Expr::Index { recv, index } => {
                self.resolve_expr(*recv);
                self.resolve_expr(*index);
            }
            Expr::Unary { operand, .. } => self.resolve_expr(*operand),
            Expr::Binary { lhs, rhs, .. } => {
                self.resolve_expr(*lhs);
                self.resolve_expr(*rhs);
            }
            Expr::OptionWrap(inner) | Expr::ToString(inner) => self.resolve_expr(*inner),
            Expr::Lambda { params, body } => {
                self.value_scopes.push(HashMap::new());
                let mut locals = Vec::with_capacity(params.len());
                for param in params {
                    if let Some(ty) = param.ty {
                        self.resolve_type(ty);
                    }
                    locals.push(self.declare_local(
                        &param.name,
                        false,
                        param.name_span,
                        BinderKind::LambdaParam,
                        param.ty,
                    ));
                }
                self.res.lambda_params.insert(id, locals);

                match body {
                    LambdaBody::Expr(expr) => self.resolve_expr(*expr),
                    LambdaBody::Block(block) => self.resolve_block(block),
                }
                self.value_scopes.pop();
            }
            Expr::If(node) => self.resolve_if(node),
            Expr::Match { scrutinee, arms } => {
                self.resolve_expr(*scrutinee);

                for arm in arms {
                    self.value_scopes.push(HashMap::new());
                    self.resolve_pattern(arm.pattern);
                    if let Some(guard) = arm.guard {
                        self.resolve_expr(guard);
                    }
                    self.resolve_arm_body(&arm.body);
                    self.value_scopes.pop();
                }
            }
            Expr::VectorLit(elements) => {
                for &element in elements {
                    self.resolve_expr(element);
                }
            }
            Expr::MapLit(entries) => {
                for (key, value) in entries {
                    self.resolve_expr(*key);
                    self.resolve_expr(*value);
                }
            }
            Expr::TupleLit(elements) => {
                for &element in elements {
                    self.resolve_expr(element);
                }
            }
            Expr::StructLit { type_name, fields } => {
                let span = hir.span_of_expr(id);

                if let Some(res) = self.type_path(type_name, span) {
                    self.res.struct_lit_res.insert(id, res);
                }
                for (_, value) in fields {
                    self.resolve_expr(*value);
                }
            }
            Expr::Range { lo, hi, .. } => {
                self.resolve_expr(*lo);
                self.resolve_expr(*hi);
            }
            Expr::Catch {
                subject,
                binding,
                arms,
                ..
            } => {
                self.resolve_expr(*subject);

                let span = hir.span_of_expr(id);
                self.value_scopes.push(HashMap::new());
                let local =
                    self.declare_local(binding, false, span, BinderKind::CatchBinding, None);
                self.res.catch_bindings.insert(id, local);

                // Bare arm type names resolve here; `panics.X` names
                // check against the builtin closed panic union (BRS-24,
                // spec: 04 — Sistema de errores — no import needed, like the
                // prelude); names in landed native-error namespaces
                // (`string.`, `proc.`, `fs.`, `json.`) check against
                // the closed native-error list (BRS-41); dotted names
                // in other roots are skipped until their namespaces
                // land. Whatever `lookup_type` returns is recorded
                // as-is — the type checker decides what the binding
                // narrows to per arm.
                for (arm_index, arm) in arms.iter().enumerate() {
                    for (type_index, arm_type) in arm.types.iter().enumerate() {
                        let CatchType::Named { name, span } = arm_type else {
                            continue;
                        };
                        // A dotted arm name is one of three things, and
                        // what the root is BOUND to decides which — not
                        // the spelling. `panics.` is reserved and needs
                        // no import, so it wins outright; otherwise a
                        // root bound to an imported file module names a
                        // type in that module, and a root bound to a
                        // `std::` module (or to nothing) names a native
                        // error.
                        if name.contains('.') {
                            if name.starts_with("panics.") {
                                self.resolve_panic_arm(id, arm_index, type_index, name, *span);
                            } else if let Some((stem, type_name)) = self.split_module_path(name) {
                                self.resolve_module_arm(
                                    id, arm_index, type_index, stem, type_name, *span,
                                );
                            } else if native_error_namespace_landed(name) {
                                self.resolve_native_error_arm(
                                    id, arm_index, type_index, name, *span,
                                );
                            }
                            continue;
                        }

                        match self.lookup_type(name) {
                            Some(res) => {
                                self.res
                                    .catch_arm_types
                                    .insert((id, arm_index, type_index), res);
                            }
                            None => {
                                self.error(err_at(
                                    codes::R_UNKNOWN_TYPE,
                                    *span,
                                    format!("unknown type `{name}`"),
                                    "not found in this scope",
                                ));
                            }
                        }
                    }

                    if let Some(guard) = arm.guard {
                        self.resolve_expr(guard);
                    }
                    self.resolve_arm_body(&arm.body);
                }
                self.value_scopes.pop();
            }
            Expr::EnumCtor { name, args } => {
                let span = hir.span_of_expr(id);
                if let Some(ctor) = self.resolve_ctor(name, span, CtorPosition::Expr) {
                    self.res.ctor_expr_res.insert(id, ctor);
                }
                for &arg in args {
                    self.resolve_expr(arg);
                }
            }
        }
    }

    /// A `catch` arm naming a type exported by an imported file module.
    /// Recorded in `catch_arm_types` like a bare name, because that is
    /// what it is: a nominal error type, reached by a longer path.
    fn resolve_module_arm(
        &mut self,
        id: ExprId,
        arm_index: usize,
        type_index: usize,
        module: &str,
        name: &str,
        span: Span,
    ) {
        let Some(res) = self.qualified_type(module, name, span) else {
            return;
        };

        self.res
            .catch_arm_types
            .insert((id, arm_index, type_index), res);
    }

    /// A `panics.`-qualified `catch` arm name: the union is closed
    /// (spec: 04 — Sistema de errores), so the name either matches a member
    /// of [`PANIC_UNION`] — recorded in `catch_arm_panics` with the
    /// canonical `&'static str` — or is an `R011` error.
    fn resolve_panic_arm(
        &mut self,
        id: ExprId,
        arm_index: usize,
        type_index: usize,
        name: &str,
        span: Span,
    ) {
        match PANIC_UNION.iter().find(|&&panic| panic == name) {
            Some(&panic) => {
                self.res
                    .catch_arm_panics
                    .insert((id, arm_index, type_index), panic);
            }
            None => {
                self.error(
                    err_at(
                        codes::R_UNKNOWN_PANIC,
                        span,
                        format!("unknown panic `{name}`"),
                        "the panic union is closed",
                    )
                    .with_note(format!("the panic union: {}", PANIC_UNION.join(", "))),
                );
            }
        }
    }

    /// A `catch` arm name in a landed native-error namespace
    /// (`string.`, `proc.`): the name either matches a member of
    /// [`NATIVE_ERRORS`] — recorded in `catch_arm_native_errors` with
    /// the canonical `&'static str` — or was reported as `R012`.
    /// Mirrors [`Self::resolve_panic_arm`].
    fn resolve_native_error_arm(
        &mut self,
        id: ExprId,
        arm_index: usize,
        type_index: usize,
        name: &str,
        span: Span,
    ) {
        if let Some(error) = self.lookup_native_error(name, span) {
            self.res
                .catch_arm_native_errors
                .insert((id, arm_index, type_index), error);
        }
    }

    /// Matches a dotted name against the closed native-error list
    /// (spec: 05 — Stdlib de scripting), returning its canonical
    /// `&'static str` or reporting `R012`. Shared by the two places an
    /// error contract names a native error — `catch` arms and `throws`
    /// declarations — so both accept exactly the same set of names.
    fn lookup_native_error(&mut self, name: &str, span: Span) -> Option<&'static str> {
        match NATIVE_ERRORS.iter().find(|&&error| error == name) {
            Some(&error) => Some(error),
            None => {
                self.error(
                    err_at(
                        codes::R_UNKNOWN_NATIVE_ERROR,
                        span,
                        format!("unknown stdlib error `{name}`"),
                        "no such stdlib error",
                    )
                    .with_note(format!("known stdlib errors: {}", NATIVE_ERRORS.join(", "))),
                );
                None
            }
        }
    }

    fn resolve_arm_body(&mut self, body: &'h ArmBody) {
        match body {
            ArmBody::Expr(expr) => self.resolve_expr(*expr),
            ArmBody::Block(block) => self.resolve_block(block),
        }
    }

    // --- patterns ------------------------------------------------------

    fn resolve_pattern(&mut self, id: PatternId) {
        let hir = self.hir;

        match hir.pattern(id) {
            Pattern::Wildcard | Pattern::Literal(_) => {}
            Pattern::Binding(name) => {
                let local = self.declare_local(
                    name,
                    false,
                    hir.span_of_pattern(id),
                    BinderKind::PatternBinding,
                    None,
                );
                self.res.pattern_locals.insert(id, local);
            }
            Pattern::Ctor { name, args } => {
                let span = hir.span_of_pattern(id);
                if let Some(ctor) = self.resolve_ctor(name, span, CtorPosition::Pattern) {
                    self.res.ctor_pattern_res.insert(id, ctor);
                }
                for &arg in args {
                    self.resolve_pattern(arg);
                }
            }
            Pattern::Tuple(elements) => {
                for &element in elements {
                    self.resolve_pattern(element);
                }
            }
        }
    }

    // --- constructors --------------------------------------------------

    /// Candidates are `Some`/`None`, the builtin `Set` constructor
    /// (expression position only), plus every variant of every enum in
    /// scope. Exactly one candidate resolves; zero or several are errors
    /// (the type checker may refine ambiguity with expected types in a
    /// later milestone).
    ///
    /// A qualified `mod.Variant` names exactly one module, so it cannot
    /// be ambiguous and takes the narrower path below.
    fn resolve_ctor(&mut self, name: &str, span: Span, position: CtorPosition) -> Option<CtorRes> {
        if let Some((module, variant)) = self.split_module_path(name) {
            return self.resolve_qualified_ctor(module, variant, span);
        }

        let hir = self.hir;
        let mut candidates: Vec<(CtorRes, &str)> = Vec::new();

        match name {
            "Some" => candidates.push((CtorRes::OptionSome, "Option")),
            "None" => candidates.push((CtorRes::OptionNone, "Option")),
            "Set" if position == CtorPosition::Expr => {
                candidates.push((CtorRes::SetCtor, "Set"));
            }
            _ => {}
        }

        for enum_item in self.scopes[self.current].enums.clone() {
            let Item::EnumDef(def) = hir.item(enum_item) else {
                continue;
            };
            for (variant_index, variant) in def.variants.iter().enumerate() {
                if variant.name == name {
                    candidates.push((
                        CtorRes::EnumVariant {
                            enum_item,
                            variant_index,
                        },
                        &def.name,
                    ));
                }
            }
        }

        match candidates.len() {
            1 => Some(candidates[0].0),
            0 => {
                if name == "Set" && position == CtorPosition::Pattern {
                    self.error(err_at(
                        codes::R_UNKNOWN_CONSTRUCTOR,
                        span,
                        "`Set(...)` is not a valid pattern".to_string(),
                        "`Set` only constructs values; match on the set's contents with its methods instead",
                    ));
                } else {
                    self.error(err_at(
                        codes::R_UNKNOWN_CONSTRUCTOR,
                        span,
                        format!("unknown constructor `{name}`"),
                        "not found in this scope",
                    ));
                }
                None
            }
            _ => {
                let owners: Vec<&str> = candidates.iter().map(|(_, owner)| *owner).collect();
                self.error(
                    err_at(
                        codes::R_AMBIGUOUS_CONSTRUCTOR,
                        span,
                        format!("ambiguous constructor `{name}`"),
                        "matches more than one enum",
                    )
                    .with_note(format!("candidates: {}", owners.join(", "))),
                );
                None
            }
        }
    }

    /// `mod.Variant`: a constructor of an enum exported by an imported
    /// file module.
    ///
    /// The candidate pool is one module's exported enums rather than
    /// every enum in scope, so the qualified form is strictly narrower
    /// than the bare one — it can still be ambiguous, but only between
    /// two enums of the same module.
    fn resolve_qualified_ctor(&mut self, module: &str, name: &str, span: Span) -> Option<CtorRes> {
        let Some(target) = self.file_module_index(module) else {
            self.error(err_at(
                codes::R_UNKNOWN_CONSTRUCTOR,
                span,
                format!("unknown module `{module}` in constructor `{module}.{name}`"),
                "no such import in this module",
            ));
            return None;
        };

        let hir = self.hir;
        let mut candidates: Vec<(CtorRes, &str)> = Vec::new();

        for enum_item in self.scopes[target].enums.clone() {
            let Item::EnumDef(def) = hir.item(enum_item) else {
                continue;
            };
            if !def.is_pub {
                continue;
            }

            for (variant_index, variant) in def.variants.iter().enumerate() {
                if variant.name == name {
                    candidates.push((
                        CtorRes::EnumVariant {
                            enum_item,
                            variant_index,
                        },
                        &def.name,
                    ));
                }
            }
        }

        match candidates.len() {
            1 => Some(candidates[0].0),
            0 => {
                self.error(err_at(
                    codes::R_UNKNOWN_MODULE_MEMBER,
                    span,
                    format!("module `{module}` exports no constructor `{name}`"),
                    "not found in that module",
                ));
                None
            }
            _ => {
                let owners: Vec<&str> = candidates.iter().map(|(_, owner)| *owner).collect();
                self.error(
                    err_at(
                        codes::R_AMBIGUOUS_CONSTRUCTOR,
                        span,
                        format!("ambiguous constructor `{module}.{name}`"),
                        "matches more than one enum in that module",
                    )
                    .with_note(format!("candidates: {}", owners.join(", "))),
                );
                None
            }
        }
    }

    /// The module index a stem binds, when it binds an imported file
    /// module in the current module's scope.
    fn file_module_index(&self, stem: &str) -> Option<usize> {
        let binding = self.scopes[self.current].values.get(stem)?;
        let Res::Module(import_item) = binding.res else {
            return None;
        };
        self.module_of_import.get(&import_item).copied()
    }

    // --- types ---------------------------------------------------------

    /// The `R003` for a bare name that resolves to no type.
    ///
    /// When an imported module exports that name, the message says which
    /// one: the fix is to qualify the path, and the resolver knows enough
    /// to name it rather than leave the reader to guess which import to
    /// look in.
    fn unknown_type(&self, span: Span, name: &str) -> Diagnostic {
        let diagnostic = err_at(
            codes::R_UNKNOWN_TYPE,
            span,
            format!("unknown type `{name}`"),
            "not found in this scope",
        );

        let exporters: Vec<&str> = self
            .imported_modules()
            .filter(|&(_, target)| {
                self.scopes[target]
                    .types
                    .get(name)
                    .is_some_and(|binding| binding.exported)
            })
            .map(|(stem, _)| stem)
            .collect();

        match exporters.as_slice() {
            [] => diagnostic,
            [stem] => diagnostic.with_note(format!(
                "module `{stem}` exports `{name}`; write it as `{stem}.{name}`"
            )),
            stems => diagnostic.with_note(format!(
                "these imported modules export `{name}`: {}",
                stems.join(", ")
            )),
        }
    }

    /// Every file module the current module imports, as (binding stem,
    /// module index).
    fn imported_modules(&self) -> impl Iterator<Item = (&'h str, usize)> {
        self.scopes[self.current]
            .values
            .iter()
            .filter_map(|(&stem, binding)| match binding.res {
                Res::Module(item) => self.module_of_import.get(&item).map(|&ix| (stem, ix)),
                _ => None,
            })
    }

    /// Resolves a written type name, qualified or not: `lib.Point`
    /// against the type scope of the module `lib` binds, a bare name
    /// against this module's scope and the prelude.
    fn type_path(&mut self, written: &str, span: Span) -> Option<TypeRes> {
        if let Some((module, name)) = self.split_module_path(written) {
            return self.qualified_type(module, name, span);
        }

        match self.lookup_type(written) {
            Some(res) => Some(res),
            None => {
                let diagnostic = self.unknown_type(span, written);
                self.error(diagnostic);
                None
            }
        }
    }

    /// Splits a written name into (module stem, member) when the stem is
    /// bound to an imported file module here. A name is only a qualified
    /// path if its root actually names one — nothing else in the type
    /// namespace contains a `.`, but a dotted `catch` arm does, and it
    /// is not a path.
    fn split_module_path<'n>(&self, written: &'n str) -> Option<(&'n str, &'n str)> {
        let (root, rest) = written.split_once('.')?;
        self.file_module_index(root)?;
        Some((root, rest))
    }

    /// Looks up `module.name` in the type namespace of the file module
    /// `module` binds — the type-namespace twin of
    /// [`Resolver::resolve_module_member`] — reporting every way the
    /// path can fail. `None` means it was reported.
    fn qualified_type(&mut self, module: &str, name: &str, span: Span) -> Option<TypeRes> {
        let Some(binding) = self.scopes[self.current].values.get(module) else {
            self.error(err_at(
                codes::R_UNKNOWN_TYPE,
                span,
                format!("unknown module `{module}` in type `{module}.{name}`"),
                "no such import in this module",
            ));
            return None;
        };

        let Res::Module(import_item) = binding.res else {
            let declared = binding.span;
            self.error(
                err_at(
                    codes::R_UNKNOWN_TYPE,
                    span,
                    format!("`{module}` is not a module"),
                    "only an imported module can qualify a type",
                )
                .with_label(declared, format!("`{module}` is bound here")),
            );
            return None;
        };

        // A `std::` module, or one whose file failed to load. The std
        // modules export no types (spec: 05 — Stdlib de scripting: their
        // members are functions and constants), and a failed load was
        // already reported by the loader.
        let Some(&target) = self.module_of_import.get(&import_item) else {
            self.error(err_at(
                codes::R_UNKNOWN_TYPE,
                span,
                format!("module `{module}` exports no types"),
                "not a file module",
            ));
            return None;
        };

        match self.scopes[target].types.get(name) {
            Some(binding) if binding.exported => Some(binding.res),
            Some(binding) => {
                let declared = binding.span;
                self.error(
                    err_at(
                        codes::R_UNKNOWN_MODULE_MEMBER,
                        span,
                        format!("`{name}` is not exported by module `{module}`"),
                        "not exported",
                    )
                    .with_label(declared, "declared without `pub` here".to_string())
                    .with_note(format!(
                        "everything in a module is private unless declared `pub`; write `pub` before `{name}`'s definition to export it"
                    )),
                );
                None
            }
            None => {
                self.error(err_at(
                    codes::R_UNKNOWN_MODULE_MEMBER,
                    span,
                    format!("module `{module}` declares no type `{name}`"),
                    "not found in that module",
                ));
                None
            }
        }
    }

    fn resolve_type(&mut self, id: TypeExprId) {
        let hir = self.hir;

        match hir.type_expr(id) {
            TypeExpr::Named { name, args } => {
                let span = hir.span_of_type_expr(id);
                if let Some(res) = self.type_path(name, span) {
                    self.res.type_res.insert(id, res);
                }

                for &arg in args {
                    self.resolve_type(arg);
                }
            }
            TypeExpr::Tuple(elements) => {
                for &element in elements {
                    self.resolve_type(element);
                }
            }
            TypeExpr::Fn { params, ret } => {
                for &param in params {
                    self.resolve_type(param);
                }
                self.resolve_type(*ret);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{STD_MODULES, builtin_type, builtin_value};
    use crate::tables::{BuiltinType, BuiltinValue};

    #[test]
    fn prelude_values_resolve() {
        assert_eq!(builtin_value("puts"), Some(BuiltinValue::Puts));
        assert_eq!(builtin_value("print"), Some(BuiltinValue::Print));
        assert_eq!(builtin_value("concurrent"), Some(BuiltinValue::Concurrent));
        assert_eq!(builtin_value("eval"), None);
    }

    #[test]
    fn prelude_types_cover_primitives_containers_and_interfaces() {
        for name in [
            "int",
            "float",
            "bool",
            "string",
            "char",
            "unit",
            "Option",
            "Vector",
            "Map",
            "Set",
            "Range",
            "Json",
            "Comparable",
            "Printable",
            "Hashable",
        ] {
            assert!(builtin_type(name).is_some(), "{name} should be builtin");
        }
        // `Regex` stays unlanded until `std::re` closes.
        assert_eq!(builtin_type("Regex"), None);
    }

    #[test]
    fn only_stdlib_interfaces_are_interfaces() {
        assert!(BuiltinType::Comparable.is_interface());
        assert!(BuiltinType::Printable.is_interface());
        assert!(BuiltinType::Hashable.is_interface());
        assert!(!BuiltinType::Option.is_interface());
        assert!(!BuiltinType::Int.is_interface());
    }

    #[test]
    fn std_modules_are_sorted_and_deduped() {
        let mut sorted = STD_MODULES.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted, STD_MODULES);
    }
}
