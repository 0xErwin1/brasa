//! Items: the program itself, definitions, and the pieces they share
//! (generics, parameters, `throws`, types, patterns).

use brasa_ast::{
    Constraint, EnumDef, Field, FuncDef, GenericParam, IfaceMember, ImportPath, InterfaceDef, Item,
    ItemId, Param, Pattern, PatternId, StructDef, Throws, TopLet, TypeExpr, TypeExprId,
};

use crate::{INDENT, Lines, Printer, indent_of};

/// A struct-body member, so fields and methods can be printed in the one
/// order the source had. `brasa_ast::StructDef` keeps them in two lists
/// (their relative order carries no meaning to the compiler), which is
/// exactly the kind of detail a formatter must not let leak into the
/// file it rewrites.
enum Member<'m> {
    Field(&'m Field),
    Method(&'m FuncDef),
}

impl Member<'_> {
    fn start(&self) -> u32 {
        match self {
            Member::Field(field) => field.name_span.start.0,
            Member::Method(func) => func.name_span.start.0,
        }
    }
}

impl<'a> Printer<'a> {
    pub(crate) fn program(&mut self, roots: &[ItemId]) -> String {
        let ast = self.ast;
        let mut lines = Lines::new();

        for &root in roots {
            let span = ast.span_of_item(root);

            self.emit_comments_before(&mut lines, 0, span.start.0);
            if self.blank_before(span.start.0) {
                lines.blank();
            }

            let text = self.item(root, 0);
            self.emit_hoisted(&mut lines, 0, span);
            lines.push(&text);
            self.emit_trailing(&mut lines, span.end.0);
        }

        self.emit_comments_before(&mut lines, 0, self.src.len() as u32);

        let mut out = lines.finish();
        out.push('\n');
        out
    }

    fn item(&mut self, id: ItemId, level: usize) -> String {
        let ast = self.ast;
        let span = ast.span_of_item(id);

        match ast.item(id) {
            Item::Import(import) => {
                let path = match &import.path {
                    ImportPath::Std(segments) => segments.join("::"),
                    ImportPath::File(path) => format!("\"{}\"", path.replace('\\', "\\\\")),
                };
                format!("{}import {path}", indent_of(level))
            }
            Item::FuncDef(func) => self.func_def(func, level, self.body_region_end(span)),
            Item::StructDef(def) => self.struct_def(def, level, self.body_region_end(span)),
            Item::EnumDef(def) => self.enum_def(def, level, self.body_region_end(span)),
            Item::InterfaceDef(def) => self.interface_def(def, level, self.body_region_end(span)),
            Item::TopLet(TopLet { is_pub, let_stmt }) => {
                let prefix = if *is_pub { "pub " } else { "" };
                let head = format!("{}{prefix}", indent_of(level));
                self.let_stmt(let_stmt, &head, level)
            }
            Item::Stmt(stmt) => self.stmt(*stmt, level),
        }
    }

    /// `pub def name<T>(a: int): ret throws E` plus the body and `end`.
    ///
    /// `bound` is the furthest the body could possibly reach, which for a
    /// struct method is only "wherever the next member starts" — the AST
    /// records no span for one. The body's real territory stops at its
    /// own `end`, so it is clipped here; without that, a comment written
    /// between this method's `end` and the next member would be swept
    /// into this body.
    pub(crate) fn func_def(&mut self, func: &FuncDef, level: usize, bound: u32) -> String {
        let mut lines = Lines::new();
        lines.push(&format!(
            "{}{}",
            indent_of(level),
            self.func_signature(func)
        ));

        // Searched forward from the body's last statement, or — when
        // there is none — from the end of the signature's line, which is
        // where an empty body starts. Both land on the `end` keyword,
        // since `next_token_pos` walks over comments rather than
        // stopping at them.
        let after_body = match func.body.last() {
            Some(stmt) => self.ast.span_of_stmt(*stmt).end.0,
            None => self.line_end(func.name_span.end.0),
        };
        let region_end = self.next_token_pos(after_body).min(bound);

        let body = self.block(&func.body, level + INDENT, region_end);
        if !body.is_empty() {
            lines.push(&body);
        }

        lines.push(&format!("{}end", indent_of(level)));
        lines.finish()
    }

