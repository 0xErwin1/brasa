//! Per-body collection: what one function, method, or lambda body
//! contributes to its own error-set, given the previous iteration's
//! sets for everything it calls.
//!
//! Call rules (`docs/spec/04-errors.md`, "Error-set inference" and
//! "Interaction with the rest of the language"):
//!
//! | Callee shape | Contribution |
//! |---|---|
//! | direct `FuncDef` item | that item's current set |
//! | declared struct method | that method's current set |
//! | `puts` / `print` | nothing (`docs/spec/05-stdlib.md`: print any value) |
//! | stdlib module member (`fs.read(...)`, `math.sqrt(...)`) | nothing — native, no errors in M2; `string.toInt` stays `Option` until BRS-41 |
//! | builtin container/primitive method | the sets of literal lambda arguments (a HOF invokes its function argument — the lambda's set "flows to whoever invokes" it); a non-literal fn-typed argument opens the set |
//! | immediately-invoked lambda literal | that lambda's set |
//! | anything else (local, parameter, `TopLet`, struct field, or generic receiver holding a function) | opens the set — indirect calls are unknowable until BRS-25's per-call-site precision |
//!
//! A lambda literal in any other position contributes nothing to the
//! enclosing body: its set is computed into
//! [`Collector::lambda_sets`] and only flows where it is invoked.
//! Argument evaluation always contributes (an argument expression may
//! itself throw), regardless of what the call rule says about the
//! callee.

use std::collections::HashMap;

use brasa_diagnostics::Diagnostic;
use brasa_hir::{
    ArmBody, Block, CatchArm, CatchType, Expr, ExprId, Hir, IfNode, Item, LambdaBody, Stmt, StmtId,
};
use brasa_resolver::{BuiltinType, DefRef, Res, Resolutions, TypeRes};
use brasa_typeck::{Type, TypeTables};

use crate::{ErrorSet, ErrorTag, Primitive, check};

pub(crate) struct Collector<'a> {
    pub hir: &'a Hir,
    pub res: &'a Resolutions,
    pub types: &'a TypeTables,
    /// Previous-iteration sets for every function and method.
    pub sets: &'a HashMap<DefRef, ErrorSet>,
    /// This-iteration lambda sets, filled as lambda literals are walked.
    pub lambda_sets: &'a mut HashMap<ExprId, ErrorSet>,
    /// `Some` only during the post-convergence checking pass: each
    /// `catch` then runs the BRS-23 arm checks against its subject's
    /// contribution set, which only exists transiently inside
    /// [`Collector::catch`]. The fixpoint iterations pass `None`.
    pub diagnostics: Option<&'a mut Vec<Diagnostic>>,
}

impl<'a> Collector<'a> {
    pub(crate) fn block(&mut self, block: &Block) -> ErrorSet {
        let mut set = ErrorSet::default();
        for &stmt in block {
            set.union_with(&self.stmt(stmt));
        }
        set
    }

    fn stmt(&mut self, id: StmtId) -> ErrorSet {
        match self.hir.stmt(id) {
            Stmt::Let(let_stmt) => self.expr(let_stmt.value),
            Stmt::Assign { target, value } => {
                let mut set = self.expr(*target);
                set.union_with(&self.expr(*value));
                set
            }
            Stmt::Return(value) => value.map(|v| self.expr(v)).unwrap_or_default(),
            Stmt::Break | Stmt::Continue => ErrorSet::default(),
            Stmt::Throw(value) => self.throw(*value),
            Stmt::If(node) => self.if_node(node),
            Stmt::While { cond, body } => {
                let mut set = self.expr(*cond);
                set.union_with(&self.block(body));
                set
            }
            // Patterns bind, they never evaluate, so only the iterable
            // and the body contribute.
            Stmt::For { iterable, body, .. } => {
                let mut set = self.expr(*iterable);
                set.union_with(&self.block(body));
                set
            }
            Stmt::Expr(expr) => self.expr(*expr),
        }
    }

    /// `throw e` contributes the evaluation of `e` plus the tag of its
    /// checked type: structs and enums tag nominally, primitives tag as
    /// [`Primitive`], and anything else — `Unknown` from a deferred
    /// construct, a container, a fn value — opens the set, because a
    /// tag the checker cannot name cannot be subtracted or matched.
    fn throw(&mut self, value: ExprId) -> ErrorSet {
        let mut set = self.expr(value);

        match tag_of(self.types.expr_types.get(&value)) {
            Some(tag) => {
                set.tags.insert(tag);
            }
            None => set.open = true,
        }

        set
    }

