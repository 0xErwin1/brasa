//! AST→HIR lowering: the one place where sugar disappears.
//!
//! Per `docs/spec/00-vision.md`'s HIR row every desugaring happens here,
//! exactly once:
//!
//! - `a |> f(b)` becomes `f(a, b)`; a non-call target calls it with `a`.
//! - `a ?? b` becomes `match a { Some(t) => t, None => b }`, so `b` is
//!   evaluated only when `a` is `None`.
//! - `a?.b` / `a?.m(args)` become `match a { Some(t) =>
//!   OptionWrap(t.b), None => None }`; see [`crate::Expr::OptionWrap`]
//!   for why the `Some` arm cannot be a plain `Some(...)` wrap.
//! - `"x#{e}y"` becomes `"x" + ToString(e) + "y"`, folded left-to-right;
//!   see [`crate::Expr::ToString`].
//! - `x += e` becomes `x = x + e`; `Field`/`Index` targets bind their
//!   receiver (and index) to fresh temps first so each evaluates once.
//!
//! Everything else is copied structurally into the HIR arenas. Every
//! synthesized node carries the span of the sugar node it came from, so
//! later diagnostics still point at real source.
//!
//! Fresh temporaries are named `$tmp0`, `$tmp1`, ... from a monotonic
//! per-lowering counter. `$` cannot appear in a Brasa identifier
//! (`docs/spec/02-grammar.md`'s lexical grammar), so these names can
//! never collide with user bindings; no separate hygiene mechanism is
//! needed.

use std::collections::HashMap;

use brasa_ast as ast;
use brasa_ast::Ast;
use brasa_diagnostics::Diagnostic;
use brasa_source::Span;

use crate::{
    ArmBody, Block, CatchArm, Constraint, EnumDef, Expr, ExprId, Field, FuncDef, GenericParam, Hir,
    IfNode, IfaceMember, InterfaceDef, Item, ItemId, LambdaBody, LambdaParam, LetStmt, MatchArm,
    Param, Pattern, PatternId, Stmt, StructDef, TopLet, TypeExpr, TypeExprId, Variant,
};

/// The sugar construct a synthesized `match` expression was desugared
/// from. The type checker uses this to report `?.`/`??` misuse in source
/// terms instead of leaking the desugared `match` (BRS-19); the HIR node
/// itself stays a plain, immutable `Expr::Match`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SugarOrigin {
    /// `a?.b` / `a?.m(args)`.
    SafeNav,
    /// `a ?? b`.
    Coalesce,
}

/// The output of lowering one parsed file: the HIR arenas, the top-level
/// item IDs in source order, the sugar-origin side table, and any
/// diagnostics. No current desugaring can fail, so `diagnostics` is
/// empty today; the channel exists because phases report structured
/// errors and only the CLI renders them.
pub struct LowerResult {
    pub hir: Hir,
    pub roots: Vec<ItemId>,
    /// Which sugar construct each synthesized `match` expression came
    /// from, keyed by the `Expr::Match` node's ID. A side table so the
    /// HIR nodes stay immutable and sugar-free.
    pub sugar_origins: HashMap<ExprId, SugarOrigin>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Lowers a parsed program (an [`Ast`] plus its top-level items in
/// source order) into a self-contained [`Hir`]. Expects an AST that
/// parsed with zero errors; recovery placeholders from an errored parse
/// are lowered structurally like everything else.
pub fn lower(ast: &Ast, roots: &[ast::ItemId]) -> LowerResult {
    let mut cx = LowerCtx {
        ast,
        hir: Hir::new(),
        sugar_origins: HashMap::new(),
        diagnostics: Vec::new(),
        next_temp: 0,
    };

    let roots = roots.iter().map(|&root| cx.lower_item(root)).collect();

    LowerResult {
        hir: cx.hir,
        roots,
        sugar_origins: cx.sugar_origins,
        diagnostics: cx.diagnostics,
    }
}

struct LowerCtx<'a> {
    ast: &'a Ast,
    hir: Hir,
    sugar_origins: HashMap<ExprId, SugarOrigin>,
    diagnostics: Vec<Diagnostic>,
    next_temp: u32,
}

impl LowerCtx<'_> {
    fn fresh_temp(&mut self) -> String {
        let name = format!("$tmp{}", self.next_temp);
        self.next_temp += 1;
        name
    }

