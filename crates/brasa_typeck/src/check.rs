//! The type-checking walk over one resolved module.
//!
//! Inference is local (`docs/spec/03-types.md`): function signatures are
//! the boundary (parameters annotated, return annotated or `unit`),
//! `let x = e` takes `e`'s type, `let x: T = e` checks `e` against `T`
//! with expected-type propagation into literals, constructors, and
//! lambda parameters. Nothing is inferred at a distance.
//!
//! Checking order: function signatures first (annotations only), then
//! top-level `let` initializers in source order (so their inferred types
//! exist before any body references them), then every remaining body.
//! Diagnostics are sorted by span afterwards, so the pass order is not
//! observable.
//!
//! Decisions this milestone fixes (documented at the rule sites):
//! struct literals are order-independent with every declared field
//! required exactly once; `if`/`match`/`catch` arm-type mismatches are
//! errors in value position and tolerated (typing `unit`) in statement
//! position; a `unit` result type never forces the body's tail value to
//! be `unit` (the value is discarded); `return` in top-level code is an
//! error; `Vector<T>.join` requires `Vector<string>`.

use std::collections::{HashMap, HashSet};

use brasa_diagnostics::{Diagnostic, Severity, codes};
use brasa_hir::{
    ArmBody, BinaryOp, CatchArm, CatchType, Constraint, EnumDef, Expr, ExprId, FuncDef,
    GenericParam, Hir, IfNode, IfaceMember, ImportPath, Item, ItemId, LambdaBody, LambdaParam,
    Literal, MatchArm, Param, Pattern, PatternId, Stmt, StmtId, SugarOrigin, TypeExpr, TypeExprId,
    UnaryOp, Variant,
};
use brasa_resolver::{BuiltinType, CtorRes, DefRef, Res, Resolutions, TypeRes};
use brasa_source::Span;

use crate::TypeTables;
use crate::builtins::{self, MethodSig, RetRule};
use crate::exhaust;
use crate::types::{Type, WrapDecision, item_name, substitute, unify};

fn err(code: &'static str, span: Span, message: String) -> Diagnostic {
    Diagnostic::new(Severity::Error, message, code.to_string(), span)
}

fn err_at(code: &'static str, span: Span, message: String, label: &str) -> Diagnostic {
    err(code, span, message).with_label(span, label.to_string())
}

/// What a member lookup produced.
enum Member {
    /// A builtin method from [`builtins`].
    Sig(MethodSig),
    /// A struct field or method, as a plain value type.
    Value(Type),
    /// A member the checker cannot see yet: stdlib module members close
    /// in M4, and flexible receivers stay silent.
    Deferred,
    /// The receiver type is known and has no such member.
    Missing,
}

/// Where a `Hashable`-constrained type appeared, for T031 wording.
#[derive(Clone, Copy)]
enum KeyRole {
    MapKey,
    SetElement,
}

impl KeyRole {
    fn phrase(self) -> &'static str {
        match self {
            KeyRole::MapKey => "`Map` key",
            KeyRole::SetElement => "`Set` element",
        }
    }
}

/// What a generic constraint means once resolved: a builtin interface
/// (closed satisfaction lists), a user interface (structural member
/// check), or an inline anonymous interface (same semantics,
/// `docs/spec/01-syntax.md`).
enum ConstraintKind<'h> {
    Builtin(BuiltinType),
    Iface(ItemId, &'h [IfaceMember]),
    Inline(&'h [IfaceMember]),
}

struct Checker<'a> {
    hir: &'a Hir,
    res: &'a Resolutions,
    /// Lowering's side table: which `match` expressions were desugared
    /// from `?.`/`??`, so misuse reports in source terms (T028–T030).
    sugar_origins: &'a HashMap<ExprId, SugarOrigin>,
    tables: TypeTables,
    diagnostics: Vec<Diagnostic>,
    /// The enclosing function's return type; `None` in top-level code,
    /// where `return` is an error.
    ret_ty: Option<Type>,
    /// The enclosing method's receiver type.
    self_ty: Option<Type>,
    /// When non-zero, `error` drops diagnostics. Used while converting
    /// interface member signatures for satisfaction checks and member
    /// lookups: those conversions run once per use site, and any type
    /// error inside an interface body is not this site's fault.
    suppressed: u32,
}

pub(crate) fn run(
    hir: &Hir,
    roots: &[ItemId],
    res: &Resolutions,
    sugar_origins: &HashMap<ExprId, SugarOrigin>,
) -> (TypeTables, Vec<Diagnostic>) {
    let mut checker = Checker {
        hir,
        res,
        sugar_origins,
        tables: TypeTables::default(),
        diagnostics: Vec::new(),
        ret_ty: None,
        self_ty: None,
        suppressed: 0,
    };

    checker.collect_signatures(roots);
    checker.check_top_lets(roots);
    checker.check_bodies(roots);

    (checker.tables, checker.diagnostics)
}

impl<'a> Checker<'a> {
    fn error(&mut self, diag: Diagnostic) {
        if self.suppressed == 0 {
            self.diagnostics.push(diag);
        }
    }

    fn mismatch(&mut self, span: Span, expected: &Type, found: &Type) {
        let expected = expected.display(self.hir);
        let found = found.display(self.hir);
        self.error(err_at(
            codes::T_MISMATCHED_TYPES,
            span,
            format!("mismatched types: expected `{expected}`, found `{found}`"),
            &format!("expected `{expected}`"),
        ));
    }

    // --- module passes -------------------------------------------------

    /// Function signatures come straight from annotations, so they exist
    /// before any body is checked (the local-inference boundary,
    /// `docs/spec/03-types.md`). Generic parameters stay rigid
    /// [`Type::Generic`]s in the stored signature; call sites substitute
    /// them once the arguments are known (`docs/spec/02-grammar.md`, no
    /// turbofish — instantiation is always inferred).
    fn collect_signatures(&mut self, roots: &[ItemId]) {
        for &root in roots {
            if let Item::FuncDef(func) = self.hir.item(root) {
                let sig = self.func_sig(func);
                self.tables.item_types.insert(root, sig);
            }
        }
    }

    /// Top-level `let`s are typed before any function body runs, in
    /// source order: bodies may reference any of them (they only execute
    /// once everything is initialized), while top-level code only sees
    /// earlier ones — which the resolver already enforced.
    fn check_top_lets(&mut self, roots: &[ItemId]) {
        for &root in roots {
            let Item::TopLet(top_let) = self.hir.item(root) else {
                continue;
            };

            let let_stmt = &top_let.let_stmt;
            let declared = let_stmt.ty.map(|ty| self.conv(ty));
            let value_ty = match &declared {
                Some(ty) => self.check_expect(let_stmt.value, ty),
                None => self.check_expr(let_stmt.value, None),
            };

            let binding_ty = declared.unwrap_or(value_ty);
            self.tables.item_types.insert(root, binding_ty);
        }
    }

    fn check_bodies(&mut self, roots: &[ItemId]) {
        for &root in roots {
            match self.hir.item(root) {
                Item::FuncDef(func) => {
                    self.check_func(DefRef::Item(root), func, None);
                }
                Item::StructDef(def) => {
                    // Methods of a generic struct see `self` as the
                    // struct applied to its own parameters, so
                    // `Point { x: self.y, y: self.x }` checks against a
                    // `Point<T>` return type.
                    let self_args: Vec<Type> = (0..def.generics.len())
                        .map(|index| Type::Generic {
                            owner: DefRef::Item(root),
                            index,
                        })
                        .collect();

                    for (index, method) in def.methods.iter().enumerate() {
                        self.check_func(
                            DefRef::Method { owner: root, index },
                            method,
                            Some(Type::Struct(root, self_args.clone())),
                        );
                    }
                }
                Item::Stmt(block) => {
                    self.check_block(block, None, false);
                }
                Item::Import(_) | Item::EnumDef(_) | Item::InterfaceDef(_) | Item::TopLet(_) => {}
            }
        }
    }

    /// Checks one function or method body against its declared
    /// signature. When the result type is `unit` (declared or default)
    /// the tail value is discarded rather than required to be `unit`
    /// (decision; scripting bodies routinely end in a non-unit call).
    fn check_func(&mut self, def_ref: DefRef, func: &FuncDef, self_ty: Option<Type>) {
        let saved_self = std::mem::replace(&mut self.self_ty, self_ty);

        if let Some(param_locals) = self.res.func_params.get(&def_ref) {
            for &slot in param_locals {
                let Some(local) = slot else { continue };
                let ty = match self.res.local(local).ty {
                    Some(annotation) => self.conv(annotation),
                    None => Type::Unknown,
                };
                self.tables.local_types.insert(local, ty);
            }
        }

        let ret = func.ret.map(|ty| self.conv(ty)).unwrap_or(Type::Unit);
        let saved_ret = self.ret_ty.replace(ret.clone());

        if ret == Type::Unit {
            self.check_block(&func.body, None, false);
        } else {
            let body_ty = self.check_block(&func.body, Some(&ret), true);
            if unify(&ret, &body_ty).is_none() {
                let span = self.item_span_of(def_ref);
                self.mismatch(span, &ret, &body_ty);
            }
        }

        self.ret_ty = saved_ret;
        self.self_ty = saved_self;
    }

    fn item_span_of(&self, def_ref: DefRef) -> Span {
        match def_ref {
            DefRef::Item(item) | DefRef::Method { owner: item, .. } => self.hir.span_of_item(item),
        }
    }

    /// Builds a function's value type from its annotations: `self` slots
    /// are dropped, missing return means `unit`.
    fn func_sig(&mut self, func: &FuncDef) -> Type {
        let params = func
            .params
            .iter()
            .filter_map(|param| match param {
                Param::SelfParam { .. } => None,
                Param::Named { ty, .. } => Some(self.conv(*ty)),
            })
            .collect();
        let ret = func.ret.map(|ty| self.conv(ty)).unwrap_or(Type::Unit);
        Type::func(params, ret)
    }

    // --- statements ----------------------------------------------------

    /// Checks a block and returns its value: the last expression
    /// statement's type (implicit return, `docs/spec/01-syntax.md`),
    /// `never` when the block ends in `return`/`throw`/`break`/`continue`
    /// (`docs/spec/03-types.md`, flow rules), `unit` otherwise. `used`
    /// says whether the value is consumed; `expected` propagates into
    /// the tail expression.
    fn check_block(&mut self, block: &[StmtId], expected: Option<&Type>, used: bool) -> Type {
        let Some((&last, init)) = block.split_last() else {
            return Type::Unit;
        };

        for &stmt in init {
            self.check_stmt(stmt);
        }

        match self.hir.stmt(last) {
            Stmt::Expr(value) => {
                let value = *value;
                self.check_value(value, expected, used)
            }
            // A trailing `if` parses as a statement, but `if` with an
            // `else` is an expression (`docs/spec/03-types.md`), so a
            // consumed block tail types it as one.
            Stmt::If(node) if used => {
                let node = node.clone();
                let span = self.hir.span_of_stmt(last);
                self.check_if_node(span, &node, expected, used)
            }
            Stmt::Return(_) | Stmt::Throw(_) | Stmt::Break | Stmt::Continue => {
                self.check_stmt(last);
                Type::Never
            }
            _ => {
                self.check_stmt(last);
                Type::Unit
            }
        }
    }

    /// Checks an expression whose conformance requirements depend on
    /// context: consumed values with an expectation are enforced,
    /// everything else takes the expectation as an inference hint only.
    fn check_value(&mut self, id: ExprId, expected: Option<&Type>, used: bool) -> Type {
        match expected {
            Some(exp) if used => self.check_expect(id, exp),
            _ => self.check_expr_used(id, expected, used),
        }
    }

    fn check_stmt(&mut self, id: StmtId) {
        let hir = self.hir;

        match hir.stmt(id) {
            Stmt::Let(let_stmt) => {
                let declared = let_stmt.ty.map(|ty| self.conv(ty));
                let value_ty = match &declared {
                    Some(ty) => self.check_expect(let_stmt.value, ty),
                    None => self.check_expr(let_stmt.value, None),
                };

                let binding_ty = declared.unwrap_or(value_ty);
                if let Some(&local) = self.res.stmt_locals.get(&id) {
                    self.tables.local_types.insert(local, binding_ty);
                }
            }
            Stmt::Assign { target, value } => {
                self.check_assign(id, *target, *value);
            }
            Stmt::Return(value) => self.check_return(id, *value),
            Stmt::Break | Stmt::Continue => {}
            Stmt::Throw(value) => {
                // The operand may be any value for now; error-set
                // inference is M2 (`docs/spec/04-errors.md`).
                self.check_expr(*value, None);
            }
            Stmt::If(node) => {
                for (cond, body) in &node.branches {
                    self.check_expect(*cond, &Type::Bool);
                    self.check_block(body, None, false);
                }
                if let Some(else_) = &node.else_ {
                    self.check_block(else_, None, false);
                }
            }
            Stmt::While { cond, body } => {
                self.check_expect(*cond, &Type::Bool);
                self.check_block(body, None, false);
            }
            Stmt::For {
                pattern,
                iterable,
                body,
            } => {
                let elem = self.check_iterable(*iterable);
                self.check_pattern(*pattern, &elem);
                self.check_block(body, None, false);
            }
            Stmt::Expr(value) => {
                self.check_expr_used(*value, None, false);
            }
        }
    }

    fn check_return(&mut self, id: StmtId, value: Option<ExprId>) {
        let Some(ret) = self.ret_ty.clone() else {
            // Decision: top-level code has no function to return from.
            let span = self.hir.span_of_stmt(id);
            self.error(err_at(
                codes::T_RETURN_OUTSIDE_FUNCTION,
                span,
                "`return` outside a function".to_string(),
                "top-level code cannot return",
            ));
            if let Some(value) = value {
                self.check_expr(value, None);
            }
            return;
        };

        match value {
            Some(value) => {
                self.check_expect(value, &ret);
            }
            None => {
                if unify(&ret, &Type::Unit).is_none() {
                    let span = self.hir.span_of_stmt(id);
                    self.mismatch(span, &ret, &Type::Unit);
                }
            }
        }
    }

