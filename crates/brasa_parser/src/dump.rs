//! A deterministic, span-free text dump of a parsed `brasa_ast::Ast`.
//!
//! Used for insta snapshots: spans are never printed (they would make
//! snapshots break on every whitespace-only edit to a fixture), and
//! iteration order always follows source order, so two parses of the
//! same input produce byte-identical dumps.

use std::fmt::Write as _;

use brasa_ast::{
    ArmBody, AssignOp, Ast, BinaryOp, Block, CatchType, Constraint, EnumDef, Expr, ExprId, FuncDef,
    IfNode, IfaceMember, ImportPath, InterfaceDef, Item, ItemId, Literal, Param, Pattern,
    PatternId, Stmt, StmtId, StringPart, StructDef, Throws, TopLet, TypeExpr, TypeExprId, UnaryOp,
};

/// Renders every root item in `roots`, in order, as an indented tree.
pub fn dump(ast: &Ast, roots: &[ItemId]) -> String {
    let mut out = String::new();

    for &root in roots {
        dump_item(ast, root, 0, &mut out);
    }

    out
}

fn indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

fn line(out: &mut String, depth: usize, text: &str) {
    indent(out, depth);
    out.push_str(text);
    out.push('\n');
}

fn dump_item(ast: &Ast, id: ItemId, depth: usize, out: &mut String) {
    match ast.item(id) {
        Item::Import(import) => match &import.path {
            ImportPath::Std(segments) => {
                line(out, depth, &format!("Import::Std({})", segments.join("::")));
            }
            ImportPath::File(path) => {
                line(out, depth, &format!("Import::File({path:?})"));
            }
        },
        Item::FuncDef(func) => dump_func(ast, func, depth, out),
        Item::StructDef(def) => dump_struct(ast, def, depth, out),
        Item::EnumDef(def) => dump_enum(ast, def, depth, out),
        Item::InterfaceDef(def) => dump_interface(ast, def, depth, out),
        Item::TopLet(top_let) => dump_top_let(ast, top_let, depth, out),
        Item::Stmt(stmt) => dump_stmt(ast, *stmt, depth, out),
    }
}

fn dump_generics(generics: &[brasa_ast::GenericParam], out: &mut String) -> String {
    if generics.is_empty() {
        return String::new();
    }

    let rendered: Vec<String> = generics
        .iter()
        .map(|g| match &g.constraint {
            None => g.name.clone(),
            Some(Constraint::Named(name)) => format!("{}: {}", g.name, name),
            Some(Constraint::Inline(members)) => {
                let parts: Vec<String> = members.iter().map(|m| m.name.clone()).collect();
                format!("{}: {{{}}}", g.name, parts.join(", "))
            }
        })
        .collect();

    let _ = out;
    format!("<{}>", rendered.join(", "))
}

fn dump_param(p: &Param) -> String {
    match p {
        Param::SelfParam { .. } => "self".to_string(),
        Param::Named { name, .. } => name.clone(),
    }
}

fn dump_throws(throws: &Option<Throws>) -> String {
    match throws {
        None => String::new(),
        Some(Throws::Never) => " throws never".to_string(),
        Some(Throws::Types(types)) => {
            let names: Vec<&str> = types.iter().map(|t| t.name.as_str()).collect();
            format!(" throws {}", names.join("|"))
        }
    }
}

fn dump_func(ast: &Ast, func: &FuncDef, depth: usize, out: &mut String) {
    let generics = dump_generics(&func.generics, out);
    let params: Vec<String> = func.params.iter().map(dump_param).collect();
    let pub_prefix = if func.is_pub { "pub " } else { "" };
    let throws = dump_throws(&func.throws);

    line(
        out,
        depth,
        &format!(
            "{pub_prefix}FuncDef {}{}({}){}",
            func.name,
            generics,
            params.join(", "),
            throws
        ),
    );
    dump_block(ast, &func.body, depth + 1, out);
}

fn dump_struct(ast: &Ast, def: &StructDef, depth: usize, out: &mut String) {
    let generics = dump_generics(&def.generics, out);
    let pub_prefix = if def.is_pub { "pub " } else { "" };
    line(
        out,
        depth,
        &format!("{pub_prefix}StructDef {}{}", def.name, generics),
    );

    for field in &def.fields {
        line(out, depth + 1, &format!("field {}", field.name));
    }
    for method in &def.methods {
        dump_func(ast, method, depth + 1, out);
    }
}

