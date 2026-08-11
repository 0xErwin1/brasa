//! The resolver walk: a declaration pass over the module followed by a
//! full body walk.
//!
//! Scoping rules implemented here (with the decisions the spec leaves
//! open, see `crate` docs):
//!
//! - Two namespaces (`docs/spec/02-grammar.md`): values (`IDENT`) and
//!   types/constructors (`TYPE_IDENT`) never collide.
//! - Items (`def`/`struct`/`enum`/`interface`) are visible module-wide,
//!   so forward references between them are fine. Top-level statements
//!   execute in order (`docs/spec/01-syntax.md`), so code in top-level
//!   position sees only top-level `let`s declared earlier; function and
//!   method bodies (which only run once everything is declared) see all
//!   of them.
//! - Shadowing is allowed in inner scopes only
//!   (`docs/spec/01-syntax.md`, `docs/spec/03-types.md`): re-binding a
//!   name in the *same* scope is a duplicate-definition error.
//! - `self` resolves only inside a function whose parameter list
//!   contains `self` (`docs/spec/01-syntax.md`); lambdas nested in such
//!   a method may use it too (they close over the method scope).
//! - Imports (`docs/spec/01-syntax.md`) bind the last `std::` segment or
//!   the file stem in the value namespace. File imports are not loaded
//!   or cycle-checked in M1 (the module loader is a later work item);
//!   the binding is recorded and member access stays opaque.
//! - `catch` arm types (`CatchType`): bare names resolve in the type
//!   namespace (`docs/spec/04-errors.md`, arms match error types
//!   nominally); `panics.X` names are validated against the builtin
//!   closed panic union (BRS-24, no import needed — like the prelude);
//!   `string.X` names are validated against the closed native-error
//!   list (BRS-41); other dotted names (`fs.`, `proc.`, `json.`) are
//!   skipped until their namespaces land (M4).

use std::collections::HashMap;
use std::path::Path;

