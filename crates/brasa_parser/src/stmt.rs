//! Statements: `let`, assignment, `return`/`break`/`continue`/`throw`,
//! `if`/`while`/`for`, and bare expression statements.

use brasa_ast::{AssignOp, Block, ExprId, IfNode, LetStmt, Stmt, StmtId};
use brasa_source::Span;
use brasa_token::TokenKind;

use crate::Parser;

impl<'a> Parser<'a> {
    /// `block = ( stmt NL )*`, stopping at any of `terminators`, `end` of
    /// input, or (for arm bodies elsewhere) a line that starts a new arm.
    pub(crate) fn parse_block(&mut self, terminators: &[TokenKind]) -> Block {
        let mut stmts = Vec::new();
        self.skip_stmt_seps();

        while !self.at_any(terminators) && !self.at(TokenKind::Eof) {
            let checkpoint = self.pos;

            stmts.push(self.parse_stmt());

            // Resynchronize only when the cursor is genuinely stuck (no
            // separator, terminator, or EOF followed the statement): a
            // diagnostic alone is not evidence of a bad cursor position,
            // since some diagnostics (e.g. the `??` AST-gap report) are
            // fully recovered in place and leave parsing exactly where a
            // clean statement would.
            if !self.skip_stmt_seps() && !self.at_any(terminators) && !self.at(TokenKind::Eof) {
                self.error_expected("a newline or 'end'");
                self.synchronize_stmt();
            }

            self.ensure_progress(checkpoint);
        }

        stmts
    }

    /// A `match`/`catch` arm's block body: like [`Self::parse_block`], but
    /// also stops as soon as the current line looks like the start of
    /// another arm (see [`Parser::line_has_top_level_fat_arrow`]).
    pub(crate) fn parse_arm_block(&mut self) -> Block {
        let mut stmts = Vec::new();
        self.skip_stmt_seps();

        while !self.at(TokenKind::End)
            && !self.at(TokenKind::Eof)
            && !self.line_has_top_level_fat_arrow()
        {
            let checkpoint = self.pos;
            stmts.push(self.parse_stmt());
            self.skip_stmt_seps();
            self.ensure_progress(checkpoint);
        }

        stmts
    }

    pub(crate) fn parse_stmt(&mut self) -> StmtId {
        let start = self.span();

        match self.kind() {
            TokenKind::Let => self.parse_let(start),
            TokenKind::Return => self.parse_return(start),
            TokenKind::Break => {
                self.bump();
                self.ast.alloc_stmt(Stmt::Break, start)
            }
            TokenKind::Continue => {
                self.bump();
                self.ast.alloc_stmt(Stmt::Continue, start)
            }
            TokenKind::Throw => self.parse_throw(start),
            TokenKind::If => {
                let node = self.parse_if();
                let end = self.span_before_cursor(self.pos);
                self.ast
                    .alloc_stmt(Stmt::If(node), Span::merge(&start, &end))
            }
            TokenKind::While => self.parse_while(start),
            TokenKind::For => self.parse_for(start),
            TokenKind::Eof | TokenKind::Newline => {
                self.error_expected("a statement");
                self.recover_stmt(start)
            }
            _ => self.parse_expr_or_assign(start),
        }
    }

    fn recover_stmt(&mut self, start: Span) -> StmtId {
        self.synchronize_stmt();
        let unit = self.ast.alloc_expr(brasa_ast::Expr::Unit, start);
        self.ast.alloc_stmt(Stmt::Expr(unit), start)
    }

    pub(crate) fn parse_let_stmt_inner(&mut self) -> LetStmt {
        self.bump(); // 'let'
        let mutable = self.eat(TokenKind::Mut).is_some();
        let name = self.expect_ident_text("a variable name");

        let ty = if self.eat(TokenKind::Colon).is_some() {
            Some(self.parse_type())
        } else {
            None
        };

        self.expect(TokenKind::Eq, "'=' in let binding");
        let value = self.parse_expr();

        LetStmt {
            mutable,
            name,
            ty,
            value,
        }
    }

