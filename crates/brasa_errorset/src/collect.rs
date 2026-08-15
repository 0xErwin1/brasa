//! Per-body collection: what one function, method, or lambda body
//! contributes to its own error-set, given the previous iteration's
//! sets for everything it calls.
//!
//! Call rules (spec: 04 — Sistema de errores, "Error-set inference" and
//! "Interaction with the rest of the language"):
//!
//! | Callee shape | Contribution |
//! |---|---|
//! | direct `FuncDef` item | that item's current set |
//! | declared struct method | that method's current set |
//! | `puts` / `print` | nothing (spec: 05 — Stdlib de scripting: print any value) |
//! | any `std::` module member | whatever `brasa_typeck::builtins::module_throws` says it raises — declared once, beside the member's signature (BRS-96) |
//! | any builtin METHOD | whatever `brasa_typeck::builtins::method_throws` says, from the same declaration (BRS-96). Today that is `string.toInt`/`toFloat` raising `string.ParseError` (BRS-41) and the regex four raising `string.RegexError` (BRS-31); this file no longer names them |
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
    ArmBody, Block, CatchArm, CatchType, Expr, ExprId, Hir, IfNode, Item, ItemId, LambdaBody, Stmt,
    StmtId,
};
use brasa_resolver::{BuiltinType, BuiltinValue, DefRef, Res, Resolutions, TypeRes};
use brasa_typeck::{Type, TypeTables};

use crate::{ErrorSet, ErrorTag, Primitive, check};

/// Where a `Task` value came from, so a `value()` read can charge the
/// spawned block's errors at the READ as well as at the spawn
/// (BRS-136).
///
/// Charging only the spawn site was correct for the enclosing body —
/// the scope rethrows every unread failure, so the total is the same
/// either way — but it left the read's own subject set empty, which
/// made a named `catch` arm around `t.value()` unreachable (E001) and
/// forced `_`. This records the block's set against the spawn call and
/// against a local a `let` bound it to; anything less direct (a task
/// in a vector, returned from a function, reassigned) stays as it was
/// and contributes nothing extra, so the change only ever ADDS
/// precision.
#[derive(Default)]
pub(crate) struct TaskOrigins {
    by_call: HashMap<ExprId, ErrorSet>,
    by_local: HashMap<brasa_resolver::LocalId, ErrorSet>,
}