    // Items ---------------------------------------------------------

    fn lower_item(&mut self, id: ast::ItemId) -> ItemId {
        let span = self.ast.span_of_item(id);

        let item = match self.ast.item(id) {
            ast::Item::Import(import) => Item::Import(import.clone()),
            ast::Item::FuncDef(func) => Item::FuncDef(self.lower_func(func)),
            ast::Item::StructDef(def) => Item::StructDef(StructDef {
                is_pub: def.is_pub,
                name: def.name.clone(),
                generics: self.lower_generics(&def.generics),
                fields: self.lower_fields(&def.fields),
                methods: def.methods.iter().map(|m| self.lower_func(m)).collect(),
            }),
            ast::Item::EnumDef(def) => Item::EnumDef(EnumDef {
                is_pub: def.is_pub,
                name: def.name.clone(),
                generics: self.lower_generics(&def.generics),
                variants: def
                    .variants
                    .iter()
                    .map(|v| Variant {
                        name: v.name.clone(),
                        name_span: v.name_span,
                        fields: self.lower_fields(&v.fields),
                    })
                    .collect(),
            }),
            ast::Item::InterfaceDef(def) => Item::InterfaceDef(InterfaceDef {
                is_pub: def.is_pub,
                name: def.name.clone(),
                generics: self.lower_generics(&def.generics),
                methods: def
                    .methods
                    .iter()
                    .map(|m| self.lower_iface_member(m))
                    .collect(),
            }),
            ast::Item::TopLet(top_let) => Item::TopLet(TopLet {
                is_pub: top_let.is_pub,
                let_stmt: self.lower_let_stmt(&top_let.let_stmt),
            }),
            ast::Item::Stmt(stmt) => {
                let mut out = Vec::new();
                self.lower_stmt(*stmt, &mut out);
                Item::Stmt(out)
            }
        };

        self.hir.alloc_item(item, span)
    }

    fn lower_func(&mut self, func: &ast::FuncDef) -> FuncDef {
        FuncDef {
            is_pub: func.is_pub,
            name: func.name.clone(),
            name_span: func.name_span,
            generics: self.lower_generics(&func.generics),
            params: func.params.iter().map(|p| self.lower_param(p)).collect(),
            ret: func.ret.map(|ty| self.lower_type_expr(ty)),
            throws: func.throws.clone(),
            body: self.lower_block(&func.body),
        }
    }

    fn lower_generics(&mut self, generics: &[ast::GenericParam]) -> Vec<GenericParam> {
        generics
            .iter()
            .map(|g| GenericParam {
                name: g.name.clone(),
                name_span: g.name_span,
                constraint: g.constraint.as_ref().map(|c| match c {
                    ast::Constraint::Named(name) => Constraint::Named(name.clone()),
                    ast::Constraint::Inline(members) => Constraint::Inline(
                        members.iter().map(|m| self.lower_iface_member(m)).collect(),
                    ),
                }),
            })
            .collect()
    }

    fn lower_param(&mut self, param: &ast::Param) -> Param {
        match param {
            ast::Param::SelfParam { span } => Param::SelfParam { span: *span },
            ast::Param::Named {
                name,
                name_span,
                ty,
            } => Param::Named {
                name: name.clone(),
                name_span: *name_span,
                ty: self.lower_type_expr(*ty),
            },
        }
    }

    fn lower_iface_member(&mut self, member: &ast::IfaceMember) -> IfaceMember {
        IfaceMember {
            name: member.name.clone(),
            name_span: member.name_span,
            params: member.params.iter().map(|p| self.lower_param(p)).collect(),
            ret: member.ret.map(|ty| self.lower_type_expr(ty)),
            throws: member.throws.clone(),
        }
    }

    fn lower_fields(&mut self, fields: &[ast::Field]) -> Vec<Field> {
        fields
            .iter()
            .map(|f| Field {
                name: f.name.clone(),
                name_span: f.name_span,
                ty: self.lower_type_expr(f.ty),
            })
            .collect()
    }

