//! Expressions.
//!
//! Every renderer here returns text whose **first line carries no
//! indentation** — it is written wherever the caller already is — while
//! continuation lines are indented from the `level` it was given. `col`
//! is the column the first line starts at, and is what the width
//! decisions are made against.
//!
//! There is one renderer, not a "flat" one and a "broken" one: a
//! composite node renders its children once, then decides whether to join
//! them on one line. Rendering a child twice is not an option, because
//! rendering is what consumes comments.

use brasa_ast::{
    ArmBody, BinaryOp, CatchArm, CatchType, Expr, ExprId, LambdaBody, LambdaParam, MatchArm,
    UnaryOp,
};
use brasa_source::Span;

use crate::{INDENT, Lines, Printer, fits, indent_of};

/// A lambda binds looser than anything it can appear inside, so it is
/// parenthesized everywhere except in the argument-like positions that
/// start a fresh expression.
const P_LAMBDA: u8 = 5;
const P_PIPE: u8 = 10;
const P_COALESCE: u8 = 20;
const P_RANGE: u8 = 70;
/// The binding power a unary operand is parsed at, so anything looser
/// inside one needs parentheses (`-(a + b)`).
const P_UNARY: u8 = 105;
/// Receivers, callees and `catch` subjects are postfix expressions:
/// anything looser has to be parenthesized to reach them (`(a + b).f()`).
const P_POSTFIX: u8 = 110;
const P_PRIMARY: u8 = 120;

/// One `.name`, `.name(args)` or `?.name(args)` step of a method chain.
struct ChainLink {
    name: String,
    args: Option<Vec<ExprId>>,
    safe: bool,
    /// End of the receiver this link hangs off, which is where the
    /// source is inspected to see whether the author put the `.` on a
    /// line of its own.
    recv_end: u32,
}

/// How a call was spelled, which the AST does not record: `f(a)`,
/// the statement-position command form `puts a`, or a trailing
/// `do ... end` block that is really the call's last argument.
enum CallShape {
    Plain,
    Command,
    TrailingDo { parens: bool },
}