    /// `let` binds immutably; only `let mut` allows reassignment, of the
    /// same type (`docs/spec/03-types.md`). Field and index targets are
    /// always assignable — immutability belongs to the variable, not the
    /// value (interior mutation, closed decision in the same spec) — but
    /// the assigned value must still match the member type.
    fn check_assign(&mut self, stmt: StmtId, target: ExprId, value: ExprId) {
        let hir = self.hir;
        let span = hir.span_of_stmt(stmt);

        match hir.expr(target) {
            Expr::Ident(name) => {
                let name = name.clone();
                self.check_named_assign(span, target, &name, value);
            }
            Expr::Field { .. } | Expr::Index { .. } => {
                let target_ty = self.check_expr(target, None);

                // A `Json` tree is immutable after `parse` (BRS-34,
                // `docs/spec/05-stdlib.md`), so a `Json` index is never
                // an assignment target.
                if let Expr::Index { recv, .. } = hir.expr(target) {
                    let json_recv = match self.tables.expr_types.get(recv) {
                        Some(Type::Json) => true,
                        Some(Type::Option(inner)) => **inner == Type::Json,
                        _ => false,
                    };

                    if json_recv {
                        self.error(err_at(
                            codes::T_CANNOT_ASSIGN,
                            span,
                            "cannot assign into a `Json` value".to_string(),
                            "`Json` is immutable",
                        ));
                    }
                }

                self.check_expect(value, &target_ty);
            }
            _ => {
                self.error(err_at(
                    codes::T_INVALID_ASSIGNMENT_TARGET,
                    span,
                    "invalid assignment target".to_string(),
                    "not assignable",
                ));
                self.check_expr(value, None);
            }
        }
    }

    fn check_named_assign(&mut self, span: Span, target: ExprId, name: &str, value: ExprId) {
        match self.res.expr_res.get(&target).copied() {
            Some(Res::Local(local)) => {
                let info = self.res.local(local);
                let (mutable, decl_span) = (info.mutable, info.span);

                let ty = self
                    .tables
                    .local_types
                    .get(&local)
                    .cloned()
                    .unwrap_or(Type::Unknown);
                self.tables.expr_types.insert(target, ty.clone());

                if !mutable {
                    self.error(
                        err_at(
                            codes::T_CANNOT_ASSIGN,
                            span,
                            format!("cannot assign to immutable binding `{name}`"),
                            "reassigned here",
                        )
                        .with_label(decl_span, "declared immutable here".to_string())
                        .with_note("declare it with `let mut` to allow reassignment".to_string()),
                    );
                }
                self.check_expect(value, &ty);
            }
            Some(Res::Item(item)) => self.check_item_assign(span, target, name, item, value),
            Some(Res::SelfParam) => {
                self.error(err_at(
                    codes::T_CANNOT_ASSIGN,
                    span,
                    "cannot assign to `self`".to_string(),
                    "not assignable",
                ));
                self.check_expr(value, None);
            }
            Some(Res::Builtin(_) | Res::Module(_)) => {
                self.error(err_at(
                    codes::T_CANNOT_ASSIGN,
                    span,
                    format!("cannot assign to `{name}`"),
                    "not assignable",
                ));
                self.check_expr(value, None);
            }
            None => {
                self.check_expr(value, None);
            }
        }
    }

    fn check_item_assign(
        &mut self,
        span: Span,
        target: ExprId,
        name: &str,
        item: ItemId,
        value: ExprId,
    ) {
        match self.hir.item(item) {
            Item::TopLet(top_let) => {
                let mutable = top_let.let_stmt.mutable;
                let decl_span = self.hir.span_of_item(item);

                let ty = self
                    .tables
                    .item_types
                    .get(&item)
                    .cloned()
                    .unwrap_or(Type::Unknown);
                self.tables.expr_types.insert(target, ty.clone());

                if !mutable {
                    self.error(
                        err_at(
                            codes::T_CANNOT_ASSIGN,
                            span,
                            format!("cannot assign to immutable binding `{name}`"),
                            "reassigned here",
                        )
                        .with_label(decl_span, "declared immutable here".to_string())
                        .with_note("declare it with `let mut` to allow reassignment".to_string()),
                    );
                }
                self.check_expect(value, &ty);
            }
            _ => {
                self.error(err_at(
                    codes::T_CANNOT_ASSIGN,
                    span,
                    format!("cannot assign to `{name}`"),
                    "not assignable",
                ));
                self.check_expr(value, None);
            }
        }
    }

    /// `for` iterates the built-in types only (`docs/spec/03-types.md`):
    /// `Vector<T>` yields `T`, `Map<K, V>` yields `(K, V)` entries,
    /// `Set<T>` yields `T`, ranges yield `int`, strings yield `char`.
    fn check_iterable(&mut self, iterable: ExprId) -> Type {
        let ty = self.check_expr(iterable, None);
        match ty {
            Type::Vector(elem) | Type::Set(elem) => *elem,
            Type::Map(key, value) => Type::Tuple(vec![*key, *value]),
            Type::Range => Type::Int,
            Type::String => Type::Char,
            ref flexible if flexible.is_flexible() => Type::Unknown,
            other => {
                let span = self.hir.span_of_expr(iterable);
                self.error(
                    err_at(
                        codes::T_CANNOT_ITERATE,
                        span,
                        format!("cannot iterate over `{}`", other.display(self.hir)),
                        "not iterable",
                    )
                    .with_note(
                        "`for` iterates `Vector`, `Map`, `Set`, ranges, and `string`".to_string(),
                    ),
                );
                Type::Unknown
            }
        }
    }

    // --- expressions ---------------------------------------------------

    fn check_expr(&mut self, id: ExprId, expected: Option<&Type>) -> Type {
        self.check_expr_used(id, expected, true)
    }

    fn check_expr_used(&mut self, id: ExprId, expected: Option<&Type>, used: bool) -> Type {
        let ty = self.infer_expr(id, expected, used);
        self.tables.expr_types.insert(id, ty.clone());
        ty
    }

    /// Checks `id` and requires it to unify with `expected`, reporting a
    /// mismatch at the expression's span otherwise. Returns the unified
    /// type, or `Unknown` after a report (poisoning suppression).
    fn check_expect(&mut self, id: ExprId, expected: &Type) -> Type {
        let found = self.check_expr(id, Some(expected));
        match unify(expected, &found) {
            Some(ty) => ty,
            None => {
                let span = self.hir.span_of_expr(id);
                self.mismatch(span, expected, &found);
                Type::Unknown
            }
        }
    }

    fn infer_expr(&mut self, id: ExprId, expected: Option<&Type>, used: bool) -> Type {
        let hir = self.hir;

        match hir.expr(id) {
            Expr::Int(_) => Type::Int,
            Expr::Float(_) => Type::Float,
            Expr::Bool(_) => Type::Bool,
            Expr::Char(_) => Type::Char,
            Expr::Unit => Type::Unit,
            Expr::Str(_) => Type::String,
            Expr::Ident(_) | Expr::SelfExpr => self.check_ident(id),
            Expr::Call { callee, args } => self.check_call(id, *callee, args.clone()),
            Expr::Field { recv, name } => {
                let (recv, name) = (*recv, name.clone());
                self.check_field(id, recv, &name)
            }
            Expr::Index { recv, index } => self.check_index(*recv, *index),
            Expr::Unary { op, operand } => self.check_unary(*op, *operand),
            Expr::Binary { op, lhs, rhs } => self.check_binary(id, *op, *lhs, *rhs),
            Expr::OptionWrap(inner) => self.check_option_wrap(id, *inner),
            Expr::ToString(inner) => {
                // Every type has a derived `toString`
                // (`docs/spec/03-types.md`), so the operand is free.
                self.check_expr(*inner, None);
                Type::String
            }
            Expr::Lambda { params, body } => {
                let (params, body) = (params.clone(), body.clone());
                self.check_lambda(id, &params, &body, expected)
            }
            Expr::If(node) => {
                let node = node.clone();
                self.check_if(id, &node, expected, used)
            }
            Expr::Match { scrutinee, arms } => {
                let (scrutinee, arms) = (*scrutinee, arms.clone());
                self.check_match(id, scrutinee, &arms, expected, used)
            }
            Expr::VectorLit(elements) => {
                let elements = elements.clone();
                self.check_vector_lit(id, &elements, expected)
            }
            Expr::MapLit(entries) => {
                let entries = entries.clone();
                self.check_map_lit(id, &entries, expected)
            }
            Expr::TupleLit(elements) => {
                let elements = elements.clone();
                self.check_tuple_lit(&elements, expected)
            }
            Expr::StructLit { type_name, fields } => {
                let (type_name, fields) = (type_name.clone(), fields.clone());
                self.check_struct_lit(id, &type_name, &fields, expected)
            }
            Expr::Range { lo, hi, .. } => {
                // Ranges are their own lazy type over ints, never
                // `Vector<int>` (`docs/spec/03-types.md`).
                self.check_expect(*lo, &Type::Int);
                self.check_expect(*hi, &Type::Int);
                Type::Range
            }
            Expr::Catch { subject, arms, .. } => {
                let (subject, arms) = (*subject, arms.clone());
                self.check_catch(id, subject, &arms, expected, used)
            }
            Expr::EnumCtor { args, .. } => {
                let args = args.clone();
                self.check_enum_ctor(id, &args, expected)
            }
        }
    }

    fn check_ident(&mut self, id: ExprId) -> Type {
        match self.res.expr_res.get(&id).copied() {
            Some(Res::Local(local)) => self
                .tables
                .local_types
                .get(&local)
                .cloned()
                .unwrap_or(Type::Unknown),
            Some(Res::Item(item)) => self
                .tables
                .item_types
                .get(&item)
                .cloned()
                .unwrap_or(Type::Unknown),
            Some(Res::SelfParam) => self.self_ty.clone().unwrap_or(Type::Unknown),
            // Module handles have no type of their own, and `puts`/
            // `print` accept any value (universal `toString`,
            // `docs/spec/05-stdlib.md`), so neither gets a `Fn` type.
            Some(Res::Module(_) | Res::Builtin(_)) => Type::Unknown,
            None => Type::Unknown,
        }
    }

    // --- calls and members ---------------------------------------------

    fn check_call(&mut self, id: ExprId, callee: ExprId, args: Vec<ExprId>) -> Type {
        let hir = self.hir;
        let span = hir.span_of_expr(id);

        if let Expr::Ident(_) = hir.expr(callee)
            && let Some(Res::Builtin(builtin)) = self.res.expr_res.get(&callee)
        {
            // `puts`/`print` print any single value via the universal
            // derived `toString` (`docs/spec/05-stdlib.md`).
            let name = builtin.name();
            self.tables
                .expr_types
                .insert(callee, Type::func(vec![Type::Unknown], Type::Unit));

            if args.len() != 1 {
                self.error(err_at(
                    codes::T_WRONG_ARG_COUNT,
                    span,
                    format!(
                        "wrong number of arguments to `{name}`: expected 1, found {}",
                        args.len()
                    ),
                    "expected exactly 1 argument",
                ));
            }
            for &arg in &args {
                self.check_expr(arg, None);
            }
            return Type::Unit;
        }

        if let Expr::Field { recv, name } = hir.expr(callee) {
            let (recv, name) = (*recv, name.clone());
            return self.check_method_call(span, callee, recv, &name, &args);
        }

        // Direct calls to generic functions solve the type parameters
        // from the arguments — there is no turbofish, instantiation is
        // always inferred at the call site (`docs/spec/02-grammar.md`).
        if let Expr::Ident(_) = hir.expr(callee)
            && let Some(&Res::Item(item)) = self.res.expr_res.get(&callee)
            && let Item::FuncDef(func) = hir.item(item)
            && !func.generics.is_empty()
        {
            return self.check_generic_call(span, callee, item, func, &args);
        }

        let callee_ty = self.check_expr(callee, None);
        match callee_ty {
            Type::Fn { params, ret } => {
                self.check_args(span, &args, &params);
                *ret
            }
            flexible if flexible.is_flexible() => {
                for &arg in &args {
                    self.check_expr(arg, None);
                }
                Type::Unknown
            }
            other => {
                self.error(err_at(
                    codes::T_NOT_CALLABLE,
                    span,
                    format!("cannot call a value of type `{}`", other.display(self.hir)),
                    "not callable",
                ));
                for &arg in &args {
                    self.check_expr(arg, None);
                }
                Type::Unknown
            }
        }
    }

    fn check_args(&mut self, span: Span, args: &[ExprId], params: &[Type]) {
        if args.len() != params.len() {
            self.error(err_at(
                codes::T_WRONG_ARG_COUNT,
                span,
                format!(
                    "wrong number of arguments: expected {}, found {}",
                    params.len(),
                    args.len()
                ),
                &format!("expected {} argument(s)", params.len()),
            ));
        }

        for (i, &arg) in args.iter().enumerate() {
            match params.get(i) {
                Some(param) => {
                    self.check_expect(arg, param);
                }
                None => {
                    self.check_expr(arg, None);
                }
            }
        }
    }

    /// Checks a direct call to a generic function. Instantiation is
    /// always inferred from the arguments — there is no turbofish
    /// (`docs/spec/02-grammar.md`) — and everything happens in the
    /// checker: the VM runs one uniform function per generic definition
    /// (`docs/spec/03-types.md`, generics execution model).
    ///
    /// Each argument checks against the signature with everything solved
    /// so far substituted in, solving remaining type parameters
    /// left-to-right (first solution wins; a later conflicting argument
    /// reports a plain mismatch against the substituted type). The
    /// expectation propagates into the argument only once its parameter
    /// type is fully solved, so literals never unify against a rigid
    /// `T`. Constraints are then checked against the solved types —
    /// satisfaction happens at the call site, where the concrete types
    /// are known (`docs/spec/03-types.md`) — and the result is the
    /// substituted return type.
    fn check_generic_call(
        &mut self,
        span: Span,
        callee: ExprId,
        item: ItemId,
        func: &'a FuncDef,
        args: &[ExprId],
    ) -> Type {
        let sig = self.tables.item_types.get(&item).cloned();
        let Some(Type::Fn { params, ret }) = sig else {
            for &arg in args {
                self.check_expr(arg, None);
            }
            return Type::Unknown;
        };

        if args.len() != params.len() {
            self.error(err_at(
                codes::T_WRONG_ARG_COUNT,
                span,
                format!(
                    "wrong number of arguments: expected {}, found {}",
                    params.len(),
                    args.len()
                ),
                &format!("expected {} argument(s)", params.len()),
            ));
        }

        let owner = DefRef::Item(item);
        let mut map: HashMap<(DefRef, usize), Type> = HashMap::new();
        let mut poisoned: HashSet<(DefRef, usize)> = HashSet::new();

        for (i, &arg) in args.iter().enumerate() {
            let Some(param) = params.get(i) else {
                self.check_expr(arg, None);
                continue;
            };

            let expected = substitute(param, &map);
            let hint = (!contains_generic_of(&expected, owner)).then_some(&expected);
            let found = self.check_expr(arg, hint);

            if !solve(&expected, &found, owner, &mut map, &mut poisoned) {
                let arg_span = self.hir.span_of_expr(arg);
                self.mismatch(arg_span, &expected, &found);
            }
        }

        self.finish_generic_solution(span, owner, &func.generics, &func.name, &mut map, &poisoned);
        self.check_constraints(span, owner, &func.generics, &map);

        let params: Vec<Type> = params.iter().map(|p| substitute(p, &map)).collect();
        let ret = substitute(&ret, &map);
        self.tables
            .expr_types
            .insert(callee, Type::func(params, ret.clone()));
        ret
    }