    // Statements ----------------------------------------------------

    fn lower_block(&mut self, block: &ast::Block) -> Block {
        let mut out = Vec::new();

        for &stmt in block {
            self.lower_stmt(stmt, &mut out);
        }

        out
    }

    /// Lowers one AST statement, pushing one or more HIR statements: a
    /// compound assignment on a `Field`/`Index` target emits temp `let`s
    /// before the plain assignment, so the output is a sequence.
    fn lower_stmt(&mut self, id: ast::StmtId, out: &mut Block) {
        let span = self.ast.span_of_stmt(id);

        let stmt = match self.ast.stmt(id) {
            ast::Stmt::Let(let_stmt) => Stmt::Let(self.lower_let_stmt(let_stmt)),
            ast::Stmt::Assign { target, op, value } => {
                self.lower_assign(*target, *op, *value, span, out);
                return;
            }
            ast::Stmt::Return(value) => Stmt::Return(value.map(|v| self.lower_expr(v))),
            ast::Stmt::Break => Stmt::Break,
            ast::Stmt::Continue => Stmt::Continue,
            ast::Stmt::Throw(value) => Stmt::Throw(self.lower_expr(*value)),
            ast::Stmt::If(node) => Stmt::If(self.lower_if(node)),
            ast::Stmt::While { cond, body } => Stmt::While {
                cond: self.lower_expr(*cond),
                body: self.lower_block(body),
            },
            ast::Stmt::For {
                pattern,
                iterable,
                body,
            } => Stmt::For {
                pattern: self.lower_pattern(*pattern),
                iterable: self.lower_expr(*iterable),
                body: self.lower_block(body),
            },
            ast::Stmt::Expr(value) => Stmt::Expr(self.lower_expr(*value)),
        };

        out.push(self.hir.alloc_stmt(stmt, span));
    }

    fn lower_let_stmt(&mut self, let_stmt: &ast::LetStmt) -> LetStmt {
        LetStmt {
            mutable: let_stmt.mutable,
            name: let_stmt.name.clone(),
            ty: let_stmt.ty.map(|ty| self.lower_type_expr(ty)),
            value: self.lower_expr(let_stmt.value),
        }
    }

    /// Lowers `target op= value` (`docs/spec/00-vision.md`: `+=` →
    /// assignment). A plain `=` copies through; a compound operator
    /// rewrites to `target = target <op> value`, rebuilding the lvalue
    /// so `Field`/`Index` receivers (and indices) are bound to fresh
    /// temps and evaluated exactly once:
    ///
    /// ```text
    /// p.x += e      =>  let $t = p       (then)  $t.x = $t.x + e
    /// v[i] += e     =>  let $t = v
    ///                   let $u = i       (then)  $t[$u] = $t[$u] + e
    /// ```
    ///
    /// An `Ident` target is a name, not a computation, so it is simply
    /// referenced twice with no temp. The parser only builds
    /// `Ident`/`Field`/`Index` targets (the `lvalue` production); any
    /// other recovered shape is copied through like `Ident` and left for
    /// the later lvalue validation to reject.
    fn lower_assign(
        &mut self,
        target: ast::ExprId,
        op: ast::AssignOp,
        value: ast::ExprId,
        span: Span,
        out: &mut Block,
    ) {
        let Some(bin_op) = compound_binary_op(op) else {
            let stmt = Stmt::Assign {
                target: self.lower_expr(target),
                value: self.lower_expr(value),
            };
            out.push(self.hir.alloc_stmt(stmt, span));
            return;
        };

        let (read, write) = match self.ast.expr(target) {
            ast::Expr::Field { recv, name } => {
                let temp = self.bind_temp(*recv, span, out);

                let read_recv = self.hir.alloc_expr(Expr::Ident(temp.clone()), span);
                let read = self.hir.alloc_expr(
                    Expr::Field {
                        recv: read_recv,
                        name: name.clone(),
                    },
                    span,
                );

                let write_recv = self.hir.alloc_expr(Expr::Ident(temp), span);
                let write = self.hir.alloc_expr(
                    Expr::Field {
                        recv: write_recv,
                        name: name.clone(),
                    },
                    span,
                );

                (read, write)
            }
            ast::Expr::Index { recv, index } => {
                let recv_temp = self.bind_temp(*recv, span, out);
                let index_temp = self.bind_temp(*index, span, out);

                let read_recv = self.hir.alloc_expr(Expr::Ident(recv_temp.clone()), span);
                let read_index = self.hir.alloc_expr(Expr::Ident(index_temp.clone()), span);
                let read = self.hir.alloc_expr(
                    Expr::Index {
                        recv: read_recv,
                        index: read_index,
                    },
                    span,
                );

                let write_recv = self.hir.alloc_expr(Expr::Ident(recv_temp), span);
                let write_index = self.hir.alloc_expr(Expr::Ident(index_temp), span);
                let write = self.hir.alloc_expr(
                    Expr::Index {
                        recv: write_recv,
                        index: write_index,
                    },
                    span,
                );

                (read, write)
            }
            _ => (self.lower_expr(target), self.lower_expr(target)),
        };

        let rhs = self.lower_expr(value);
        let combined = self.hir.alloc_expr(
            Expr::Binary {
                op: bin_op,
                lhs: read,
                rhs,
            },
            span,
        );

        let stmt = Stmt::Assign {
            target: write,
            value: combined,
        };
        out.push(self.hir.alloc_stmt(stmt, span));
    }