impl<'a> Printer<'a> {
    pub(crate) fn expr(&mut self, id: ExprId, col: usize, level: usize) -> String {
        if let Some(text) = self.try_chain(id, col, level) {
            return text;
        }

        let ast = self.ast;
        let span = ast.span_of_expr(id);

        match ast.expr(id) {
            // Literals are printed from the source, so `0xFF` stays
            // hexadecimal, `1.50` keeps its trailing zero, and a string
            // keeps its own escapes and its own interpolations.
            Expr::Int(_)
            | Expr::Float(_)
            | Expr::Bool(_)
            | Expr::Char(_)
            | Expr::Unit
            | Expr::StringLit { .. } => self.slice(span).to_string(),
            Expr::SelfExpr => "self".to_string(),
            Expr::Ident(name) => name.clone(),

            Expr::EnumCtor { name, args } if args.is_empty() => name.clone(),
            Expr::EnumCtor { name, args } => {
                let args = args.clone();
                self.arg_list(name, &args, col, level)
            }

            Expr::Call { callee, args } => {
                let (callee, args) = (*callee, args.clone());
                self.call(callee, &args, col, level)
            }

            Expr::Field { recv, name } => {
                let text = self.child(*recv, P_POSTFIX, col, level);
                format!("{text}.{name}")
            }

            Expr::SafeNav { recv, name, args } => {
                let (recv, name, args) = (*recv, name.clone(), args.clone());
                let text = self.child(recv, P_POSTFIX, col, level);
                match args {
                    None => format!("{text}?.{name}"),
                    Some(args) => {
                        let head = format!("{text}?.{name}");
                        self.arg_list(&head, &args, col, level)
                    }
                }
            }

            Expr::Index { recv, index } => {
                let (recv, index) = (*recv, *index);
                let text = self.child(recv, P_POSTFIX, col, level);
                let inner = self.expr(index, col + text.chars().count() + 1, level);
                format!("{text}[{inner}]")
            }

            Expr::Unary { op, operand } => {
                let (op, operand) = (*op, *operand);
                let symbol = self.unary_text(op, span.start.0);
                let text = self.child(operand, P_UNARY, col + symbol.len(), level);
                format!("{symbol}{text}")
            }

            Expr::Binary { op, lhs, rhs } => {
                let (op, lhs, rhs) = (*op, *lhs, *rhs);
                self.binary(op, lhs, rhs, col, level)
            }

            Expr::Coalesce { lhs, rhs } => {
                let (lhs, rhs) = (*lhs, *rhs);
                let left = self.child(lhs, P_COALESCE + 1, col, level);
                let right = self.child(rhs, P_COALESCE, col + left.chars().count() + 4, level);
                format!("{left} ?? {right}")
            }

            Expr::Range { lo, hi, inclusive } => {
                let (lo, hi, inclusive) = (*lo, *hi, *inclusive);
                let op = if inclusive { "..=" } else { ".." };
                let left = self.child(lo, P_RANGE + 1, col, level);
                let right = self.child(
                    hi,
                    P_RANGE + 1,
                    col + left.chars().count() + op.len(),
                    level,
                );
                format!("{left}{op}{right}")
            }

            Expr::Pipe { lhs, target } => {
                let (lhs, target) = (*lhs, *target);
                self.pipe(lhs, target, col, level)
            }

            Expr::VectorLit(items) => {
                let items = items.clone();
                self.delimited("[", "]", &items, col, level)
            }

            Expr::TupleLit(items) => {
                let items = items.clone();
                self.tuple(&items, col, level)
            }

            Expr::MapLit(entries) => {
                let entries = entries.clone();
                let rendered = self.entries(&entries, level);
                self.wrap_pairs("{", "}", rendered, col, level)
            }

            Expr::StructLit { type_name, fields } => {
                let (type_name, fields) = (type_name.clone(), fields.clone());
                let inner = level + INDENT;
                let rendered: Vec<String> = fields
                    .iter()
                    .map(|(name, value)| {
                        let head = format!("{name}: ");
                        let value = self.expr(*value, inner + head.chars().count(), inner);
                        format!("{head}{value}")
                    })
                    .collect();
                if rendered.is_empty() {
                    return format!("{type_name} {{}}");
                }
                let body = self.wrap_pairs("{", "}", rendered, col + type_name.len() + 1, level);
                format!("{type_name} {body}")
            }

            Expr::Lambda { params, body } => {
                let (params, body) = (params.clone(), body.clone());
                self.lambda(&params, &body, col, level, span)
            }

            Expr::If(node) => self.if_node(node, level, self.body_region_end(span)),
            Expr::Match { scrutinee, arms } => {
                let (scrutinee, arms) = (*scrutinee, arms.clone());
                self.match_expr(scrutinee, &arms, col, level, span)
            }
            Expr::Catch {
                subject,
                exhaustive,
                binding,
                arms,
            } => {
                let (subject, exhaustive, binding, arms) =
                    (*subject, *exhaustive, binding.clone(), arms.clone());
                self.catch_expr(subject, exhaustive, &binding, &arms, col, level, span)
            }
        }
    }

    /// Renders `id` as a child that must bind at least as tightly as
    /// `min_prec`, adding the parentheses the AST dropped when it does
    /// not. `brasa_ast` has no node for grouping, so every parenthesis in
    /// the output is derived here from the precedence table in
    /// `docs/spec/02-grammar.md`.
    fn child(&mut self, id: ExprId, min_prec: u8, col: usize, level: usize) -> String {
        if self.prec(id) >= min_prec {
            return self.expr(id, col, level);
        }

        let inner = self.expr(id, col + 1, level);
        format!("({inner})")
    }

