//! Expressions: the Pratt binary/unary ladder, postfix chains (call,
//! index, field, safe-nav, catch, trailing `do`-blocks), primaries
//! (literals, collections, lambdas, `if`/`match`), and the pipe operator.

use brasa_ast::{
    ArmBody, BinaryOp, CatchArm, CatchType, Expr, ExprId, LambdaBody, LambdaParam, MatchArm,
    PipeTarget, StringPart, UnaryOp,
};
use brasa_source::Span;
use brasa_token::TokenKind;

use crate::Parser;

/// `(left_bp, right_bp)` for each binary operator at precedence levels
/// 2-10 of `docs/spec/02-grammar.md`. `**` is the only right-associative
/// entry (its right bp is lower than its left bp); ranges are
/// non-associative and use equal bps plus an explicit check in
/// [`Parser::parse_bp`] that rejects chaining.
///
/// `??` (level 2) is left-associative like every other entry here and
/// builds `Expr::Coalesce` in [`Parser::parse_bp`], the same way the
/// other entries build `Expr::Binary`.
fn binding_power(kind: TokenKind) -> Option<(u8, u8)> {
    use TokenKind::*;

    Some(match kind {
        QuestionQuestion => (20, 21),
        OrOr | Or => (30, 31),
        AndAnd | And => (40, 41),
        EqEq | NotEq => (50, 51),
        Lt | LtEq | Gt | GtEq => (60, 61),
        DotDot | DotDotEq => (70, 70),
        Plus | Minus => (80, 81),
        Star | Slash | Percent => (90, 91),
        StarStar => (101, 100),
        _ => return None,
    })
}

/// Binding power an operand of a unary prefix operator (`-`, `!`, `not`)
/// is parsed at. Per the grammar's own ordering (level 10 for `**`, level
/// 11 for unary), unary binds *tighter* than `**`: `-x ** 2` parses as
/// `(-x) ** 2`, not `-(x ** 2)`. Set higher than every binary left bp so
/// no binary operator is absorbed into the unary operand.
const UNARY_BP: u8 = 105;

fn binary_op(kind: TokenKind) -> Option<BinaryOp> {
    use TokenKind::*;

    Some(match kind {
        Plus => BinaryOp::Add,
        Minus => BinaryOp::Sub,
        Star => BinaryOp::Mul,
        Slash => BinaryOp::Div,
        Percent => BinaryOp::Rem,
        StarStar => BinaryOp::Pow,
        EqEq => BinaryOp::Eq,
        NotEq => BinaryOp::NotEq,
        Lt => BinaryOp::Lt,
        LtEq => BinaryOp::LtEq,
        Gt => BinaryOp::Gt,
        GtEq => BinaryOp::GtEq,
        AndAnd | And => BinaryOp::And,
        OrOr | Or => BinaryOp::Or,
        _ => return None,
    })
}

pub(crate) fn is_bare_ident_callee(ast: &brasa_ast::Ast, expr: ExprId) -> bool {
    matches!(ast.expr(expr), Expr::Ident(_))
}

/// Whether `kind` can start a fresh primary expression that, appearing
/// directly after a bare identifier with no operator between them, reads
/// as a parenless "command call" (`puts "hello"` ≡ `puts("hello")`,
/// Ruby-style, extended to `puts a, b` for more than one argument).
///
/// This is not in `docs/spec/02-grammar.md`'s formal grammar, which
/// states plainly that "parentheses are mandatory in calls... there is
/// no call without parentheses". It is applied anyway (ruled: statement
/// position only, see `Parser::maybe_apply_command_call` in `stmt.rs`)
/// because every one of the bundled `examples/*.brs` fixtures (and every
/// `puts ...` line in `docs/spec/01-syntax.md`) relies on exactly this
/// shape and could not otherwise parse. Scoping it to statement position
/// keeps `(...)` mandatory everywhere else: `let x = puts "a"` still
/// fails to parse cleanly, since nothing in expression position ever
/// calls this.
pub(crate) fn starts_command_call_arg(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Int
            | TokenKind::Float
            | TokenKind::Char
            | TokenKind::StringStart
            | TokenKind::RawStringStart
            | TokenKind::True
            | TokenKind::False
            | TokenKind::Unit
            | TokenKind::SelfKw
            | TokenKind::Ident
            | TokenKind::TypeIdent
            | TokenKind::LBrace
            | TokenKind::Pipe
            | TokenKind::Minus
            | TokenKind::Bang
            | TokenKind::Not
            | TokenKind::If
            | TokenKind::Match
    )
}