    fn if_node(&mut self, node: &IfNode) -> ErrorSet {
        let mut set = ErrorSet::default();
        for (cond, body) in &node.branches {
            set.union_with(&self.expr(*cond));
            set.union_with(&self.block(body));
        }
        if let Some(else_) = &node.else_ {
            set.union_with(&self.block(else_));
        }
        set
    }

    fn expr(&mut self, id: ExprId) -> ErrorSet {
        match self.hir.expr(id) {
            Expr::Int(_)
            | Expr::Float(_)
            | Expr::Bool(_)
            | Expr::Char(_)
            | Expr::Unit
            | Expr::Str(_)
            | Expr::Ident(_)
            | Expr::SelfExpr => ErrorSet::default(),
            Expr::Call { callee, args } => self.call(*callee, args),
            // A field read cannot throw; index, unary, and binary
            // operations only panic (IndexOutOfBounds, DivisionByZero,
            // IntegerOverflow), and panics are not in error-sets
            // (`docs/spec/04-errors.md`, "Panics vs errors").
            Expr::Field { recv, .. } => self.expr(*recv),
            Expr::Index { recv, index } => {
                let mut set = self.expr(*recv);
                set.union_with(&self.expr(*index));
                set
            }
            Expr::Unary { operand, .. } => self.expr(*operand),
            Expr::Binary { lhs, rhs, .. } => {
                let mut set = self.expr(*lhs);
                set.union_with(&self.expr(*rhs));
                set
            }
            Expr::OptionWrap(inner) | Expr::ToString(inner) => self.expr(*inner),
            Expr::Lambda { .. } => {
                self.lambda(id);
                ErrorSet::default()
            }
            Expr::If(node) => self.if_node(node),
            Expr::Match { scrutinee, arms } => {
                let mut set = self.expr(*scrutinee);
                for arm in arms {
                    if let Some(guard) = arm.guard {
                        set.union_with(&self.expr(guard));
                    }
                    set.union_with(&self.arm_body(&arm.body));
                }
                set
            }
            Expr::VectorLit(elems) => {
                let mut set = ErrorSet::default();
                for &elem in elems {
                    set.union_with(&self.expr(elem));
                }
                set
            }
            Expr::MapLit(entries) => {
                let mut set = ErrorSet::default();
                for &(key, value) in entries {
                    set.union_with(&self.expr(key));
                    set.union_with(&self.expr(value));
                }
                set
            }
            Expr::StructLit { fields, .. } => {
                let mut set = ErrorSet::default();
                for &(_, value) in fields {
                    set.union_with(&self.expr(value));
                }
                set
            }
            Expr::Range { lo, hi, .. } => {
                let mut set = self.expr(*lo);
                set.union_with(&self.expr(*hi));
                set
            }
            Expr::Catch {
                subject,
                exhaustive,
                arms,
                ..
            } => self.catch(id, *subject, *exhaustive, arms),
            Expr::EnumCtor { args, .. } => {
                let mut set = ErrorSet::default();
                for &arg in args {
                    set.union_with(&self.expr(arg));
                }
                set
            }
        }
    }

    fn arm_body(&mut self, body: &ArmBody) -> ErrorSet {
        match body {
            ArmBody::Expr(expr) => self.expr(*expr),
            ArmBody::Block(block) => self.block(block),
        }
    }

    /// Computes the body set of the lambda literal at `id`, records it
    /// in [`Self::lambda_sets`], and returns it. The set does not leak
    /// into the enclosing function unless the lambda is invoked there
    /// (`docs/spec/04-errors.md`: "their error-set flows to whoever
    /// invokes them").
    fn lambda(&mut self, id: ExprId) -> ErrorSet {
        let Expr::Lambda { body, .. } = self.hir.expr(id) else {
            return ErrorSet::default();
        };

        let set = match body {
            LambdaBody::Expr(expr) => self.expr(*expr),
            LambdaBody::Block(block) => self.block(block),
        };

        self.lambda_sets.insert(id, set.clone());
        set
    }

    fn call(&mut self, callee: ExprId, args: &[ExprId]) -> ErrorSet {
        match self.hir.expr(callee) {
            Expr::Field { recv, name } => self.method_call(*recv, name, args),
            Expr::Ident(_) => {
                let mut set = self.args(args);
                set.union_with(&self.ident_callee(callee));
                set
            }
            // An immediately-invoked lambda literal: its set flows to
            // this invocation site.
            Expr::Lambda { .. } => {
                let mut set = self.args(args);
                set.union_with(&self.lambda(callee));
                set
            }
            // Any other callee shape (a call returning a function, an
            // indexed fn value, ...) is an indirect call.
            _ => {
                let mut set = self.expr(callee);
                set.union_with(&self.args(args));
                set.open = true;
                set
            }
        }
    }