    /// How tightly `id` binds, as a threshold a parent compares its own
    /// binding power against.
    ///
    /// A binary node reports the *lower* of its two binding powers. For
    /// the left-associative operators the two are adjacent and the lower
    /// one is the left power, so this is the obvious answer; for a
    /// right-associative one it is the only correct answer. `**` is
    /// (101, 100): reporting 101 would let the left child of a `**` pass
    /// its parent's own 101 threshold and lose the parentheses that
    /// `(a ** b) ** c` needs to survive a reparse.
    fn prec(&self, id: ExprId) -> u8 {
        match self.ast.expr(id) {
            Expr::Lambda { .. } => P_LAMBDA,
            Expr::Pipe { .. } => P_PIPE,
            Expr::Coalesce { .. } => P_COALESCE,
            Expr::Range { .. } => P_RANGE,
            Expr::Binary { op, .. } => {
                let (left, right) = binary_bp(*op);
                left.min(right)
            }
            Expr::Unary { .. } => P_UNARY,
            Expr::Call { .. }
            | Expr::Field { .. }
            | Expr::SafeNav { .. }
            | Expr::Index { .. }
            | Expr::Catch { .. } => P_POSTFIX,
            _ => P_PRIMARY,
        }
    }

    /// `&&` and `and` are the same operator to the AST, as are `!` and
    /// `not`; the spelling is read back from the gap between the operands.
    fn binary_text(&self, op: BinaryOp, lhs_end: u32, rhs_start: u32) -> &'static str {
        let between = self.text(lhs_end, rhs_start);

        match op {
            BinaryOp::And if !between.contains("&&") => "and",
            BinaryOp::Or if !between.contains("||") => "or",
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

    fn unary_text(&self, op: UnaryOp, start: u32) -> &'static str {
        match op {
            UnaryOp::Neg => "-",
            UnaryOp::Not if self.keyword_follows(start, "not") => "not ",
            UnaryOp::Not => "!",
        }
    }

    /// A binary expression is never broken across lines: outside
    /// brackets a newline ends the statement, so a break would change
    /// what the source parses as. An over-long one stays over-long.
    fn binary(
        &mut self,
        op: BinaryOp,
        lhs: ExprId,
        rhs: ExprId,
        col: usize,
        level: usize,
    ) -> String {
        let ast = self.ast;
        let (lbp, rbp) = binary_bp(op);
        let symbol = self.binary_text(
            op,
            ast.span_of_expr(lhs).end.0,
            ast.span_of_expr(rhs).start.0,
        );

        let left = self.child(lhs, lbp, col, level);
        let right = self.child(
            rhs,
            rbp,
            col + left.chars().count() + symbol.len() + 2,
            level,
        );
        format!("{left} {symbol} {right}")
    }

    /// `lhs |> target`, split onto its own line when it does not fit: a
    /// newline before `|>` is one of the grammar's explicit continuation
    /// rules, so this break is safe anywhere.
    fn pipe(&mut self, lhs: ExprId, target: ExprId, col: usize, level: usize) -> String {
        let left = self.child(lhs, P_PIPE, col, level);
        let inner = level + INDENT;
        let right = self.child(target, P_POSTFIX, inner + 3, inner);

        let flat = format!("{left} |> {right}");
        if !flat.contains('\n') && fits(col, &flat) {
            return flat;
        }

        format!("{left}\n{}|> {right}", indent_of(inner))
    }

    fn call(&mut self, callee: ExprId, args: &[ExprId], col: usize, level: usize) -> String {
        let head = self.child(callee, P_POSTFIX, col, level);

        match self.call_shape(callee, args) {
            CallShape::Plain => self.arg_list(&head, args, col, level),
            CallShape::Command => {
                let mut rendered = Vec::new();
                let mut at = col + head.chars().count() + 1;
                for arg in args {
                    let text = self.expr(*arg, at, level);
                    at += text.chars().count() + 2;
                    rendered.push(text);
                }
                format!("{head} {}", rendered.join(", "))
            }
            CallShape::TrailingDo { parens } => {
                let (block, rest) = args
                    .split_last()
                    .expect("a trailing do implies an argument");
                let head = if parens || !rest.is_empty() {
                    self.arg_list(&head, rest, col, level)
                } else {
                    head
                };
                let block = self.expr(*block, col, level);
                format!("{head} {block}")
            }
        }
    }