fn dump_enum(_ast: &Ast, def: &EnumDef, depth: usize, out: &mut String) {
    let generics = dump_generics(&def.generics, out);
    let pub_prefix = if def.is_pub { "pub " } else { "" };
    line(
        out,
        depth,
        &format!("{pub_prefix}EnumDef {}{}", def.name, generics),
    );

    for variant in &def.variants {
        let fields: Vec<&str> = variant.fields.iter().map(|f| f.name.as_str()).collect();
        line(
            out,
            depth + 1,
            &format!("variant {}({})", variant.name, fields.join(", ")),
        );
    }
}

fn dump_interface(_ast: &Ast, def: &InterfaceDef, depth: usize, out: &mut String) {
    let generics = dump_generics(&def.generics, out);
    let pub_prefix = if def.is_pub { "pub " } else { "" };
    line(
        out,
        depth,
        &format!("{pub_prefix}InterfaceDef {}{}", def.name, generics),
    );

    for member in &def.methods {
        dump_iface_member(member, depth + 1, out);
    }
}

fn dump_iface_member(member: &IfaceMember, depth: usize, out: &mut String) {
    let params: Vec<String> = member.params.iter().map(dump_param).collect();
    let throws = dump_throws(&member.throws);
    line(
        out,
        depth,
        &format!("def {}({}){}", member.name, params.join(", "), throws),
    );
}

fn dump_top_let(ast: &Ast, top_let: &TopLet, depth: usize, out: &mut String) {
    let pub_prefix = if top_let.is_pub { "pub " } else { "" };
    let mut_prefix = if top_let.let_stmt.mutable { "mut " } else { "" };
    line(
        out,
        depth,
        &format!("{pub_prefix}Let {mut_prefix}{}", top_let.let_stmt.name),
    );
    dump_expr(ast, top_let.let_stmt.value, depth + 1, out);
}

fn dump_block(ast: &Ast, block: &Block, depth: usize, out: &mut String) {
    for &stmt in block {
        dump_stmt(ast, stmt, depth, out);
    }
}

fn dump_stmt(ast: &Ast, id: StmtId, depth: usize, out: &mut String) {
    match ast.stmt(id) {
        Stmt::Let(let_stmt) => {
            let mut_prefix = if let_stmt.mutable { "mut " } else { "" };
            line(out, depth, &format!("Let {mut_prefix}{}", let_stmt.name));
            dump_expr(ast, let_stmt.value, depth + 1, out);
        }
        Stmt::Assign { target, op, value } => {
            line(out, depth, &format!("Assign {}", dump_assign_op(*op)));
            dump_expr(ast, *target, depth + 1, out);
            dump_expr(ast, *value, depth + 1, out);
        }
        Stmt::Return(value) => {
            line(out, depth, "Return");
            if let Some(value) = value {
                dump_expr(ast, *value, depth + 1, out);
            }
        }
        Stmt::Break => line(out, depth, "Break"),
        Stmt::Continue => line(out, depth, "Continue"),
        Stmt::Throw(value) => {
            line(out, depth, "Throw");
            dump_expr(ast, *value, depth + 1, out);
        }
        Stmt::If(node) => dump_if(ast, node, depth, out),
        Stmt::While { cond, body } => {
            line(out, depth, "While");
            dump_expr(ast, *cond, depth + 1, out);
            dump_block(ast, body, depth + 1, out);
        }
        Stmt::For {
            pattern,
            iterable,
            body,
        } => {
            line(out, depth, "For");
            dump_pattern(ast, *pattern, depth + 1, out);
            dump_expr(ast, *iterable, depth + 1, out);
            dump_block(ast, body, depth + 1, out);
        }
        Stmt::Expr(value) => dump_expr(ast, *value, depth, out),
    }
}

fn dump_assign_op(op: AssignOp) -> &'static str {
    match op {
        AssignOp::Assign => "=",
        AssignOp::AddAssign => "+=",
        AssignOp::SubAssign => "-=",
        AssignOp::MulAssign => "*=",
        AssignOp::DivAssign => "/=",
        AssignOp::RemAssign => "%=",
    }
}

fn dump_if(ast: &Ast, node: &IfNode, depth: usize, out: &mut String) {
    line(out, depth, "If");
    for (i, (cond, body)) in node.branches.iter().enumerate() {
        line(out, depth + 1, if i == 0 { "cond" } else { "elsif" });
        dump_expr(ast, *cond, depth + 2, out);
        line(out, depth + 1, "then");
        dump_block(ast, body, depth + 2, out);
    }
    if let Some(else_) = &node.else_ {
        line(out, depth + 1, "else");
        dump_block(ast, else_, depth + 2, out);
    }
}