    /// Reports `cannot infer type parameter` for every generic that
    /// stayed unsolved and fills it with `Unknown`. Parameters whose
    /// only evidence was a flexible (poisoned or `never`) type are
    /// filled silently: the cause was already reported.
    fn finish_generic_solution(
        &mut self,
        span: Span,
        owner: DefRef,
        generics: &[GenericParam],
        owner_name: &str,
        map: &mut HashMap<(DefRef, usize), Type>,
        poisoned: &HashSet<(DefRef, usize)>,
    ) {
        for (index, generic) in generics.iter().enumerate() {
            if map.contains_key(&(owner, index)) {
                continue;
            }

            if !poisoned.contains(&(owner, index)) {
                let name = &generic.name;
                self.error(err_at(
                    codes::T_CANNOT_INFER_TYPE_PARAM,
                    span,
                    format!("cannot infer type parameter `{name}` of `{owner_name}`"),
                    "no argument determines it",
                ));
            }
            map.insert((owner, index), Type::Unknown);
        }
    }

    fn check_method_call(
        &mut self,
        span: Span,
        callee: ExprId,
        recv: ExprId,
        name: &str,
        args: &[ExprId],
    ) -> Type {
        if self.is_module_ref(recv) {
            self.check_expr(recv, None);
            return self.check_module_call(span, callee, recv, name, args);
        }

        let recv_ty = self.check_expr(recv, None);

        // `reduce` and `zip` need argument-driven inference the fixed
        // method table cannot express (BRS-35): `reduce`'s accumulator
        // type comes from `init`, `zip`'s pair type from the other
        // vector.
        if let Type::Vector(elem) = &recv_ty {
            let elem = (**elem).clone();
            match name {
                "reduce" => return self.check_reduce(span, callee, elem, args),
                "zip" => return self.check_zip(span, callee, elem, args),
                _ => {}
            }
        }

        match self.lookup_member(&recv_ty, name) {
            Member::Sig(sig) => {
                self.check_args(span, args, &sig.params);

                let ret = match sig.ret {
                    RetRule::Fixed(ty) => ty,
                    RetRule::VectorOfFnRet => {
                        let fn_ret = args
                            .first()
                            .and_then(|arg| self.tables.expr_types.get(arg))
                            .and_then(|ty| match ty {
                                Type::Fn { ret, .. } => Some((**ret).clone()),
                                _ => None,
                            })
                            .unwrap_or(Type::Unknown);
                        Type::vector(fn_ret)
                    }
                };

                self.tables
                    .expr_types
                    .insert(callee, Type::func(sig.params, ret.clone()));
                ret
            }
            Member::Value(Type::Fn { params, ret }) => {
                self.check_args(span, args, &params);
                self.tables.expr_types.insert(
                    callee,
                    Type::Fn {
                        params,
                        ret: ret.clone(),
                    },
                );
                *ret
            }
            Member::Value(other) => {
                self.tables.expr_types.insert(callee, other.clone());
                if !other.is_flexible() {
                    self.error(err_at(
                        codes::T_NOT_CALLABLE,
                        span,
                        format!("cannot call a value of type `{}`", other.display(self.hir)),
                        "not callable",
                    ));
                }
                for &arg in args {
                    self.check_expr(arg, None);
                }
                Type::Unknown
            }
            Member::Deferred => {
                self.tables.expr_types.insert(callee, Type::Unknown);
                for &arg in args {
                    self.check_expr(arg, None);
                }
                Type::Unknown
            }
            Member::Missing => {
                self.report_missing_member(span, &recv_ty, name);
                self.tables.expr_types.insert(callee, Type::Unknown);
                for &arg in args {
                    self.check_expr(arg, None);
                }
                Type::Unknown
            }
        }
    }

    /// A member call on a std module handle. Modules whose signatures
    /// have closed (`proc`, `env` — BRS-32) check like builtin methods:
    /// arity within the required..=required+optional range, arguments
    /// against their declared types, result the declared type. Every
    /// other module (and file imports) stays `Unknown`-typed until its
    /// signatures close during M4.
    fn check_module_call(
        &mut self,
        span: Span,
        callee: ExprId,
        recv: ExprId,
        name: &str,
        args: &[ExprId],
    ) -> Type {
        let unknown = |checker: &mut Self| {
            checker.tables.expr_types.insert(callee, Type::Unknown);
            for &arg in args {
                checker.check_expr(arg, None);
            }
            Type::Unknown
        };

        let Some(module) = self.std_module_of(recv) else {
            return unknown(self);
        };

        if builtins::module_member_special(&module, name) {
            return self.check_module_special(span, callee, &module, name, args);
        }

        let Some(sig) = builtins::module_member(&module, name) else {
            // Calling a module constant (`math.pi()`) is a call on a
            // plain value, not an unknown member (BRS-35).
            if let Some(ty) = builtins::module_constant(&module, name) {
                self.error(err_at(
                    codes::T_NOT_CALLABLE,
                    span,
                    format!("cannot call a value of type `{}`", ty.display(self.hir)),
                    "not callable",
                ));
                for &arg in args {
                    self.check_expr(arg, None);
                }
                self.tables.expr_types.insert(callee, ty);
                return Type::Unknown;
            }
            if builtins::module_closed(&module) {
                self.error(err_at(
                    codes::T_UNKNOWN_MEMBER,
                    span,
                    format!("module `{module}` has no member `{name}`"),
                    "unknown member",
                ));
            }
            return unknown(self);
        };

        let (min, max) = (sig.required.len(), sig.required.len() + sig.optional.len());
        if args.len() < min || args.len() > max {
            let expected = if min == max {
                format!("{min}")
            } else {
                format!("{min} to {max}")
            };
            self.error(err_at(
                codes::T_WRONG_ARG_COUNT,
                span,
                format!(
                    "wrong number of arguments: expected {expected}, found {}",
                    args.len()
                ),
                &format!("expected {expected} argument(s)"),
            ));
        }

        let params = sig.required.iter().chain(sig.optional.iter());
        for (&arg, param) in args.iter().zip(params) {
            match param {
                builtins::ModuleParam::Ty(ty) => {
                    let ty = ty.clone();
                    self.check_expect(arg, &ty);
                }
                builtins::ModuleParam::Command => {
                    let found = self.check_expr(arg, None);
                    let accepted = unify(&found, &Type::String).is_some()
                        || unify(&found, &Type::vector(Type::String)).is_some();
                    if !accepted {
                        let found = found.display(self.hir);
                        let arg_span = self.hir.span_of_expr(arg);
                        self.error(err_at(
                            codes::T_MISMATCHED_TYPES,
                            arg_span,
                            format!(
                                "mismatched types: expected `Vector<string>` or `string`, \
                                 found `{found}`"
                            ),
                            "expected `Vector<string>` or `string`",
                        ));
                    }
                }
            }
        }
        for &arg in args.iter().skip(max) {
            self.check_expr(arg, None);
        }

        self.tables.expr_types.insert(callee, Type::Unknown);
        sig.ret
    }

    /// The module members whose types the fixed table cannot express
    /// (BRS-35): `math.abs`/`min`/`max` are polymorphic over
    /// `int`/`float`, and `rand.choice`/`shuffle` are generic over the
    /// vector element.
    fn check_module_special(
        &mut self,
        span: Span,
        callee: ExprId,
        module: &str,
        name: &str,
        args: &[ExprId],
    ) -> Type {
        let ret = match (module, name) {
            ("math", "abs") => self.check_numeric_args(span, args, 1),
            ("math", "min" | "max") => self.check_numeric_args(span, args, 2),
            ("rand", "choice" | "shuffle") => {
                self.check_args(span, args, &[Type::vector(Type::Unknown)]);

                let elem = match args.first().and_then(|arg| self.tables.expr_types.get(arg)) {
                    Some(Type::Vector(inner)) => (**inner).clone(),
                    _ => Type::Unknown,
                };
                if name == "choice" {
                    elem
                } else {
                    Type::vector(elem)
                }
            }
            _ => unreachable!("module_member_special and this table agree"),
        };

        self.tables.expr_types.insert(callee, Type::Unknown);
        ret
    }

    /// Checks the arguments of a numeric-polymorphic `math` member:
    /// every argument must be `int` or `float`, all of the same kind,
    /// and the result is that kind (BRS-35).
    fn check_numeric_args(&mut self, span: Span, args: &[ExprId], count: usize) -> Type {
        if args.len() != count {
            self.error(err_at(
                codes::T_WRONG_ARG_COUNT,
                span,
                format!(
                    "wrong number of arguments: expected {count}, found {}",
                    args.len()
                ),
                &format!("expected {count} argument(s)"),
            ));
        }

        let mut decided: Option<Type> = None;
        for (i, &arg) in args.iter().enumerate() {
            if i >= count {
                self.check_expr(arg, None);
                continue;
            }

            if let Some(ty) = decided.clone()
                && !ty.is_flexible()
            {
                self.check_expect(arg, &ty);
                continue;
            }

            let found = self.check_expr(arg, None);
            match found {
                Type::Int | Type::Float => decided = Some(found),
                flexible if flexible.is_flexible() => {
                    decided.get_or_insert(Type::Unknown);
                }
                other => {
                    let arg_span = self.hir.span_of_expr(arg);
                    let shown = other.display(self.hir);
                    self.error(err_at(
                        codes::T_MISMATCHED_TYPES,
                        arg_span,
                        format!("mismatched types: expected `int` or `float`, found `{shown}`"),
                        "expected `int` or `float`",
                    ));
                    decided.get_or_insert(Type::Unknown);
                }
            }
        }

        decided.unwrap_or(Type::Unknown)
    }

    /// `Vector<T>.reduce(init, f)` folds left with `f: (U, T) -> U`;
    /// the accumulator type `U` comes from `init` (BRS-35).
    fn check_reduce(&mut self, span: Span, callee: ExprId, elem: Type, args: &[ExprId]) -> Type {
        if args.len() != 2 {
            self.error(err_at(
                codes::T_WRONG_ARG_COUNT,
                span,
                format!(
                    "wrong number of arguments: expected 2, found {}",
                    args.len()
                ),
                "expected 2 argument(s)",
            ));
            for &arg in args {
                self.check_expr(arg, None);
            }
            self.tables.expr_types.insert(callee, Type::Unknown);
            return Type::Unknown;
        }

        let acc = self.check_expr(args[0], None);
        let f = Type::func(vec![acc.clone(), elem], acc.clone());
        self.check_expect(args[1], &f);

        self.tables
            .expr_types
            .insert(callee, Type::func(vec![acc.clone(), f], acc.clone()));
        acc
    }

    /// `Vector<T>.zip(other: Vector<U>) -> Vector<(T, U)>`: the pair's
    /// second element comes from the argument (BRS-35).
    fn check_zip(&mut self, span: Span, callee: ExprId, elem: Type, args: &[ExprId]) -> Type {
        self.check_args(span, args, &[Type::vector(Type::Unknown)]);

        let other = match args.first().and_then(|arg| self.tables.expr_types.get(arg)) {
            Some(Type::Vector(inner)) => (**inner).clone(),
            _ => Type::Unknown,
        };

        let ret = Type::vector(Type::Tuple(vec![elem, other.clone()]));
        self.tables
            .expr_types
            .insert(callee, Type::func(vec![Type::vector(other)], ret.clone()));
        ret
    }

    /// The module name of a `Res::Module` receiver when it is a
    /// `std::` import; `None` for file imports.
    fn std_module_of(&self, recv: ExprId) -> Option<String> {
        let Some(&Res::Module(item)) = self.res.expr_res.get(&recv) else {
            return None;
        };
        let Item::Import(import) = self.hir.item(item) else {
            return None;
        };

        match &import.path {
            ImportPath::Std(segments) => segments.last().cloned(),
            ImportPath::File(_) => None,
        }
    }

    fn check_field(&mut self, id: ExprId, recv: ExprId, name: &str) -> Type {
        if self.is_module_ref(recv) {
            self.check_expr(recv, None);

            // Module constants (`math.pi`, BRS-35) are the only typed
            // plain-value members; every other member read stays
            // `Unknown` (a bound module member is untyped in v1).
            if let Some(module) = self.std_module_of(recv)
                && let Some(ty) = builtins::module_constant(&module, name)
            {
                return ty;
            }
            return Type::Unknown;
        }

        let recv_ty = self.check_expr(recv, None);
        match self.lookup_member(&recv_ty, name) {
            Member::Sig(sig) => {
                let ret = match sig.ret {
                    RetRule::Fixed(ty) => ty,
                    RetRule::VectorOfFnRet => Type::vector(Type::Unknown),
                };
                Type::func(sig.params, ret)
            }
            Member::Value(ty) => ty,
            Member::Deferred => Type::Unknown,
            Member::Missing => {
                let span = self.hir.span_of_expr(id);
                self.report_missing_member(span, &recv_ty, name);
                Type::Unknown
            }
        }
    }

    fn is_module_ref(&self, expr: ExprId) -> bool {
        matches!(self.hir.expr(expr), Expr::Ident(_))
            && matches!(self.res.expr_res.get(&expr), Some(Res::Module(_)))
    }

    fn lookup_member(&mut self, recv: &Type, name: &str) -> Member {
        if recv.is_flexible() {
            return Member::Deferred;
        }

        if let Type::Struct(item, args) = recv {
            let args = args.clone();
            return self.lookup_struct_member(*item, &args, name);
        }
        if let Type::Generic { owner, index } = recv {
            return self.lookup_generic_member(*owner, *index, recv, name);
        }
        // The `Output` record's closed field set
        // (`docs/spec/05-stdlib.md`, BRS-32); everything else on it is
        // the universal `toString` or missing.
        if let Type::ProcOutput = recv {
            match name {
                "stdout" | "stderr" => return Member::Value(Type::String),
                "code" => return Member::Value(Type::Int),
                _ => {}
            }
        }

        if let Some(sig) = builtins::method(recv, name) {
            return Member::Sig(sig);
        }
        if name == "toString" {
            // Universal derived `toString` (`docs/spec/03-types.md`).
            return Member::Sig(MethodSig {
                params: vec![],
                ret: RetRule::Fixed(Type::String),
            });
        }
        Member::Missing
    }