    /// Which of the three call spellings the source used. A trailing
    /// `do` block is recognized by the lambda's own span, which starts at
    /// the `do` keyword; the parenthesis-less command form by there being
    /// no `(` between the callee and its first argument.
    fn call_shape(&self, callee: ExprId, args: &[ExprId]) -> CallShape {
        let ast = self.ast;
        let Some(first) = args.first() else {
            return CallShape::Plain;
        };

        let callee_end = ast.span_of_expr(callee).end.0;
        let gap = self.text(callee_end, ast.span_of_expr(*first).start.0);
        let parens = gap.contains('(');

        let last = *args.last().expect("checked non-empty above");
        if self.is_do_lambda(last) {
            return CallShape::TrailingDo { parens };
        }

        if parens || !matches!(ast.expr(callee), Expr::Ident(_)) {
            return CallShape::Plain;
        }

        CallShape::Command
    }

    fn is_do_lambda(&self, id: ExprId) -> bool {
        matches!(
            self.ast.expr(id),
            Expr::Lambda {
                body: LambdaBody::Block(_),
                ..
            }
        ) && self.slice(self.ast.span_of_expr(id)).starts_with("do")
    }

    /// `head(a, b)`, one argument per line when that does not fit.
    ///
    /// A lone argument is instead *hugged*: it is rendered as if it
    /// started right after the `(`, so a call whose single argument is a
    /// literal or a block keeps its opening on the call's own line
    /// (`push(Input {` ... `})`) rather than being pushed down a level.
    fn arg_list(&mut self, head: &str, args: &[ExprId], col: usize, level: usize) -> String {
        if args.is_empty() {
            return format!("{head}()");
        }

        if let [only] = args {
            let text = self.expr(*only, col + head.chars().count() + 1, level);
            return format!("{head}({text})");
        }

        let inner = level + INDENT;
        let rendered: Vec<String> = args
            .iter()
            .map(|arg| self.expr(*arg, inner, inner))
            .collect();

        let flat = format!("{head}({})", rendered.join(", "));
        if !flat.contains('\n') && fits(col, &flat) {
            return flat;
        }

        format!(
            "{head}(\n{}\n{})",
            join_broken(&rendered, inner),
            indent_of(level)
        )
    }

    fn delimited(
        &mut self,
        open: &str,
        close: &str,
        items: &[ExprId],
        col: usize,
        level: usize,
    ) -> String {
        if items.is_empty() {
            return format!("{open}{close}");
        }

        let inner = level + INDENT;
        let rendered: Vec<String> = items
            .iter()
            .map(|item| self.expr(*item, inner, inner))
            .collect();

        let flat = format!("{open}{}{close}", rendered.join(", "));
        if !flat.contains('\n') && fits(col, &flat) {
            return flat;
        }

        format!(
            "{open}\n{}\n{}{close}",
            join_broken(&rendered, inner),
            indent_of(level)
        )
    }

    /// A tuple keeps the comma that makes `(a,)` a one-element tuple
    /// rather than a parenthesized `a`.
    fn tuple(&mut self, items: &[ExprId], col: usize, level: usize) -> String {
        let inner = level + INDENT;
        let rendered: Vec<String> = items
            .iter()
            .map(|item| self.expr(*item, inner, inner))
            .collect();

        // The comma is what makes `(a,)` a one-element tuple rather than
        // a parenthesized `a`; broken lists carry it on every element.
        let tail = if rendered.len() == 1 { "," } else { "" };
        let flat = format!("({}{tail})", rendered.join(", "));
        if !flat.contains('\n') && fits(col, &flat) {
            return flat;
        }

        format!(
            "(\n{}\n{})",
            join_broken(&rendered, inner),
            indent_of(level)
        )
    }

    fn entries(&mut self, entries: &[(ExprId, ExprId)], level: usize) -> Vec<String> {
        let inner = level + INDENT;

        entries
            .iter()
            .map(|(key, value)| {
                let key = self.expr(*key, inner, inner);
                let head = format!("{key}: ");
                let value = self.expr(*value, inner + head.chars().count(), inner);
                format!("{head}{value}")
            })
            .collect()
    }

    /// The braced forms — map literals and struct literals — which carry
    /// a space inside the braces when they fit on one line.
    fn wrap_pairs(
        &mut self,
        open: &str,
        close: &str,
        rendered: Vec<String>,
        col: usize,
        level: usize,
    ) -> String {
        if rendered.is_empty() {
            return format!("{open}{close}");
        }

        let flat = format!("{open} {} {close}", rendered.join(", "));
        if !flat.contains('\n') && fits(col, &flat) {
            return flat;
        }

        let inner = level + INDENT;
        format!(
            "{open}\n{}\n{}{close}",
            join_broken(&rendered, inner),
            indent_of(level)
        )
    }