fn dump_binary_op(op: BinaryOp) -> &'static str {
    match op {
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

fn dump_unary_op(op: UnaryOp) -> &'static str {
    match op {
        UnaryOp::Neg => "-",
        UnaryOp::Not => "!",
    }
}

fn dump_expr(ast: &Ast, id: ExprId, depth: usize, out: &mut String) {
    match ast.expr(id) {
        Expr::Int(value) => line(out, depth, &format!("Int({value})")),
        Expr::Float(value) => line(out, depth, &format!("Float({value})")),
        Expr::Bool(value) => line(out, depth, &format!("Bool({value})")),
        Expr::Char(value) => line(out, depth, &format!("Char({value:?})")),
        Expr::Unit => line(out, depth, "Unit"),
        Expr::StringLit { parts } => {
            line(out, depth, "StringLit");
            for part in parts {
                match part {
                    StringPart::Text { text, raw } => {
                        line(out, depth + 1, &format!("Text(raw={raw}) {text:?}"));
                    }
                    StringPart::Interp(value) => {
                        line(out, depth + 1, "Interp");
                        dump_expr(ast, *value, depth + 2, out);
                    }
                }
            }
        }
        Expr::Ident(name) => line(out, depth, &format!("Ident({name})")),
        Expr::SelfExpr => line(out, depth, "SelfExpr"),
        Expr::Call { callee, args } => {
            line(out, depth, "Call");
            dump_expr(ast, *callee, depth + 1, out);
            for arg in args {
                dump_expr(ast, *arg, depth + 1, out);
            }
        }
        Expr::Field { recv, name } => {
            line(out, depth, &format!("Field({name})"));
            dump_expr(ast, *recv, depth + 1, out);
        }
        Expr::SafeNav { recv, name, args } => {
            let mut header = format!("SafeNav({name})");
            if args.is_some() {
                let _ = write!(header, " [call]");
            }
            line(out, depth, &header);
            dump_expr(ast, *recv, depth + 1, out);
            if let Some(args) = args {
                for arg in args {
                    dump_expr(ast, *arg, depth + 1, out);
                }
            }
        }
        Expr::Index { recv, index } => {
            line(out, depth, "Index");
            dump_expr(ast, *recv, depth + 1, out);
            dump_expr(ast, *index, depth + 1, out);
        }
        Expr::Unary { op, operand } => {
            line(out, depth, &format!("Unary({})", dump_unary_op(*op)));
            dump_expr(ast, *operand, depth + 1, out);
        }
        Expr::Binary { op, lhs, rhs } => {
            line(out, depth, &format!("Binary({})", dump_binary_op(*op)));
            dump_expr(ast, *lhs, depth + 1, out);
            dump_expr(ast, *rhs, depth + 1, out);
        }
        Expr::Coalesce { lhs, rhs } => {
            line(out, depth, "Coalesce");
            dump_expr(ast, *lhs, depth + 1, out);
            dump_expr(ast, *rhs, depth + 1, out);
        }
        Expr::Pipe { lhs, target } => {
            line(out, depth, "Pipe");
            dump_expr(ast, *lhs, depth + 1, out);
            dump_expr(ast, *target, depth + 1, out);
        }
        Expr::Lambda { params, body } => {
            let names: Vec<&str> = params.iter().map(|p| p.name.as_str()).collect();
            line(out, depth, &format!("Lambda({})", names.join(", ")));
            match body {
                brasa_ast::LambdaBody::Expr(e) => dump_expr(ast, *e, depth + 1, out),
                brasa_ast::LambdaBody::Block(block) => dump_block(ast, block, depth + 1, out),
            }
        }
        Expr::If(node) => dump_if_expr(ast, node, depth, out),
        Expr::Match { scrutinee, arms } => {
            line(out, depth, "Match");
            dump_expr(ast, *scrutinee, depth + 1, out);
            for arm in arms {
                dump_pattern(ast, arm.pattern, depth + 1, out);
                if let Some(guard) = arm.guard {
                    line(out, depth + 1, "guard");
                    dump_expr(ast, guard, depth + 2, out);
                }
                dump_arm_body(ast, &arm.body, depth + 1, out);
            }
        }
        Expr::VectorLit(elements) => {
            line(out, depth, "VectorLit");
            for element in elements {
                dump_expr(ast, *element, depth + 1, out);
            }
        }
        Expr::MapLit(entries) => {
            line(out, depth, "MapLit");
            for (key, value) in entries {
                line(out, depth + 1, "entry");
                dump_expr(ast, *key, depth + 2, out);
                dump_expr(ast, *value, depth + 2, out);
            }
        }
        Expr::StructLit { type_name, fields } => {
            line(out, depth, &format!("StructLit({type_name})"));
            for (name, value) in fields {
                line(out, depth + 1, &format!("field {name}"));
                dump_expr(ast, *value, depth + 2, out);
            }
        }
        Expr::Range { lo, hi, inclusive } => {
            line(out, depth, &format!("Range(inclusive={inclusive})"));
            dump_expr(ast, *lo, depth + 1, out);
            dump_expr(ast, *hi, depth + 1, out);
        }
        Expr::Catch {
            subject,
            exhaustive,
            binding,
            arms,
        } => {
            line(
                out,
                depth,
                &format!("Catch(exhaustive={exhaustive}, binding={binding})"),
            );
            dump_expr(ast, *subject, depth + 1, out);
            for arm in arms {
                let types: Vec<String> = arm.types.iter().map(dump_catch_type).collect();
                line(out, depth + 1, &format!("arm {}", types.join("|")));
                if let Some(guard) = arm.guard {
                    line(out, depth + 2, "guard");
                    dump_expr(ast, guard, depth + 3, out);
                }
                dump_arm_body(ast, &arm.body, depth + 2, out);
            }
        }
        Expr::EnumCtor { name, args } => {
            line(out, depth, &format!("EnumCtor({name})"));
            for arg in args {
                dump_expr(ast, *arg, depth + 1, out);
            }
        }
    }
}

