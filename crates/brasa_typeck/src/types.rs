//! The checker's type representation (`docs/spec/03-types.md`).
//!
//! Types are plain values built with `Box`; there is no interning. The
//! checker clones freely — Brasa types are shallow in practice (a couple
//! of generic layers at most), so sharing machinery would buy nothing.

use std::collections::HashMap;

use brasa_hir::{Hir, Item, ItemId};
use brasa_resolver::DefRef;

/// A Brasa type as the checker sees it.
///
/// `Range` is its own lazy type over ints, not `Vector<int>`
/// (`docs/spec/03-types.md`). `Never` is the type of `return`, `throw`,
/// `break`, and `continue`; it unifies with everything so
/// `let x = if ok then v else return end` works
/// (`docs/spec/03-types.md`, flow rules). `Unknown` stands for every
/// construct deferred past this milestone — error sets (M2), stdlib
/// module members (M4) — and doubles as the error type after a reported
/// mismatch: it unifies silently and never produces follow-on
/// diagnostics.
///
/// `Generic` is a rigid type parameter of the item (or struct method)
/// that declared it: instantiation is checker-side only, since the VM
/// executes one uniform function per generic definition — there is no
/// monomorphization (`docs/spec/03-types.md`, generics execution model).
/// `Struct`/`Enum` carry their generic arguments; the vector is empty
/// for non-generic definitions.
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
    Fn {
        params: Vec<Type>,
        ret: Box<Type>,
    },
    Struct(ItemId, Vec<Type>),
    Enum(ItemId, Vec<Type>),
    Generic {
        owner: DefRef,
        index: usize,
    },
    /// The compiler-known `Output` record returned by the `std::proc`
    /// runners (`docs/spec/05-stdlib.md`, BRS-32): exactly the fields
    /// `stdout: string`, `stderr: string`, `code: int`. Native — not
    /// user-constructible and not a pattern.
    ProcOutput,
    /// The compiler-known `Json` document type (`docs/spec/05-stdlib.md`,
    /// BRS-34): an immutable parsed JSON tree produced by `json.parse`.
    /// Opaque in v1 — no constructors and no patterns; access goes
    /// through Option-yielding indexing and the `as*` accessors.
    Json,
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
            Type::Struct(item, args) | Type::Enum(item, args) => {
                let name = item_name(hir, *item);
                if args.is_empty() {
                    name
                } else {
                    let parts: Vec<String> = args.iter().map(|a| a.display(hir)).collect();
                    format!("{}<{}>", name, parts.join(", "))
                }
            }
            Type::Generic { owner, index } => generic_name(hir, *owner, *index),
            Type::ProcOutput => "Output".to_string(),
            Type::Json => "Json".to_string(),
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

/// The declared name of a generic parameter, resolved through its
/// owner's generics list (a struct method resolves via the owning
/// struct's method list, mirroring `brasa_resolver`'s dump).
pub(crate) fn generic_name(hir: &Hir, owner: DefRef, index: usize) -> String {
    let generics = match owner {
        DefRef::Item(item) => match hir.item(item) {
            Item::FuncDef(func) => Some(&func.generics),
            Item::StructDef(def) => Some(&def.generics),
            Item::EnumDef(def) => Some(&def.generics),
            Item::InterfaceDef(def) => Some(&def.generics),
            _ => None,
        },
        DefRef::Method { owner, index } => match hir.item(owner) {
            Item::StructDef(def) => def.methods.get(index).map(|m| &m.generics),
            _ => None,
        },
    };

    generics
        .and_then(|g| g.get(index))
        .map(|g| g.name.clone())
        .unwrap_or_else(|| "<generic>".to_string())
}

/// Structural unification: the joined type when `a` and `b` are
/// compatible, `None` when they are not.
///
/// `Unknown` and `Never` join with anything and the more-known side
/// wins, so deferred constructs and `never`-typed flow never fail
/// unification (`docs/spec/03-types.md`, flow rules). Everything else
/// follows the no-coercion rule: types are compatible only when they
/// are structurally identical. A `Generic` is rigid: it unifies only
/// with the same `(owner, index)` parameter (via the equality fallback);
/// call sites substitute concrete types *before* unifying, so a generic
/// never has to unify with its instantiation here.
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
        (Type::Struct(x, xa), Type::Struct(y, ya)) if x == y && xa.len() == ya.len() => {
            let args: Option<Vec<Type>> = xa.iter().zip(ya).map(|(a, b)| unify(a, b)).collect();
            Some(Type::Struct(*x, args?))
        }
        (Type::Enum(x, xa), Type::Enum(y, ya)) if x == y && xa.len() == ya.len() => {
            let args: Option<Vec<Type>> = xa.iter().zip(ya).map(|(a, b)| unify(a, b)).collect();
            Some(Type::Enum(*x, args?))
        }
        _ => {
            if a == b {
                Some(a.clone())
            } else {
                None
            }
        }
    }
}

/// Replaces every `Generic` whose `(owner, index)` appears in `map` with
/// the mapped type, recursively. Call sites solve their generic
/// parameters into such a map and substitute signatures before checking
/// arguments and results, so `unify` never needs instantiation logic.
pub(crate) fn substitute(ty: &Type, map: &HashMap<(DefRef, usize), Type>) -> Type {
    match ty {
        Type::Generic { owner, index } => map
            .get(&(*owner, *index))
            .cloned()
            .unwrap_or_else(|| ty.clone()),
        Type::Vector(elem) => Type::Vector(Box::new(substitute(elem, map))),
        Type::Set(elem) => Type::Set(Box::new(substitute(elem, map))),
        Type::Option(inner) => Type::Option(Box::new(substitute(inner, map))),
        Type::Map(key, value) => Type::Map(
            Box::new(substitute(key, map)),
            Box::new(substitute(value, map)),
        ),
        Type::Tuple(elems) => Type::Tuple(elems.iter().map(|e| substitute(e, map)).collect()),
        Type::Fn { params, ret } => Type::Fn {
            params: params.iter().map(|p| substitute(p, map)).collect(),
            ret: Box::new(substitute(ret, map)),
        },
        Type::Struct(item, args) => {
            Type::Struct(*item, args.iter().map(|a| substitute(a, map)).collect())
        }
        Type::Enum(item, args) => {
            Type::Enum(*item, args.iter().map(|a| substitute(a, map)).collect())
        }
        _ => ty.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::{Type, substitute, unify};
    use brasa_arena::Id;
    use brasa_resolver::DefRef;
    use std::collections::HashMap;

    fn generic(index: usize) -> Type {
        Type::Generic {
            owner: DefRef::Item(Id::new(0)),
            index,
        }
    }

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

    #[test]
    fn generics_are_rigid() {
        assert_eq!(unify(&generic(0), &generic(0)), Some(generic(0)));
        assert_eq!(unify(&generic(0), &generic(1)), None);
        assert_eq!(unify(&generic(0), &Type::Int), None);
        assert_eq!(unify(&generic(0), &Type::Unknown), Some(generic(0)));
    }

    #[test]
    fn substitute_replaces_mapped_generics_recursively() {
        let mut map = HashMap::new();
        map.insert((DefRef::Item(Id::new(0)), 0), Type::Int);

        assert_eq!(substitute(&generic(0), &map), Type::Int);
        assert_eq!(
            substitute(&Type::vector(generic(0)), &map),
            Type::vector(Type::Int)
        );
        assert_eq!(substitute(&generic(1), &map), generic(1));
    }
}