    fn lambda(
        &mut self,
        params: &[LambdaParam],
        body: &LambdaBody,
        col: usize,
        level: usize,
        span: Span,
    ) -> String {
        let params = self.lambda_params(params);

        match body {
            LambdaBody::Expr(expr) => {
                let head = format!("{params} ");
                let text = self.expr(*expr, col + head.chars().count(), level);
                format!("{head}{text}")
            }
            LambdaBody::Block(block) => {
                let head = if params == "||" {
                    "do".to_string()
                } else {
                    format!("do {params}")
                };

                let mut lines = Lines::new();
                lines.push(&head);
                self.push_body(&mut lines, block, level, span.start.0);
                lines.push(&format!("{}end", indent_of(level)));
                lines.finish()
            }
        }
    }

    fn lambda_params(&self, params: &[LambdaParam]) -> String {
        let rendered: Vec<String> = params
            .iter()
            .map(|param| {
                let name = match param.pattern {
                    Some(pattern) => self.pattern(pattern),
                    None => param.name.clone(),
                };
                match param.ty {
                    Some(ty) => format!("{name}: {}", self.ty(ty)),
                    None => name,
                }
            })
            .collect();

        format!("|{}|", rendered.join(", "))
    }

    fn match_expr(
        &mut self,
        scrutinee: ExprId,
        arms: &[MatchArm],
        col: usize,
        level: usize,
        span: Span,
    ) -> String {
        let head = "match ";
        let text = self.expr(scrutinee, col + head.len(), level);

        let mut lines = Lines::new();
        lines.push(&format!("{head}{text}"));

        let inner = level + INDENT;
        let mut body = Lines::new();

        for (index, arm) in arms.iter().enumerate() {
            let start = self.ast.span_of_pattern(arm.pattern).start.0;
            let next_arm_start = arms
                .get(index + 1)
                .map(|next| self.ast.span_of_pattern(next.pattern).start.0);

            let guard = arm.guard.map(|guard| {
                let text = self.expr(guard, inner, inner);
                format!(" if {text}")
            });
            let head = format!(
                "{}{}{}",
                self.pattern(arm.pattern),
                guard.unwrap_or_default(),
                " =>"
            );

            self.arm(
                &mut body,
                &head,
                &arm.body,
                inner,
                start,
                next_arm_start,
                span,
            );
        }

        self.emit_comments_before(&mut body, inner, self.body_region_end(span));

        let body = body.finish();
        if !body.is_empty() {
            lines.push(&body);
        }
        lines.push(&format!("{}end", indent_of(level)));
        lines.finish()
    }

    #[allow(clippy::too_many_arguments)]
    fn catch_expr(
        &mut self,
        subject: ExprId,
        exhaustive: bool,
        binding: &str,
        arms: &[CatchArm],
        col: usize,
        level: usize,
        span: Span,
    ) -> String {
        let text = self.child(subject, P_POSTFIX, col, level);
        let keyword = if exhaustive { "catch!" } else { "catch" };

        let mut lines = Lines::new();
        lines.push(&format!("{text} {keyword} ({binding})"));

        let inner = level + INDENT;
        let mut body = Lines::new();

        for (index, arm) in arms.iter().enumerate() {
            let start = catch_arm_start(arm);
            let next_arm_start = arms.get(index + 1).map(catch_arm_start);

            let types: Vec<String> = arm
                .types
                .iter()
                .map(|ty| match ty {
                    CatchType::Named { name, .. } => name.clone(),
                    CatchType::Wildcard { .. } => "_".to_string(),
                })
                .collect();
            let guard = arm.guard.map(|guard| {
                let text = self.expr(guard, inner, inner);
                format!(" if {text}")
            });
            let head = format!("{}{} =>", types.join(" | "), guard.unwrap_or_default());

            self.arm(
                &mut body,
                &head,
                &arm.body,
                inner,
                start,
                next_arm_start,
                span,
            );
        }

        self.emit_comments_before(&mut body, inner, self.body_region_end(span));

        let body = body.finish();
        if !body.is_empty() {
            lines.push(&body);
        }
        lines.push(&format!("{}end", indent_of(level)));
        lines.finish()
    }