use brasa_diagnostics::{Diagnostic, Severity, codes};
use brasa_hir::{
    ArmBody, CatchType, Constraint, EnumDef, Expr, ExprId, Field, FuncDef, GenericParam, Hir,
    IfNode, IfaceMember, Import, ImportPath, Item, ItemId, LambdaBody, Param, Pattern, PatternId,
    Stmt, StmtId, Throws, TypeExpr, TypeExprId,
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

/// The std modules that exist in v1 (`docs/spec/05-stdlib.md`).
pub(crate) const STD_MODULES: &[&str] = &[
    "env", "fs", "io", "json", "math", "proc", "rand", "re", "time",
];

pub(crate) fn builtin_value(name: &str) -> Option<BuiltinValue> {
    match name {
        "puts" => Some(BuiltinValue::Puts),
        "print" => Some(BuiltinValue::Print),
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
        "Comparable" => Some(BuiltinType::Comparable),
        "Printable" => Some(BuiltinType::Printable),
        "Hashable" => Some(BuiltinType::Hashable),
        _ => None,
    }
}

/// The name an import binds in the value namespace: the last `std::`
/// segment, or the file stem for file imports
/// (`docs/spec/01-syntax.md`). `None` when the path is degenerate (which
/// the parser already reported).
pub(crate) fn import_binding_name(import: &Import) -> Option<&str> {
    match &import.path {
        ImportPath::Std(segments) => segments
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
}

struct TypeBinding {
    res: TypeRes,
    span: Span,
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
    /// `value_scopes[0]` is the module scope; the prelude sits behind it
    /// as a hardcoded fallback rather than a real scope.
    value_scopes: Vec<HashMap<&'h str, ValueBinding>>,
    module_types: HashMap<&'h str, TypeBinding>,
    /// Generic-parameter (and interface `Self`) frames, innermost last.
    type_frames: Vec<HashMap<&'h str, TypeBinding>>,
    /// Every enum item in the module, in source order; the candidate pool
    /// for constructor resolution alongside `Some`/`None`.
    enums: Vec<ItemId>,
    /// `Some(n)`: resolving code in top-level execution position, where
    /// only the first `n` top-level `let`s are initialized. `None`:
    /// resolving a function/method body, where all of them are visible.
    /// The watermark deliberately stays active inside lambdas written in
    /// top-level position: their bodies are resolved lexically.
    top_let_watermark: Option<usize>,
    /// Whether `self` is currently a valid expression.
    self_allowed: bool,
}

pub(crate) fn run(hir: &Hir, roots: &[ItemId]) -> (Resolutions, Vec<Diagnostic>) {
    let mut resolver = Resolver {
        hir,
        res: Resolutions::default(),
        diagnostics: Vec::new(),
        value_scopes: vec![HashMap::new()],
        module_types: HashMap::new(),
        type_frames: Vec::new(),
        enums: Vec::new(),
        top_let_watermark: None,
        self_allowed: false,
    };

    let top_lets_before = resolver.collect_module(roots);
    resolver.resolve_items(roots, &top_lets_before);

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

    /// Declares every module-level name and returns, for each root index,
    /// how many top-level `let`s precede it.
    fn collect_module(&mut self, roots: &[ItemId]) -> Vec<usize> {
        let hir = self.hir;
        let mut top_lets_before = Vec::with_capacity(roots.len());
        let mut top_let_count = 0usize;

        for &root in roots {
            top_lets_before.push(top_let_count);
            let span = hir.span_of_item(root);

            match hir.item(root) {
                Item::Import(import) => {
                    self.check_import(import, span);
                    if let Some(name) = import_binding_name(import) {
                        self.declare_module_value(name, Res::Module(root), span, None);
                    }
                }
                Item::FuncDef(func) => {
                    self.declare_module_value(&func.name, Res::Item(root), span, None);
                }
                Item::TopLet(top_let) => {
                    self.declare_module_value(
                        &top_let.let_stmt.name,
                        Res::Item(root),
                        span,
                        Some(top_let_count),
                    );
                    top_let_count += 1;
                }
                Item::StructDef(def) => {
                    self.declare_module_type(&def.name, root, span);
                    self.check_duplicate_fields(&def.fields);
                }
                Item::EnumDef(def) => {
                    self.declare_module_type(&def.name, root, span);
                    self.check_enum_hygiene(def);
                    self.enums.push(root);
                }
                Item::InterfaceDef(def) => self.declare_module_type(&def.name, root, span),
                Item::Stmt(_) => {}
            }
        }

        top_lets_before
    }

    /// Enum definition hygiene (BRS-18): a repeated variant name within
    /// one enum is a duplicate-definition error, reported here at the
    /// declaration site — without this it would only surface as a
    /// self-ambiguity at every constructor use. Both labels point at the
    /// variant names themselves (`docs/spec/06-diagnostics.md`). Fields
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

    /// A repeated field name within one struct or enum variant is a
    /// duplicate-definition error; both labels point at the field names
    /// themselves (`docs/spec/06-diagnostics.md`).
    fn check_duplicate_fields(&mut self, fields: &'h [Field]) {
        let mut seen: HashMap<&'h str, Span> = HashMap::new();

        for field in fields {
            if let Some(&prev_span) = seen.get(field.name.as_str()) {
                self.duplicate_error(&field.name, field.name_span, prev_span);
            } else {
                seen.insert(&field.name, field.name_span);
            }
        }
    }

    fn check_import(&mut self, import: &Import, span: Span) {
        let ImportPath::Std(segments) = &import.path else {
            return;
        };

        if segments.first().map(String::as_str) != Some("std") {
            self.error(
                err_at(
                    codes::R_UNKNOWN_IMPORT_ROOT,
                    span,
                    format!("unknown import root `{}`", segments.join("::")),
                    "expected `std::` or a file path",
                )
                .with_note("only `std::` imports and file imports exist in v1".to_string()),
            );
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
        name: &'h str,
        res: Res,
        span: Span,
        top_let_order: Option<usize>,
    ) {
        let scope = &mut self.value_scopes[0];
        if let Some(prev) = scope.get(name) {
            let prev_span = prev.span;
            self.duplicate_error(name, span, prev_span);
            return;
        }

        scope.insert(
            name,
            ValueBinding {
                res,
                span,
                top_let_order,
            },
        );
    }

    fn declare_module_type(&mut self, name: &'h str, item: ItemId, span: Span) {
        if let Some(prev) = self.module_types.get(name) {
            let prev_span = prev.span;
            self.duplicate_error(name, span, prev_span);
            return;
        }

        self.module_types.insert(
            name,
            TypeBinding {
                res: TypeRes::Item(item),
                span,
            },
        );
    }

    // --- pass 2: bodies and signatures ---------------------------------

    fn resolve_items(&mut self, roots: &[ItemId], top_lets_before: &[usize]) {
        let hir = self.hir;

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
    /// (interface bodies, `docs/spec/03-types.md`). Diagnostics about a
    /// generic (duplicates, bad constraints) point at the parameter's
    /// name (`docs/spec/06-diagnostics.md`); `span` — the owning item's —
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
                },
            );
        }

        if with_self {
            frame.insert(
                "Self",
                TypeBinding {
                    res: TypeRes::SelfType,
                    span,
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
                        },
                    );
                    self.type_frames.push(self_frame);

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
                if self.lookup_type(&throws_type.name).is_none() {
                    self.error(err_at(
                        codes::R_UNKNOWN_TYPE,
                        throws_type.span,
                        format!("unknown type `{}`", throws_type.name),
                        "not found in this scope",
                    ));
                }
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

    /// Resolves a `throws Type | ...` declaration list in the type
    /// namespace, mirroring `catch` arm types: every name records its
    /// result positionally, an unknown name is `R003` and records `None`
    /// so later slots stay aligned with the declared list. Runs inside
    /// the function's generic frame, so a `throws T` naming a generic
    /// parameter resolves (the error-set checker decides what it can do
    /// with it). `throws never` declares no names and records nothing.
    fn resolve_throws(&mut self, owner: DefRef, func: &'h FuncDef) {
        let Some(Throws::Types(types)) = &func.throws else {
            return;
        };

        let resolved = types
            .iter()
            .map(|throws_type| match self.lookup_type(&throws_type.name) {
                Some(res) => Some(res),
                None => {
                    self.error(err_at(
                        codes::R_UNKNOWN_TYPE,
                        throws_type.span,
                        format!("unknown type `{}`", throws_type.name),
                        "not found in this scope",
                    ));
                    None
                }
            })
            .collect();

        self.res.throws_types.insert(owner, resolved);
    }

    // --- scopes and bindings -------------------------------------------

    /// Declares a local in the innermost value scope. A clash in the
    /// *same* scope is a duplicate-definition error (shadowing is only
    /// allowed in inner scopes, `docs/spec/03-types.md`); the newer
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
                },
            );

        local
    }

    fn lookup_value(&self, name: &str) -> ValueLookup {
        for (depth, scope) in self.value_scopes.iter().enumerate().rev() {
            if let Some(binding) = scope.get(name) {
                if depth == 0
                    && let (Some(order), Some(watermark)) =
                        (binding.top_let_order, self.top_let_watermark)
                    && order >= watermark
                {
                    return ValueLookup::UseBeforeDef(binding.span);
                }
                return ValueLookup::Found(binding.res);
            }
        }

        match builtin_value(name) {
            Some(builtin) => ValueLookup::Found(Res::Builtin(builtin)),
            None => ValueLookup::Missing,
        }
    }

    fn lookup_type(&self, name: &str) -> Option<TypeRes> {
        for frame in self.type_frames.iter().rev() {
            if let Some(binding) = frame.get(name) {
                return Some(binding.res);
            }
        }
        if let Some(binding) = self.module_types.get(name) {
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
            // Member names stay unresolved until the type checker knows
            // the receiver's type (or module).
            Expr::Field { recv, .. } => self.resolve_expr(*recv),
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
            Expr::StructLit { type_name, fields } => {
                match self.lookup_type(type_name) {
                    Some(res) => {
                        self.res.struct_lit_res.insert(id, res);
                    }
                    None => {
                        let span = hir.span_of_expr(id);
                        self.error(err_at(
                            codes::R_UNKNOWN_TYPE,
                            span,
                            format!("unknown type `{type_name}`"),
                            "not found in this scope",
                        ));
                    }
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
                // `docs/spec/04-errors.md` — no import needed, like the
                // prelude); names in landed native-error namespaces
                // (`string.`, `proc.`) check against the closed
                // native-error list (BRS-41); other dotted names
                // (`fs.`, `json.`) are skipped until their namespaces
                // land (M4). Whatever `lookup_type` returns is recorded
                // as-is — the type checker decides what the binding
                // narrows to per arm.
                for (arm_index, arm) in arms.iter().enumerate() {
                    for (type_index, arm_type) in arm.types.iter().enumerate() {
                        let CatchType::Named { name, span } = arm_type else {
                            continue;
                        };
                        if name.contains('.') {
                            if name.starts_with("panics.") {
                                self.resolve_panic_arm(id, arm_index, type_index, name, *span);
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

    /// A `panics.`-qualified `catch` arm name: the union is closed
    /// (`docs/spec/04-errors.md`), so the name either matches a member
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
    /// (`string.`, `proc.`): the native-error list is closed
    /// (`docs/spec/05-stdlib.md`), so the name either matches a
    /// member of [`NATIVE_ERRORS`] — recorded in
    /// `catch_arm_native_errors` with the canonical `&'static str` — or
    /// is an `R012` error. Mirrors [`Self::resolve_panic_arm`].
    fn resolve_native_error_arm(
        &mut self,
        id: ExprId,
        arm_index: usize,
        type_index: usize,
        name: &str,
        span: Span,
    ) {
        match NATIVE_ERRORS.iter().find(|&&error| error == name) {
            Some(&error) => {
                self.res
                    .catch_arm_native_errors
                    .insert((id, arm_index, type_index), error);
            }
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
    fn resolve_ctor(&mut self, name: &str, span: Span, position: CtorPosition) -> Option<CtorRes> {
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

        for &enum_item in &self.enums {
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

    // --- types ---------------------------------------------------------

    fn resolve_type(&mut self, id: TypeExprId) {
        let hir = self.hir;

        match hir.type_expr(id) {
            TypeExpr::Named { name, args } => {
                match self.lookup_type(name) {
                    Some(res) => {
                        self.res.type_res.insert(id, res);
                    }
                    None => {
                        let span = hir.span_of_type_expr(id);
                        self.error(err_at(
                            codes::R_UNKNOWN_TYPE,
                            span,
                            format!("unknown type `{name}`"),
                            "not found in this scope",
                        ));
                    }
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
            "Comparable",
            "Printable",
            "Hashable",
        ] {
            assert!(builtin_type(name).is_some(), "{name} should be builtin");
        }
        assert_eq!(builtin_type("Json"), None);
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