    /// Lowers `value` and binds it to a fresh immutable temp, pushing the
    /// `let` and returning the temp's name.
    fn bind_temp(&mut self, value: ast::ExprId, span: Span, out: &mut Block) -> String {
        let name = self.fresh_temp();

        let let_stmt = Stmt::Let(LetStmt {
            mutable: false,
            name: name.clone(),
            ty: None,
            value: self.lower_expr(value),
        });
        out.push(self.hir.alloc_stmt(let_stmt, span));

        name
    }

    fn lower_if(&mut self, node: &ast::IfNode) -> IfNode {
        IfNode {
            branches: node
                .branches
                .iter()
                .map(|(cond, body)| (self.lower_expr(*cond), self.lower_block(body)))
                .collect(),
            else_: node.else_.as_ref().map(|body| self.lower_block(body)),
        }
    }

    // Expressions ---------------------------------------------------

    fn lower_expr(&mut self, id: ast::ExprId) -> ExprId {
        let span = self.ast.span_of_expr(id);

        let expr = match self.ast.expr(id) {
            ast::Expr::Int(value) => Expr::Int(*value),
            ast::Expr::Float(value) => Expr::Float(*value),
            ast::Expr::Bool(value) => Expr::Bool(*value),
            ast::Expr::Char(value) => Expr::Char(*value),
            ast::Expr::Unit => Expr::Unit,
            ast::Expr::StringLit { parts } => return self.lower_string_lit(parts, span),
            ast::Expr::Ident(name) => Expr::Ident(name.clone()),
            ast::Expr::SelfExpr => Expr::SelfExpr,
            ast::Expr::Call { callee, args } => Expr::Call {
                callee: self.lower_expr(*callee),
                args: args.iter().map(|a| self.lower_expr(*a)).collect(),
            },
            ast::Expr::Field { recv, name } => Expr::Field {
                recv: self.lower_expr(*recv),
                name: name.clone(),
            },
            ast::Expr::SafeNav { recv, name, args } => {
                return self.lower_safe_nav(*recv, name, args.as_deref(), span);
            }
            ast::Expr::Index { recv, index } => Expr::Index {
                recv: self.lower_expr(*recv),
                index: self.lower_expr(*index),
            },
            ast::Expr::Unary { op, operand } => Expr::Unary {
                op: *op,
                operand: self.lower_expr(*operand),
            },
            ast::Expr::Binary { op, lhs, rhs } => Expr::Binary {
                op: *op,
                lhs: self.lower_expr(*lhs),
                rhs: self.lower_expr(*rhs),
            },
            ast::Expr::Pipe { lhs, target } => return self.lower_pipe(*lhs, *target, span),
            ast::Expr::Coalesce { lhs, rhs } => return self.lower_coalesce(*lhs, *rhs, span),
            ast::Expr::Lambda { params, body } => Expr::Lambda {
                params: params
                    .iter()
                    .map(|p| LambdaParam {
                        name: p.name.clone(),
                        name_span: p.name_span,
                        ty: p.ty.map(|ty| self.lower_type_expr(ty)),
                    })
                    .collect(),
                body: match body {
                    ast::LambdaBody::Expr(value) => LambdaBody::Expr(self.lower_expr(*value)),
                    ast::LambdaBody::Block(block) => LambdaBody::Block(self.lower_block(block)),
                },
            },
            ast::Expr::If(node) => Expr::If(self.lower_if(node)),
            ast::Expr::Match { scrutinee, arms } => Expr::Match {
                scrutinee: self.lower_expr(*scrutinee),
                arms: arms
                    .iter()
                    .map(|arm| MatchArm {
                        pattern: self.lower_pattern(arm.pattern),
                        guard: arm.guard.map(|g| self.lower_expr(g)),
                        body: self.lower_arm_body(&arm.body),
                    })
                    .collect(),
            },
            ast::Expr::VectorLit(elements) => {
                Expr::VectorLit(elements.iter().map(|e| self.lower_expr(*e)).collect())
            }
            ast::Expr::MapLit(entries) => Expr::MapLit(
                entries
                    .iter()
                    .map(|(key, value)| (self.lower_expr(*key), self.lower_expr(*value)))
                    .collect(),
            ),
            ast::Expr::StructLit { type_name, fields } => Expr::StructLit {
                type_name: type_name.clone(),
                fields: fields
                    .iter()
                    .map(|(name, value)| (name.clone(), self.lower_expr(*value)))
                    .collect(),
            },
            ast::Expr::Range { lo, hi, inclusive } => Expr::Range {
                lo: self.lower_expr(*lo),
                hi: self.lower_expr(*hi),
                inclusive: *inclusive,
            },
            ast::Expr::Catch {
                subject,
                exhaustive,
                binding,
                arms,
            } => Expr::Catch {
                subject: self.lower_expr(*subject),
                exhaustive: *exhaustive,
                binding: binding.clone(),
                arms: arms
                    .iter()
                    .map(|arm| CatchArm {
                        types: arm.types.clone(),
                        guard: arm.guard.map(|g| self.lower_expr(g)),
                        body: self.lower_arm_body(&arm.body),
                    })
                    .collect(),
            },
            ast::Expr::EnumCtor { name, args } => Expr::EnumCtor {
                name: name.clone(),
                args: args.iter().map(|a| self.lower_expr(*a)).collect(),
            },
        };

        self.hir.alloc_expr(expr, span)
    }

