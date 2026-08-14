//! Blocks and statements.

use brasa_ast::{AssignOp, Block, ExprId, IfNode, LetStmt, Stmt, StmtId};

use crate::{INDENT, Lines, Printer, indent_of};

impl<'a> Printer<'a> {
    /// Prints `stmts` one per line at `level`, with the comments and
    /// blank lines the author put between them. `region_end` is where the
    /// block's territory stops — the closing keyword's offset — so a
    /// comment written after the last statement still lands inside.
    pub(crate) fn block(&mut self, stmts: &[StmtId], level: usize, region_end: u32) -> String {
        let ast = self.ast;
        let mut lines = Lines::new();

        for &stmt in stmts {
            let span = ast.span_of_stmt(stmt);

            self.emit_comments_before(&mut lines, level, span.start.0);
            if self.blank_before(span.start.0) {
                lines.blank();
            }

            let text = self.stmt(stmt, level);
            self.emit_hoisted(&mut lines, level, span);
            lines.push(&text);
            self.emit_trailing(&mut lines, span.end.0);
        }

        self.emit_comments_before(&mut lines, level, region_end);
        lines.finish()
    }

    /// The offset of the next real token at or after `from`, skipping
    /// whitespace and whole comment lines.
    ///
    /// Used to locate the keyword that closes a block (`end`, `elsif`,
    /// `else`, or the next `match` arm) without needing a span for it.
    /// Because comments are skipped rather than stopped at, everything
    /// written between the block's last statement and its closing keyword
    /// stays inside the block.
    pub(crate) fn next_token_pos(&self, from: u32) -> u32 {
        let bytes = self.src.as_bytes();
        let mut index = from as usize;

        loop {
            while index < bytes.len() && bytes[index].is_ascii_whitespace() {
                index += 1;
            }

            if index < bytes.len() && bytes[index] == b'#' {
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
                continue;
            }

            return index as u32;
        }
    }

    /// The offset just past the end of the line `pos` sits on.
    ///
    /// A function signature always ends its line — the grammar requires
    /// a newline between it and the block — so this reaches past a
    /// signature from anywhere inside it, which is the only anchor an
    /// empty body has. A signature the author split across lines (legal
    /// inside the parameter parentheses) lands short instead, which
    /// leaves the comments after it to the enclosing scope rather than
    /// swallowing them.
    pub(crate) fn line_end(&self, pos: u32) -> u32 {
        match self.src[pos as usize..].find('\n') {
            Some(offset) => pos + offset as u32 + 1,
            None => self.src.len() as u32,
        }
    }

    /// Where a block ends: after its last statement, or after `opener`
    /// when it has none.
    pub(crate) fn block_region_end(&self, stmts: &Block, opener: u32) -> u32 {
        let from = stmts
            .last()
            .map_or(opener, |stmt| self.ast.span_of_stmt(*stmt).end.0);
        self.next_token_pos(from)
    }

    pub(crate) fn stmt(&mut self, id: StmtId, level: usize) -> String {
        let ast = self.ast;
        let span = ast.span_of_stmt(id);
        let pad = indent_of(level);

        match ast.stmt(id) {
            Stmt::Let(let_stmt) => self.let_stmt(let_stmt, &pad, level),
            Stmt::Assign { target, op, value } => {
                let target = self.expr(*target, level, level);
                let op = match op {
                    AssignOp::Assign => "=",
                    AssignOp::AddAssign => "+=",
                    AssignOp::SubAssign => "-=",
                    AssignOp::MulAssign => "*=",
                    AssignOp::DivAssign => "/=",
                    AssignOp::RemAssign => "%=",
                };
                let head = format!("{pad}{target} {op} ");
                let value = self.expr(*value, head.chars().count(), level);
                format!("{head}{value}")
            }
            Stmt::Return(None) => format!("{pad}return"),
            Stmt::Return(Some(value)) => {
                let head = format!("{pad}return ");
                let value = self.expr(*value, head.chars().count(), level);
                format!("{head}{value}")
            }
            Stmt::Break => format!("{pad}break"),
            Stmt::Continue => format!("{pad}continue"),
            Stmt::Throw(value) => {
                let head = format!("{pad}throw ");
                let value = self.expr(*value, head.chars().count(), level);
                format!("{head}{value}")
            }
            Stmt::If(node) => {
                let text = self.if_node(node, level, self.body_region_end(span));
                format!("{pad}{text}")
            }
            Stmt::While { cond, body } => {
                let head = format!("{pad}while ");
                let cond_text = self.expr(*cond, head.chars().count(), level);

                let mut lines = Lines::new();
                lines.push(&format!("{head}{cond_text}"));
                self.push_body(&mut lines, body, level, ast.span_of_expr(*cond).end.0);
                lines.push(&format!("{pad}end"));
                lines.finish()
            }
            Stmt::For {
                pattern,
                iterable,
                body,
            } => {
                let head = format!("{pad}for {} in ", self.pattern(*pattern));
                let iter_text = self.expr(*iterable, head.chars().count(), level);

                let mut lines = Lines::new();
                lines.push(&format!("{head}{iter_text}"));
                self.push_body(&mut lines, body, level, ast.span_of_expr(*iterable).end.0);
                lines.push(&format!("{pad}end"));
                lines.finish()
            }
            Stmt::Expr(expr) => {
                let text = self.expr(*expr, level, level);
                format!("{pad}{text}")
            }
        }
    }