    /// Looks up a field or method on a struct receiver. When the struct
    /// is generic, the receiver's arguments substitute the struct's own
    /// parameters in the member's declared type, so `Point<int>.x` is
    /// `int` and `Point<int>.swap` returns `Point<int>`.
    fn lookup_struct_member(&mut self, item: ItemId, args: &[Type], name: &str) -> Member {
        let Item::StructDef(def) = self.hir.item(item) else {
            return Member::Deferred;
        };

        let owner = DefRef::Item(item);
        let map: HashMap<(DefRef, usize), Type> = args
            .iter()
            .enumerate()
            .map(|(index, arg)| ((owner, index), arg.clone()))
            .collect();

        if let Some(field) = def.fields.iter().find(|f| f.name == name) {
            let ty = self.conv(field.ty);
            return Member::Value(substitute(&ty, &map));
        }
        if let Some(method) = def.methods.iter().find(|m| m.name == name) {
            let sig = self.func_sig(method);
            return Member::Value(substitute(&sig, &map));
        }
        if name == "toString" {
            // The universal derived `toString`; a declared method of the
            // same name replaces it (`docs/spec/03-types.md`).
            return Member::Sig(MethodSig {
                params: vec![],
                ret: RetRule::Fixed(Type::String),
            });
        }
        Member::Missing
    }

    /// Looks up a member on a value of generic type: only the members of
    /// the parameter's constraint (with `Self` mapped to the receiver)
    /// and the universal `toString` are visible. Builtin interfaces
    /// contribute operators, not members, so `Comparable`/`Hashable`
    /// expose nothing here.
    fn lookup_generic_member(
        &mut self,
        owner: DefRef,
        index: usize,
        recv: &Type,
        name: &str,
    ) -> Member {
        if let Some(members) = self.generic_constraint_members(owner, index)
            && let Some(member) = members.iter().find(|m| m.name == name)
        {
            return Member::Value(self.iface_member_fn(member, recv));
        }

        if name == "toString" {
            return Member::Sig(MethodSig {
                params: vec![],
                ret: RetRule::Fixed(Type::String),
            });
        }
        Member::Missing
    }

    fn report_missing_member(&mut self, span: Span, recv: &Type, name: &str) {
        let shown = recv.display(self.hir);

        if let Type::Generic { owner, index } = recv {
            let message = match self
                .generic_decl(*owner, *index)
                .and_then(|g| g.constraint.as_ref())
            {
                Some(Constraint::Named(iface)) => {
                    format!("`{shown}` is only known to satisfy `{iface}`; no method `{name}`")
                }
                Some(Constraint::Inline(_)) => format!(
                    "`{shown}` is only known to satisfy its inline constraint; no method `{name}`"
                ),
                None => format!("`{shown}` has no constraint; no method `{name}`"),
            };
            self.error(
                err_at(codes::T_UNKNOWN_MEMBER, span, message, "unknown method").with_note(
                "only the constraint's members and the universal `toString` are available on a generic value"
                    .to_string(),
            ));
            return;
        }

        if let Type::Struct(..) = recv {
            self.error(err_at(
                codes::T_UNKNOWN_MEMBER,
                span,
                format!("struct `{shown}` has no field or method `{name}`"),
                "unknown member",
            ));
            return;
        }

        if let Type::Vector(_) = recv
            && name == "join"
        {
            self.error(err_at(
                codes::T_JOIN_REQUIRES_STRING_VECTOR,
                span,
                format!("`join` requires `Vector<string>`, found `{shown}`"),
                "element type is not `string`",
            ));
            return;
        }

        self.error(err_at(
            codes::T_UNKNOWN_MEMBER,
            span,
            format!("no method `{name}` on `{shown}`"),
            "unknown method",
        ));
    }

    // --- generics and constraints --------------------------------------