    /// One `match`/`catch` arm. `head` is everything through the `=>`.
    ///
    /// The arm body keeps the form it was written in: the parser
    /// normalizes an inline `=> throw e` into a one-statement block, so
    /// only the absence of a newline after the `=>` distinguishes it from
    /// a real block body.
    #[allow(clippy::too_many_arguments)]
    fn arm(
        &mut self,
        lines: &mut Lines,
        head: &str,
        body: &ArmBody,
        level: usize,
        start: u32,
        next_arm_start: Option<u32>,
        span: Span,
    ) {
        self.emit_comments_before(lines, level, start);
        if self.blank_before(start) {
            lines.blank();
        }

        let pad = indent_of(level);

        match body {
            ArmBody::Expr(expr) => {
                let head = format!("{pad}{head} ");
                let text = self.expr(*expr, head.chars().count(), level);
                lines.push(&format!("{head}{text}"));
                self.emit_trailing(lines, self.ast.span_of_expr(*expr).end.0);
            }
            ArmBody::Block(block) => {
                if let Some(text) = self.inline_arm_block(block, head, level) {
                    lines.push(&format!("{pad}{text}"));
                    let end = block
                        .last()
                        .map_or(start, |stmt| self.ast.span_of_stmt(*stmt).end.0);
                    self.emit_trailing(lines, end);
                    return;
                }

                lines.push(&format!("{pad}{head}"));

                // The arm's territory stops at the next arm, or at the
                // construct's own `end` when this is the last one.
                let fallback = next_arm_start.unwrap_or_else(|| self.body_region_end(span));
                let region_end = block.last().map_or(fallback, |stmt| {
                    self.next_token_pos(self.ast.span_of_stmt(*stmt).end.0)
                });
                let text = self.block(block, level + INDENT, region_end);
                if !text.is_empty() {
                    lines.push(&text);
                }
            }
        }
    }

    /// An arm body the author wrote on the `=>` line, rendered back onto
    /// it. `None` when it was a real multi-statement block.
    fn inline_arm_block(
        &mut self,
        block: &[brasa_ast::StmtId],
        head: &str,
        level: usize,
    ) -> Option<String> {
        let [stmt] = block else {
            return None;
        };

        let start = self.ast.span_of_stmt(*stmt).start.0;
        if self.src[..start as usize]
            .rsplit_once("=>")
            .is_none_or(|(_, gap)| gap.contains('\n'))
        {
            return None;
        }

        let text = self.stmt(*stmt, level);
        let pad = indent_of(level);
        let text = text.strip_prefix(&pad).unwrap_or(&text).to_string();
        Some(format!("{head} {text}"))
    }

    /// Renders `id` as a broken method chain when it is one worth
    /// breaking, and returns `None` otherwise so the caller falls back to
    /// the ordinary postfix rendering.
    fn try_chain(&mut self, id: ExprId, col: usize, level: usize) -> Option<String> {
        let (base, links) = self.chain_links(id);
        if links.len() < 2 {
            return None;
        }

        let inner = level + INDENT;
        let base_text = self.child(base, P_POSTFIX, col, level);
        let rendered: Vec<String> = links
            .iter()
            .map(|link| {
                let head = format!("{}{}", if link.safe { "?." } else { "." }, link.name);
                match &link.args {
                    None => head,
                    Some(args) => {
                        let args = args.clone();
                        self.chain_call(&head, &args, inner)
                    }
                }
            })
            .collect();

        // A chain the author split across lines stays split: the leading
        // dots are the canonical style for pipelines
        // (`docs/spec/01-syntax.md`) and collapsing them back onto one
        // line would be the formatter overruling a real decision.
        let split = links.iter().any(|link| self.dot_on_new_line(link.recv_end));
        let flat = format!("{base_text}{}", rendered.concat());
        if !split && !flat.contains('\n') && fits(col, &flat) {
            return Some(flat);
        }

        let mut out = base_text;
        for text in rendered {
            out.push('\n');
            out.push_str(&indent_of(inner));
            out.push_str(&text);
        }
        Some(out)
    }