impl<'a> Parser<'a> {
    /// `expr = pipe_expr`. The full entry point used everywhere an
    /// expression is expected, including inside string interpolation.
    pub(crate) fn parse_expr(&mut self) -> ExprId {
        self.parse_pipe()
    }

    /// `pipe_expr = coalesce ( "|>" pipe_target )*`.
    fn parse_pipe(&mut self) -> ExprId {
        let start = self.ast_start_span();
        let mut lhs = self.parse_bp(0);

        loop {
            self.try_continue_across_newlines();
            if !self.at(TokenKind::PipeGt) {
                break;
            }
            self.bump();
            let target = self.parse_pipe_target();
            let end = self.span_before_cursor(self.pos);
            lhs = self
                .ast
                .alloc_expr(Expr::Pipe { lhs, target }, Span::merge(&start, &end));
        }

        lhs
    }

    fn parse_pipe_target(&mut self) -> PipeTarget {
        if self.eat(TokenKind::Dot).is_some() {
            let name = self.expect_ident_text("a method name");
            let args = if self.at(TokenKind::LParen) {
                self.parse_call_args()
            } else {
                self.error_expected("'(' after a pipe method target");
                Vec::new()
            };
            PipeTarget::Method { name, args }
        } else {
            PipeTarget::Call(self.parse_postfix())
        }
    }

    /// The Pratt loop for binary operators at precedence levels 2-10.
    ///
    /// This is the single reentry point for the whole expression mutual
    /// recursion cluster (parens, vectors, maps, call arguments, lambda
    /// bodies, `if`/`match` branches, ...): every one of those constructs
    /// reaches a fresh primary through [`Self::parse_expr`], which always
    /// comes back through here. Guarding recursion here alone is enough
    /// to bound the whole cluster's native stack usage.
    fn parse_bp(&mut self, min_bp: u8) -> ExprId {
        if !self.enter_recursion() {
            let span = self.span();
            let bail = self.ast.alloc_expr(Expr::Unit, span);
            self.exit_recursion();
            return bail;
        }

        let start = self.ast_start_span();
        let mut lhs = self.parse_unary();
        let mut just_built_range = false;

        loop {
            self.try_continue_across_newlines();
            let kind = self.kind();

            let Some((lbp, rbp)) = binding_power(kind) else {
                break;
            };
            if lbp < min_bp {
                break;
            }

            let is_range = matches!(kind, TokenKind::DotDot | TokenKind::DotDotEq);
            if is_range && just_built_range {
                self.error_at(
                    self.span(),
                    "ranges are non-associative: use parentheses to chain them".to_string(),
                );
                self.bump();
                self.parse_bp(rbp + 1);
                continue;
            }

            self.bump();
            // Ranges are non-associative: the right-hand side is parsed
            // one binding power above `rbp` so a second `..`/`..=` is
            // never silently absorbed into it (which would make `a..b..c`
            // parse as `a..(b..c)` instead of being rejected below).
            let rhs_min_bp = if is_range { rbp + 1 } else { rbp };
            let rhs = self.parse_bp(rhs_min_bp);
            let end = self.span_before_cursor(self.pos);

            lhs = if is_range {
                let inclusive = kind == TokenKind::DotDotEq;
                self.ast.alloc_expr(
                    Expr::Range {
                        lo: lhs,
                        hi: rhs,
                        inclusive,
                    },
                    Span::merge(&start, &end),
                )
            } else if kind == TokenKind::QuestionQuestion {
                self.ast
                    .alloc_expr(Expr::Coalesce { lhs, rhs }, Span::merge(&start, &end))
            } else {
                let op = binary_op(kind).expect("checked by binding_power above");
                self.ast
                    .alloc_expr(Expr::Binary { op, lhs, rhs }, Span::merge(&start, &end))
            };

            just_built_range = is_range;
        }

        self.exit_recursion();
        lhs
    }