    fn func_signature(&self, func: &FuncDef) -> String {
        let pub_ = if func.is_pub { "pub " } else { "" };
        let generics = self.generics(&func.generics);
        let params = self.params(&func.params);
        let ret = self.ret(func.ret);
        let throws = self.throws(func.throws.as_ref());

        format!("{pub_}def {}{generics}({params}){ret}{throws}", func.name)
    }

    fn struct_def(&mut self, def: &StructDef, level: usize, region_end: u32) -> String {
        let pub_ = if def.is_pub { "pub " } else { "" };
        let mut lines = Lines::new();
        lines.push(&format!(
            "{}{pub_}struct {}{}",
            indent_of(level),
            def.name,
            self.generics(&def.generics)
        ));

        let mut members: Vec<Member<'_>> = def
            .fields
            .iter()
            .map(Member::Field)
            .chain(def.methods.iter().map(Member::Method))
            .collect();
        members.sort_by_key(Member::start);

        let inner = level + INDENT;
        let mut body = Lines::new();

        for (index, member) in members.iter().enumerate() {
            let start = member.start();
            // A method's own span is not recorded, so its body reaches to
            // wherever the next member starts (or to the struct's `end`).
            let member_region_end = members
                .get(index + 1)
                .map_or(region_end, |next| next.start());

            self.emit_comments_before(&mut body, inner, start);
            if self.blank_before(start) {
                body.blank();
            }

            match member {
                Member::Field(field) => {
                    body.push(&format!("{}{}", indent_of(inner), self.field(field)));
                    self.emit_trailing(&mut body, self.ast.span_of_type_expr(field.ty).end.0);
                }
                Member::Method(func) => {
                    let text = self.func_def(func, inner, member_region_end);
                    body.push(&text);
                }
            }
        }

        self.emit_comments_before(&mut body, inner, region_end);

        let body = body.finish();
        if !body.is_empty() {
            lines.push(&body);
        }
        lines.push(&format!("{}end", indent_of(level)));
        lines.finish()
    }

    fn enum_def(&mut self, def: &EnumDef, level: usize, region_end: u32) -> String {
        let pub_ = if def.is_pub { "pub " } else { "" };
        let mut lines = Lines::new();
        lines.push(&format!(
            "{}{pub_}enum {}{}",
            indent_of(level),
            def.name,
            self.generics(&def.generics)
        ));

        let inner = level + INDENT;
        let mut body = Lines::new();

        for variant in &def.variants {
            let start = variant.name_span.start.0;
            self.emit_comments_before(&mut body, inner, start);
            if self.blank_before(start) {
                body.blank();
            }

            let fields = if variant.fields.is_empty() {
                String::new()
            } else {
                let rendered: Vec<String> = variant.fields.iter().map(|f| self.field(f)).collect();
                format!("({})", rendered.join(", "))
            };
            body.push(&format!("{}{}{fields}", indent_of(inner), variant.name));

            let end = variant
                .fields
                .last()
                .map_or(variant.name_span.end.0, |field| {
                    self.ast.span_of_type_expr(field.ty).end.0 + 1
                });
            self.emit_trailing(&mut body, end);
        }

        self.emit_comments_before(&mut body, inner, region_end);

        let body = body.finish();
        if !body.is_empty() {
            lines.push(&body);
        }
        lines.push(&format!("{}end", indent_of(level)));
        lines.finish()
    }

    fn interface_def(&mut self, def: &InterfaceDef, level: usize, region_end: u32) -> String {
        let pub_ = if def.is_pub { "pub " } else { "" };
        let mut lines = Lines::new();
        lines.push(&format!(
            "{}{pub_}interface {}{}",
            indent_of(level),
            def.name,
            self.generics(&def.generics)
        ));

        let inner = level + INDENT;
        let mut body = Lines::new();

        for method in &def.methods {
            let start = method.name_span.start.0;
            self.emit_comments_before(&mut body, inner, start);
            if self.blank_before(start) {
                body.blank();
            }
            body.push(&format!(
                "{}{}",
                indent_of(inner),
                self.iface_member(method)
            ));
        }

        self.emit_comments_before(&mut body, inner, region_end);

        let body = body.finish();
        if !body.is_empty() {
            lines.push(&body);
        }
        lines.push(&format!("{}end", indent_of(level)));
        lines.finish()
    }

