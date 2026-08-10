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
        match self.kind() {
            TokenKind::Ident | TokenKind::TypeIdent => self.parse_named_type(),
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
        }
    }

    fn parse_named_type(&mut self) -> TypeExprId {
        let start = self.span();
        let name = self.slice().to_string();
        self.bump();

        let mut args = Vec::new();
        let mut end = start;

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
