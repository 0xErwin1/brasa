//! Type expressions (the `type` grammar production).

use brasa_ast::{TypeExpr, TypeExprId};
use brasa_source::Span;
use brasa_token::TokenKind;

use crate::Parser;

impl<'a> Parser<'a> {
    /// `type = TYPE_IDENT ("<" type ("," type)* ">")? | "(" type ("," type)* ")" | fn_type`.
    ///
    /// The grammar's lexicon only allows `TYPE_IDENT` (uppercase) as a
    /// type name, but `docs/spec/01-syntax.md` uses lowercase primitives
    /// (`int`, `float`, `bool`, `string`, `char`, `unit`) everywhere,
    /// including in the examples this parser must accept. This resolves
    /// that inconsistency by accepting either an `Ident` or a `TypeIdent`
    /// as a type name.
    pub(crate) fn parse_type(&mut self) -> TypeExprId {
        if !self.enter_recursion() {
            let span = self.span();
            let bail = self.ast.alloc_type_expr(
                TypeExpr::Named {
                    name: "<error>".to_string(),
                    args: Vec::new(),
                },
                span,
            );
            self.exit_recursion();
            return bail;
        }

        let result = match self.kind() {
            // `unit` lexes as its own keyword (`TokenKind::Unit`), not as
            // a lowercase `Ident`, so it needs its own arm even though it
            // is otherwise an ordinary named primitive type.
            TokenKind::Ident | TokenKind::TypeIdent | TokenKind::Unit => self.parse_named_type(),
            TokenKind::LParen => self.parse_paren_or_fn_type(),
            _ => {
                let span = self.span();
                self.error_expected("a type");
                self.ast.alloc_type_expr(
                    TypeExpr::Named {
                        name: "<error>".to_string(),
                        args: Vec::new(),
                    },
                    span,
                )
            }
        };

        self.exit_recursion();
        result
    }

    fn parse_named_type(&mut self) -> TypeExprId {
        let start = self.span();
        let first = self.slice().to_string();
        let first_kind = self.kind();
        self.bump();

        // `lib.Point`: a module stem is a file stem, which lexes as a
        // lowercase `Ident`, so only an `Ident` can carry a qualifier.
        // `int.Foo` would be nonsense and `Point.Foo` names nothing —
        // neither is a shape the grammar has to accept. The path is kept
        // as one name; see `brasa_ast::TypeExpr::Named`.
        let name = if first_kind == TokenKind::Ident && self.at(TokenKind::Dot) {
            self.bump();
            let member = self.expect_type_name("a type name after the module");
            format!("{first}.{member}")
        } else {
            first
        };

        let mut args = Vec::new();
        let mut end = self.prev_span();

        if self.eat(TokenKind::Lt).is_some() {
            while !self.at(TokenKind::Gt) && !self.at(TokenKind::Eof) {
                let checkpoint = self.pos;
                args.push(self.parse_type());
                if self.eat(TokenKind::Comma).is_none() {
                    break;
                }
                self.ensure_progress(checkpoint);
            }
            if let Some(tok) = self.expect(TokenKind::Gt, "'>' to close generic arguments") {
                end = tok.span;
            }
        }

        self.ast
            .alloc_type_expr(TypeExpr::Named { name, args }, Span::merge(&start, &end))
    }

    /// A type name in either lexical class, for the same reason
    /// [`Self::parse_type`] accepts both: the grammar says `TYPE_IDENT`
    /// but the spec's own primitives are lowercase.
    fn expect_type_name(&mut self, what: &str) -> String {
        if self.at(TokenKind::Ident) || self.at(TokenKind::TypeIdent) || self.at(TokenKind::Unit) {
            let name = self.slice().to_string();
            self.bump();
            return name;
        }

        self.error_expected(what);
        "<error>".to_string()
    }

    /// Disambiguates a tuple type from a function type: both start with
    /// `"(" (type ("," type)*)? ")"`, and only a following `->` decides
    /// which one it is.
    fn parse_paren_or_fn_type(&mut self) -> TypeExprId {
        let start = self.span();
        self.bump(); // '('

        let mut elements = Vec::new();

        while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
            let checkpoint = self.pos;
            elements.push(self.parse_type());
            if self.eat(TokenKind::Comma).is_none() {
                break;
            }
            self.ensure_progress(checkpoint);
        }

        let close = self.expect(TokenKind::RParen, "')' to close the type");

        if self.eat(TokenKind::Arrow).is_some() {
            let ret = self.parse_type();
            let end = self.ast.span_of_type_expr(ret);
            self.ast.alloc_type_expr(
                TypeExpr::Fn {
                    params: elements,
                    ret,
                },
                Span::merge(&start, &end),
            )
        } else {
            let end = close.map(|t| t.span).unwrap_or(start);
            self.ast
                .alloc_type_expr(TypeExpr::Tuple(elements), Span::merge(&start, &end))
        }
    }
}