    fn lower_arm_body(&mut self, body: &ast::ArmBody) -> ArmBody {
        match body {
            ast::ArmBody::Expr(value) => ArmBody::Expr(self.lower_expr(*value)),
            ast::ArmBody::Block(block) => ArmBody::Block(self.lower_block(block)),
        }
    }

    /// `a |> f(b, c)` → `f(a, b, c)`. Pure syntactic rewriting
    /// (`docs/spec/03-types.md`'s operator table).
    ///
    /// The parser's pipe target is a whole postfix expression, so a
    /// target that is not itself a call (`a |> foo.filter`) lowers to
    /// calling it with the piped value as the only argument — the same
    /// `foo.filter(a)` the parenthesized form would produce.
    fn lower_pipe(&mut self, lhs: ast::ExprId, target: ast::ExprId, span: Span) -> ExprId {
        let lhs = self.lower_expr(lhs);

        let expr = match self.ast.expr(target) {
            ast::Expr::Call { callee, args } => {
                let callee = self.lower_expr(*callee);

                let mut new_args = Vec::with_capacity(args.len() + 1);
                new_args.push(lhs);
                new_args.extend(args.iter().map(|a| self.lower_expr(*a)));

                Expr::Call {
                    callee,
                    args: new_args,
                }
            }
            _ => Expr::Call {
                callee: self.lower_expr(target),
                args: vec![lhs],
            },
        };

        self.hir.alloc_expr(expr, span)
    }

