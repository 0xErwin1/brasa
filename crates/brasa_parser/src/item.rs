//! Top-level items: `import`, `def`, `struct`, `enum`, `interface`, and
//! top-level `let`, plus generics/params/throws shared by several of them.

use brasa_ast::{
    Constraint, EnumDef, Field, FuncDef, GenericParam, IfaceMember, Import, ImportPath,
    InterfaceDef, Item, ItemId, Param, StructDef, Throws, TopLet, Variant,
};
use brasa_diagnostics::codes;
use brasa_source::Span;
use brasa_token::TokenKind;

use crate::Parser;

impl<'a> Parser<'a> {
    pub(crate) fn parse_item(&mut self) -> ItemId {
        let start = self.span();

        let is_pub = self.eat(TokenKind::Pub).is_some();

        match self.kind() {
            TokenKind::Import if !is_pub => self.parse_import(start),
            TokenKind::Def => self.parse_func_item(is_pub, start),
            TokenKind::Struct => self.parse_struct_item(is_pub, start),
            TokenKind::Enum => self.parse_enum_item(is_pub, start),
            TokenKind::Interface => self.parse_interface_item(is_pub, start),
            TokenKind::Let => self.parse_top_let(is_pub, start),
            _ => {
                if is_pub {
                    self.error_expected("'def', 'struct', 'enum', 'interface', or 'let' after pub");
                }
                self.parse_stmt_item(start)
            }
        }
    }

    fn parse_stmt_item(&mut self, start: Span) -> ItemId {
        let checkpoint = self.pos;
        let stmt = self.parse_stmt();
        let end = self.span_before_cursor(checkpoint);

        self.ast
            .alloc_item(Item::Stmt(stmt), Span::merge(&start, &end))
    }

    /// The span of the token just before the cursor, used to build an
    /// item's span from a nested statement/expr parse that already
    /// consumed everything belonging to it.
    pub(crate) fn span_before_cursor(&self, checkpoint: usize) -> Span {
        if self.pos > checkpoint {
            self.tokens[self.pos - 1].span
        } else {
            self.span()
        }
    }

    fn parse_import(&mut self, start: Span) -> ItemId {
        self.bump(); // 'import'

        let path = if self.at(TokenKind::StringStart) || self.at(TokenKind::RawStringStart) {
            let (text, _span) = self.parse_plain_string();
            ImportPath::File(text)
        } else {
            let mut segments = vec![self.expect_ident_text("import segment")];

            if self.eat(TokenKind::ColonColon).is_none() {
                self.error_expected("'::' after the first import segment");
            } else {
                segments.push(self.expect_ident_text("import segment"));
                while self.eat(TokenKind::ColonColon).is_some() {
                    segments.push(self.expect_ident_text("import segment"));
                }
            }

            ImportPath::Std(segments)
        };

        let end = self.span_before_cursor(self.pos);
        self.ast
            .alloc_item(Item::Import(Import { path }), Span::merge(&start, &end))
    }

    pub(crate) fn expect_ident_text(&mut self, what: &str) -> String {
        self.expect_ident_spanned(what).0
    }

    pub(crate) fn expect_type_ident_text(&mut self, what: &str) -> String {
        self.expect_type_ident_spanned(what).0
    }

    /// Like [`Self::expect_ident_text`], also returning the identifier
    /// token's span. On a failed expectation the span is the current
    /// (unconsumed) token's, the same position the diagnostic points at.
    pub(crate) fn expect_ident_spanned(&mut self, what: &str) -> (String, Span) {
        let tok = self.expect(TokenKind::Ident, what);
        self.spanned_text(tok)
    }

    /// Like [`Self::expect_ident_spanned`], for `TYPE_IDENT` tokens.
    pub(crate) fn expect_type_ident_spanned(&mut self, what: &str) -> (String, Span) {
        let tok = self.expect(TokenKind::TypeIdent, what);
        self.spanned_text(tok)
    }

    fn spanned_text(&self, tok: Option<brasa_token::Token>) -> (String, Span) {
        match tok {
            Some(tok) => (
                self.source[tok.span.start.0 as usize..tok.span.end.0 as usize].to_string(),
                tok.span,
            ),
            None => ("<error>".to_string(), self.span()),
        }
    }