    /// The declaration of one generic parameter, resolved through its
    /// owner (a struct method resolves via the owning struct's method
    /// list).
    fn generic_decl(&self, owner: DefRef, index: usize) -> Option<&'a GenericParam> {
        match owner {
            DefRef::Item(item) => match self.hir.item(item) {
                Item::FuncDef(func) => func.generics.get(index),
                Item::StructDef(def) => def.generics.get(index),
                Item::EnumDef(def) => def.generics.get(index),
                Item::InterfaceDef(def) => def.generics.get(index),
                _ => None,
            },
            DefRef::Method {
                owner: struct_item,
                index: method_index,
            } => match self.hir.item(struct_item) {
                Item::StructDef(def) => def
                    .methods
                    .get(method_index)
                    .and_then(|m| m.generics.get(index)),
                _ => None,
            },
        }
    }

    /// The member list a generic parameter's constraint provides: a user
    /// interface's methods or an inline constraint's members. Builtin
    /// interfaces and unconstrained parameters provide none.
    fn generic_constraint_members(&self, owner: DefRef, index: usize) -> Option<&'a [IfaceMember]> {
        let generic = self.generic_decl(owner, index)?;

        match generic.constraint.as_ref()? {
            Constraint::Named(_) => match self.res.constraint_res.get(&(owner, index)).copied() {
                Some(TypeRes::Item(item)) => match self.hir.item(item) {
                    Item::InterfaceDef(def) => Some(&def.methods),
                    _ => None,
                },
                _ => None,
            },
            Constraint::Inline(members) => Some(members),
        }
    }

    /// Whether a generic parameter's constraint is exactly the given
    /// builtin interface. This is the only way a generic entails
    /// `Comparable` or `Hashable`: an inline constraint never grants a
    /// builtin (`docs/spec/03-types.md`, closed satisfaction lists).
    fn generic_has_builtin(&self, owner: DefRef, index: usize, builtin: BuiltinType) -> bool {
        matches!(
            self.res.constraint_res.get(&(owner, index)),
            Some(TypeRes::Builtin(b)) if *b == builtin
        )
    }

    /// Converts an interface member signature into a function type with
    /// `Self` mapped to `self_ty` — the type satisfying the interface
    /// (`docs/spec/03-types.md`, structural interfaces). Conversion runs
    /// suppressed: type errors inside an interface body are not this use
    /// site's fault.
    fn iface_member_fn(&mut self, member: &IfaceMember, self_ty: &Type) -> Type {
        let saved = self.self_ty.replace(self_ty.clone());
        self.suppressed += 1;

        let params: Vec<Type> = member
            .params
            .iter()
            .filter_map(|param| match param {
                Param::SelfParam { .. } => None,
                Param::Named { ty, .. } => Some(self.conv(*ty)),
            })
            .collect();
        let ret = member.ret.map(|ty| self.conv(ty)).unwrap_or(Type::Unit);

        self.suppressed -= 1;
        self.self_ty = saved;
        Type::func(params, ret)
    }

    /// What a generic parameter's constraint means, resolved through
    /// `constraint_res` for the named form.
    fn constraint_kind(
        &self,
        owner: DefRef,
        index: usize,
        constraint: &'a Constraint,
    ) -> Option<ConstraintKind<'a>> {
        match constraint {
            Constraint::Named(_) => match self.res.constraint_res.get(&(owner, index)).copied() {
                Some(TypeRes::Builtin(builtin)) => Some(ConstraintKind::Builtin(builtin)),
                Some(TypeRes::Item(item)) => match self.hir.item(item) {
                    Item::InterfaceDef(def) => Some(ConstraintKind::Iface(item, &def.methods)),
                    _ => None,
                },
                _ => None,
            },
            Constraint::Inline(members) => Some(ConstraintKind::Inline(members)),
        }
    }

    /// Checks every solved type parameter against its declared
    /// constraint. Satisfaction is checked here, at the use site, where
    /// the concrete types are known (`docs/spec/03-types.md`). Flexible
    /// solutions skip silently (their cause was already reported), and
    /// constraints the resolver failed to resolve were reported there.
    fn check_constraints(
        &mut self,
        span: Span,
        owner: DefRef,
        generics: &'a [GenericParam],
        map: &HashMap<(DefRef, usize), Type>,
    ) {
        for (index, generic) in generics.iter().enumerate() {
            let Some(constraint) = &generic.constraint else {
                continue;
            };
            let Some(solved) = map.get(&(owner, index)) else {
                continue;
            };
            if solved.is_flexible() {
                continue;
            }
            let Some(kind) = self.constraint_kind(owner, index, constraint) else {
                continue;
            };

            let solved = solved.clone();
            if let Err(missing) = self.satisfies(&solved, &kind) {
                self.report_constraint_violation(span, &solved, &kind, generic, missing);
            }
        }
    }

    /// Whether `candidate` satisfies the constraint: it has all the
    /// interface's members with compatible signatures, `Self` replaced
    /// by the candidate — no conformance declarations
    /// (`docs/spec/03-types.md`). Builtin interfaces use their closed
    /// lists: `Comparable` is `int`/`float`/`string`/`char`, `Printable`
    /// is every type, `Hashable` is `int`/`string`/`char`/`bool` and
    /// tuples of those. A generic candidate satisfies a constraint only
    /// when its own constraint entails it: the same interface, the same
    /// builtin, `Printable` always, or its member set structurally
    /// covers the required members. `Err` carries the first missing or
    /// mismatched member name when there is one to point at.
    fn satisfies(
        &mut self,
        candidate: &Type,
        kind: &ConstraintKind<'a>,
    ) -> Result<(), Option<String>> {
        match kind {
            ConstraintKind::Builtin(BuiltinType::Printable) => Ok(()),
            ConstraintKind::Builtin(BuiltinType::Comparable) => match candidate {
                Type::Int | Type::Float | Type::String | Type::Char => Ok(()),
                Type::Generic { owner, index }
                    if self.generic_has_builtin(*owner, *index, BuiltinType::Comparable) =>
                {
                    Ok(())
                }
                _ => Err(None),
            },
            ConstraintKind::Builtin(BuiltinType::Hashable) => {
                if self.hashable(candidate) {
                    Ok(())
                } else {
                    Err(None)
                }
            }
            // The resolver only records interface builtins as
            // constraints, so nothing else reaches here.
            ConstraintKind::Builtin(_) => Ok(()),
            ConstraintKind::Iface(item, members) => {
                if let Type::Generic { owner, index } = candidate
                    && matches!(
                        self.res.constraint_res.get(&(*owner, *index)),
                        Some(TypeRes::Item(own)) if own == item
                    )
                {
                    return Ok(());
                }
                self.satisfies_members(candidate, members)
            }
            ConstraintKind::Inline(members) => self.satisfies_members(candidate, members),
        }
    }

    fn satisfies_members(
        &mut self,
        candidate: &Type,
        members: &'a [IfaceMember],
    ) -> Result<(), Option<String>> {
        for member in members {
            if !self.member_satisfied(candidate, member) {
                return Err(Some(member.name.clone()));
            }
        }
        Ok(())
    }

    /// Whether `candidate` provides one interface member with a
    /// compatible signature. Lookup goes through [`Self::lookup_member`],
    /// so struct candidates match their declared methods (and the
    /// universal derived `toString`), non-struct candidates their
    /// builtin methods, and generic candidates the members of their own
    /// constraint — which is exactly member-set entailment.
    fn member_satisfied(&mut self, candidate: &Type, member: &'a IfaceMember) -> bool {
        let required = self.iface_member_fn(member, candidate);

        self.suppressed += 1;
        let found = self.lookup_member(candidate, &member.name);
        self.suppressed -= 1;

        match found {
            Member::Sig(sig) => unify(&required, &fn_of_sig(sig)).is_some(),
            Member::Value(found_ty @ Type::Fn { .. }) => unify(&required, &found_ty).is_some(),
            Member::Deferred => true,
            Member::Value(_) | Member::Missing => false,
        }
    }

    /// The closed `Hashable` list: `int`, `string`, `char`, `bool`, and
    /// tuples of those recursively — never `float`, structs, or
    /// collections (`docs/spec/03-types.md`).
    fn hashable(&self, ty: &Type) -> bool {
        match ty {
            Type::Int | Type::String | Type::Char | Type::Bool => true,
            Type::Tuple(elems) => elems.iter().all(|e| self.hashable(e)),
            Type::Generic { owner, index } => {
                self.generic_has_builtin(*owner, *index, BuiltinType::Hashable)
            }
            _ => false,
        }
    }

    /// Enforces the closed `Hashable` list at a point where a concrete
    /// `Map` key or `Set` element type is established: type
    /// annotations, map literals, and the `Set` constructor. Key-taking
    /// methods check against the type established here, so they never
    /// re-report.
    fn check_key_hashable(&mut self, span: Span, ty: &Type, role: KeyRole) {
        if !self.hashable_violation(ty) {
            return;
        }

        let shown = ty.display(self.hir);
        self.error(err_at(
            codes::T_KEY_NOT_HASHABLE,
            span,
            format!(
                "`{shown}` cannot be a {}: `Hashable` is closed to `int`, `string`, `char`, `bool`, and tuples of those",
                role.phrase()
            ),
            "not `Hashable`",
        ));
    }

    /// Whether `ty` definitely falls outside the closed `Hashable`
    /// list: [`Self::hashable`] negated, except flexible components,
    /// which stay silent — their cause was already reported.
    fn hashable_violation(&self, ty: &Type) -> bool {
        match ty {
            Type::Tuple(elems) => elems.iter().any(|e| self.hashable_violation(e)),
            flexible if flexible.is_flexible() => false,
            other => !self.hashable(other),
        }
    }

    fn report_constraint_violation(
        &mut self,
        span: Span,
        solved: &Type,
        kind: &ConstraintKind<'a>,
        generic: &GenericParam,
        missing: Option<String>,
    ) {
        let shown = solved.display(self.hir);
        let message = match kind {
            ConstraintKind::Builtin(builtin) => {
                format!("`{shown}` does not satisfy `{}`", builtin.name())
            }
            ConstraintKind::Iface(item, _) => {
                format!(
                    "`{shown}` does not satisfy `{}`",
                    item_name(self.hir, *item)
                )
            }
            ConstraintKind::Inline(_) => format!(
                "`{shown}` does not satisfy the inline constraint on `{}`",
                generic.name
            ),
        };

        let mut diag = err_at(
            codes::T_CONSTRAINT_NOT_SATISFIED,
            span,
            message,
            "constraint not satisfied",
        );
        if let Some(member) = missing {
            diag = diag.with_note(format!(
                "`{shown}` has no member `{member}` with a compatible signature"
            ));
        }
        self.error(diag);
    }

    // --- operators -----------------------------------------------------

    fn check_index(&mut self, recv: ExprId, index: ExprId) -> Type {
        let recv_ty = self.check_expr(recv, None);

        match recv_ty {
            // `Vector[int] -> T`; out of range is a runtime panic, not a
            // type matter (`docs/spec/03-types.md`).
            Type::Vector(elem) => {
                self.check_expect(index, &Type::Int);
                *elem
            }
            // `Map[K] -> Option<V>`: a missing key is a normal case
            // (`docs/spec/03-types.md`).
            Type::Map(key, value) => {
                self.check_expect(index, &key);
                Type::Option(value)
            }
            // `Json[string]`/`Json[int] -> Option<Json>` — indexing is
            // total: a missing member, an out-of-range position, or a
            // node of the wrong kind is `None`, never a panic. Chains
            // flatten: indexing an `Option<Json>` propagates `None`
            // (BRS-34, `docs/spec/05-stdlib.md`).
            Type::Json => {
                self.check_json_index(index);
                Type::option(Type::Json)
            }
            Type::Option(inner) if *inner == Type::Json => {
                self.check_json_index(index);
                Type::option(Type::Json)
            }
            Type::String => {
                self.check_expr(index, None);
                let span = self.hir.span_of_expr(recv);
                self.error(
                    err_at(
                        codes::T_STRINGS_NOT_INDEXABLE,
                        span,
                        "strings are not indexable".to_string(),
                        "cannot index a string",
                    )
                    .with_note("use `chars()` or `slice(from, to)` instead".to_string()),
                );
                Type::Unknown
            }
            flexible if flexible.is_flexible() => {
                self.check_expr(index, None);
                Type::Unknown
            }
            other => {
                self.check_expr(index, None);
                let span = self.hir.span_of_expr(recv);
                self.error(err_at(
                    codes::T_CANNOT_INDEX,
                    span,
                    format!("cannot index `{}`", other.display(self.hir)),
                    "not indexable",
                ));
                Type::Unknown
            }
        }
    }

    /// A `Json` index is an object key (`string`) or an array position
    /// (`int`); anything else is a mismatch.
    fn check_json_index(&mut self, index: ExprId) {
        let found = self.check_expr(index, None);

        let accepted =
            unify(&found, &Type::String).is_some() || unify(&found, &Type::Int).is_some();
        if !accepted {
            let found = found.display(self.hir);
            let span = self.hir.span_of_expr(index);
            self.error(err_at(
                codes::T_MISMATCHED_TYPES,
                span,
                format!("mismatched types: expected `string` or `int`, found `{found}`"),
                "expected `string` or `int`",
            ));
        }
    }

    fn check_unary(&mut self, op: UnaryOp, operand: ExprId) -> Type {
        match op {
            UnaryOp::Not => {
                self.check_expect(operand, &Type::Bool);
                Type::Bool
            }
            UnaryOp::Neg => {
                let ty = self.check_expr(operand, None);
                match ty {
                    Type::Int | Type::Float => ty,
                    flexible if flexible.is_flexible() => Type::Unknown,
                    other => {
                        let span = self.hir.span_of_expr(operand);
                        self.error(err_at(
                            codes::T_INVALID_OPERANDS,
                            span,
                            format!(
                                "operator `-` expects `int` or `float`, found `{}`",
                                other.display(self.hir)
                            ),
                            "not negatable",
                        ));
                        Type::Unknown
                    }
                }
            }
        }
    }

    fn check_binary(&mut self, id: ExprId, op: BinaryOp, lhs: ExprId, rhs: ExprId) -> Type {
        match op {
            BinaryOp::Add
            | BinaryOp::Sub
            | BinaryOp::Mul
            | BinaryOp::Div
            | BinaryOp::Rem
            | BinaryOp::Pow => self.check_arithmetic(id, op, lhs, rhs),
            BinaryOp::Eq | BinaryOp::NotEq => self.check_equality(id, lhs, rhs),
            BinaryOp::Lt | BinaryOp::LtEq | BinaryOp::Gt | BinaryOp::GtEq => {
                self.check_ordering(id, op, lhs, rhs)
            }
            BinaryOp::And | BinaryOp::Or => {
                // `&&`/`||` take `bool` only; nothing is truthy
                // (`docs/spec/03-types.md`).
                self.check_expect(lhs, &Type::Bool);
                self.check_expect(rhs, &Type::Bool);
                Type::Bool
            }
        }
    }

    /// `+ - * / % **` work on `int×int` and `float×float` with no
    /// mixing; `+` additionally concatenates `string×string`
    /// (`docs/spec/03-types.md`, operator table).
    fn check_arithmetic(&mut self, id: ExprId, op: BinaryOp, lhs: ExprId, rhs: ExprId) -> Type {
        let lt = self.check_expr(lhs, None);
        let rt = self.check_expr(rhs, None);

        let valid = |ty: &Type| {
            matches!(ty, Type::Int | Type::Float)
                || (op == BinaryOp::Add && matches!(ty, Type::String))
        };

        if lt.is_flexible() || rt.is_flexible() {
            let known = if lt.is_flexible() { rt } else { lt };
            return if valid(&known) { known } else { Type::Unknown };
        }

        if lt == rt && valid(&lt) {
            return lt;
        }

        let span = self.hir.span_of_expr(id);
        let mut diag = err_at(
            codes::T_INVALID_OPERANDS,
            span,
            format!(
                "invalid operands for `{}`: `{}` and `{}`",
                op_str(op),
                lt.display(self.hir),
                rt.display(self.hir)
            ),
            "operands must be two `int`s or two `float`s",
        );
        if matches!(
            (&lt, &rt),
            (Type::Int, Type::Float) | (Type::Float, Type::Int)
        ) {
            diag = diag.with_note(
                "there are no implicit numeric conversions: convert explicitly with `toFloat()` or `toInt()`"
                    .to_string(),
            );
        }
        self.error(diag);
        Type::Unknown
    }

    /// `==`/`!=` require both sides to have the same type; comparison is
    /// structural (`docs/spec/03-types.md`, operator table).
    fn check_equality(&mut self, id: ExprId, lhs: ExprId, rhs: ExprId) -> Type {
        let lt = self.check_expr(lhs, None);
        let rt = self.check_expr(rhs, None);

        if unify(&lt, &rt).is_none() {
            let span = self.hir.span_of_expr(id);
            self.error(err_at(
                codes::T_CANNOT_COMPARE_EQUALITY,
                span,
                format!(
                    "cannot compare `{}` and `{}` for equality",
                    lt.display(self.hir),
                    rt.display(self.hir)
                ),
                "both sides must have the same type",
            ));
        }
        Type::Bool
    }

    /// `< <= > >=` order `int`, `float`, `string`, `char`, and generics
    /// constrained by `Comparable` (`docs/spec/03-types.md`, operator
    /// table). Only the named `Comparable` constraint grants ordering:
    /// an inline constraint never entails a builtin interface.
    fn check_ordering(&mut self, id: ExprId, op: BinaryOp, lhs: ExprId, rhs: ExprId) -> Type {
        let lt = self.check_expr(lhs, None);
        let rt = self.check_expr(rhs, None);

        if lt.is_flexible() || rt.is_flexible() {
            return Type::Bool;
        }

        let comparable_generic = matches!(
            &lt,
            Type::Generic { owner, index }
                if self.generic_has_builtin(*owner, *index, BuiltinType::Comparable)
        );

        let span = self.hir.span_of_expr(id);
        if lt != rt {
            self.error(err_at(
                codes::T_UNSUPPORTED_ORDERING,
                span,
                format!(
                    "cannot compare `{}` and `{}` with `{}`",
                    lt.display(self.hir),
                    rt.display(self.hir),
                    op_str(op)
                ),
                "both sides must have the same type",
            ));
        } else if !matches!(lt, Type::Int | Type::Float | Type::String | Type::Char)
            && !comparable_generic
        {
            self.error(err_at(
                codes::T_UNSUPPORTED_ORDERING,
                span,
                format!(
                    "`{}` does not support ordering with `{}`",
                    lt.display(self.hir),
                    op_str(op)
                ),
                "only `int`, `float`, `string`, `char`, and `Comparable` generics are ordered",
            ));
        }
        Type::Bool
    }

    // --- structured expressions ----------------------------------------

    /// `?.` flattens: when the member value is already an `Option` the
    /// wrap is a no-op, otherwise it wraps in `Some`
    /// (`docs/spec/03-types.md`, the `?.` operator rule). The per-node
    /// decision is recorded for the tree-walker; nodes whose operand
    /// type is deferred (`Unknown`) record nothing.
    fn check_option_wrap(&mut self, id: ExprId, inner: ExprId) -> Type {
        let ty = self.check_expr(inner, None);

        match ty {
            Type::Option(_) => {
                self.tables.wrap_decisions.insert(id, WrapDecision::NoOp);
                ty
            }
            flexible if flexible.is_flexible() => Type::Unknown,
            other => {
                self.tables.wrap_decisions.insert(id, WrapDecision::Wrap);
                Type::option(other)
            }
        }
    }

    /// Lambda parameters take their explicit annotations, then the
    /// expected function type from context, and are an error otherwise
    /// (`docs/spec/03-types.md`, inference rules). An expected `unit`
    /// return discards the body value, mirroring `unit` functions.
    fn check_lambda(
        &mut self,
        id: ExprId,
        params: &[LambdaParam],
        body: &LambdaBody,
        expected: Option<&Type>,
    ) -> Type {
        let (exp_params, exp_ret) = match expected {
            Some(Type::Fn { params, ret }) => (Some(params.as_slice()), Some((**ret).clone())),
            _ => (None, None),
        };

        let locals = self.res.lambda_params.get(&id).cloned().unwrap_or_default();

        let mut param_tys = Vec::with_capacity(params.len());
        for (i, param) in params.iter().enumerate() {
            let ty = match param.ty {
                Some(annotation) => self.conv(annotation),
                None => match exp_params.and_then(|p| p.get(i)) {
                    Some(expected) => expected.clone(),
                    None => {
                        let span = self.hir.span_of_expr(id);
                        self.error(err_at(
                            codes::T_LAMBDA_PARAM_NEEDS_ANNOTATION,
                            span,
                            format!("lambda parameter `{}` needs a type annotation", param.name),
                            "no type to infer from in this context",
                        ));
                        Type::Unknown
                    }
                },
            };

            if let Some(&local) = locals.get(i) {
                self.tables.local_types.insert(local, ty.clone());
            }
            param_tys.push(ty);
        }

        let ret = match exp_ret {
            Some(Type::Unit) => {
                self.check_lambda_body(body, None, false);
                Type::Unit
            }
            Some(expected_ret) => {
                let body_ty = self.check_lambda_body(body, Some(&expected_ret), true);
                match unify(&expected_ret, &body_ty) {
                    Some(ty) => ty,
                    None => {
                        let span = self.hir.span_of_expr(id);
                        self.mismatch(span, &expected_ret, &body_ty);
                        Type::Unknown
                    }
                }
            }
            None => self.check_lambda_body(body, None, true),
        };

        Type::func(param_tys, ret)
    }

    fn check_lambda_body(
        &mut self,
        body: &LambdaBody,
        expected: Option<&Type>,
        used: bool,
    ) -> Type {
        match body {
            LambdaBody::Expr(expr) => self.check_value(*expr, expected, used),
            LambdaBody::Block(block) => self.check_block(block, expected, used),
        }
    }

    /// `if` is an expression when there is an `else`; without one it is
    /// `unit` (`docs/spec/03-types.md`). Decision: branch-type
    /// mismatches are errors in value position and tolerated (typing
    /// `unit`) in statement position, mirroring `match`.
    fn check_if(&mut self, id: ExprId, node: &IfNode, expected: Option<&Type>, used: bool) -> Type {
        let span = self.hir.span_of_expr(id);
        self.check_if_node(span, node, expected, used)
    }

    fn check_if_node(
        &mut self,
        span: Span,
        node: &IfNode,
        expected: Option<&Type>,
        used: bool,
    ) -> Type {
        let has_else = node.else_.is_some();
        let value_ctx = used && has_else;
        let branch_expected = if value_ctx { expected } else { None };

        let mut branch_tys = Vec::new();
        for (cond, body) in &node.branches {
            self.check_expect(*cond, &Type::Bool);
            branch_tys.push(self.check_block(body, branch_expected, value_ctx));
        }
        if let Some(else_) = &node.else_ {
            branch_tys.push(self.check_block(else_, branch_expected, value_ctx));
        }

        if !value_ctx {
            return Type::Unit;
        }

        let mut acc = Type::Never;
        for ty in branch_tys {
            match unify(&acc, &ty) {
                Some(joined) => acc = joined,
                None => {
                    self.error(err_at(
                        codes::T_BRANCH_TYPE_MISMATCH,
                        span,
                        format!(
                            "`if` branches have mismatched types: `{}` vs `{}`",
                            acc.display(self.hir),
                            ty.display(self.hir)
                        ),
                        "all branches must produce the same type",
                    ));
                    return Type::Unknown;
                }
            }
        }
        acc
    }

    /// `match` is an expression whose arms must all produce the same
    /// type, or it is used as a statement and types `unit`
    /// (`docs/spec/03-types.md`). Either way it must be exhaustive
    /// (BRS-18): the check runs after the arms are typed, in
    /// [`Self::check_exhaustiveness`].
    fn check_match(
        &mut self,
        id: ExprId,
        scrutinee: ExprId,
        arms: &[MatchArm],
        expected: Option<&Type>,
        used: bool,
    ) -> Type {
        let scrutinee_ty = self.check_expr(scrutinee, None);

        if let Some(origin) = self.sugar_origins.get(&id).copied() {
            return self.check_sugar_match(scrutinee, &scrutinee_ty, origin, arms, expected, used);
        }

        let acc = self.check_match_arms(&scrutinee_ty, arms, expected, used, None);
        self.check_exhaustiveness(id, &scrutinee_ty, arms);

        if used { acc } else { Type::Unit }
    }

    /// Types every arm against the scrutinee and returns the unified arm
    /// type (value position only; mismatches in statement position are
    /// tolerated, matching `if`). `origin` marks a `??`-desugared match:
    /// its arm mismatch is the operator's fallback disagreeing with the
    /// carried type, reported as T030 in source terms, and the result
    /// poisons to `Unknown`.
    fn check_match_arms(
        &mut self,
        scrutinee_ty: &Type,
        arms: &[MatchArm],
        expected: Option<&Type>,
        used: bool,
        origin: Option<SugarOrigin>,
    ) -> Type {
        let mut acc = Type::Never;
        let mut poisoned = false;

        for arm in arms {
            self.check_pattern(arm.pattern, scrutinee_ty);
            if let Some(guard) = arm.guard {
                self.check_expect(guard, &Type::Bool);
            }

            let body_ty = self.check_arm_body(&arm.body, expected, used);
            if !used {
                continue;
            }

            match unify(&acc, &body_ty) {
                Some(joined) => acc = joined,
                None if origin == Some(SugarOrigin::Coalesce) => {
                    self.report_coalesce_mismatch(arm, &acc, &body_ty);
                    poisoned = true;
                }
                None => {
                    let span = self.arm_span(arm.pattern, &arm.body);
                    self.error(err_at(
                        codes::T_BRANCH_TYPE_MISMATCH,
                        span,
                        format!(
                            "`match` arms have mismatched types: `{}` vs `{}`",
                            acc.display(self.hir),
                            body_ty.display(self.hir)
                        ),
                        "all arms must produce the same type",
                    ));
                }
            }
        }

        if poisoned { Type::Unknown } else { acc }
    }

    /// Types a `match` desugared from `?.`/`??` (`docs/spec/03-types.md`,
    /// operator table). A non-`Option` receiver/left side is the
    /// operator's own error (T028/T029) at the real user expression;
    /// otherwise the arms type like any match — `OptionWrap` implements
    /// the `?.` flatten rule and `??` unwraps into its fallback. These
    /// matches are exhaustive by construction (`Some`/`None` cover
    /// `Option`), so the exhaustiveness check never runs on them.
    fn check_sugar_match(
        &mut self,
        scrutinee: ExprId,
        scrutinee_ty: &Type,
        origin: SugarOrigin,
        arms: &[MatchArm],
        expected: Option<&Type>,
        used: bool,
    ) -> Type {
        if !matches!(scrutinee_ty, Type::Option(_)) && !scrutinee_ty.is_flexible() {
            return self.report_sugar_needs_option(scrutinee, scrutinee_ty, origin, arms);
        }

        let acc = self.check_match_arms(scrutinee_ty, arms, expected, used, Some(origin));
        if used { acc } else { Type::Unit }
    }

    /// Reports `?.`/`??` applied to a non-`Option` (T028/T029) at the
    /// receiver's/left side's span, then types the synthesized arms
    /// silently — patterns bind against `Unknown` and bodies are checked
    /// without an expectation — so the desugaring never cascades into
    /// pattern or arm-mismatch errors. `?.` results in `Unknown`; `??`
    /// results in its fallback's type, the value the operator would have
    /// produced.
    fn report_sugar_needs_option(
        &mut self,
        scrutinee: ExprId,
        scrutinee_ty: &Type,
        origin: SugarOrigin,
        arms: &[MatchArm],
    ) -> Type {
        let span = self.hir.span_of_expr(scrutinee);
        let shown = scrutinee_ty.display(self.hir);

        match origin {
            SugarOrigin::SafeNav => self.error(
                err_at(
                    codes::T_SAFE_NAV_NEEDS_OPTION,
                    span,
                    format!("`?.` requires an `Option` receiver, found `{shown}`"),
                    "not an `Option`",
                )
                .with_note("`?.` unwraps an `Option` and propagates `None`".to_string()),
            ),
            SugarOrigin::Coalesce => self.error(
                err_at(
                    codes::T_COALESCE_NEEDS_OPTION,
                    span,
                    format!("`??` requires an `Option` on its left side, found `{shown}`"),
                    "not an `Option`",
                )
                .with_note("`??` supplies a fallback for when an `Option` is `None`".to_string()),
            ),
        }

        let mut result = Type::Unknown;
        for (index, arm) in arms.iter().enumerate() {
            self.check_pattern(arm.pattern, &Type::Unknown);

            // The desugar puts the user's fallback in the last (`None`)
            // arm of a `??`; its type is what the operator produces.
            let is_coalesce_fallback =
                origin == SugarOrigin::Coalesce && index == arms.len().saturating_sub(1);
            let body_ty = self.check_arm_body(&arm.body, None, is_coalesce_fallback);
            if is_coalesce_fallback {
                result = body_ty;
            }
        }
        result
    }

    /// The `??` fallback (the `None` arm's body, i.e. the user's right
    /// side) failed to unify with the type the `Option` carries. When
    /// the fallback is itself exactly `Option<carried>` the user likely
    /// stopped a chain one step early, so the note suggests finishing
    /// it. The whole expression poisons to `Unknown` afterwards.
    fn report_coalesce_mismatch(&mut self, arm: &MatchArm, carried: &Type, fallback: &Type) {
        let span = self.arm_span(arm.pattern, &arm.body);
        let carried_shown = carried.display(self.hir);
        let fallback_shown = fallback.display(self.hir);

        let mut diag = err_at(
            codes::T_COALESCE_TYPE_MISMATCH,
            span,
            format!(
                "`??` fallback has type `{fallback_shown}`, but the `Option` carries `{carried_shown}`"
            ),
            &format!("expected `{carried_shown}`"),
        );

        if matches!(fallback, Type::Option(inner) if **inner == *carried) {
            diag = diag.with_note(format!(
                "an `Option` right side only chains: add another `??` with a plain `{carried_shown}` fallback to end the chain"
            ));
        }
        self.error(diag);
    }

    /// Reports the missing cases of a non-exhaustive `match`
    /// (`docs/spec/01-syntax.md`: cover every case or use `_`;
    /// `docs/spec/03-types.md`: the checker understands enums, bools,
    /// tuples, and nested patterns, and requires `_` for `int`/
    /// `string`). The algorithm and the decisions it fixes live in
    /// [`crate::exhaust`]; a flexible scrutinee skips the check.
    fn check_exhaustiveness(&mut self, id: ExprId, scrutinee_ty: &Type, arms: &[MatchArm]) {
        let Some(missing) = exhaust::missing_cases(self.hir, self.res, scrutinee_ty, arms) else {
            return;
        };

        let shown: Vec<String> = missing
            .witnesses
            .iter()
            .map(|witness| format!("`{witness}`"))
            .collect();
        let extra = missing.total.saturating_sub(shown.len() as u64);

        let listed = match (shown.as_slice(), extra) {
            ([one], 0) => format!("{one} is"),
            ([first, second], 0) => format!("{first} and {second} are"),
            (all, 0) => {
                let (last, init) = all.split_last().expect("witness list is non-empty");
                format!("{}, and {last} are", init.join(", "))
            }
            (all, extra) => format!("{}, and {extra} more are", all.join(", ")),
        };

        let span = self.hir.span_of_expr(id);
        self.error(err_at(
            codes::T_NON_EXHAUSTIVE_MATCH,
            span,
            format!("non-exhaustive match: {listed} not covered"),
            "add arms for the missing cases or a `_` arm",
        ));
    }

    /// `catch` produces the subject's type, so every arm must unify with
    /// it (`docs/spec/04-errors.md`); in statement position the arm
    /// values are discarded like `match` arms (decision).
    ///
    /// The binding is re-typed per arm, mirroring the interpreter's
    /// per-arm re-bind: each arm narrows `e` to the arm's error type
    /// (`04-errors.md`, "`e` is bound with the arm's type") *before* the
    /// guard is checked, so guards see the narrowed binding just as they
    /// run after the type match at runtime. See
    /// [`Self::catch_arm_binding_type`] for the narrowing rules.
    fn check_catch(
        &mut self,
        id: ExprId,
        subject: ExprId,
        arms: &[CatchArm],
        expected: Option<&Type>,
        used: bool,
    ) -> Type {
        let subject_ty = self.check_expr(subject, expected);

        for (arm_index, arm) in arms.iter().enumerate() {
            if let Some(&local) = self.res.catch_bindings.get(&id) {
                let binding_ty = self.catch_arm_binding_type(id, arm_index, arm);
                self.tables.local_types.insert(local, binding_ty);
            }

            if let Some(guard) = arm.guard {
                self.check_expect(guard, &Type::Bool);
            }

            let body_ty = self.check_arm_body(&arm.body, Some(&subject_ty), used);
            if used && unify(&subject_ty, &body_ty).is_none() {
                let span = match &arm.body {
                    ArmBody::Expr(expr) => self.hir.span_of_expr(*expr),
                    ArmBody::Block(block) => block
                        .last()
                        .map(|&stmt| self.hir.span_of_stmt(stmt))
                        .unwrap_or_else(|| self.hir.span_of_expr(id)),
                };
                self.mismatch(span, &subject_ty, &body_ty);
            }
        }

        subject_ty
    }

    /// What the catch binding narrows to in one arm
    /// (`docs/spec/04-errors.md`, per-arm binding):
    ///
    /// - exactly one named type with a recorded resolution → that type
    ///   (structs/enums, or a primitive — the spec allows throwing
    ///   `string`/`int`/... values, so primitive arm types are
    ///   legitimate);
    /// - exactly one resolved panic name (`panics.X`, BRS-24) →
    ///   `string`: the runtime binds the panic's detail message
    ///   (`brasa_interp`'s `eval_catch`; `04-errors.md`);
    /// - exactly one resolved native-error name (`string.ParseError`,
    ///   BRS-41) → `string`, for the same reason: a native error
    ///   carries only a message, and the runtime binds it in the arm —
    ///   symmetric with panic arms;
    /// - a `|` group → `Unknown` (the spec binds "what's common to
    ///   both"; common-interface narrowing is deferred to error-set
    ///   work, BRS-22/23);
    /// - `_`, dotted names in namespaces that have not landed, and
    ///   unresolved names → `Unknown`.
    fn catch_arm_binding_type(&self, id: ExprId, arm_index: usize, arm: &CatchArm) -> Type {
        let [CatchType::Named { .. }] = arm.types.as_slice() else {
            return Type::Unknown;
        };

        if self.res.catch_arm_panics.contains_key(&(id, arm_index, 0))
            || self
                .res
                .catch_arm_native_errors
                .contains_key(&(id, arm_index, 0))
        {
            return Type::String;
        }

        match self.res.catch_arm_types.get(&(id, arm_index, 0)) {
            Some(&res) => self.catch_type_res_to_type(res),
            None => Type::Unknown,
        }
    }

    /// Converts a resolved arm type without a `TypeExprId` (arm types
    /// are written bare, so [`Self::conv`] does not apply). A generic
    /// struct/enum named bare in an arm has no arguments to convert;
    /// its declared generics become `Unknown` args (decision — user
    /// error types are typically non-generic). Anything that is not a
    /// throwable nominal type (interfaces, generic params, non-primitive
    /// builtins) leaves the binding `Unknown` rather than erroring:
    /// unreachable/invalid-arm checks need error sets (BRS-22/23).
    fn catch_type_res_to_type(&self, res: TypeRes) -> Type {
        match res {
            TypeRes::Item(item) => match self.hir.item(item) {
                Item::StructDef(def) => Type::Struct(item, vec![Type::Unknown; def.generics.len()]),
                Item::EnumDef(def) => Type::Enum(item, vec![Type::Unknown; def.generics.len()]),
                _ => Type::Unknown,
            },
            TypeRes::Builtin(builtin) => match builtin {
                BuiltinType::Int => Type::Int,
                BuiltinType::Float => Type::Float,
                BuiltinType::Bool => Type::Bool,
                BuiltinType::String => Type::String,
                BuiltinType::Char => Type::Char,
                BuiltinType::Unit => Type::Unit,
                _ => Type::Unknown,
            },
            TypeRes::GenericParam { .. } | TypeRes::SelfType => Type::Unknown,
        }
    }

    fn check_arm_body(&mut self, body: &ArmBody, expected: Option<&Type>, used: bool) -> Type {
        match body {
            ArmBody::Expr(expr) => self.check_value(*expr, expected, used),
            ArmBody::Block(block) => self.check_block(block, expected, used),
        }
    }

    fn arm_span(&self, pattern: PatternId, body: &ArmBody) -> Span {
        match body {
            ArmBody::Expr(expr) => self.hir.span_of_expr(*expr),
            ArmBody::Block(block) => block
                .last()
                .map(|&stmt| self.hir.span_of_stmt(stmt))
                .unwrap_or_else(|| self.hir.span_of_pattern(pattern)),
        }
    }

    // --- literals and constructors -------------------------------------

    /// Vector literals unify every element against the expected element
    /// type or, absent one, the first element (decision); an empty
    /// literal without context cannot infer its element type
    /// (`docs/spec/03-types.md`, inference rules).
    fn check_vector_lit(
        &mut self,
        id: ExprId,
        elements: &[ExprId],
        expected: Option<&Type>,
    ) -> Type {
        let mut elem: Option<Type> = match expected {
            Some(Type::Vector(inner)) => Some((**inner).clone()),
            _ => None,
        };

        if elements.is_empty() && elem.is_none() {
            let span = self.hir.span_of_expr(id);
            self.error(
                err_at(
                    codes::T_EMPTY_LITERAL_NO_TYPE,
                    span,
                    "cannot infer the element type of an empty vector literal".to_string(),
                    "no element to infer from",
                )
                .with_note("annotate the binding, e.g. `let xs: Vector<int> = []`".to_string()),
            );
            return Type::vector(Type::Unknown);
        }

        for &element in elements {
            match &elem {
                Some(expected_elem) => {
                    let expected_elem = expected_elem.clone();
                    let found = self.check_expr(element, Some(&expected_elem));
                    match unify(&expected_elem, &found) {
                        Some(joined) => elem = Some(joined),
                        None => {
                            let span = self.hir.span_of_expr(element);
                            self.mismatch(span, &expected_elem, &found);
                        }
                    }
                }
                None => elem = Some(self.check_expr(element, None)),
            }
        }

        Type::vector(elem.unwrap_or(Type::Unknown))
    }

    /// Map literals mirror vector literals: keys and values unify
    /// against the expected types or the first entry (decision).
    fn check_map_lit(
        &mut self,
        id: ExprId,
        entries: &[(ExprId, ExprId)],
        expected: Option<&Type>,
    ) -> Type {
        let (mut key, mut value): (Option<Type>, Option<Type>) = match expected {
            Some(Type::Map(k, v)) => (Some((**k).clone()), Some((**v).clone())),
            _ => (None, None),
        };

        if entries.is_empty() && key.is_none() {
            let span = self.hir.span_of_expr(id);
            self.error(
                err_at(
                    codes::T_EMPTY_LITERAL_NO_TYPE,
                    span,
                    "cannot infer the key and value types of an empty map literal".to_string(),
                    "no entry to infer from",
                )
                .with_note("annotate the binding, e.g. `let m: Map<string, int> = {}`".to_string()),
            );
            return Type::Map(Box::new(Type::Unknown), Box::new(Type::Unknown));
        }

        let key_from_entries = key.is_none();
        for &(entry_key, entry_value) in entries {
            key = Some(self.check_lit_slot(entry_key, key));
            value = Some(self.check_lit_slot(entry_value, value));
        }

        // An expected key type was already checked at its annotation;
        // only a key unified from the entries is established here.
        if key_from_entries
            && let Some(key_ty) = key.clone()
            && let Some(&(first_key, _)) = entries.first()
        {
            let key_span = self.hir.span_of_expr(first_key);
            self.check_key_hashable(key_span, &key_ty, KeyRole::MapKey);
        }

        Type::Map(
            Box::new(key.unwrap_or(Type::Unknown)),
            Box::new(value.unwrap_or(Type::Unknown)),
        )
    }

    /// Tuples are structural and positional: the literal's type is the
    /// tuple of its element types, with no unification across positions.
    ///
    /// An expected tuple type of the same arity propagates element-wise,
    /// so `let p: (int, Vector<int>) = (1, [])` infers the empty vector
    /// and a bad element is reported at that element. Any other
    /// expectation — an arity mismatch included — is ignored here: the
    /// elements are inferred on their own and the caller reports the one
    /// mismatch between the whole tuples, rather than a cascade of
    /// per-element errors against positions that do not correspond.
    fn check_tuple_lit(&mut self, elements: &[ExprId], expected: Option<&Type>) -> Type {
        let expected_elems = match expected {
            Some(Type::Tuple(tys)) if tys.len() == elements.len() => Some(tys.clone()),
            _ => None,
        };

        let found = elements
            .iter()
            .enumerate()
            .map(|(i, &element)| {
                let slot = expected_elems.as_ref().map(|tys| tys[i].clone());
                self.check_lit_slot(element, slot)
            })
            .collect();

        Type::Tuple(found)
    }

    /// Checks one literal element against the running unified type,
    /// reporting at the offending element and keeping the previous type
    /// on mismatch.
    fn check_lit_slot(&mut self, element: ExprId, current: Option<Type>) -> Type {
        match current {
            Some(expected) => {
                let found = self.check_expr(element, Some(&expected));
                match unify(&expected, &found) {
                    Some(joined) => joined,
                    None => {
                        let span = self.hir.span_of_expr(element);
                        self.mismatch(span, &expected, &found);
                        expected
                    }
                }
            }
            None => self.check_expr(element, None),
        }
    }

    /// Struct literals are order-independent and must provide every
    /// declared field exactly once (decision); unknown, duplicate, and
    /// missing fields are each errors, and field values check against
    /// the declared field types.
    ///
    /// For a generic struct there is no explicit instantiation syntax,
    /// so the arguments are inferred: first from the expected type, then
    /// by unifying the field values against the declared field types
    /// (`docs/spec/02-grammar.md`, no turbofish). Constraints are
    /// checked against the solved arguments right here, where the
    /// concrete types are known (`docs/spec/03-types.md`).
    fn check_struct_lit(
        &mut self,
        id: ExprId,
        type_name: &str,
        fields: &[(String, ExprId)],
        expected: Option<&Type>,
    ) -> Type {
        let span = self.hir.span_of_expr(id);

        let struct_item = match self.res.struct_lit_res.get(&id).copied() {
            Some(TypeRes::Item(item)) if matches!(self.hir.item(item), Item::StructDef(_)) => {
                Some(item)
            }
            None => None,
            Some(_) => {
                self.error(err_at(
                    codes::T_NOT_A_STRUCT,
                    span,
                    format!("`{type_name}` is not a struct"),
                    "only structs have literals",
                ));
                None
            }
        };

        let Some(item) = struct_item else {
            for &(_, value) in fields {
                self.check_expr(value, None);
            }
            return Type::Unknown;
        };

        let Item::StructDef(def) = self.hir.item(item) else {
            unreachable!("struct_item is only Some for StructDef items");
        };
        let declared: Vec<(String, TypeExprId)> =
            def.fields.iter().map(|f| (f.name.clone(), f.ty)).collect();
        let struct_display = def.name.clone();
        let generics_count = def.generics.len();

        let owner = DefRef::Item(item);
        let mut map: HashMap<(DefRef, usize), Type> = HashMap::new();
        let mut poisoned: HashSet<(DefRef, usize)> = HashSet::new();

        if let Some(Type::Struct(expected_item, expected_args)) = expected
            && *expected_item == item
            && expected_args.len() == generics_count
        {
            for (index, arg) in expected_args.iter().enumerate() {
                if !arg.is_flexible() {
                    map.insert((owner, index), arg.clone());
                }
            }
        }

        let mut seen: HashMap<&str, ()> = HashMap::new();
        for (name, value) in fields {
            let value_span = self.hir.span_of_expr(*value);

            match declared.iter().find(|(decl, _)| decl == name) {
                Some(&(_, field_ty)) => {
                    if seen.insert(name.as_str(), ()).is_some() {
                        self.error(err_at(
                            codes::T_STRUCT_LIT_DUPLICATE_FIELD,
                            value_span,
                            format!("duplicate field `{name}` in struct literal"),
                            "already provided",
                        ));
                    }

                    let field_ty = self.conv(field_ty);
                    let field_expected = substitute(&field_ty, &map);
                    let hint =
                        (!contains_generic_of(&field_expected, owner)).then_some(&field_expected);
                    let found = self.check_expr(*value, hint);

                    if !solve(&field_expected, &found, owner, &mut map, &mut poisoned) {
                        self.mismatch(value_span, &field_expected, &found);
                    }
                }
                None => {
                    self.error(err_at(
                        codes::T_STRUCT_LIT_UNKNOWN_FIELD,
                        value_span,
                        format!("unknown field `{name}` on struct `{struct_display}`"),
                        "not a declared field",
                    ));
                    self.check_expr(*value, None);
                }
            }
        }

        for (name, _) in &declared {
            if !seen.contains_key(name.as_str()) {
                self.error(err_at(
                    codes::T_STRUCT_LIT_MISSING_FIELD,
                    span,
                    format!("missing field `{name}` in struct literal of `{struct_display}`"),
                    "field not provided",
                ));
            }
        }

        self.finish_generic_solution(
            span,
            owner,
            &def.generics,
            &struct_display,
            &mut map,
            &poisoned,
        );
        self.check_constraints(span, owner, &def.generics, &map);

        let args = (0..generics_count)
            .map(|index| map.remove(&(owner, index)).unwrap_or(Type::Unknown))
            .collect();
        Type::Struct(item, args)
    }

    /// `Some(x)` is `Option<typeof x>`; a bare `None` takes the expected
    /// `Option<T>` when the context provides one and `Option<unknown>`
    /// otherwise (refined by BRS-19). Enum variants produce their enum's
    /// nominal type with payload arity and types checked.
    fn check_enum_ctor(&mut self, id: ExprId, args: &[ExprId], expected: Option<&Type>) -> Type {
        let span = self.hir.span_of_expr(id);

        match self.res.ctor_expr_res.get(&id).copied() {
            None => {
                for &arg in args {
                    self.check_expr(arg, None);
                }
                Type::Unknown
            }
            Some(CtorRes::OptionSome) => {
                if args.len() != 1 {
                    self.error(err_at(
                        codes::T_WRONG_ARG_COUNT,
                        span,
                        format!("`Some` takes exactly 1 argument, found {}", args.len()),
                        "expected 1 argument",
                    ));
                    for &arg in args {
                        self.check_expr(arg, None);
                    }
                    return Type::option(Type::Unknown);
                }

                let inner_expected = match expected {
                    Some(Type::Option(inner)) => Some((**inner).clone()),
                    _ => None,
                };
                let inner = match inner_expected {
                    Some(exp) => self.check_expect(args[0], &exp),
                    None => self.check_expr(args[0], None),
                };
                Type::option(inner)
            }
            Some(CtorRes::OptionNone) => {
                if !args.is_empty() {
                    self.error(err_at(
                        codes::T_WRONG_ARG_COUNT,
                        span,
                        "`None` takes no arguments".to_string(),
                        "unexpected arguments",
                    ));
                    for &arg in args {
                        self.check_expr(arg, None);
                    }
                }

                match expected {
                    Some(Type::Option(inner)) => Type::Option(inner.clone()),
                    _ => Type::option(Type::Unknown),
                }
            }
            Some(CtorRes::SetCtor) => self.check_set_ctor(span, args, expected),
            Some(CtorRes::EnumVariant {
                enum_item,
                variant_index,
            }) => {
                if let Item::EnumDef(def) = self.hir.item(enum_item)
                    && !def.generics.is_empty()
                {
                    return self.check_generic_variant(
                        span,
                        enum_item,
                        def,
                        variant_index,
                        args,
                        expected,
                    );
                }

                self.check_variant_args(span, enum_item, variant_index, args);
                Type::Enum(enum_item, vec![])
            }
        }
    }

    /// `Set(v)` builds a `Set<T>` from a `Vector<T>`
    /// (`docs/spec/01-syntax.md`, collection literals): exactly one
    /// argument, which must be a `Vector`. An expected `Set<T>` flows
    /// into the vector's element type.
    fn check_set_ctor(&mut self, span: Span, args: &[ExprId], expected: Option<&Type>) -> Type {
        if args.len() != 1 {
            self.error(err_at(
                codes::T_WRONG_ARG_COUNT,
                span,
                format!("`Set` takes exactly 1 argument, found {}", args.len()),
                "expected 1 argument (a `Vector`)",
            ));
            for &arg in args {
                self.check_expr(arg, None);
            }
            return Type::Set(Box::new(Type::Unknown));
        }

        let elem_expected = match expected {
            Some(Type::Set(elem)) => Some((**elem).clone()),
            _ => None,
        };
        let elem_inferred = elem_expected.is_none();
        let arg_ty = match elem_expected {
            Some(elem) => self.check_expect(args[0], &Type::vector(elem)),
            None => self.check_expr(args[0], None),
        };

        match arg_ty {
            Type::Vector(elem) => {
                // An expected element type was already checked at its
                // annotation; only an element inferred from the vector
                // argument is established here.
                if elem_inferred {
                    self.check_key_hashable(span, &elem, KeyRole::SetElement);
                }
                Type::Set(elem)
            }
            flexible if flexible.is_flexible() => Type::Set(Box::new(Type::Unknown)),
            other => {
                self.error(err_at(
                    codes::T_MISMATCHED_TYPES,
                    span,
                    format!(
                        "`Set` takes a `Vector`, found `{}`",
                        other.display(self.hir)
                    ),
                    "expected a `Vector` argument",
                ));
                Type::Set(Box::new(Type::Unknown))
            }
        }
    }

    /// A variant constructor of a generic enum infers the enum's type
    /// arguments like a generic call: from the expected type first, then
    /// from the payload values (`docs/spec/02-grammar.md`, no
    /// turbofish). Constraints are checked against the solved arguments.
    fn check_generic_variant(
        &mut self,
        span: Span,
        enum_item: ItemId,
        def: &'a EnumDef,
        variant_index: usize,
        args: &[ExprId],
        expected: Option<&Type>,
    ) -> Type {
        let owner = DefRef::Item(enum_item);
        let mut map: HashMap<(DefRef, usize), Type> = HashMap::new();
        let mut poisoned: HashSet<(DefRef, usize)> = HashSet::new();

        if let Some(Type::Enum(expected_item, expected_args)) = expected
            && *expected_item == enum_item
            && expected_args.len() == def.generics.len()
        {
            for (index, arg) in expected_args.iter().enumerate() {
                if !arg.is_flexible() {
                    map.insert((owner, index), arg.clone());
                }
            }
        }

        if let Some((variant_name, field_tys)) = self.variant(enum_item, variant_index) {
            if args.len() != field_tys.len() {
                self.error(err_at(
                    codes::T_WRONG_ARG_COUNT,
                    span,
                    format!(
                        "`{variant_name}` takes {} argument(s), found {}",
                        field_tys.len(),
                        args.len()
                    ),
                    &format!("expected {} argument(s)", field_tys.len()),
                ));
            }

            for (i, &arg) in args.iter().enumerate() {
                let Some(field_ty) = field_tys.get(i) else {
                    self.check_expr(arg, None);
                    continue;
                };

                let field_expected = substitute(field_ty, &map);
                let hint =
                    (!contains_generic_of(&field_expected, owner)).then_some(&field_expected);
                let found = self.check_expr(arg, hint);

                if !solve(&field_expected, &found, owner, &mut map, &mut poisoned) {
                    let arg_span = self.hir.span_of_expr(arg);
                    self.mismatch(arg_span, &field_expected, &found);
                }
            }
        } else {
            for &arg in args {
                self.check_expr(arg, None);
            }
        }

        self.finish_generic_solution(span, owner, &def.generics, &def.name, &mut map, &poisoned);
        self.check_constraints(span, owner, &def.generics, &map);

        let solved = (0..def.generics.len())
            .map(|index| map.remove(&(owner, index)).unwrap_or(Type::Unknown))
            .collect();
        Type::Enum(enum_item, solved)
    }

    fn check_variant_args(
        &mut self,
        span: Span,
        enum_item: ItemId,
        variant_index: usize,
        args: &[ExprId],
    ) {
        let variant = self.variant(enum_item, variant_index);
        let Some((variant_name, field_tys)) = variant else {
            for &arg in args {
                self.check_expr(arg, None);
            }
            return;
        };

        if args.len() != field_tys.len() {
            self.error(err_at(
                codes::T_WRONG_ARG_COUNT,
                span,
                format!(
                    "`{variant_name}` takes {} argument(s), found {}",
                    field_tys.len(),
                    args.len()
                ),
                &format!("expected {} argument(s)", field_tys.len()),
            ));
        }

        for (i, &arg) in args.iter().enumerate() {
            match field_tys.get(i) {
                Some(field_ty) => {
                    let field_ty = field_ty.clone();
                    self.check_expect(arg, &field_ty);
                }
                None => {
                    self.check_expr(arg, None);
                }
            }
        }
    }

    fn variant(&mut self, enum_item: ItemId, variant_index: usize) -> Option<(String, Vec<Type>)> {
        let Item::EnumDef(def) = self.hir.item(enum_item) else {
            return None;
        };
        let variant: &Variant = def.variants.get(variant_index)?;

        let field_tys = variant.fields.iter().map(|f| self.conv(f.ty)).collect();
        Some((variant.name.clone(), field_tys))
    }

    // --- patterns ------------------------------------------------------

    /// Checks a pattern against the scrutinee type where it is trivially
    /// known; a flexible scrutinee makes every binding `Unknown`.
    /// Exhaustiveness is BRS-18; this only types bindings and rejects
    /// shape mismatches.
    fn check_pattern(&mut self, id: PatternId, expected: &Type) {
        let hir = self.hir;
        let span = hir.span_of_pattern(id);

        match hir.pattern(id) {
            Pattern::Wildcard => {}
            Pattern::Literal(literal) => {
                let literal_ty = literal_type(literal);
                if unify(expected, &literal_ty).is_none() {
                    self.mismatch(span, expected, &literal_ty);
                }
            }
            Pattern::Binding(_) => {
                if let Some(&local) = self.res.pattern_locals.get(&id) {
                    self.tables.local_types.insert(local, expected.clone());
                }
            }
            Pattern::Ctor { args, .. } => {
                let args = args.clone();
                self.check_ctor_pattern(id, span, &args, expected);
            }
            Pattern::Tuple(elements) => {
                let elements = elements.clone();
                self.check_tuple_pattern(span, &elements, expected);
            }
        }
    }

    fn check_ctor_pattern(
        &mut self,
        id: PatternId,
        span: Span,
        args: &[PatternId],
        expected: &Type,
    ) {
        match self.res.ctor_pattern_res.get(&id).copied() {
            // Unresolved, and `Set` (which the resolver rejects in
            // pattern position, so it never lands here with a
            // resolution): bind the sub-patterns and move on.
            None | Some(CtorRes::SetCtor) => self.bind_patterns_unknown(args),
            Some(CtorRes::OptionSome) => {
                let inner = match expected {
                    Type::Option(inner) => (**inner).clone(),
                    flexible if flexible.is_flexible() => Type::Unknown,
                    other => {
                        self.error(err_at(
                            codes::T_PATTERN_TYPE_MISMATCH,
                            span,
                            format!(
                                "`Some` pattern does not match type `{}`",
                                other.display(self.hir)
                            ),
                            "the scrutinee is not an `Option`",
                        ));
                        Type::Unknown
                    }
                };

                if args.len() != 1 {
                    self.error(err_at(
                        codes::T_WRONG_ARG_COUNT,
                        span,
                        format!(
                            "`Some` pattern takes exactly 1 argument, found {}",
                            args.len()
                        ),
                        "expected 1 argument",
                    ));
                }
                for &arg in args {
                    self.check_pattern(arg, &inner);
                }
            }
            Some(CtorRes::OptionNone) => {
                if !matches!(expected, Type::Option(_)) && !expected.is_flexible() {
                    self.error(err_at(
                        codes::T_PATTERN_TYPE_MISMATCH,
                        span,
                        format!(
                            "`None` pattern does not match type `{}`",
                            expected.display(self.hir)
                        ),
                        "the scrutinee is not an `Option`",
                    ));
                }
                if !args.is_empty() {
                    self.error(err_at(
                        codes::T_WRONG_ARG_COUNT,
                        span,
                        "`None` pattern takes no arguments".to_string(),
                        "unexpected arguments",
                    ));
                    self.bind_patterns_unknown(args);
                }
            }
            Some(CtorRes::EnumVariant {
                enum_item,
                variant_index,
            }) => {
                let matches_scrutinee = match expected {
                    Type::Enum(item, _) => *item == enum_item,
                    flexible => flexible.is_flexible(),
                };
                if !matches_scrutinee {
                    self.error(err_at(
                        codes::T_PATTERN_TYPE_MISMATCH,
                        span,
                        format!(
                            "pattern for enum `{}` does not match type `{}`",
                            item_name(self.hir, enum_item),
                            expected.display(self.hir)
                        ),
                        "wrong scrutinee type",
                    ));
                }

                let Some((variant_name, field_tys)) = self.variant(enum_item, variant_index) else {
                    self.bind_patterns_unknown(args);
                    return;
                };

                // A generic scrutinee carries the enum's arguments, so
                // payload bindings type with them substituted in.
                let field_tys: Vec<Type> = match expected {
                    Type::Enum(item, scrutinee_args) if *item == enum_item => {
                        let owner = DefRef::Item(enum_item);
                        let map: HashMap<(DefRef, usize), Type> = scrutinee_args
                            .iter()
                            .enumerate()
                            .map(|(index, arg)| ((owner, index), arg.clone()))
                            .collect();
                        field_tys.iter().map(|ty| substitute(ty, &map)).collect()
                    }
                    _ => field_tys,
                };

                if args.len() != field_tys.len() {
                    self.error(err_at(
                        codes::T_WRONG_ARG_COUNT,
                        span,
                        format!(
                            "`{variant_name}` pattern takes {} argument(s), found {}",
                            field_tys.len(),
                            args.len()
                        ),
                        &format!("expected {} argument(s)", field_tys.len()),
                    ));
                }
                for (i, &arg) in args.iter().enumerate() {
                    let field_ty = field_tys.get(i).cloned().unwrap_or(Type::Unknown);
                    self.check_pattern(arg, &field_ty);
                }
            }
        }
    }

    fn check_tuple_pattern(&mut self, span: Span, elements: &[PatternId], expected: &Type) {
        match expected {
            Type::Tuple(tys) if tys.len() == elements.len() => {
                let tys = tys.clone();
                for (&element, ty) in elements.iter().zip(&tys) {
                    self.check_pattern(element, ty);
                }
            }
            Type::Tuple(tys) => {
                self.error(err_at(
                    codes::T_PATTERN_TYPE_MISMATCH,
                    span,
                    format!(
                        "tuple pattern has {} element(s), but the scrutinee has {}",
                        elements.len(),
                        tys.len()
                    ),
                    "wrong number of elements",
                ));
                self.bind_patterns_unknown(elements);
            }
            flexible if flexible.is_flexible() => self.bind_patterns_unknown(elements),
            other => {
                self.error(err_at(
                    codes::T_PATTERN_TYPE_MISMATCH,
                    span,
                    format!(
                        "tuple pattern does not match type `{}`",
                        other.display(self.hir)
                    ),
                    "the scrutinee is not a tuple",
                ));
                self.bind_patterns_unknown(elements);
            }
        }
    }

    fn bind_patterns_unknown(&mut self, patterns: &[PatternId]) {
        for &pattern in patterns {
            self.check_pattern(pattern, &Type::Unknown);
        }
    }

    // --- type expressions ----------------------------------------------

    /// Converts an annotation into a checker type using the resolver's
    /// `type_res` table; the resolver already reported unknown names.
    /// Generic parameters become rigid [`Type::Generic`]s. Interfaces in
    /// type position are an error: generic constraints are the only
    /// place interfaces are usable in v1 — there are no interface-typed
    /// values (`docs/spec/03-types.md`, structural interfaces).
    fn conv(&mut self, id: TypeExprId) -> Type {
        let hir = self.hir;

        match hir.type_expr(id) {
            TypeExpr::Named { args, .. } => match self.res.type_res.get(&id).copied() {
                Some(TypeRes::Builtin(builtin)) => self.conv_builtin(id, builtin, args),
                Some(TypeRes::Item(item)) => self.conv_item(id, item, args),
                Some(TypeRes::GenericParam { owner, index }) => Type::Generic { owner, index },
                Some(TypeRes::SelfType) => self.self_ty.clone().unwrap_or(Type::Unknown),
                None => Type::Unknown,
            },
            TypeExpr::Tuple(elements) => {
                Type::Tuple(elements.iter().map(|&e| self.conv(e)).collect())
            }
            TypeExpr::Fn { params, ret } => Type::Fn {
                params: params.iter().map(|&p| self.conv(p)).collect(),
                ret: Box::new(self.conv(*ret)),
            },
        }
    }

    /// Converts a user-defined nominal type reference, checking generic
    /// arity and the declared constraints against the given arguments —
    /// the annotation establishes concrete arguments just like a call
    /// or literal does (`docs/spec/03-types.md`, satisfaction at the
    /// use site). Interfaces are rejected here: they are only usable as
    /// generic constraints in v1 (`docs/spec/03-types.md`).
    fn conv_item(&mut self, id: TypeExprId, item: ItemId, args: &[TypeExprId]) -> Type {
        let hir = self.hir;

        let generics: &'a [GenericParam] = match hir.item(item) {
            Item::StructDef(def) => &def.generics,
            Item::EnumDef(def) => &def.generics,
            Item::InterfaceDef(_) => {
                self.report_interface_as_type(id);
                return Type::Unknown;
            }
            _ => return Type::Unknown,
        };
        let expected = generics.len();

        let conv_args: Vec<Type> = args.iter().map(|&a| self.conv(a)).collect();
        let span = self.hir.span_of_type_expr(id);
        if conv_args.len() != expected {
            let name = item_name(self.hir, item);
            self.error(err_at(
                codes::T_WRONG_TYPE_ARG_COUNT,
                span,
                format!(
                    "wrong number of type arguments for `{name}`: expected {expected}, found {}",
                    conv_args.len()
                ),
                &format!("expected {expected} type argument(s)"),
            ));
            return Type::Unknown;
        }

        let owner = DefRef::Item(item);
        let map: HashMap<(DefRef, usize), Type> = conv_args
            .iter()
            .enumerate()
            .map(|(index, arg)| ((owner, index), arg.clone()))
            .collect();
        self.check_constraints(span, owner, generics, &map);

        match self.hir.item(item) {
            Item::StructDef(_) => Type::Struct(item, conv_args),
            Item::EnumDef(_) => Type::Enum(item, conv_args),
            _ => unreachable!("only struct and enum items reach here"),
        }
    }

    fn conv_builtin(&mut self, id: TypeExprId, builtin: BuiltinType, args: &[TypeExprId]) -> Type {
        let arg =
            |this: &mut Self, i: usize| args.get(i).map(|&a| this.conv(a)).unwrap_or(Type::Unknown);

        match builtin {
            BuiltinType::Int => Type::Int,
            BuiltinType::Float => Type::Float,
            BuiltinType::Bool => Type::Bool,
            BuiltinType::String => Type::String,
            BuiltinType::Char => Type::Char,
            BuiltinType::Unit => Type::Unit,
            BuiltinType::Range => Type::Range,
            BuiltinType::Json => Type::Json,
            BuiltinType::Option => Type::option(arg(self, 0)),
            BuiltinType::Vector => Type::vector(arg(self, 0)),
            BuiltinType::Set => {
                let elem = arg(self, 0);
                if let Some(&elem_expr) = args.first() {
                    let elem_span = self.hir.span_of_type_expr(elem_expr);
                    self.check_key_hashable(elem_span, &elem, KeyRole::SetElement);
                }
                Type::Set(Box::new(elem))
            }
            BuiltinType::Map => {
                let key = arg(self, 0);
                let value = arg(self, 1);
                if let Some(&key_expr) = args.first() {
                    let key_span = self.hir.span_of_type_expr(key_expr);
                    self.check_key_hashable(key_span, &key, KeyRole::MapKey);
                }
                Type::Map(Box::new(key), Box::new(value))
            }
            BuiltinType::Comparable | BuiltinType::Printable | BuiltinType::Hashable => {
                self.report_interface_as_type(id);
                Type::Unknown
            }
        }
    }

    fn report_interface_as_type(&mut self, id: TypeExprId) {
        let span = self.hir.span_of_type_expr(id);
        self.error(err_at(
            codes::T_INTERFACE_AS_TYPE,
            span,
            "interfaces cannot be used as types in v1; use a generic constraint".to_string(),
            "interfaces only constrain generics",
        ));
    }
}

