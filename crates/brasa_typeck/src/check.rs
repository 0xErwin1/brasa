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

use std::collections::HashMap;

use brasa_diagnostics::{Diagnostic, Severity};
use brasa_hir::{
    ArmBody, BinaryOp, CatchArm, Expr, ExprId, FuncDef, Hir, IfNode, Item, ItemId, LambdaBody,
    LambdaParam, Literal, MatchArm, Pattern, PatternId, Stmt, StmtId, TypeExpr, TypeExprId,
    UnaryOp, Variant,
};
use brasa_resolver::{CtorRes, DefRef, Res, Resolutions, TypeRes};
use brasa_source::Span;

use crate::TypeTables;
use crate::builtins::{self, MethodSig, RetRule};
use crate::types::{Type, WrapDecision, item_name, unify};

fn err(span: Span, message: String) -> Diagnostic {
    Diagnostic::new(Severity::Error, message, "BRS-TYPECK".to_string(), span)
}

fn err_at(span: Span, message: String, label: &str) -> Diagnostic {
    err(span, message).with_label(span, label.to_string())
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

struct Checker<'a> {
    hir: &'a Hir,
    res: &'a Resolutions,
    tables: TypeTables,
    diagnostics: Vec<Diagnostic>,
    /// The enclosing function's return type; `None` in top-level code,
    /// where `return` is an error.
    ret_ty: Option<Type>,
    /// The enclosing method's receiver type.
    self_ty: Option<Type>,
}

pub(crate) fn run(hir: &Hir, roots: &[ItemId], res: &Resolutions) -> (TypeTables, Vec<Diagnostic>) {
    let mut checker = Checker {
        hir,
        res,
        tables: TypeTables::default(),
        diagnostics: Vec::new(),
        ret_ty: None,
        self_ty: None,
    };

    checker.collect_signatures(roots);
    checker.check_top_lets(roots);
    checker.check_bodies(roots);

    (checker.tables, checker.diagnostics)
}

impl<'a> Checker<'a> {
    fn error(&mut self, diag: Diagnostic) {
        self.diagnostics.push(diag);
    }

    fn mismatch(&mut self, span: Span, expected: &Type, found: &Type) {
        let expected = expected.display(self.hir);
        let found = found.display(self.hir);
        self.error(err_at(
            span,
            format!("mismatched types: expected `{expected}`, found `{found}`"),
            &format!("expected `{expected}`"),
        ));
    }

    // --- module passes -------------------------------------------------