    fn parse_generics(&mut self) -> Vec<GenericParam> {
        if self.eat(TokenKind::Lt).is_none() {
            return Vec::new();
        }

        let mut params = Vec::new();

        while !self.at(TokenKind::Gt) && !self.at(TokenKind::Eof) {
            let checkpoint = self.pos;
            let (name, name_span) = self.expect_type_ident_spanned("a generic parameter name");
            let constraint = if self.eat(TokenKind::Colon).is_some() {
                Some(self.parse_constraint())
            } else {
                None
            };
            params.push(GenericParam {
                name,
                name_span,
                constraint,
            });

            if self.eat(TokenKind::Comma).is_none() {
                break;
            }
            self.ensure_progress(checkpoint);
        }

        self.expect(TokenKind::Gt, "'>' to close generics");
        params
    }

    fn parse_constraint(&mut self) -> Constraint {
        if self.eat(TokenKind::LBrace).is_some() {
            let mut members = Vec::new();

            while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
                let checkpoint = self.pos;
                members.push(self.parse_iface_member());
                if self.eat(TokenKind::Comma).is_none() {
                    break;
                }
                self.ensure_progress(checkpoint);
            }

            self.expect(TokenKind::RBrace, "'}' to close the inline interface");
            Constraint::Inline(members)
        } else {
            Constraint::Named(self.expect_type_ident_text("an interface name"))
        }
    }

    fn parse_params(&mut self) -> Vec<Param> {
        self.expect(TokenKind::LParen, "'(' to start parameters");

        let mut params = Vec::new();

        while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
            let checkpoint = self.pos;

            if self.at(TokenKind::SelfKw) {
                let tok = self.bump();
                params.push(Param::SelfParam { span: tok.span });
            } else {
                let (name, name_span) = self.expect_ident_spanned("a parameter name");
                self.expect(TokenKind::Colon, "':' before the parameter type");
                let ty = self.parse_type();
                params.push(Param::Named {
                    name,
                    name_span,
                    ty,
                });
            }

            if self.eat(TokenKind::Comma).is_none() {
                break;
            }
            self.ensure_progress(checkpoint);
        }

        self.expect(TokenKind::RParen, "')' to close parameters");
        params
    }

    fn parse_ret(&mut self) -> Option<brasa_ast::TypeExprId> {
        if self.eat(TokenKind::Colon).is_some() {
            Some(self.parse_type())
        } else {
            None
        }
    }

    fn parse_throws(&mut self) -> Option<Throws> {
        self.eat(TokenKind::Throws)?;

        if self.eat(TokenKind::Never).is_some() {
            return Some(Throws::Never);
        }

        let mut types = vec![self.parse_throws_type()];
        while self.eat(TokenKind::Pipe).is_some() {
            types.push(self.parse_throws_type());
        }

        Some(Throws::Types(types))
    }

    fn parse_throws_type(&mut self) -> brasa_ast::ThrowsType {
        let (name, span) = self.expect_type_ident_spanned("an error type");
        brasa_ast::ThrowsType { name, span }
    }

    pub(crate) fn parse_func_def(&mut self, is_pub: bool, start: Span) -> (FuncDef, Span) {
        self.bump(); // 'def'

        let name = self.expect_ident_text("a function name");
        let generics = self.parse_generics();
        let params = self.parse_params();
        let ret = self.parse_ret();
        let throws = self.parse_throws();

        self.skip_stmt_seps();
        let body = self.parse_block(&[TokenKind::End]);
        let end_tok = self.expect(TokenKind::End, "'end' to close the function");
        let end = end_tok.map(|t| t.span).unwrap_or_else(|| self.span());

        (
            FuncDef {
                is_pub,
                name,
                generics,
                params,
                ret,
                throws,
                body,
            },
            Span::merge(&start, &end),
        )
    }

    fn parse_func_item(&mut self, is_pub: bool, start: Span) -> ItemId {
        let (func, span) = self.parse_func_def(is_pub, start);
        self.ast.alloc_item(Item::FuncDef(func), span)
    }

    fn parse_field(&mut self) -> Field {
        let (name, name_span) = self.expect_ident_spanned("a field name");
        self.expect(TokenKind::Colon, "':' before the field type");
        let ty = self.parse_type();
        Field {
            name,
            name_span,
            ty,
        }
    }

    fn parse_struct_item(&mut self, is_pub: bool, start: Span) -> ItemId {
        self.bump(); // 'struct'

        let name = self.expect_type_ident_text("a struct name");
        let generics = self.parse_generics();
        self.skip_stmt_seps();

        let mut fields = Vec::new();
        let mut methods = Vec::new();

        while !self.at(TokenKind::End) && !self.at(TokenKind::Eof) {
            let checkpoint = self.pos;

            if self.at(TokenKind::Def) || (self.at(TokenKind::Pub) && self.peek_is_def()) {
                let member_pub = self.eat(TokenKind::Pub).is_some();
                let member_start = self.span();
                let (func, _) = self.parse_func_def(member_pub, member_start);
                methods.push(func);
            } else {
                fields.push(self.parse_field());
            }

            self.skip_stmt_seps();
            self.ensure_progress(checkpoint);
        }

        let end_tok = self.expect(TokenKind::End, "'end' to close the struct");
        let end = end_tok.map(|t| t.span).unwrap_or_else(|| self.span());

        self.ast.alloc_item(
            Item::StructDef(StructDef {
                is_pub,
                name,
                generics,
                fields,
                methods,
            }),
            Span::merge(&start, &end),
        )
    }

    fn peek_is_def(&self) -> bool {
        self.tokens.get(self.pos + 1).map(|t| t.kind) == Some(TokenKind::Def)
    }

    fn parse_enum_item(&mut self, is_pub: bool, start: Span) -> ItemId {
        self.bump(); // 'enum'

        let name = self.expect_type_ident_text("an enum name");
        let generics = self.parse_generics();
        self.skip_stmt_seps();

        let mut variants = Vec::new();

        while !self.at(TokenKind::End) && !self.at(TokenKind::Eof) {
            let checkpoint = self.pos;
            let (variant_name, variant_name_span) =
                self.expect_type_ident_spanned("a variant name");

            let fields = if self.eat(TokenKind::LParen).is_some() {
                let mut fields = Vec::new();
                while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
                    let field_checkpoint = self.pos;
                    fields.push(self.parse_field());
                    if self.eat(TokenKind::Comma).is_none() {
                        break;
                    }
                    self.ensure_progress(field_checkpoint);
                }
                self.expect(TokenKind::RParen, "')' to close variant fields");
                fields
            } else {
                Vec::new()
            };

            variants.push(Variant {
                name: variant_name,
                name_span: variant_name_span,
                fields,
            });

            self.skip_stmt_seps();
            self.ensure_progress(checkpoint);
        }

        let end_tok = self.expect(TokenKind::End, "'end' to close the enum");
        let end = end_tok.map(|t| t.span).unwrap_or_else(|| self.span());

        // Grammar: `enum_def = ... NL variant+ "end"` — at least one
        // variant is required.
        if variants.is_empty() {
            self.error_at(
                codes::P_EMPTY_ENUM,
                Span::merge(&start, &end),
                format!("enum `{name}` must have at least one variant"),
            );
        }

        self.ast.alloc_item(
            Item::EnumDef(EnumDef {
                is_pub,
                name,
                generics,
                variants,
            }),
            Span::merge(&start, &end),
        )
    }

    fn parse_iface_member(&mut self) -> IfaceMember {
        self.expect(TokenKind::Def, "'def' to start an interface member");
        let (name, name_span) = self.expect_ident_spanned("a method name");
        let params = self.parse_params();
        let ret = self.parse_ret();
        let throws = self.parse_throws();

        IfaceMember {
            name,
            name_span,
            params,
            ret,
            throws,
        }
    }

    fn parse_interface_item(&mut self, is_pub: bool, start: Span) -> ItemId {
        self.bump(); // 'interface'

        let name = self.expect_type_ident_text("an interface name");
        let generics = self.parse_generics();
        self.skip_stmt_seps();

        let mut methods = Vec::new();

        while !self.at(TokenKind::End) && !self.at(TokenKind::Eof) {
            let checkpoint = self.pos;
            methods.push(self.parse_iface_member());
            self.skip_stmt_seps();
            self.ensure_progress(checkpoint);
        }

        let end_tok = self.expect(TokenKind::End, "'end' to close the interface");
        let end = end_tok.map(|t| t.span).unwrap_or_else(|| self.span());

        // Grammar: `interface_def = ... NL iface_member+ "end"` — at least
        // one member is required.
        if methods.is_empty() {
            self.error_at(
                codes::P_EMPTY_INTERFACE,
                Span::merge(&start, &end),
                format!("interface `{name}` must have at least one member"),
            );
        }

        self.ast.alloc_item(
            Item::InterfaceDef(InterfaceDef {
                is_pub,
                name,
                generics,
                methods,
            }),
            Span::merge(&start, &end),
        )
    }

    fn parse_top_let(&mut self, is_pub: bool, start: Span) -> ItemId {
        let let_stmt = self.parse_let_stmt_inner();
        let end = self.span_before_cursor(self.pos);

        self.ast.alloc_item(
            Item::TopLet(TopLet { is_pub, let_stmt }),
            Span::merge(&start, &end),
        )
    }
}