    fn parse_let(&mut self, start: Span) -> StmtId {
        let let_stmt = self.parse_let_stmt_inner();
        let end = self.span_before_cursor(self.pos);
        self.ast
            .alloc_stmt(Stmt::Let(let_stmt), Span::merge(&start, &end))
    }

    fn parse_return(&mut self, start: Span) -> StmtId {
        self.bump(); // 'return'

        let value = if self.starts_expr() {
            Some(self.parse_expr())
        } else {
            None
        };

        let end = self.span_before_cursor(self.pos);
        self.ast
            .alloc_stmt(Stmt::Return(value), Span::merge(&start, &end))
    }

    fn parse_throw(&mut self, start: Span) -> StmtId {
        self.bump(); // 'throw'
        let value = self.parse_expr();
        let end = self.span_before_cursor(self.pos);
        self.ast
            .alloc_stmt(Stmt::Throw(value), Span::merge(&start, &end))
    }

    /// A statement that starts with an expression: either a bare
    /// expression statement, or the left-hand side of an assignment.
    /// `=`/`+=`/... are not part of the expression grammar, so
    /// [`Self::parse_expr`] naturally stops right before one.
    fn parse_expr_or_assign(&mut self, start: Span) -> StmtId {
        let target = self.parse_expr();
        let target = self.maybe_apply_command_call(target, start);

        let op = match self.kind() {
            TokenKind::Eq => Some(AssignOp::Assign),
            TokenKind::PlusEq => Some(AssignOp::AddAssign),
            TokenKind::MinusEq => Some(AssignOp::SubAssign),
            TokenKind::StarEq => Some(AssignOp::MulAssign),
            TokenKind::SlashEq => Some(AssignOp::DivAssign),
            TokenKind::PercentEq => Some(AssignOp::RemAssign),
            _ => None,
        };

        let Some(op) = op else {
            let end = self.span_before_cursor(self.pos);
            return self
                .ast
                .alloc_stmt(Stmt::Expr(target), Span::merge(&start, &end));
        };

        self.bump();
        let value = self.parse_expr();
        let end = self.span_before_cursor(self.pos);

        self.ast.alloc_stmt(
            Stmt::Assign { target, op, value },
            Span::merge(&start, &end),
        )
    }

    /// The statement-position-only "command call" sugar: a bare `Expr::Ident`
    /// callee directly followed, on the same line and with no operator
    /// between, by one or more comma-separated expressions
    /// (`puts "hi"`, `puts a, b`) is read as a call with those expressions
    /// as arguments.
    ///
    /// Ruled scope: statement position only (this method, plus the
    /// equally statement-like inline `match`/`catch` arm body in
    /// `expr.rs`'s `parse_arm_body`). Everywhere else — call arguments,
    /// binary/pipe operands, `let` values, inline `if...then` branches —
    /// parentheses stay mandatory, since nothing there calls this;
    /// `let x = puts "a"` still fails to parse cleanly.
    pub(crate) fn maybe_apply_command_call(&mut self, expr: ExprId, start: Span) -> ExprId {
        if !matches!(self.ast.expr(expr), brasa_ast::Expr::Ident(_)) {
            return expr;
        }
        if !crate::expr::starts_command_call_arg(self.kind()) {
            return expr;
        }

        let mut args = vec![self.parse_expr()];
        while self.eat(TokenKind::Comma).is_some() {
            args.push(self.parse_expr());
        }

        let end = self.span_before_cursor(self.pos);
        self.ast.alloc_expr(
            brasa_ast::Expr::Call { callee: expr, args },
            Span::merge(&start, &end),
        )
    }