    /// The callee contribution of a direct `name(...)` call; see the
    /// module-level rules table.
    fn ident_callee(&mut self, callee: ExprId) -> ErrorSet {
        match self.res.expr_res.get(&callee) {
            Some(Res::Builtin(_) | Res::Module(_)) => ErrorSet::default(),
            Some(&Res::Item(item)) => match self.hir.item(item) {
                Item::FuncDef(_) => self
                    .sets
                    .get(&DefRef::Item(item))
                    .cloned()
                    .unwrap_or_default(),
                // A `TopLet` holding a function value: indirect.
                _ => ErrorSet::open(),
            },
            Some(Res::Local(_) | Res::SelfParam) | None => ErrorSet::open(),
        }
    }

    fn method_call(&mut self, recv: ExprId, name: &str, args: &[ExprId]) -> ErrorSet {
        if self.is_module_ref(recv) {
            // Stdlib module members are native and throw nothing in M2
            // (`docs/spec/05-stdlib.md`); their signatures close in M4
            // (BRS-41), when throwing members become [`ErrorTag`]s.
            return self.args(args);
        }

        let mut set = self.expr(recv);

        match self.types.expr_types.get(&recv) {
            Some(Type::Struct(item, _)) => {
                let item = *item;
                set.union_with(&self.args(args));
                set.union_with(&self.struct_method(item, name));
            }
            // Builtin receivers: primitives, containers, ranges,
            // options, tuples, and enums (whose only member is the
            // derived `toString`). Builtin methods themselves throw
            // nothing in M2; only function arguments they may invoke
            // contribute (HOF transparency).
            Some(
                Type::Int
                | Type::Float
                | Type::Bool
                | Type::String
                | Type::Char
                | Type::Unit
                | Type::Range
                | Type::Vector(_)
                | Type::Map(_, _)
                | Type::Set(_)
                | Type::Option(_)
                | Type::Tuple(_)
                | Type::Enum(_, _),
            ) => set.union_with(&self.hof_args(args)),
            // A generic receiver dispatches through its constraint
            // (indirect until BRS-25's per-call-site inheritance), and
            // an unknown/absent receiver type is unknowable.
            Some(Type::Fn { .. } | Type::Generic { .. } | Type::Unknown | Type::Never) | None => {
                set.union_with(&self.args(args));
                set.open = true;
            }
        }

        set
    }

    /// The callee contribution of `recv.name(...)` on a struct: a
    /// declared method uses its [`DefRef::Method`] set; the universal
    /// derived `toString` (`docs/spec/03-types.md`) throws nothing; any
    /// other member is a field holding a function — an indirect call.
    fn struct_method(&mut self, item: brasa_hir::ItemId, name: &str) -> ErrorSet {
        let Item::StructDef(def) = self.hir.item(item) else {
            return ErrorSet::open();
        };

        match def.methods.iter().position(|m| m.name == name) {
            Some(index) => self
                .sets
                .get(&DefRef::Method { owner: item, index })
                .cloned()
                .unwrap_or_default(),
            None if name == "toString" => ErrorSet::default(),
            None => ErrorSet::open(),
        }
    }

    /// Plain argument evaluation: every argument expression contributes
    /// (a lambda literal argument contributes nothing itself — its set
    /// is recorded and flows only where it is invoked).
    fn args(&mut self, args: &[ExprId]) -> ErrorSet {
        let mut set = ErrorSet::default();
        for &arg in args {
            set.union_with(&self.expr(arg));
        }
        set
    }

    /// Arguments of a builtin higher-order method (`map`, `filter`,
    /// `each`, `sortBy`, ...): a literal lambda argument adds its set —
    /// the HOF invokes it — and a non-literal fn-typed argument opens
    /// the set (which function it is cannot be known here).
    fn hof_args(&mut self, args: &[ExprId]) -> ErrorSet {
        let mut set = ErrorSet::default();

        for &arg in args {
            if matches!(self.hir.expr(arg), Expr::Lambda { .. }) {
                set.union_with(&self.lambda(arg));
                continue;
            }

            set.union_with(&self.expr(arg));
            if matches!(self.types.expr_types.get(&arg), Some(Type::Fn { .. })) {
                set.open = true;
            }
        }

        set
    }