pub(crate) struct Collector<'a> {
    pub hir: &'a Hir,
    pub res: &'a Resolutions,
    pub types: &'a TypeTables,
    /// Previous-iteration sets for every function and method.
    pub sets: &'a HashMap<DefRef, ErrorSet>,
    /// This-iteration lambda sets, filled as lambda literals are walked.
    pub lambda_sets: &'a mut HashMap<ExprId, ErrorSet>,
    /// Per-body: which spawned block each reachable `Task` value came
    /// from ([`TaskOrigins`]).
    pub tasks: TaskOrigins,
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

    /// The pseudo-body of top-level code: every `TopLet` initializer
    /// and `Item::Stmt` block under `roots`, in source order, collected
    /// as one body. The top level declares no `throws` contract, so the
    /// returned set is allowed to be non-empty (an uncaught top-level
    /// throw ends the script with exit 70 at runtime); collecting it
    /// exists so `catch`/`catch!` expressions in top-level code get
    /// the same E001/E002/E003 checks as any function body.
    pub(crate) fn top_level(&mut self, roots: &[ItemId]) -> ErrorSet {
        let mut set = ErrorSet::default();

        for &item in roots {
            match self.hir.item(item) {
                Item::TopLet(top_let) => set.union_with(&self.expr(top_let.let_stmt.value)),
                Item::Stmt(block) => set.union_with(&self.block(block)),
                _ => {}
            }
        }

        set
    }

    fn stmt(&mut self, id: StmtId) -> ErrorSet {
        match self.hir.stmt(id) {
            Stmt::Let(let_stmt) => {
                let set = self.expr(let_stmt.value);

                // `let t = scope.spawn do ... end` — carry the block's
                // set from the call onto the binding, so a later
                // `t.value()` can charge it.
                if let Some(spawned) = self.tasks.by_call.get(&let_stmt.value).cloned()
                    && let Some(&local) = self.res.stmt_locals.get(&id)
                {
                    self.tasks.by_local.insert(local, spawned);
                }

                set
            }
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
            Expr::Call { callee, args } => self.call(id, *callee, args),
            // A field read cannot throw; index, unary, and binary
            // operations only panic (IndexOutOfBounds, DivisionByZero,
            // IntegerOverflow), and panics are not in error-sets
            // (spec: 04 — Sistema de errores, "Panics vs errors").
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
            Expr::TupleLit(elems) => {
                let mut set = ErrorSet::default();
                for &elem in elems {
                    set.union_with(&self.expr(elem));
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
    /// (spec: 04 — Sistema de errores: "their error-set flows to whoever
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

    fn call(&mut self, id: ExprId, callee: ExprId, args: &[ExprId]) -> ErrorSet {
        // `mod.f(...)` on an imported file module resolved to `f`'s
        // item, so its declared or inferred set is the callee's — the
        // call graph the fixpoint walks crosses files exactly here.
        if matches!(self.res.expr_res.get(&callee), Some(Res::Item(_))) {
            let mut set = self.args(args);
            set.union_with(&self.ident_callee(callee));
            return set;
        }

        match self.hir.expr(callee) {
            Expr::Field { recv, name } => self.method_call(id, *recv, name, args),
            // `concurrent(lambda)` is a builtin HOF in free-call
            // position (BRS-133): the scope body runs inside the call,
            // so its literal-lambda set flows here exactly as a
            // `Vector.map` argument's does. Plain `args()` would record
            // the lambda's set and drop it — a body that throws would
            // escape `concurrent` invisibly. The pinned BRS-25 rule is
            // untouched: it is about USER-defined callees, and
            // `concurrent` is a builtin.
            Expr::Ident(_)
                if matches!(
                    self.res.expr_res.get(&callee),
                    Some(Res::Builtin(BuiltinValue::Concurrent))
                ) =>
            {
                self.hof_args(args)
            }
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

    fn method_call(&mut self, id: ExprId, recv: ExprId, name: &str, args: &[ExprId]) -> ErrorSet {
        if self.is_module_ref(recv) {
            // The throwing module members whose signatures have closed
            // (spec: 05 — Stdlib de scripting). BRS-32: the `proc` runners
            // raise `proc.NonZeroExit` on a non-zero exit and
            // `proc.SpawnError` when the child cannot start. BRS-33:
            // the filesystem-touching `fs` members and `env.cd` raise
            // the three `fs` errors; `fs.abs` and `env.cwd` only
            // `fs.IoError` (an unreadable current directory); the
            // predicates and pure path helpers never throw. Members of
            // still-open modules are native and throw nothing until
            // their signatures close during M4.
            let mut set = self.args(args);

            if let Some(module) = self.std_module_of(recv) {
                for name in brasa_typeck::builtins::module_throws(&module, name) {
                    set.tags.insert(ErrorTag::Opaque(name));
                }
            }

            return set;
        }

        let mut set = self.expr(recv);

        match self.types.expr_types.get(&recv) {
            Some(Type::Struct(item, _)) => {
                let item = *item;
                set.union_with(&self.args(args));
                set.union_with(&self.struct_method(item, name));
            }
            // `scope.spawn do ... end` (BRS-133): the block's set is
            // recorded against this call so a later `value()` read can
            // charge it too (BRS-136). It is STILL charged here as
            // well — the scope rethrows every unread failure before
            // `concurrent` returns, so an unread task's errors escape
            // whether or not anyone reads it.
            Some(Type::ConcurrentScope) if name == "spawn" => {
                if let [block] = args
                    && matches!(self.hir.expr(*block), Expr::Lambda { .. })
                {
                    self.lambda(*block);
                    if let Some(spawned) = self.lambda_sets.get(block).cloned() {
                        self.tasks.by_call.insert(id, spawned);
                    }
                }

                set.union_with(&self.hof_args(args));
                for error in brasa_typeck::builtins::method_throws(&Type::ConcurrentScope, name) {
                    set.tags.insert(ErrorTag::Opaque(error));
                }
            }
            // `t.value()` (BRS-136): the read rethrows what the block
            // threw, so the arms around it are checked against that set
            // rather than against nothing. Untraceable receivers keep
            // contributing nothing — the enclosing body already carries
            // the set from the spawn site, so this only sharpens the
            // subject of a `catch`, never widens a `throws`.
            Some(Type::Task(_)) if name == "value" => {
                if let Some(spawned) = self.task_origin(recv) {
                    set.union_with(&spawned);
                }
            }
            // The throwing builtin methods, from the same declaration
            // their signatures come from (`brasa_stdlib`, BRS-96):
            // `string.toInt`/`toFloat` raise `string.ParseError`
            // (BRS-41), the regex four raise `string.RegexError`
            // (BRS-31), and `Scope.spawn` raises
            // `concurrent.ScopeExited` (BRS-133).
            //
            // This used to be two hand-written arms here, a table away
            // from the signatures they belong to. A method that started
            // throwing had to be remembered in both places, and
            // forgetting this one made `throws never` verifiable over a
            // body that throws.
            //
            // Arguments go through `hof_args`, not `args`: `spawn`'s
            // literal lambda is INVOKED by the concurrency machinery,
            // so its set must flow at this call site — the spawn site
            // is where the lambda is syntactically present, and the
            // scope rethrows every unread failure before `concurrent`
            // returns, so the enclosing set is identical whether the
            // block's errors are charged here or at `value()`. For the
            // string methods the two are the same function: none takes
            // a fn-typed argument.
            Some(recv) if !brasa_typeck::builtins::method_throws(recv, name).is_empty() => {
                let recv = recv.clone();
                set.union_with(&self.hof_args(args));

                for error in brasa_typeck::builtins::method_throws(&recv, name) {
                    set.tags.insert(ErrorTag::Opaque(error));
                }
            }
            // Builtin receivers: primitives, containers, ranges,
            // options, tuples, enums (whose only member is the derived
            // `toString`), the `proc` `Output`, `http` `Response`,
            // `fs` `Walk` and `Stat`, and `NativeError` records (fields plus
            // `toString` only, and `Response::header`, which never
            // throws), and `Json` (the `as*`
            // accessors and `null?` never throw — BRS-34). Every other builtin method
            // throws nothing in M2; only function arguments they may
            // invoke contribute (HOF transparency).
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
                | Type::Enum(_, _)
                | Type::ProcOutput
                | Type::HttpResponse
                | Type::CliArgs
                | Type::Walk
                | Type::Stat
                | Type::NativeError
                | Type::ProcNonZeroExit
                | Type::Json
                | Type::ConcurrentScope
                | Type::Task(_),
            ) => set.union_with(&self.hof_args(args)),
            // A generic receiver dispatches through its constraint, and
            // the constraint's own declaration bounds what that can
            // throw (BRS-141). Charging the declared set needs the
            // bound to be TRUE, which is what `E008` enforces for a
            // struct candidate and `T027` for a builtin one — without
            // both, this would infer `throws never` over a body that
            // throws. The concrete method is still unknown and does not
            // need to be: an upper bound is what inference wants.
            Some(Type::Generic { owner, index }) => {
                let (owner, index) = (*owner, *index);
                set.union_with(&self.args(args));

                match self.constraint_member_throws(owner, index, name) {
                    Some(tags) => set.tags.extend(tags),
                    None => set.open = true,
                }
            }
            // An unknown/absent receiver type is unknowable.
            Some(Type::Fn { .. } | Type::Unknown | Type::Never) | None => {
                set.union_with(&self.args(args));
                set.open = true;
            }
        }

        set
    }

    /// The callee contribution of `recv.name(...)` on a struct: a
    /// declared method uses its [`DefRef::Method`] set; the universal
    /// derived `toString` (spec: 03 — Sistema de tipos) throws nothing; any
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
    ///
    /// Pinned BRS-25 behavior: a throwing lambda literal passed to a
    /// user-defined function (`apply(|x: int| boom(x), n)`) tags
    /// NEITHER side. The callee invokes it through a parameter, which
    /// opens the callee's set, and the caller inherits that openness —
    /// the literal's known set never narrows it. Per-call-site
    /// inheritance of literal argument sets is the BRS-25 precision
    /// gap; the literal-lambda special case in [`Self::hof_args`]
    /// exists only for builtin HOF methods.
    /// The set of the block behind a `Task`-valued expression, when it
    /// is reachable: the spawn call itself, or a local a `let` bound
    /// straight to one.
    fn task_origin(&self, recv: ExprId) -> Option<ErrorSet> {
        if let Some(set) = self.tasks.by_call.get(&recv) {
            return Some(set.clone());
        }

        match self.res.expr_res.get(&recv) {
            Some(&Res::Local(local)) => self.tasks.by_local.get(&local).cloned(),
            _ => None,
        }
    }

    /// What a call through a constrained generic receiver charges: the
    /// tags the interface member declares (BRS-141).
    ///
    /// `None` means nothing bounds the call and the set opens, as it
    /// always did — a builtin constraint, an inline one (anonymous, so
    /// the resolver has nowhere to record its contracts), a member the
    /// constraint does not declare, or a member with no `throws` clause
    /// at all. A missing clause promises nothing, so a satisfying
    /// method may throw anything and the caller must assume it does;
    /// `throws never` promises the most and charges nothing.
    fn constraint_member_throws(
        &self,
        owner: DefRef,
        index: usize,
        name: &str,
    ) -> Option<std::collections::BTreeSet<ErrorTag>> {
        let TypeRes::Item(iface) = self.res.constraint_res.get(&(owner, index)).copied()? else {
            return None;
        };
        let Item::InterfaceDef(def) = self.hir.item(iface) else {
            return None;
        };

        let member_index = def.methods.iter().position(|m| m.name == name)?;
        let member = &def.methods[member_index];

        match &member.throws {
            None => None,
            Some(brasa_hir::Throws::Never) => Some(std::collections::BTreeSet::new()),
            Some(brasa_hir::Throws::Types(names)) => Some(
                (0..names.len())
                    .filter_map(|name_index| {
                        iface_member_tag(self.hir, self.res, iface, member_index, name_index)
                    })
                    .collect(),
            ),
        }
    }

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
    /// wrap — spec: 04 — Sistema de errores, "Re-throwing with wrapping is a
    /// normal `throw` inside the arm").
    ///
    /// Subtraction rules:
    /// - an unguarded arm naming `T` subtracts `T`'s tag;
    /// - an unguarded arm naming a native error (`string.ParseError`,
    ///   recorded in `catch_arm_native_errors`) subtracts its `Opaque`
    ///   tag — native errors are ordinary errors, unlike panics;
    /// - an unguarded `_` subtracts everything AND closes openness —
    ///   `_` catches every remaining error (never panics), so nothing
    ///   unknowable escapes either (decision recorded here);
    /// - a GUARDED arm subtracts nothing: the guard may be false, the
    ///   same rule exhaustiveness uses;
    /// - panic arms (`panics.X`, recorded in `catch_arm_panics`, which
    ///   this pass never reads) subtract nothing: panics are not in
    ///   error-sets (spec: 04 — Sistema de errores);
    /// - dotted names in namespaces that have not landed and
    ///   unresolved arm names subtract nothing.
    ///
    /// `catch!` filters identically: exhaustiveness enforcement is a
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
                    CatchType::Wildcard { .. } => {
                        set.tags.clear();
                        set.open = false;
                    }
                    CatchType::Named { .. } => {
                        if let Some(tag) = arm_tag(self.hir, self.res, (id, arm_index, type_index))
                        {
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

    /// The module name of a `Res::Module` receiver when it is a
    /// `std::` import; `None` for file imports.
    fn std_module_of(&self, recv: ExprId) -> Option<String> {
        let Some(&Res::Module(item)) = self.res.expr_res.get(&recv) else {
            return None;
        };
        let Item::Import(import) = self.hir.item(item) else {
            return None;
        };

        import.path.std_module().map(str::to_string)
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

/// The tag one named `catch` arm slot subtracts (and, in the checks,
/// matches against the subject set): a native-error name maps to its
/// `Opaque` tag, a resolved type name through [`caught_tag`]. Panic
/// slots live in `catch_arm_panics`, which neither table here covers,
/// so they map to nothing — panics are not in error-sets.
pub(crate) fn arm_tag(
    hir: &Hir,
    res: &Resolutions,
    key: (ExprId, usize, usize),
) -> Option<ErrorTag> {
    if let Some(&native) = res.catch_arm_native_errors.get(&key) {
        return Some(ErrorTag::Opaque(native));
    }

    res.catch_arm_types
        .get(&key)
        .and_then(|&type_res| caught_tag(hir, type_res))
}

/// The tag one name of a declared `throws` list contributes to the
/// contract, by the declaring definition and the name's index. The
/// `throws` twin of [`arm_tag`], and separate for the same reason: a
/// native error resolves to no `TypeRes`, so its canonical name is
/// recorded in its own table.
pub(crate) fn throws_tag(hir: &Hir, res: &Resolutions, key: (DefRef, usize)) -> Option<ErrorTag> {
    if let Some(&native) = res.throws_native_errors.get(&key) {
        return Some(ErrorTag::Opaque(native));
    }

    res.throws_types
        .get(&key.0)
        .and_then(|declared| declared.get(key.1).copied().flatten())
        .and_then(|type_res| caught_tag(hir, type_res))
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

/// The tag one name of an interface member's `throws` clause stands
/// for — [`crate::collect::throws_tag`]'s twin over the resolver's
/// interface tables, which are keyed by the member's position rather
/// than by a `DefRef` an interface member does not have.
pub(crate) fn iface_member_tag(
    hir: &Hir,
    res: &Resolutions,
    iface: brasa_hir::ItemId,
    member: usize,
    name: usize,
) -> Option<ErrorTag> {
    if let Some(&native) = res.iface_member_throws_natives.get(&(iface, member, name)) {
        return Some(ErrorTag::Opaque(native));
    }

    res.iface_member_throws
        .get(&(iface, member))
        .and_then(|declared| declared.get(name).copied().flatten())
        .and_then(|type_res| caught_tag(hir, type_res))
}