    /// Function signatures come straight from annotations, so they exist
    /// before any body is checked (the local-inference boundary,
    /// `docs/spec/03-types.md`). Generic parameters become `Unknown`
    /// (BRS-17), so generic calls still check arity but not constraints.
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
                    for (index, method) in def.methods.iter().enumerate() {
                        self.check_func(
                            DefRef::Method { owner: root, index },
                            method,
                            Some(Type::Struct(root)),
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
    fn func_sig(&self, func: &FuncDef) -> Type {
        let params = func
            .params
            .iter()
            .filter_map(|param| match param {
                brasa_hir::Param::SelfParam => None,
                brasa_hir::Param::Named { ty, .. } => Some(self.conv(*ty)),
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
                self.check_expect(value, &target_ty);
            }
            _ => {
                self.error(err_at(
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
                    span,
                    "cannot assign to `self`".to_string(),
                    "not assignable",
                ));
                self.check_expr(value, None);
            }
            Some(Res::Builtin(_) | Res::Module(_)) => {
                self.error(err_at(
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
                self.check_match(scrutinee, &arms, expected, used)
            }
            Expr::VectorLit(elements) => {
                let elements = elements.clone();
                self.check_vector_lit(id, &elements, expected)
            }
            Expr::MapLit(entries) => {
                let entries = entries.clone();
                self.check_map_lit(id, &entries, expected)
            }
            Expr::StructLit { type_name, fields } => {
                let (type_name, fields) = (type_name.clone(), fields.clone());
                self.check_struct_lit(id, &type_name, &fields)
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

    fn check_method_call(
        &mut self,
        span: Span,
        callee: ExprId,
        recv: ExprId,
        name: &str,
        args: &[ExprId],
    ) -> Type {
        if self.is_module_ref(recv) {
            // Stdlib module members close in M4; stay silent until then.
            self.check_expr(recv, None);
            self.tables.expr_types.insert(callee, Type::Unknown);
            for &arg in args {
                self.check_expr(arg, None);
            }
            return Type::Unknown;
        }

        let recv_ty = self.check_expr(recv, None);
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

    fn check_field(&mut self, id: ExprId, recv: ExprId, name: &str) -> Type {
        if self.is_module_ref(recv) {
            self.check_expr(recv, None);
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

    fn lookup_member(&self, recv: &Type, name: &str) -> Member {
        if recv.is_flexible() {
            return Member::Deferred;
        }

        if let Type::Struct(item) = recv {
            return self.lookup_struct_member(*item, name);
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

    fn lookup_struct_member(&self, item: ItemId, name: &str) -> Member {
        let Item::StructDef(def) = self.hir.item(item) else {
            return Member::Deferred;
        };

        if let Some(field) = def.fields.iter().find(|f| f.name == name) {
            return Member::Value(self.conv(field.ty));
        }
        if let Some(method) = def.methods.iter().find(|m| m.name == name) {
            return Member::Value(self.func_sig(method));
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

    fn report_missing_member(&mut self, span: Span, recv: &Type, name: &str) {
        let shown = recv.display(self.hir);

        if let Type::Struct(_) = recv {
            self.error(err_at(
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
                span,
                format!("`join` requires `Vector<string>`, found `{shown}`"),
                "element type is not `string`",
            ));
            return;
        }

        self.error(err_at(
            span,
            format!("no method `{name}` on `{shown}`"),
            "unknown method",
        ));
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
            Type::String => {
                self.check_expr(index, None);
                let span = self.hir.span_of_expr(recv);
                self.error(
                    err_at(
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
                    span,
                    format!("cannot index `{}`", other.display(self.hir)),
                    "not indexable",
                ));
                Type::Unknown
            }
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

    /// `< <= > >=` order `int`, `float`, `string`, and `char`
    /// (`docs/spec/03-types.md`, operator table). `T: Comparable`
    /// generics are BRS-17; their `Unknown` operands pass silently.
    fn check_ordering(&mut self, id: ExprId, op: BinaryOp, lhs: ExprId, rhs: ExprId) -> Type {
        let lt = self.check_expr(lhs, None);
        let rt = self.check_expr(rhs, None);

        if lt.is_flexible() || rt.is_flexible() {
            return Type::Bool;
        }

        let span = self.hir.span_of_expr(id);
        if lt != rt {
            self.error(err_at(
                span,
                format!(
                    "cannot compare `{}` and `{}` with `{}`",
                    lt.display(self.hir),
                    rt.display(self.hir),
                    op_str(op)
                ),
                "both sides must have the same type",
            ));
        } else if !matches!(lt, Type::Int | Type::Float | Type::String | Type::Char) {
            self.error(err_at(
                span,
                format!(
                    "`{}` does not support ordering with `{}`",
                    lt.display(self.hir),
                    op_str(op)
                ),
                "only `int`, `float`, `string`, and `char` are ordered",
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
    /// (`docs/spec/03-types.md`). Exhaustiveness is BRS-18.
    fn check_match(
        &mut self,
        scrutinee: ExprId,
        arms: &[MatchArm],
        expected: Option<&Type>,
        used: bool,
    ) -> Type {
        let scrutinee_ty = self.check_expr(scrutinee, None);

        let mut acc = Type::Never;
        for arm in arms {
            self.check_pattern(arm.pattern, &scrutinee_ty);
            if let Some(guard) = arm.guard {
                self.check_expect(guard, &Type::Bool);
            }

            let body_ty = self.check_arm_body(&arm.body, expected, used);
            if !used {
                continue;
            }

            match unify(&acc, &body_ty) {
                Some(joined) => acc = joined,
                None => {
                    let span = self.arm_span(arm.pattern, &arm.body);
                    self.error(err_at(
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

        if used { acc } else { Type::Unit }
    }

    /// `catch` produces the subject's type, so every arm must unify with
    /// it (`docs/spec/04-errors.md`); the binding stays `Unknown` until
    /// error-set inference (M2). Decision: in statement position the
    /// arm values are discarded like `match` arms.
    fn check_catch(
        &mut self,
        id: ExprId,
        subject: ExprId,
        arms: &[CatchArm],
        expected: Option<&Type>,
        used: bool,
    ) -> Type {
        let subject_ty = self.check_expr(subject, expected);

        if let Some(&local) = self.res.catch_bindings.get(&id) {
            self.tables.local_types.insert(local, Type::Unknown);
        }

        for arm in arms {
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
                    span,
                    "cannot infer the key and value types of an empty map literal".to_string(),
                    "no entry to infer from",
                )
                .with_note("annotate the binding, e.g. `let m: Map<string, int> = {}`".to_string()),
            );
            return Type::Map(Box::new(Type::Unknown), Box::new(Type::Unknown));
        }

        for &(entry_key, entry_value) in entries {
            key = Some(self.check_lit_slot(entry_key, key));
            value = Some(self.check_lit_slot(entry_value, value));
        }

        Type::Map(
            Box::new(key.unwrap_or(Type::Unknown)),
            Box::new(value.unwrap_or(Type::Unknown)),
        )
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
    fn check_struct_lit(
        &mut self,
        id: ExprId,
        type_name: &str,
        fields: &[(String, ExprId)],
    ) -> Type {
        let span = self.hir.span_of_expr(id);

        let struct_item = match self.res.struct_lit_res.get(&id).copied() {
            Some(TypeRes::Item(item)) if matches!(self.hir.item(item), Item::StructDef(_)) => {
                Some(item)
            }
            None => None,
            Some(_) => {
                self.error(err_at(
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

        let mut seen: HashMap<&str, ()> = HashMap::new();
        for (name, value) in fields {
            let value_span = self.hir.span_of_expr(*value);

            match declared.iter().find(|(decl, _)| decl == name) {
                Some(&(_, field_ty)) => {
                    if seen.insert(name.as_str(), ()).is_some() {
                        self.error(err_at(
                            value_span,
                            format!("duplicate field `{name}` in struct literal"),
                            "already provided",
                        ));
                    }
                    let field_ty = self.conv(field_ty);
                    self.check_expect(*value, &field_ty);
                }
                None => {
                    self.error(err_at(
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
                    span,
                    format!("missing field `{name}` in struct literal of `{struct_display}`"),
                    "field not provided",
                ));
            }
        }

        Type::Struct(item)
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
            Some(CtorRes::EnumVariant {
                enum_item,
                variant_index,
            }) => {
                self.check_variant_args(span, enum_item, variant_index, args);
                Type::Enum(enum_item)
            }
        }
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

    fn variant(&self, enum_item: ItemId, variant_index: usize) -> Option<(String, Vec<Type>)> {
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
            None => self.bind_patterns_unknown(args),
            Some(CtorRes::OptionSome) => {
                let inner = match expected {
                    Type::Option(inner) => (**inner).clone(),
                    flexible if flexible.is_flexible() => Type::Unknown,
                    other => {
                        self.error(err_at(
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
                    Type::Enum(item) => *item == enum_item,
                    flexible => flexible.is_flexible(),
                };
                if !matches_scrutinee {
                    self.error(err_at(
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

                if args.len() != field_tys.len() {
                    self.error(err_at(
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
    /// `type_res` table. Generic parameters and interfaces become
    /// `Unknown` (BRS-17: no constraint checking, no interface-typed
    /// values in v1); the resolver already reported unknown names.
    fn conv(&self, id: TypeExprId) -> Type {
        let hir = self.hir;

        match hir.type_expr(id) {
            TypeExpr::Named { args, .. } => match self.res.type_res.get(&id).copied() {
                Some(TypeRes::Builtin(builtin)) => self.conv_builtin(builtin, args),
                Some(TypeRes::Item(item)) => match hir.item(item) {
                    Item::StructDef(_) => Type::Struct(item),
                    Item::EnumDef(_) => Type::Enum(item),
                    _ => Type::Unknown,
                },
                Some(TypeRes::GenericParam { .. }) => Type::Unknown,
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

    fn conv_builtin(&self, builtin: brasa_resolver::BuiltinType, args: &[TypeExprId]) -> Type {
        use brasa_resolver::BuiltinType;

        let arg = |i: usize| args.get(i).map(|&a| self.conv(a)).unwrap_or(Type::Unknown);

        match builtin {
            BuiltinType::Int => Type::Int,
            BuiltinType::Float => Type::Float,
            BuiltinType::Bool => Type::Bool,
            BuiltinType::String => Type::String,
            BuiltinType::Char => Type::Char,
            BuiltinType::Unit => Type::Unit,
            BuiltinType::Range => Type::Range,
            BuiltinType::Option => Type::option(arg(0)),
            BuiltinType::Vector => Type::vector(arg(0)),
            BuiltinType::Set => Type::Set(Box::new(arg(0))),
            BuiltinType::Map => Type::Map(Box::new(arg(0)), Box::new(arg(1))),
            BuiltinType::Comparable | BuiltinType::Printable | BuiltinType::Hashable => {
                Type::Unknown
            }
        }
    }
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
