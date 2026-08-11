//! Free-variable analysis for lambdas: which enclosing locals (and
//! whether `self`) a closure must snapshot at creation time.
//!
//! The free set is every `LocalId` referenced anywhere in the lambda
//! body — nested lambdas included, since an inner capture must be
//! visible in the enclosing frame to be re-captured — minus every
//! `LocalId` bound inside the body (parameters, `let`s, pattern and
//! `catch` bindings, nested lambda parameters). `LocalId`s are globally
//! unique, so one referenced/bound pair of sets suffices.
//!
//! Capture order contract (see the crate docs): `self` first when used,
//! then the free locals in ascending `LocalId` order.

use std::collections::HashSet;

use brasa_hir::{
    ArmBody, Block, Expr, ExprId, Hir, IfNode, LambdaBody, Pattern, PatternId, Stmt, StmtId,
};
use brasa_resolver::{LocalId, Res, Resolutions};

pub(crate) struct Captures {
    pub(crate) uses_self: bool,
    /// Free locals in ascending `LocalId` order.
    pub(crate) locals: Vec<LocalId>,
}

pub(crate) fn lambda_captures(hir: &Hir, res: &Resolutions, lambda: ExprId) -> Captures {
    let Expr::Lambda { body, .. } = hir.expr(lambda) else {
        return Captures {
            uses_self: false,
            locals: Vec::new(),
        };
    };

    let mut walker = Walker {
        hir,
        res,
        referenced: HashSet::new(),
        bound: HashSet::new(),
        uses_self: false,
    };

    if let Some(params) = res.lambda_params.get(&lambda) {
        walker.bound.extend(params.iter().copied());
    }
    match body {
        LambdaBody::Expr(expr) => walker.expr(*expr),
        LambdaBody::Block(block) => walker.block(block),
    }

    let mut locals: Vec<LocalId> = walker
        .referenced
        .difference(&walker.bound)
        .copied()
        .collect();
    locals.sort_by_key(|local| local.0);

    Captures {
        uses_self: walker.uses_self,
        locals,
    }
}

struct Walker<'a> {
    hir: &'a Hir,
    res: &'a Resolutions,
    referenced: HashSet<LocalId>,
    bound: HashSet<LocalId>,
    uses_self: bool,
}

impl Walker<'_> {
    fn block(&mut self, block: &Block) {
        for &stmt in block {
            self.stmt(stmt);
        }
    }

    fn stmt(&mut self, id: StmtId) {
        match self.hir.stmt(id) {
            Stmt::Let(let_stmt) => {
                if let Some(&local) = self.res.stmt_locals.get(&id) {
                    self.bound.insert(local);
                }
                self.expr(let_stmt.value);
            }
            Stmt::Assign { target, value } => {
                self.expr(*target);
                self.expr(*value);
            }
            Stmt::Return(value) => {
                if let Some(value) = value {
                    self.expr(*value);
                }
            }
            Stmt::Break | Stmt::Continue => {}
            Stmt::Throw(value) => self.expr(*value),
            Stmt::If(node) => self.if_node(node),
            Stmt::While { cond, body } => {
                self.expr(*cond);
                self.block(body);
            }
            Stmt::For {
                pattern,
                iterable,
                body,
            } => {
                self.pattern(*pattern);
                self.expr(*iterable);
                self.block(body);
            }
            Stmt::Expr(expr) => self.expr(*expr),
        }
    }

    fn expr(&mut self, id: ExprId) {
        match self.hir.expr(id) {
            Expr::Int(_)
            | Expr::Float(_)
            | Expr::Bool(_)
            | Expr::Char(_)
            | Expr::Unit
            | Expr::Str(_) => {}
            Expr::Ident(_) => match self.res.expr_res.get(&id) {
                Some(Res::Local(local)) => {
                    self.referenced.insert(*local);
                }
                Some(Res::SelfParam) => self.uses_self = true,
                _ => {}
            },
            Expr::SelfExpr => self.uses_self = true,
            Expr::Call { callee, args } => {
                self.expr(*callee);
                for &arg in args {
                    self.expr(arg);
                }
            }
            Expr::Field { recv, .. } => self.expr(*recv),
            Expr::Index { recv, index } => {
                self.expr(*recv);
                self.expr(*index);
            }
            Expr::Unary { operand, .. } => self.expr(*operand),
            Expr::Binary { lhs, rhs, .. } => {
                self.expr(*lhs);
                self.expr(*rhs);
            }
            Expr::OptionWrap(inner) | Expr::ToString(inner) => self.expr(*inner),
            Expr::Lambda { body, .. } => {
                if let Some(params) = self.res.lambda_params.get(&id) {
                    self.bound.extend(params.iter().copied());
                }
                match body {
                    LambdaBody::Expr(expr) => self.expr(*expr),
                    LambdaBody::Block(block) => self.block(block),
                }
            }
            Expr::If(node) => self.if_node(node),
            Expr::Match { scrutinee, arms } => {
                self.expr(*scrutinee);
                for arm in arms {
                    self.pattern(arm.pattern);
                    if let Some(guard) = arm.guard {
                        self.expr(guard);
                    }
                    self.arm_body(&arm.body);
                }
            }
            Expr::VectorLit(elements) => {
                for &element in elements {
                    self.expr(element);
                }
            }
            Expr::MapLit(pairs) => {
                for &(key, value) in pairs {
                    self.expr(key);
                    self.expr(value);
                }
            }
            Expr::TupleLit(elements) => {
                for &element in elements {
                    self.expr(element);
                }
            }
            Expr::StructLit { fields, .. } => {
                for &(_, value) in fields {
                    self.expr(value);
                }
            }
            Expr::Range { lo, hi, .. } => {
                self.expr(*lo);
                self.expr(*hi);
            }
            Expr::Catch { subject, arms, .. } => {
                if let Some(&local) = self.res.catch_bindings.get(&id) {
                    self.bound.insert(local);
                }
                self.expr(*subject);
                for arm in arms {
                    if let Some(guard) = arm.guard {
                        self.expr(guard);
                    }
                    self.arm_body(&arm.body);
                }
            }
            Expr::EnumCtor { args, .. } => {
                for &arg in args {
                    self.expr(arg);
                }
            }
        }
    }

    fn if_node(&mut self, node: &IfNode) {
        for (cond, block) in &node.branches {
            self.expr(*cond);
            self.block(block);
        }
        if let Some(block) = &node.else_ {
            self.block(block);
        }
    }

    fn arm_body(&mut self, body: &ArmBody) {
        match body {
            ArmBody::Expr(expr) => self.expr(*expr),
            ArmBody::Block(block) => self.block(block),
        }
    }

    fn pattern(&mut self, id: PatternId) {
        match self.hir.pattern(id) {
            Pattern::Wildcard | Pattern::Literal(_) => {}
            Pattern::Binding(_) => {
                if let Some(&local) = self.res.pattern_locals.get(&id) {
                    self.bound.insert(local);
                }
            }
            Pattern::Ctor { args, .. } => {
                for &arg in args {
                    self.pattern(arg);
                }
            }
            Pattern::Tuple(elements) => {
                for &element in elements {
                    self.pattern(element);
                }
            }
        }
    }
}