    fn parse_unary(&mut self) -> ExprId {
        let start = self.span();

        let op = match self.kind() {
            TokenKind::Minus => Some(UnaryOp::Neg),
            TokenKind::Bang | TokenKind::Not => Some(UnaryOp::Not),
            _ => None,
        };

        let Some(op) = op else {
            return self.parse_postfix();
        };

        self.bump();
        let operand = self.parse_bp(UNARY_BP);
        let end = self.span_before_cursor(self.pos);

        self.ast
            .alloc_expr(Expr::Unary { op, operand }, Span::merge(&start, &end))
    }

    /// `postfix = primary ( "(" args? ")" | "[" expr "]" | "." IDENT
    /// | "?." IDENT | catch_clause )*`, extended with the trailing
    /// `do`-block exception: a `do |params| ... end` directly after a
    /// callable postfix result (a bare name, a field/method name, or an
    /// already-parenthesized call) appends the block as that call's last
    /// argument, creating the call if one didn't already exist.
    fn parse_postfix(&mut self) -> ExprId {
        let start = self.ast_start_span();
        let mut expr = self.parse_primary();

        loop {
            self.try_continue_across_newlines();

            match self.kind() {
                TokenKind::LParen => {
                    let args = self.parse_call_args_with_optional_do();
                    let end = self.span_before_cursor(self.pos);
                    expr = self
                        .ast
                        .alloc_expr(Expr::Call { callee: expr, args }, Span::merge(&start, &end));
                }
                TokenKind::LBracket => {
                    self.bump();
                    let index = self.parse_expr();
                    self.expect(TokenKind::RBracket, "']' to close the index expression");
                    let end = self.span_before_cursor(self.pos);
                    expr = self
                        .ast
                        .alloc_expr(Expr::Index { recv: expr, index }, Span::merge(&start, &end));
                }
                TokenKind::Dot => {
                    self.bump();
                    let name = self.expect_ident_text("a field or method name");

                    if self.at(TokenKind::LParen) {
                        let callee = self.ast.alloc_expr(
                            Expr::Field { recv: expr, name },
                            Span::merge(&start, &self.span_before_cursor(self.pos)),
                        );
                        let args = self.parse_call_args_with_optional_do();
                        let end = self.span_before_cursor(self.pos);
                        expr = self
                            .ast
                            .alloc_expr(Expr::Call { callee, args }, Span::merge(&start, &end));
                    } else if self.at(TokenKind::Do) {
                        let callee = self.ast.alloc_expr(
                            Expr::Field { recv: expr, name },
                            Span::merge(&start, &self.span_before_cursor(self.pos)),
                        );
                        let args = self.parse_do_only_call_args();
                        let end = self.span_before_cursor(self.pos);
                        expr = self
                            .ast
                            .alloc_expr(Expr::Call { callee, args }, Span::merge(&start, &end));
                    } else {
                        let end = self.span_before_cursor(self.pos);
                        expr = self.ast.alloc_expr(
                            Expr::Field { recv: expr, name },
                            Span::merge(&start, &end),
                        );
                    }
                }
                TokenKind::QuestionDot => {
                    self.bump();
                    let name = self.expect_ident_text("a field or method name");

                    let args = if self.at(TokenKind::LParen) {
                        Some(self.parse_call_args_with_optional_do())
                    } else if self.at(TokenKind::Do) {
                        Some(self.parse_do_only_call_args())
                    } else {
                        None
                    };

                    let end = self.span_before_cursor(self.pos);
                    expr = self.ast.alloc_expr(
                        Expr::SafeNav {
                            recv: expr,
                            name,
                            args,
                        },
                        Span::merge(&start, &end),
                    );
                }
                TokenKind::Catch | TokenKind::CatchAll => {
                    expr = self.parse_catch(expr, start);
                }
                TokenKind::Do if is_bare_ident_callee(&self.ast, expr) => {
                    let args = self.parse_do_only_call_args();
                    let end = self.span_before_cursor(self.pos);
                    expr = self
                        .ast
                        .alloc_expr(Expr::Call { callee: expr, args }, Span::merge(&start, &end));
                }
                _ => break,
            }
        }

        expr
    }

