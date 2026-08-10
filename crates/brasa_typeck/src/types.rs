//! The checker's type representation (`docs/spec/03-types.md`).
//!
//! Types are plain values built with `Box`; there is no interning. The
//! checker clones freely — Brasa types are shallow in practice (a couple
//! of generic layers at most), so sharing machinery would buy nothing.

use brasa_hir::{Hir, Item, ItemId};

/// A Brasa type as the checker sees it.
///
/// `Range` is its own lazy type over ints, not `Vector<int>`
/// (`docs/spec/03-types.md`). `Never` is the type of `return`, `throw`,
/// `break`, and `continue`; it unifies with everything so
/// `let x = if ok then v else return end` works
/// (`docs/spec/03-types.md`, flow rules). `Unknown` stands for every
/// construct deferred past this milestone — generic parameters (BRS-17),
/// error sets (M2), stdlib module members (M4) — and doubles as the
/// error type after a reported mismatch: it unifies silently and never
/// produces follow-on diagnostics.
#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    Int,
    Float,
    Bool,
    String,
    Char,
    Unit,
    Never,
    Unknown,
    Range,
    Vector(Box<Type>),
    Map(Box<Type>, Box<Type>),
    Set(Box<Type>),
    Option(Box<Type>),
    Tuple(Vec<Type>),
    Fn { params: Vec<Type>, ret: Box<Type> },
    Struct(ItemId),
    Enum(ItemId),
}

/// How one `Expr::OptionWrap` node resolved: `?.` flattens, so the
/// checker decides per node whether the member value gets wrapped in
/// `Some` or is already an `Option` (`docs/spec/03-types.md`, the `?.`
/// operator rule). The tree-walker consumes this table verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WrapDecision {
    Wrap,
    NoOp,
}

impl Type {
    pub fn vector(elem: Type) -> Type {
        Type::Vector(Box::new(elem))
    }

    pub fn option(inner: Type) -> Type {
        Type::Option(Box::new(inner))
    }

    pub fn func(params: Vec<Type>, ret: Type) -> Type {
        Type::Fn {
            params,
            ret: Box::new(ret),
        }
    }

    /// Whether this type silently accepts any operation: `Unknown`
    /// (deferred/poisoned) and `Never` (unreachable) never produce
    /// follow-on diagnostics.
    pub fn is_flexible(&self) -> bool {
        matches!(self, Type::Unknown | Type::Never)
    }

    /// Renders the type the way diagnostics and dumps spell it. Nominal
    /// types print their declared name, which needs the HIR.
    pub fn display(&self, hir: &Hir) -> String {
        match self {
            Type::Int => "int".to_string(),
            Type::Float => "float".to_string(),
            Type::Bool => "bool".to_string(),
            Type::String => "string".to_string(),
            Type::Char => "char".to_string(),
            Type::Unit => "unit".to_string(),
            Type::Never => "never".to_string(),
            Type::Unknown => "unknown".to_string(),
            Type::Range => "Range".to_string(),
            Type::Vector(elem) => format!("Vector<{}>", elem.display(hir)),
            Type::Map(key, value) => {
                format!("Map<{}, {}>", key.display(hir), value.display(hir))
            }
            Type::Set(elem) => format!("Set<{}>", elem.display(hir)),
            Type::Option(inner) => format!("Option<{}>", inner.display(hir)),
            Type::Tuple(elems) => {
                let parts: Vec<String> = elems.iter().map(|e| e.display(hir)).collect();
                format!("({})", parts.join(", "))
            }
            Type::Fn { params, ret } => {
                let parts: Vec<String> = params.iter().map(|p| p.display(hir)).collect();
                format!("({}) -> {}", parts.join(", "), ret.display(hir))
            }
            Type::Struct(item) | Type::Enum(item) => item_name(hir, *item),
        }
    }
}

/// The declared name of a nominal item, for diagnostics and dumps.
pub(crate) fn item_name(hir: &Hir, item: ItemId) -> String {
    match hir.item(item) {
        Item::FuncDef(def) => def.name.clone(),
        Item::StructDef(def) => def.name.clone(),
        Item::EnumDef(def) => def.name.clone(),
        Item::InterfaceDef(def) => def.name.clone(),
        Item::TopLet(top_let) => top_let.let_stmt.name.clone(),
        Item::Import(_) | Item::Stmt(_) => "<item>".to_string(),
    }
}

/// Structural unification: the joined type when `a` and `b` are
/// compatible, `None` when they are not.
///
/// `Unknown` and `Never` join with anything and the more-known side
/// wins, so deferred constructs and `never`-typed flow never fail
/// unification (`docs/spec/03-types.md`, flow rules). Everything else
/// follows the no-coercion rule: types are compatible only when they
/// are structurally identical.
pub fn unify(a: &Type, b: &Type) -> Option<Type> {
    match (a, b) {
        (Type::Unknown, other) | (other, Type::Unknown) => Some(other.clone()),
        (Type::Never, other) | (other, Type::Never) => Some(other.clone()),
        (Type::Vector(x), Type::Vector(y)) => Some(Type::Vector(Box::new(unify(x, y)?))),
        (Type::Set(x), Type::Set(y)) => Some(Type::Set(Box::new(unify(x, y)?))),
        (Type::Option(x), Type::Option(y)) => Some(Type::Option(Box::new(unify(x, y)?))),
        (Type::Map(ka, va), Type::Map(kb, vb)) => Some(Type::Map(
            Box::new(unify(ka, kb)?),
            Box::new(unify(va, vb)?),
        )),
        (Type::Tuple(xs), Type::Tuple(ys)) => {
            if xs.len() != ys.len() {
                return None;
            }

            let elems: Option<Vec<Type>> = xs.iter().zip(ys).map(|(x, y)| unify(x, y)).collect();
            Some(Type::Tuple(elems?))
        }
        (
            Type::Fn {
                params: pa,
                ret: ra,
            },
            Type::Fn {
                params: pb,
                ret: rb,
            },
        ) => {
            if pa.len() != pb.len() {
                return None;
            }

            let params: Option<Vec<Type>> = pa.iter().zip(pb).map(|(x, y)| unify(x, y)).collect();
            Some(Type::Fn {
                params: params?,
                ret: Box::new(unify(ra, rb)?),
            })
        }
        (Type::Struct(x), Type::Struct(y)) if x == y => Some(Type::Struct(*x)),
        (Type::Enum(x), Type::Enum(y)) if x == y => Some(Type::Enum(*x)),
        _ => {
            if a == b {
                Some(a.clone())
            } else {
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Type, unify};

    #[test]
    fn unknown_and_never_join_with_anything() {
        assert_eq!(unify(&Type::Unknown, &Type::Int), Some(Type::Int));
        assert_eq!(unify(&Type::Never, &Type::String), Some(Type::String));
        assert_eq!(
            unify(&Type::vector(Type::Unknown), &Type::vector(Type::Bool)),
            Some(Type::vector(Type::Bool))
        );
    }

    #[test]
    fn no_implicit_coercions() {
        assert_eq!(unify(&Type::Int, &Type::Float), None);
        assert_eq!(
            unify(&Type::vector(Type::Int), &Type::vector(Type::Float)),
            None
        );
        assert_eq!(
            unify(
                &Type::Tuple(vec![Type::Int]),
                &Type::Tuple(vec![Type::Int, Type::Int])
            ),
            None
        );
    }
}