/// Structural unification that additionally binds unsolved generic
/// parameters of `owner` appearing on the expected side ("first solution
/// wins"). A flexible found type never binds; it marks the parameter
/// poisoned so [`Checker::finish_generic_solution`] skips the
/// cannot-infer report — the cause was already reported. Callers
/// substitute already-solved parameters before calling, so a bound
/// generic is only re-encountered when one parameter mentions it twice.
fn solve(
    expected: &Type,
    found: &Type,
    owner: DefRef,
    map: &mut HashMap<(DefRef, usize), Type>,
    poisoned: &mut HashSet<(DefRef, usize)>,
) -> bool {
    match (expected, found) {
        (Type::Generic { owner: o, index }, _) if *o == owner => {
            if let Some(bound) = map.get(&(owner, *index)) {
                return unify(bound, found).is_some();
            }
            if found.is_flexible() {
                poisoned.insert((owner, *index));
                return true;
            }

            map.insert((owner, *index), found.clone());
            true
        }
        (Type::Unknown | Type::Never, _) | (_, Type::Unknown | Type::Never) => true,
        (Type::Vector(x), Type::Vector(y))
        | (Type::Set(x), Type::Set(y))
        | (Type::Option(x), Type::Option(y)) => solve(x, y, owner, map, poisoned),
        (Type::Map(ka, va), Type::Map(kb, vb)) => {
            solve(ka, kb, owner, map, poisoned) && solve(va, vb, owner, map, poisoned)
        }
        (Type::Tuple(xs), Type::Tuple(ys)) => {
            xs.len() == ys.len()
                && xs
                    .iter()
                    .zip(ys)
                    .all(|(x, y)| solve(x, y, owner, map, poisoned))
        }
        (
            Type::Fn {
                params: pa,
                ret: ra,
            },
            Type::Fn {
                params: pb,
                ret: rb,
            },
        ) => {
            pa.len() == pb.len()
                && pa
                    .iter()
                    .zip(pb)
                    .all(|(x, y)| solve(x, y, owner, map, poisoned))
                && solve(ra, rb, owner, map, poisoned)
        }
        (Type::Struct(x, xa), Type::Struct(y, ya)) | (Type::Enum(x, xa), Type::Enum(y, ya)) => {
            x == y
                && xa.len() == ya.len()
                && xa
                    .iter()
                    .zip(ya)
                    .all(|(a, b)| solve(a, b, owner, map, poisoned))
        }
        _ => unify(expected, found).is_some(),
    }
}