    /// `lhs ?? rhs` → `match lhs { Some($t) => $t, None => rhs }`. The
    /// `match` is what keeps `??` lazy: `rhs` sits in the `None` arm and
    /// is evaluated only when `lhs` is `None`
    /// (`docs/spec/03-types.md`'s operator table).
    fn lower_coalesce(&mut self, lhs: ast::ExprId, rhs: ast::ExprId, span: Span) -> ExprId {
        let scrutinee = self.lower_expr(lhs);
        let temp = self.fresh_temp();

        let some_binding = self.hir.alloc_pattern(Pattern::Binding(temp.clone()), span);
        let some_pattern = self.hir.alloc_pattern(
            Pattern::Ctor {
                name: "Some".to_string(),
                args: vec![some_binding],
            },
            span,
        );
        let some_body = self.hir.alloc_expr(Expr::Ident(temp), span);

        let none_pattern = self.hir.alloc_pattern(
            Pattern::Ctor {
                name: "None".to_string(),
                args: vec![],
            },
            span,
        );
        let none_body = self.lower_expr(rhs);

        let match_expr = self.hir.alloc_expr(
            Expr::Match {
                scrutinee,
                arms: vec![
                    MatchArm {
                        pattern: some_pattern,
                        guard: None,
                        body: ArmBody::Expr(some_body),
                    },
                    MatchArm {
                        pattern: none_pattern,
                        guard: None,
                        body: ArmBody::Expr(none_body),
                    },
                ],
            },
            span,
        );
        self.sugar_origins.insert(match_expr, SugarOrigin::Coalesce);
        match_expr
    }

    /// `a?.b` → `match a { Some($t) => OptionWrap($t.b), None => None }`;
    /// `a?.m(args)` wraps the method call the same way. `OptionWrap`
    /// rather than a plain `Some(...)` because `?.`'s no-nested-Option
    /// flatten rule (`docs/spec/03-types.md`) is type-directed; see
    /// [`crate::Expr::OptionWrap`]. Chained `a?.b?.c` needs nothing
    /// special: each `SafeNav` lowers independently and the outer one
    /// matches on the inner one's `Option` result.
    fn lower_safe_nav(
        &mut self,
        recv: ast::ExprId,
        name: &str,
        args: Option<&[ast::ExprId]>,
        span: Span,
    ) -> ExprId {
        let scrutinee = self.lower_expr(recv);
        let temp = self.fresh_temp();

        let some_binding = self.hir.alloc_pattern(Pattern::Binding(temp.clone()), span);
        let some_pattern = self.hir.alloc_pattern(
            Pattern::Ctor {
                name: "Some".to_string(),
                args: vec![some_binding],
            },
            span,
        );

        let temp_ref = self.hir.alloc_expr(Expr::Ident(temp), span);
        let member = self.hir.alloc_expr(
            Expr::Field {
                recv: temp_ref,
                name: name.to_string(),
            },
            span,
        );
        let accessed = match args {
            None => member,
            Some(args) => {
                let args = args.iter().map(|a| self.lower_expr(*a)).collect();
                self.hir.alloc_expr(
                    Expr::Call {
                        callee: member,
                        args,
                    },
                    span,
                )
            }
        };
        let some_body = self.hir.alloc_expr(Expr::OptionWrap(accessed), span);

        let none_pattern = self.hir.alloc_pattern(
            Pattern::Ctor {
                name: "None".to_string(),
                args: vec![],
            },
            span,
        );
        let none_body = self.hir.alloc_expr(
            Expr::EnumCtor {
                name: "None".to_string(),
                args: vec![],
            },
            span,
        );

        let match_expr = self.hir.alloc_expr(
            Expr::Match {
                scrutinee,
                arms: vec![
                    MatchArm {
                        pattern: some_pattern,
                        guard: None,
                        body: ArmBody::Expr(some_body),
                    },
                    MatchArm {
                        pattern: none_pattern,
                        guard: None,
                        body: ArmBody::Expr(none_body),
                    },
                ],
            },
            span,
        );
        self.sugar_origins.insert(match_expr, SugarOrigin::SafeNav);
        match_expr
    }