fn dump_catch_type(ty: &CatchType) -> String {
    match ty {
        CatchType::Named { name, .. } => name.clone(),
        CatchType::Wildcard { .. } => "_".to_string(),
    }
}

fn dump_arm_body(ast: &Ast, body: &ArmBody, depth: usize, out: &mut String) {
    match body {
        ArmBody::Expr(value) => dump_expr(ast, *value, depth + 1, out),
        ArmBody::Block(block) => dump_block(ast, block, depth + 1, out),
    }
}

fn dump_if_expr(ast: &Ast, node: &IfNode, depth: usize, out: &mut String) {
    dump_if(ast, node, depth, out);
}

fn dump_pattern(ast: &Ast, id: PatternId, depth: usize, out: &mut String) {
    match ast.pattern(id) {
        Pattern::Wildcard => line(out, depth, "Wildcard"),
        Pattern::Literal(lit) => line(out, depth, &format!("Literal({})", dump_literal(lit))),
        Pattern::Binding(name) => line(out, depth, &format!("Binding({name})")),
        Pattern::Ctor { name, args } => {
            line(out, depth, &format!("Ctor({name})"));
            for arg in args {
                dump_pattern(ast, *arg, depth + 1, out);
            }
        }
        Pattern::Tuple(elements) => {
            line(out, depth, "Tuple");
            for element in elements {
                dump_pattern(ast, *element, depth + 1, out);
            }
        }
    }
}

fn dump_literal(lit: &Literal) -> String {
    match lit {
        Literal::Int(v) => format!("{v}"),
        Literal::Float(v) => format!("{v}"),
        Literal::Bool(v) => format!("{v}"),
        Literal::Char(v) => format!("{v:?}"),
        Literal::Str(v) => format!("{v:?}"),
    }
}

// `TypeExprId`/`TypeExpr` are not reachable from any dumped node today
// (types are not printed to keep dumps focused on shape/values, matching
// what the precedence/newline/error-recovery test suites assert on), but
// are kept importable here for a future extension without another
// `pub(crate)` surface change.
#[allow(dead_code)]
fn dump_type(ast: &Ast, id: TypeExprId, depth: usize, out: &mut String) {
    match ast.type_expr(id) {
        TypeExpr::Named { name, args } => {
            line(out, depth, &format!("Type({name})"));
            for arg in args {
                dump_type(ast, *arg, depth + 1, out);
            }
        }
        TypeExpr::Tuple(elements) => {
            line(out, depth, "TupleType");
            for element in elements {
                dump_type(ast, *element, depth + 1, out);
            }
        }
        TypeExpr::Fn { params, ret } => {
            line(out, depth, "FnType");
            for param in params {
                dump_type(ast, *param, depth + 1, out);
            }
            dump_type(ast, *ret, depth + 1, out);
        }
    }
}