    /// Prints an indented body under a header line, skipping the line
    /// entirely when the body has nothing in it.
    pub(crate) fn push_body(&mut self, lines: &mut Lines, body: &Block, level: usize, opener: u32) {
        let region_end = self.block_region_end(body, opener);
        let text = self.block(body, level + INDENT, region_end);
        if !text.is_empty() {
            lines.push(&text);
        }
    }

    pub(crate) fn let_stmt(&mut self, let_stmt: &LetStmt, head: &str, level: usize) -> String {
        let mutable = if let_stmt.mutable { "mut " } else { "" };
        let ty = let_stmt
            .ty
            .map_or(String::new(), |ty| format!(": {}", self.ty(ty)));

        let binding = match let_stmt.pattern {
            Some(pattern) => self.pattern(pattern),
            None => let_stmt.name.clone(),
        };

        let head = format!("{head}let {mutable}{binding}{ty} = ");
        let value = self.expr(let_stmt.value, head.chars().count(), level);
        format!("{head}{value}")
    }

    /// `if`/`elsif`/`else`, in whichever of the two surface forms the
    /// author used: the inline `if c then a else b end` form is kept
    /// inline, since the AST normalizes it into the same node as the
    /// block form and only the source can tell them apart.
    pub(crate) fn if_node(&mut self, node: &IfNode, level: usize, region_end: u32) -> String {
        if self.is_inline_if(node) {
            return self.inline_if(node, level);
        }

        let ast = self.ast;
        let pad = indent_of(level);
        let mut lines = Lines::new();

        for (index, (cond, body)) in node.branches.iter().enumerate() {
            // The first line is written wherever the caller already is,
            // so only the `elsif`/`else`/`end` lines carry the padding.
            let head = if index == 0 {
                "if ".to_string()
            } else {
                format!("{pad}elsif ")
            };
            let cond_text = self.expr(*cond, head.chars().count(), level);
            lines.push(&format!("{head}{cond_text}"));
            self.push_body(&mut lines, body, level, ast.span_of_expr(*cond).end.0);
        }

        if let Some(else_body) = &node.else_ {
            lines.push(&format!("{pad}else"));

            let last_branch_end = node.branches.last().map_or(region_end, |(cond, body)| {
                self.block_region_end(body, ast.span_of_expr(*cond).end.0)
            });
            self.push_body(&mut lines, else_body, level, last_branch_end);
        }

        lines.push(&format!("{pad}end"));
        lines.finish()
    }

    /// The inline form is spelled with `then` right after the first
    /// condition; nothing in the AST records it.
    fn is_inline_if(&self, node: &IfNode) -> bool {
        let Some((cond, _)) = node.branches.first() else {
            return false;
        };

        self.keyword_follows(self.ast.span_of_expr(*cond).end.0, "then")
            && node
                .branches
                .iter()
                .all(|(_, body)| self.single_expr(body).is_some())
            && node
                .else_
                .as_ref()
                .is_none_or(|body| self.single_expr(body).is_some())
    }

    fn inline_if(&mut self, node: &IfNode, level: usize) -> String {
        let mut out = String::new();

        for (index, (cond, body)) in node.branches.iter().enumerate() {
            let keyword = if index == 0 { "if" } else { " elsif" };
            let cond_text = self.expr(*cond, level, level);
            let body_expr = self.single_expr(body).expect("checked by is_inline_if");
            let body_text = self.expr(body_expr, level, level);
            out.push_str(&format!("{keyword} {cond_text} then {body_text}"));
        }

        if let Some(else_body) = &node.else_ {
            let body_expr = self
                .single_expr(else_body)
                .expect("checked by is_inline_if");
            let body_text = self.expr(body_expr, level, level);
            out.push_str(&format!(" else {body_text}"));
        }

        out.push_str(" end");
        out
    }

    /// The one expression a block holds, when that is all it holds.
    pub(crate) fn single_expr(&self, body: &Block) -> Option<ExprId> {
        if body.len() != 1 {
            return None;
        }

        match self.ast.stmt(body[0]) {
            Stmt::Expr(expr) => Some(*expr),
            _ => None,
        }
    }
}