    /// `subject catch (e) arms`: the SUBJECT's contributions are
    /// filtered by the unguarded arms, then guards and arm bodies add
    /// their own contributions (guards run, and an arm may rethrow or
    /// wrap — `docs/spec/04-errors.md`, "Re-throwing with wrapping is a
    /// normal `throw` inside the arm").
    ///
    /// Subtraction rules:
    /// - an unguarded arm naming `T` subtracts `T`'s tag;
    /// - an unguarded `_` subtracts everything AND closes openness —
    ///   `_` catches every remaining error (never panics), so nothing
    ///   unknowable escapes either (decision recorded here);
    /// - a GUARDED arm subtracts nothing: the guard may be false, the
    ///   same rule exhaustiveness uses;
    /// - dotted (`panics.X`, unresolved until M4) and unresolved arm
    ///   names subtract nothing.
    ///
    /// `catch_all` filters identically: exhaustiveness enforcement is a
    /// check on the subject's contribution set, not a set
    /// transformation, and runs here only during the checking pass —
    /// the subject set exists nowhere else.
    fn catch(
        &mut self,
        id: ExprId,
        subject: ExprId,
        exhaustive: bool,
        arms: &[CatchArm],
    ) -> ErrorSet {
        let mut set = self.expr(subject);

        if let Some(diagnostics) = self.diagnostics.as_deref_mut() {
            check::catch_expr(self.hir, self.res, id, exhaustive, arms, &set, diagnostics);
        }

        for (arm_index, arm) in arms.iter().enumerate() {
            if arm.guard.is_some() {
                continue;
            }

            for (type_index, catch_type) in arm.types.iter().enumerate() {
                match catch_type {
                    CatchType::Wildcard => {
                        set.tags.clear();
                        set.open = false;
                    }
                    CatchType::Named { .. } => {
                        let res = self.res.catch_arm_types.get(&(id, arm_index, type_index));
                        if let Some(tag) = res.and_then(|&res| caught_tag(self.hir, res)) {
                            set.tags.remove(&tag);
                        }
                    }
                }
            }
        }

        for arm in arms {
            if let Some(guard) = arm.guard {
                set.union_with(&self.expr(guard));
            }
            set.union_with(&self.arm_body(&arm.body));
        }

        set
    }

    fn is_module_ref(&self, expr: ExprId) -> bool {
        matches!(self.hir.expr(expr), Expr::Ident(_))
            && matches!(self.res.expr_res.get(&expr), Some(Res::Module(_)))
    }
}

/// The tag of a thrown value's checked type; `None` means the set must
/// open (see [`Collector::throw`]).
fn tag_of(ty: Option<&Type>) -> Option<ErrorTag> {
    match ty? {
        Type::Struct(item, _) | Type::Enum(item, _) => Some(ErrorTag::Item(*item)),
        Type::Int => Some(ErrorTag::Primitive(Primitive::Int)),
        Type::Float => Some(ErrorTag::Primitive(Primitive::Float)),
        Type::Bool => Some(ErrorTag::Primitive(Primitive::Bool)),
        Type::String => Some(ErrorTag::Primitive(Primitive::String)),
        Type::Char => Some(ErrorTag::Primitive(Primitive::Char)),
        Type::Unit => Some(ErrorTag::Primitive(Primitive::Unit)),
        _ => None,
    }
}

/// The tag an unguarded `catch` arm naming a resolved type subtracts —
/// and, symmetrically, the tag a `throws` declaration names. Only
/// throwable nominals and primitives map; interfaces, generic
/// parameters, `Self`, and non-primitive builtins subtract nothing.
pub(crate) fn caught_tag(hir: &Hir, res: TypeRes) -> Option<ErrorTag> {
    match res {
        TypeRes::Item(item) => match hir.item(item) {
            Item::StructDef(_) | Item::EnumDef(_) => Some(ErrorTag::Item(item)),
            _ => None,
        },
        TypeRes::Builtin(builtin) => match builtin {
            BuiltinType::Int => Some(ErrorTag::Primitive(Primitive::Int)),
            BuiltinType::Float => Some(ErrorTag::Primitive(Primitive::Float)),
            BuiltinType::Bool => Some(ErrorTag::Primitive(Primitive::Bool)),
            BuiltinType::String => Some(ErrorTag::Primitive(Primitive::String)),
            BuiltinType::Char => Some(ErrorTag::Primitive(Primitive::Char)),
            BuiltinType::Unit => Some(ErrorTag::Primitive(Primitive::Unit)),
            _ => None,
        },
        TypeRes::GenericParam { .. } | TypeRes::SelfType => None,
    }
}