    fn iface_member(&self, member: &IfaceMember) -> String {
        format!(
            "def {}({}){}{}",
            member.name,
            self.params(&member.params),
            self.ret(member.ret),
            self.throws(member.throws.as_ref())
        )
    }

    fn field(&self, field: &Field) -> String {
        format!("{}: {}", field.name, self.ty(field.ty))
    }

    pub(crate) fn generics(&self, generics: &[GenericParam]) -> String {
        if generics.is_empty() {
            return String::new();
        }

        let rendered: Vec<String> = generics
            .iter()
            .map(|param| match &param.constraint {
                None => param.name.clone(),
                Some(Constraint::Named(name)) => format!("{}: {name}", param.name),
                Some(Constraint::Inline(members)) => {
                    let members: Vec<String> =
                        members.iter().map(|m| self.iface_member(m)).collect();
                    format!("{}: {{ {} }}", param.name, members.join(", "))
                }
            })
            .collect();

        format!("<{}>", rendered.join(", "))
    }

    fn params(&self, params: &[Param]) -> String {
        let rendered: Vec<String> = params
            .iter()
            .map(|param| match param {
                Param::SelfParam { .. } => "self".to_string(),
                Param::Named { name, ty, .. } => format!("{name}: {}", self.ty(*ty)),
            })
            .collect();

        rendered.join(", ")
    }

    fn ret(&self, ret: Option<TypeExprId>) -> String {
        ret.map_or(String::new(), |ty| format!(": {}", self.ty(ty)))
    }

    fn throws(&self, throws: Option<&Throws>) -> String {
        match throws {
            None => String::new(),
            Some(Throws::Never) => " throws never".to_string(),
            Some(Throws::Types(types)) => {
                let names: Vec<&str> = types.iter().map(|t| t.name.as_str()).collect();
                format!(" throws {}", names.join(" | "))
            }
        }
    }

    pub(crate) fn ty(&self, id: TypeExprId) -> String {
        match self.ast.type_expr(id) {
            TypeExpr::Named { name, args } if args.is_empty() => name.clone(),
            TypeExpr::Named { name, args } => {
                let args: Vec<String> = args.iter().map(|arg| self.ty(*arg)).collect();
                format!("{name}<{}>", args.join(", "))
            }
            TypeExpr::Tuple(elements) => {
                let elements: Vec<String> = elements.iter().map(|el| self.ty(*el)).collect();
                format!("({})", elements.join(", "))
            }
            TypeExpr::Fn { params, ret } => {
                let params: Vec<String> = params.iter().map(|p| self.ty(*p)).collect();
                format!("({}) -> {}", params.join(", "), self.ty(*ret))
            }
        }
    }

    pub(crate) fn pattern(&self, id: PatternId) -> String {
        match self.ast.pattern(id) {
            Pattern::Wildcard => "_".to_string(),
            // Printed from the source so a literal keeps its own
            // spelling: `0xFF` stays hexadecimal, `1.50` keeps its
            // trailing zero, and a string keeps its escapes.
            Pattern::Literal(_) => self.slice(self.ast.span_of_pattern(id)).to_string(),
            Pattern::Binding(name) => name.clone(),
            Pattern::Ctor { name, args } if args.is_empty() => name.clone(),
            Pattern::Ctor { name, args } => {
                let args: Vec<String> = args.iter().map(|arg| self.pattern(*arg)).collect();
                format!("{name}({})", args.join(", "))
            }
            Pattern::Tuple(elements) => {
                let elements: Vec<String> = elements.iter().map(|el| self.pattern(*el)).collect();
                format!("({})", elements.join(", "))
            }
        }
    }
}