    fn parse_while(&mut self, start: Span) -> StmtId {
        self.bump(); // 'while'
        let cond = self.parse_expr();
        self.skip_stmt_seps();
        let body = self.parse_block(&[TokenKind::End]);
        let end_tok = self.expect(TokenKind::End, "'end' to close the while loop");
        let end = end_tok.map(|t| t.span).unwrap_or_else(|| self.span());

        self.ast
            .alloc_stmt(Stmt::While { cond, body }, Span::merge(&start, &end))
    }

    fn parse_for(&mut self, start: Span) -> StmtId {
        self.bump(); // 'for'
        let pattern = self.parse_pattern();
        self.expect(TokenKind::In, "'in' after the for-loop pattern");
        let iterable = self.parse_expr();
        self.skip_stmt_seps();
        let body = self.parse_block(&[TokenKind::End]);
        let end_tok = self.expect(TokenKind::End, "'end' to close the for loop");
        let end = end_tok.map(|t| t.span).unwrap_or_else(|| self.span());

        self.ast.alloc_stmt(
            Stmt::For {
                pattern,
                iterable,
                body,
            },
            Span::merge(&start, &end),
        )
    }

    /// Shared by `Stmt::If` and `Expr::If`, matching the grammar's
    /// "if expression vs statement: same node" resolution.
    ///
    /// The token right after the condition decides the surface form: a
    /// `then` starts the inline, single-expression-branch form; anything
    /// else is treated as the block form (which itself requires a
    /// newline before the body).
    pub(crate) fn parse_if(&mut self) -> IfNode {
        self.bump(); // 'if'
        let cond = self.parse_expr();

        if self.at(TokenKind::Then) {
            self.parse_if_inline(cond)
        } else {
            self.parse_if_block(cond)
        }
    }

    fn wrap_single_stmt(&mut self, value: ExprId) -> Block {
        let span = self.ast.span_of_expr(value);
        vec![self.ast.alloc_stmt(Stmt::Expr(value), span)]
    }

    fn parse_if_inline(&mut self, cond: ExprId) -> IfNode {
        self.expect(TokenKind::Then, "'then'");
        let then_expr = self.parse_expr();
        let then_block = self.wrap_single_stmt(then_expr);
        let mut branches = vec![(cond, then_block)];

        loop {
            if self.eat(TokenKind::Elsif).is_none() {
                break;
            }
            let elsif_cond = self.parse_expr();
            self.expect(TokenKind::Then, "'then'");
            let elsif_expr = self.parse_expr();
            let elsif_block = self.wrap_single_stmt(elsif_expr);
            branches.push((elsif_cond, elsif_block));
        }

        let else_ = if self.eat(TokenKind::Else).is_some() {
            let else_expr = self.parse_expr();
            Some(self.wrap_single_stmt(else_expr))
        } else {
            None
        };

        self.expect(TokenKind::End, "'end' to close the if expression");
        IfNode { branches, else_ }
    }

    fn parse_if_block(&mut self, cond: ExprId) -> IfNode {
        self.skip_stmt_seps();
        let body = self.parse_block(&[TokenKind::Elsif, TokenKind::Else, TokenKind::End]);
        let mut branches = vec![(cond, body)];

        loop {
            if self.eat(TokenKind::Elsif).is_none() {
                break;
            }
            let elsif_cond = self.parse_expr();
            self.skip_stmt_seps();
            let elsif_body = self.parse_block(&[TokenKind::Elsif, TokenKind::Else, TokenKind::End]);
            branches.push((elsif_cond, elsif_body));
        }

        let else_ = if self.eat(TokenKind::Else).is_some() {
            self.skip_stmt_seps();
            Some(self.parse_block(&[TokenKind::End]))
        } else {
            None
        };

        self.expect(TokenKind::End, "'end' to close the if statement");
        IfNode { branches, else_ }
    }

    /// Whether the current token can plausibly start an expression, used
    /// to decide if a bare `return` carries a value.
    fn starts_expr(&self) -> bool {
        !matches!(
            self.kind(),
            TokenKind::Newline
                | TokenKind::Eof
                | TokenKind::End
                | TokenKind::Elsif
                | TokenKind::Else
        )
    }
}