    /// Interpolation → concatenation (`docs/spec/00-vision.md`): text
    /// parts become plain `Str` literals, each `#{e}` becomes
    /// `ToString(e)`, and the pieces fold left-to-right with string `+`.
    /// A literal with no interpolation collapses to one `Str` (its text
    /// parts joined), so `"#{e}"` alone lowers to just `ToString(e)`
    /// with no concatenation.
    fn lower_string_lit(&mut self, parts: &[ast::StringPart], span: Span) -> ExprId {
        let has_interp = parts
            .iter()
            .any(|p| matches!(p, ast::StringPart::Interp(_)));

        if !has_interp {
            let text: String = parts
                .iter()
                .map(|p| match p {
                    ast::StringPart::Text { text, .. } => text.as_str(),
                    ast::StringPart::Interp(_) => unreachable!(),
                })
                .collect();
            return self.hir.alloc_expr(Expr::Str(text), span);
        }

        let mut acc: Option<ExprId> = None;

        for part in parts {
            let piece = match part {
                ast::StringPart::Text { text, .. } => {
                    self.hir.alloc_expr(Expr::Str(text.clone()), span)
                }
                ast::StringPart::Interp(value) => {
                    let interp_span = self.ast.span_of_expr(*value);
                    let inner = self.lower_expr(*value);
                    self.hir.alloc_expr(Expr::ToString(inner), interp_span)
                }
            };

            acc = Some(match acc {
                None => piece,
                Some(lhs) => self.hir.alloc_expr(
                    Expr::Binary {
                        op: ast::BinaryOp::Add,
                        lhs,
                        rhs: piece,
                    },
                    span,
                ),
            });
        }

        acc.expect("interpolated string literal has at least one part")
    }

    // Patterns and types --------------------------------------------

    fn lower_pattern(&mut self, id: ast::PatternId) -> PatternId {
        let span = self.ast.span_of_pattern(id);

        let pattern = match self.ast.pattern(id) {
            ast::Pattern::Wildcard => Pattern::Wildcard,
            ast::Pattern::Literal(lit) => Pattern::Literal(lit.clone()),
            ast::Pattern::Binding(name) => Pattern::Binding(name.clone()),
            ast::Pattern::Ctor { name, args } => Pattern::Ctor {
                name: name.clone(),
                args: args.iter().map(|a| self.lower_pattern(*a)).collect(),
            },
            ast::Pattern::Tuple(elements) => {
                Pattern::Tuple(elements.iter().map(|e| self.lower_pattern(*e)).collect())
            }
        };

        self.hir.alloc_pattern(pattern, span)
    }

    fn lower_type_expr(&mut self, id: ast::TypeExprId) -> TypeExprId {
        let span = self.ast.span_of_type_expr(id);

        let type_expr = match self.ast.type_expr(id) {
            ast::TypeExpr::Named { name, args } => TypeExpr::Named {
                name: name.clone(),
                args: args.iter().map(|a| self.lower_type_expr(*a)).collect(),
            },
            ast::TypeExpr::Tuple(elements) => {
                TypeExpr::Tuple(elements.iter().map(|e| self.lower_type_expr(*e)).collect())
            }
            ast::TypeExpr::Fn { params, ret } => TypeExpr::Fn {
                params: params.iter().map(|p| self.lower_type_expr(*p)).collect(),
                ret: self.lower_type_expr(*ret),
            },
        };

        self.hir.alloc_type_expr(type_expr, span)
    }
}

/// The binary operator behind a compound assignment, or `None` for plain
/// `=`.
fn compound_binary_op(op: ast::AssignOp) -> Option<ast::BinaryOp> {
    match op {
        ast::AssignOp::Assign => None,
        ast::AssignOp::AddAssign => Some(ast::BinaryOp::Add),
        ast::AssignOp::SubAssign => Some(ast::BinaryOp::Sub),
        ast::AssignOp::MulAssign => Some(ast::BinaryOp::Mul),
        ast::AssignOp::DivAssign => Some(ast::BinaryOp::Div),
        ast::AssignOp::RemAssign => Some(ast::BinaryOp::Rem),
    }
}