    /// A chain link's own argument list, which may itself carry a
    /// trailing `do` block.
    fn chain_call(&mut self, head: &str, args: &[ExprId], level: usize) -> String {
        let col = level + head.chars().count();

        match args.split_last() {
            Some((last, rest)) if self.is_do_lambda(*last) => {
                let head = if rest.is_empty() && !self.has_parens_before(*last) {
                    head.to_string()
                } else {
                    self.arg_list(head, rest, col, level)
                };
                let block = self.expr(*last, col, level);
                format!("{head} {block}")
            }
            _ => self.arg_list(head, args, col, level),
        }
    }

    /// Whether a `(` appears just before `id` in the source, which is
    /// what tells `.each() do` apart from `.each do`.
    fn has_parens_before(&self, id: ExprId) -> bool {
        let start = self.ast.span_of_expr(id).start.0 as usize;
        self.src[..start].trim_end().ends_with(['(', ')'])
    }

    /// Collects the `.name(...)` steps hanging off a common receiver,
    /// innermost first once reversed, together with the expression they
    /// all hang off.
    fn chain_links(&self, id: ExprId) -> (ExprId, Vec<ChainLink>) {
        let ast = self.ast;
        let mut node = id;
        let mut links = Vec::new();

        loop {
            match ast.expr(node) {
                Expr::Call { callee, args } => match ast.expr(*callee) {
                    Expr::Field { recv, name } => {
                        links.push(ChainLink {
                            name: name.clone(),
                            args: Some(args.clone()),
                            safe: false,
                            recv_end: ast.span_of_expr(*recv).end.0,
                        });
                        node = *recv;
                    }
                    _ => break,
                },
                Expr::Field { recv, name } => {
                    links.push(ChainLink {
                        name: name.clone(),
                        args: None,
                        safe: false,
                        recv_end: ast.span_of_expr(*recv).end.0,
                    });
                    node = *recv;
                }
                Expr::SafeNav { recv, name, args } => {
                    links.push(ChainLink {
                        name: name.clone(),
                        args: args.clone(),
                        safe: true,
                        recv_end: ast.span_of_expr(*recv).end.0,
                    });
                    node = *recv;
                }
                _ => break,
            }
        }

        links.reverse();
        (node, links)
    }

    /// Whether the `.` following the receiver that ends at `recv_end`
    /// was written on a line of its own.
    fn dot_on_new_line(&self, recv_end: u32) -> bool {
        let rest = &self.src[recv_end as usize..];
        match rest.find('.') {
            Some(dot) => rest[..dot].contains('\n'),
            None => false,
        }
    }
}

fn catch_arm_start(arm: &CatchArm) -> u32 {
    match arm.types.first() {
        Some(CatchType::Named { span, .. }) | Some(CatchType::Wildcard { span }) => span.start.0,
        None => 0,
    }
}

/// The left and right binding powers of a binary operator, matching
/// `binding_power` in `brasa_parser`: the right power is the lower one
/// for the right-associative operators, which is what makes a same-power
/// child need parentheses on one side and not the other.
fn binary_bp(op: BinaryOp) -> (u8, u8) {
    match op {
        BinaryOp::Or => (30, 31),
        BinaryOp::And => (40, 41),
        BinaryOp::Eq | BinaryOp::NotEq => (50, 51),
        BinaryOp::Lt | BinaryOp::LtEq | BinaryOp::Gt | BinaryOp::GtEq => (60, 61),
        BinaryOp::Add | BinaryOp::Sub => (80, 81),
        BinaryOp::Mul | BinaryOp::Div | BinaryOp::Rem => (90, 91),
        BinaryOp::Pow => (101, 100),
    }
}

/// One item per line at `level`, each with a trailing comma.
///
/// The comma after the last item is kept: every list the formatter
/// breaks accepts one, and keeping it means adding an item later is a
/// one-line diff.
fn join_broken(items: &[String], level: usize) -> String {
    let pad = indent_of(level);
    items
        .iter()
        .map(|item| format!("{pad}{item},"))
        .collect::<Vec<_>>()
        .join("\n")
}