/// Whether `ty` still mentions a generic parameter of `owner`. Used to
/// decide whether an expectation is safe to propagate into an argument:
/// a rigid unsolved `T` would spuriously fail to unify with literal
/// contents.
fn contains_generic_of(ty: &Type, owner: DefRef) -> bool {
    match ty {
        Type::Generic { owner: o, .. } => *o == owner,
        Type::Vector(elem) | Type::Set(elem) | Type::Option(elem) => {
            contains_generic_of(elem, owner)
        }
        Type::Map(key, value) => {
            contains_generic_of(key, owner) || contains_generic_of(value, owner)
        }
        Type::Tuple(elems) => elems.iter().any(|e| contains_generic_of(e, owner)),
        Type::Fn { params, ret } => {
            params.iter().any(|p| contains_generic_of(p, owner)) || contains_generic_of(ret, owner)
        }
        Type::Struct(_, args) | Type::Enum(_, args) => {
            args.iter().any(|a| contains_generic_of(a, owner))
        }
        _ => false,
    }
}

/// A builtin method signature as a plain function type, for signature
/// compatibility checks. `VectorOfFnRet` results depend on the argument,
/// so they compare as `Vector<unknown>` (which unifies with anything).
fn fn_of_sig(sig: MethodSig) -> Type {
    let ret = match sig.ret {
        RetRule::Fixed(ty) => ty,
        RetRule::VectorOfFnRet => Type::vector(Type::Unknown),
    };
    Type::func(sig.params, ret)
}

fn literal_type(literal: &Literal) -> Type {
    match literal {
        Literal::Int(_) => Type::Int,
        Literal::Float(_) => Type::Float,
        Literal::Bool(_) => Type::Bool,
        Literal::Char(_) => Type::Char,
        Literal::Str(_) => Type::String,
    }
}

fn op_str(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Add => "+",
        BinaryOp::Sub => "-",
        BinaryOp::Mul => "*",
        BinaryOp::Div => "/",
        BinaryOp::Rem => "%",
        BinaryOp::Pow => "**",
        BinaryOp::Eq => "==",
        BinaryOp::NotEq => "!=",
        BinaryOp::Lt => "<",
        BinaryOp::LtEq => "<=",
        BinaryOp::Gt => ">",
        BinaryOp::GtEq => ">=",
        BinaryOp::And => "&&",
        BinaryOp::Or => "||",
    }
}
