//! Tree depth of AST nodes, recorded as a side table while the tree is
//! built.
//!
//! Every later phase (lowering, resolution, type checking, error-set
//! inference, code generation, and the tree-walker) descends this tree
//! with real Rust recursion, so the depth recorded here is the bound on
//! how deep those recursions can go. The parser's own recursion counter
//! cannot serve that purpose: a left-leaning chain like `1 + 1 + 1 + ...`
//! is built by a Pratt *loop*, so it costs the parser a constant number
//! of frames while producing an arbitrarily deep tree.
//!
//! Depth is computed at allocation time, in constant work per node: a
//! node's children always exist before the node itself, so their depths
//! are already in the side table.

use crate::expr::ArmBody;
use crate::{Ast, Expr, ExprId, LambdaBody, MatchArm, Stmt, StmtId, StringPart};

/// The depth of an expression node: one more than its deepest child.
pub(crate) fn of_expr(ast: &Ast, expr: &Expr) -> u32 {
    let children = match expr {
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Bool(_)
        | Expr::Char(_)
        | Expr::Unit
        | Expr::Ident(_)
        | Expr::SelfExpr => 0,

        Expr::StringLit { parts } => parts
            .iter()
            .map(|part| match part {
                StringPart::Text { .. } => 0,
                StringPart::Interp(id) => ast.expr_depth(*id),
            })
            .max()
            .unwrap_or(0),

        Expr::Call { callee, args } => ast.expr_depth(*callee).max(max_expr(ast, args)),
        Expr::Field { recv, .. } => ast.expr_depth(*recv),
        Expr::SafeNav { recv, args, .. } => ast
            .expr_depth(*recv)
            .max(args.as_deref().map_or(0, |args| max_expr(ast, args))),
        Expr::Index { recv, index } => ast.expr_depth(*recv).max(ast.expr_depth(*index)),
        Expr::Unary { operand, .. } => ast.expr_depth(*operand),
        Expr::Binary { lhs, rhs, .. }
        | Expr::Pipe { lhs, target: rhs }
        | Expr::Coalesce { lhs, rhs } => ast.expr_depth(*lhs).max(ast.expr_depth(*rhs)),

        Expr::Lambda { body, .. } => match body {
            LambdaBody::Expr(id) => ast.expr_depth(*id),
            LambdaBody::Block(block) => max_stmt(ast, block),
        },

        Expr::If(node) => node
            .branches
            .iter()
            .map(|(cond, block)| ast.expr_depth(*cond).max(max_stmt(ast, block)))
            .max()
            .unwrap_or(0)
            .max(node.else_.as_deref().map_or(0, |b| max_stmt(ast, b))),

        Expr::Match { scrutinee, arms } => ast.expr_depth(*scrutinee).max(max_arms(ast, arms)),

        Expr::VectorLit(elements)
        | Expr::TupleLit(elements)
        | Expr::EnumCtor { args: elements, .. } => max_expr(ast, elements),
        Expr::MapLit(pairs) => pairs
            .iter()
            .map(|(k, v)| ast.expr_depth(*k).max(ast.expr_depth(*v)))
            .max()
            .unwrap_or(0),
        Expr::StructLit { fields, .. } => fields
            .iter()
            .map(|(_, value)| ast.expr_depth(*value))
            .max()
            .unwrap_or(0),

        Expr::Range { lo, hi, .. } => ast.expr_depth(*lo).max(ast.expr_depth(*hi)),

        Expr::Catch { subject, arms, .. } => ast.expr_depth(*subject).max(
            arms.iter()
                .map(|arm| {
                    arm.guard
                        .map_or(0, |g| ast.expr_depth(g))
                        .max(of_arm_body(ast, &arm.body))
                })
                .max()
                .unwrap_or(0),
        ),
    };

    children + 1
}

/// The depth of a statement node: one more than its deepest child.
pub(crate) fn of_stmt(ast: &Ast, stmt: &Stmt) -> u32 {
    let children = match stmt {
        Stmt::Break | Stmt::Continue => 0,
        Stmt::Let(let_stmt) => ast.expr_depth(let_stmt.value),
        Stmt::Assign { target, value, .. } => ast.expr_depth(*target).max(ast.expr_depth(*value)),
        Stmt::Return(value) => value.map_or(0, |id| ast.expr_depth(id)),
        Stmt::Throw(value) | Stmt::Expr(value) => ast.expr_depth(*value),
        Stmt::If(node) => node
            .branches
            .iter()
            .map(|(cond, block)| ast.expr_depth(*cond).max(max_stmt(ast, block)))
            .max()
            .unwrap_or(0)
            .max(node.else_.as_deref().map_or(0, |b| max_stmt(ast, b))),
        Stmt::While { cond, body } => ast.expr_depth(*cond).max(max_stmt(ast, body)),
        Stmt::For { iterable, body, .. } => ast.expr_depth(*iterable).max(max_stmt(ast, body)),
    };

    children + 1
}

fn of_arm_body(ast: &Ast, body: &ArmBody) -> u32 {
    match body {
        ArmBody::Expr(id) => ast.expr_depth(*id),
        ArmBody::Block(block) => max_stmt(ast, block),
    }
}

fn max_arms(ast: &Ast, arms: &[MatchArm]) -> u32 {
    arms.iter()
        .map(|arm| {
            arm.guard
                .map_or(0, |g| ast.expr_depth(g))
                .max(of_arm_body(ast, &arm.body))
        })
        .max()
        .unwrap_or(0)
}

fn max_expr(ast: &Ast, ids: &[ExprId]) -> u32 {
    ids.iter().map(|id| ast.expr_depth(*id)).max().unwrap_or(0)
}

fn max_stmt(ast: &Ast, block: &[StmtId]) -> u32 {
    block
        .iter()
        .map(|id: &StmtId| ast.stmt_depth(*id))
        .max()
        .unwrap_or(0)
}