    /// `'(' args ')'`, followed by an optional trailing `do ... end` lambda
    /// appended as one more argument. Shared by every call-like postfix
    /// production (`f(...)`, `.m(...)`, `?.m(...)`) so the "call then
    /// maybe-do" shape lives in one place.
    fn parse_call_args_with_optional_do(&mut self) -> Vec<ExprId> {
        let mut args = self.parse_call_args();
        if self.at(TokenKind::Do) {
            args.push(self.parse_trailing_do_lambda());
        }
        args
    }

    /// The parenthesis-less call shape: just a trailing `do ... end`
    /// lambda as the sole argument (`.m do ... end`, `f do ... end`).
    fn parse_do_only_call_args(&mut self) -> Vec<ExprId> {
        vec![self.parse_trailing_do_lambda()]
    }

    fn parse_call_args(&mut self) -> Vec<ExprId> {
        self.bump(); // '('

        let mut args = Vec::new();
        while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
            let checkpoint = self.pos;
            args.push(self.parse_expr());
            if self.eat(TokenKind::Comma).is_none() {
                break;
            }
            self.ensure_progress(checkpoint);
        }

        self.expect(TokenKind::RParen, "')' to close the argument list");
        args
    }

    fn parse_trailing_do_lambda(&mut self) -> ExprId {
        let start = self.span();
        let (params, body) = self.parse_do_lambda_body();
        let end = self.span_before_cursor(self.pos);

        self.ast.alloc_expr(
            Expr::Lambda {
                params,
                body: LambdaBody::Block(body),
            },
            Span::merge(&start, &end),
        )
    }

    /// `"do" "|" lparams? "|" NL block "end"`. Per the ambiguity note in
    /// `docs/spec/02-grammar.md` ("a parameterless lambda uses `do ...
    /// end` or `|_|`"), the `|params|` delimiters themselves are treated
    /// as optional when there are no parameters, accepting both `do NL
    /// block end` and `do || NL block end`-style spellings.
    fn parse_do_lambda_body(&mut self) -> (Vec<LambdaParam>, brasa_ast::Block) {
        self.bump(); // 'do'
        let params = self.parse_lambda_params_if_present();
        self.skip_stmt_seps();
        let body = self.parse_block(&[TokenKind::End]);
        self.expect(TokenKind::End, "'end' to close the do-block");
        (params, body)
    }

    fn parse_lambda_params_if_present(&mut self) -> Vec<LambdaParam> {
        if self.eat(TokenKind::Pipe).is_none() {
            return Vec::new();
        }

        let mut params = Vec::new();

        while !self.at(TokenKind::Pipe) && !self.at(TokenKind::Eof) {
            let checkpoint = self.pos;
            params.push(self.parse_lambda_param());
            if self.eat(TokenKind::Comma).is_none() {
                break;
            }
            self.ensure_progress(checkpoint);
        }

        self.expect(TokenKind::Pipe, "'|' to close the lambda parameters");
        params
    }

    fn parse_lambda_param(&mut self) -> LambdaParam {
        let name = if self.eat(TokenKind::Underscore).is_some() {
            "_".to_string()
        } else {
            self.expect_ident_text("a lambda parameter name")
        };

        let ty = if self.eat(TokenKind::Colon).is_some() {
            Some(self.parse_type())
        } else {
            None
        };

        LambdaParam { name, ty }
    }

    fn parse_primary(&mut self) -> ExprId {
        let start = self.span();

        match self.kind() {
            TokenKind::Int => {
                let value = brasa_token::parse_int(self.slice()).unwrap_or_else(|err| {
                    let message = match err {
                        brasa_token::IntParseError::NoDigits => {
                            "integer literal has no digits after its `0x`/`0b` prefix"
                        }
                        brasa_token::IntParseError::Overflow => "integer literal out of range",
                    };
                    self.error_at(start, message.to_string());
                    0
                });
                self.bump();
                self.ast.alloc_expr(Expr::Int(value), start)
            }
            TokenKind::Float => {
                let value = brasa_token::parse_float(self.slice()).unwrap_or_default();
                self.bump();
                self.ast.alloc_expr(Expr::Float(value), start)
            }
            TokenKind::True | TokenKind::False => {
                let value = self.at(TokenKind::True);
                self.bump();
                self.ast.alloc_expr(Expr::Bool(value), start)
            }
            TokenKind::Char => {
                let (value, unknown_escape) = crate::pattern::parse_char_literal(self.slice());
                self.report_char_unknown_escape(start, unknown_escape);
                self.bump();
                self.ast.alloc_expr(Expr::Char(value), start)
            }
            TokenKind::Unit => {
                self.bump();
                self.ast.alloc_expr(Expr::Unit, start)
            }
            TokenKind::SelfKw => {
                self.bump();
                self.ast.alloc_expr(Expr::SelfExpr, start)
            }
            TokenKind::StringStart | TokenKind::RawStringStart => self.parse_interpolated_string(),
            TokenKind::Do => {
                let (params, body) = self.parse_do_lambda_body();
                let end = self.span_before_cursor(self.pos);
                self.ast.alloc_expr(
                    Expr::Lambda {
                        params,
                        body: LambdaBody::Block(body),
                    },
                    Span::merge(&start, &end),
                )
            }
            TokenKind::Ident => {
                let name = self.slice().to_string();
                self.bump();
                self.ast.alloc_expr(Expr::Ident(name), start)
            }
            TokenKind::TypeIdent => self.parse_type_ident_primary(),
            TokenKind::LParen => self.parse_group(),
            TokenKind::LBracket => self.parse_vector_lit(),
            TokenKind::LBrace => self.parse_map_lit(),
            TokenKind::Pipe => self.parse_lambda_expr(),
            TokenKind::If => {
                let node = self.parse_if();
                let end = self.span_before_cursor(self.pos);
                self.ast
                    .alloc_expr(Expr::If(node), Span::merge(&start, &end))
            }
            TokenKind::Match => self.parse_match(),
            _ => {
                self.error_expected("an expression");
                self.synchronize_stmt();
                self.ast.alloc_expr(Expr::Unit, start)
            }
        }
    }

    fn parse_type_ident_primary(&mut self) -> ExprId {
        let start = self.span();
        let name = self.slice().to_string();
        self.bump();

        if self.at(TokenKind::LParen) {
            let args = self.parse_call_args();
            let end = self.span_before_cursor(self.pos);
            self.ast
                .alloc_expr(Expr::EnumCtor { name, args }, Span::merge(&start, &end))
        } else if self.at(TokenKind::LBrace) {
            self.bump();
            let mut fields = Vec::new();
            let mut seen_fields = std::collections::HashSet::new();

            while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
                let checkpoint = self.pos;
                let field_start = self.span();
                let field_name = self.expect_ident_text("a field name");
                // A repeated field is recovery-friendly, not fatal: report
                // it and keep both occurrences in the AST (the checker's
                // problem, not the parser's, to decide which value wins).
                if !seen_fields.insert(field_name.clone()) {
                    self.error_at(
                        field_start,
                        format!("duplicate field `{field_name}` in struct literal"),
                    );
                }
                self.expect(TokenKind::Colon, "':' before the field value");
                let value = self.parse_expr();
                fields.push((field_name, value));
                if self.eat(TokenKind::Comma).is_none() {
                    break;
                }
                self.ensure_progress(checkpoint);
            }

            self.expect(TokenKind::RBrace, "'}' to close the struct literal");
            let end = self.span_before_cursor(self.pos);
            self.ast.alloc_expr(
                Expr::StructLit {
                    type_name: name,
                    fields,
                },
                Span::merge(&start, &end),
            )
        } else {
            self.ast.alloc_expr(
                Expr::EnumCtor {
                    name,
                    args: Vec::new(),
                },
                start,
            )
        }
    }

    fn parse_group(&mut self) -> ExprId {
        self.bump(); // '('
        let inner = self.parse_expr();
        self.expect(
            TokenKind::RParen,
            "')' to close the parenthesized expression",
        );
        inner
    }

    fn parse_vector_lit(&mut self) -> ExprId {
        let start = self.span();
        self.bump(); // '['

        let mut elements = Vec::new();
        while !self.at(TokenKind::RBracket) && !self.at(TokenKind::Eof) {
            let checkpoint = self.pos;
            elements.push(self.parse_expr());
            if self.eat(TokenKind::Comma).is_none() {
                break;
            }
            self.ensure_progress(checkpoint);
        }

        let close = self.expect(TokenKind::RBracket, "']' to close the vector literal");
        let end = close.map(|t| t.span).unwrap_or(start);
        self.ast
            .alloc_expr(Expr::VectorLit(elements), Span::merge(&start, &end))
    }

    fn parse_map_lit(&mut self) -> ExprId {
        let start = self.span();
        self.bump(); // '{'

        let mut entries = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            let checkpoint = self.pos;
            let key = self.parse_expr();
            self.expect(TokenKind::Colon, "':' between a map key and its value");
            let value = self.parse_expr();
            entries.push((key, value));
            if self.eat(TokenKind::Comma).is_none() {
                break;
            }
            self.ensure_progress(checkpoint);
        }

        let close = self.expect(TokenKind::RBrace, "'}' to close the map literal");
        let end = close.map(|t| t.span).unwrap_or(start);
        self.ast
            .alloc_expr(Expr::MapLit(entries), Span::merge(&start, &end))
    }

    /// `lambda = "|" lparams? "|" expr` (the non-`do` form).
    fn parse_lambda_expr(&mut self) -> ExprId {
        let start = self.span();
        let params = self.parse_lambda_params_if_present();
        let body = self.parse_expr();
        let end = self.span_before_cursor(self.pos);

        self.ast.alloc_expr(
            Expr::Lambda {
                params,
                body: LambdaBody::Expr(body),
            },
            Span::merge(&start, &end),
        )
    }

    fn parse_match(&mut self) -> ExprId {
        let start = self.span();
        self.bump(); // 'match'

        let scrutinee = self.parse_expr();
        self.skip_stmt_seps();

        let mut arms = Vec::new();
        while !self.at(TokenKind::End) && !self.at(TokenKind::Eof) {
            let checkpoint = self.pos;
            let pattern = self.parse_pattern();
            let guard = if self.eat(TokenKind::If).is_some() {
                Some(self.parse_expr())
            } else {
                None
            };
            self.expect(TokenKind::FatArrow, "'=>' in match arm");
            let body = self.parse_arm_body();
            arms.push(MatchArm {
                pattern,
                guard,
                body,
            });
            self.skip_stmt_seps();
            self.ensure_progress(checkpoint);
        }

        let end_tok = self.expect(TokenKind::End, "'end' to close the match expression");
        let end = end_tok.map(|t| t.span).unwrap_or_else(|| self.span());

        self.ast
            .alloc_expr(Expr::Match { scrutinee, arms }, Span::merge(&start, &end))
    }

    /// `( expr | NL block )`, shared by match and catch arms.
    /// `( expr | NL block )`. `throw`/`return`/`break`/`continue` are
    /// statements, not expressions, but the inline single-line arm form
    /// has no separate "statement" spelling; a bare `throw`/etc. right
    /// after `=>` is normalized into a one-statement block, the same way
    /// `docs/spec/02-grammar.md`'s inline `if...then` branches are
    /// normalized into one-statement blocks (`brasa_ast::stmt::IfNode`'s
    /// docs).
    fn parse_arm_body(&mut self) -> ArmBody {
        if self.at(TokenKind::Newline) {
            self.skip_stmt_seps();
            ArmBody::Block(self.parse_arm_block())
        } else if matches!(
            self.kind(),
            TokenKind::Throw | TokenKind::Return | TokenKind::Break | TokenKind::Continue
        ) {
            ArmBody::Block(vec![self.parse_stmt()])
        } else {
            let start = self.span();
            let value = self.parse_expr();
            // An inline arm body is a single "statement slot" the same
            // way a top-level statement is (see the ruling this mirrors
            // in `stmt.rs`'s `maybe_apply_command_call`), so it gets the
            // same command-call sugar: `None => puts "no nickname"`.
            let value = self.maybe_apply_command_call(value, start);
            ArmBody::Expr(value)
        }
    }

    /// `catch_clause = ("catch" | "catch_all") "(" IDENT ")" NL
    /// catch_arm+ "end"`.
    fn parse_catch(&mut self, subject: ExprId, start: Span) -> ExprId {
        let exhaustive = self.at(TokenKind::CatchAll);
        self.bump(); // 'catch' or 'catch_all'

        self.expect(TokenKind::LParen, "'(' before the catch binding");
        let binding = self.expect_ident_text("a catch binding name");
        self.expect(TokenKind::RParen, "')' after the catch binding");
        self.skip_stmt_seps();

        let mut arms = Vec::new();
        while !self.at(TokenKind::End) && !self.at(TokenKind::Eof) {
            let checkpoint = self.pos;
            let types = self.parse_catch_types();
            let guard = if self.eat(TokenKind::If).is_some() {
                Some(self.parse_expr())
            } else {
                None
            };
            self.expect(TokenKind::FatArrow, "'=>' in catch arm");
            let body = self.parse_arm_body();
            arms.push(CatchArm { types, guard, body });
            self.skip_stmt_seps();
            self.ensure_progress(checkpoint);
        }

        let end_tok = self.expect(TokenKind::End, "'end' to close the catch clause");
        let end = end_tok.map(|t| t.span).unwrap_or_else(|| self.span());

        self.ast.alloc_expr(
            Expr::Catch {
                subject,
                exhaustive,
                binding,
                arms,
            },
            Span::merge(&start, &end),
        )
    }

    /// `catch_types = ( TYPE_IDENT | "_" ) ( "|" TYPE_IDENT )*`, where
    /// each `TYPE_IDENT` may in practice be a dotted path (e.g.
    /// `panics.IndexOutOfBounds`, `fs.NotFound`): the grammar names this
    /// slot `TYPE_IDENT` but built-in error namespaces are lowercase
    /// modules qualifying an uppercase error name. [`Self::parse_qualified_type_name`]
    /// accepts both a bare `TYPE_IDENT` and such a dotted path.
    fn parse_catch_types(&mut self) -> Vec<CatchType> {
        let mut types = vec![self.parse_one_catch_type()];

        while self.eat(TokenKind::Pipe).is_some() {
            types.push(self.parse_one_catch_type());
        }

        types
    }

    fn parse_one_catch_type(&mut self) -> CatchType {
        if self.eat(TokenKind::Underscore).is_some() {
            CatchType::Wildcard
        } else {
            CatchType::Named(self.parse_qualified_type_name())
        }
    }

    fn parse_qualified_type_name(&mut self) -> String {
        let mut segments = vec![self.parse_one_name_segment()];

        while self.at(TokenKind::Dot) {
            let next_is_name = matches!(
                self.tokens.get(self.pos + 1).map(|t| t.kind),
                Some(TokenKind::Ident) | Some(TokenKind::TypeIdent)
            );
            if !next_is_name {
                break;
            }
            self.bump(); // '.'
            segments.push(self.parse_one_name_segment());
        }

        segments.join(".")
    }

    fn parse_one_name_segment(&mut self) -> String {
        if self.at(TokenKind::Ident) || self.at(TokenKind::TypeIdent) {
            let text = self.slice().to_string();
            self.bump();
            text
        } else {
            self.error_expected("a type name");
            "<error>".to_string()
        }
    }

    /// Parses a full interpolated string (`"..."` or `"""..."""`) from
    /// its opening token to its closing `StringEnd`, reentering
    /// [`Self::parse_expr`] for each `#{...}` interpolation.
    fn parse_interpolated_string(&mut self) -> ExprId {
        let start = self.span();
        let raw = self.at(TokenKind::RawStringStart);
        self.bump(); // opening quote

        let mut parts = Vec::new();

        loop {
            match self.kind() {
                TokenKind::StringText => {
                    let tok = self.bump();
                    let slice = &self.source[tok.span.start.0 as usize..tok.span.end.0 as usize];
                    let text = if raw {
                        slice.to_string()
                    } else {
                        let (text, unknown) = brasa_token::unescape_string_text_checked(slice);
                        self.report_unknown_escapes(tok.span, &unknown);
                        text
                    };
                    parts.push(StringPart::Text { text, raw });
                }
                TokenKind::InterpStart => {
                    self.bump();
                    let value = self.parse_expr();
                    self.expect(TokenKind::InterpEnd, "'}' to close the interpolation");
                    parts.push(StringPart::Interp(value));
                }
                TokenKind::StringEnd => {
                    self.bump();
                    break;
                }
                _ => {
                    self.error_expected("string content or the closing quote");
                    break;
                }
            }
        }

        let end = self.span_before_cursor(self.pos);
        self.ast
            .alloc_expr(Expr::StringLit { parts }, Span::merge(&start, &end))
    }

    /// Parses a string literal with no interpolation allowed, used by
    /// `import "path"` and by string patterns. Any `#{` inside is
    /// reported and its expression is parsed and discarded, so the
    /// cursor still ends up past the whole literal.
    pub(crate) fn parse_plain_string(&mut self) -> (String, Span) {
        let start = self.span();
        self.bump(); // opening quote

        let mut text = String::new();

        loop {
            match self.kind() {
                TokenKind::StringText => {
                    let tok = self.bump();
                    text.push_str(&self.source[tok.span.start.0 as usize..tok.span.end.0 as usize]);
                }
                TokenKind::InterpStart => {
                    let span = self.span();
                    self.error_at(span, "interpolation is not allowed here".to_string());
                    self.bump();
                    let _ = self.parse_expr();
                    self.expect(TokenKind::InterpEnd, "'}' to close the interpolation");
                }
                TokenKind::StringEnd => {
                    self.bump();
                    break;
                }
                _ => {
                    self.error_expected("string content or the closing quote");
                    break;
                }
            }
        }

        let end = self.span_before_cursor(self.pos);
        (text, Span::merge(&start, &end))
    }

    /// The starting span used to build a node covering the token about to
    /// be parsed; a plain read of the current token's span.
    fn ast_start_span(&self) -> Span {
        self.span()
    }

    /// Reports every `UnknownEscape` found in a `StringText` token, one
    /// diagnostic per escape at its exact span (see
    /// [`Parser::report_unknown_escape`]).
    fn report_unknown_escapes(&mut self, text_span: Span, unknown: &[brasa_token::UnknownEscape]) {
        for esc in unknown {
            let backslash_start = text_span.start.0 + esc.offset;
            let span = crate::escape_span(text_span.file, backslash_start, esc.escaped);
            self.report_unknown_escape(span, esc.escaped);
        }
    }
}
