//! The builtin method table: what `.method(...)` means on primitive and
//! container types.
//!
//! This is the minimal M1 slice of `docs/spec/05-stdlib.md` — exact
//! stdlib signatures close module by module during M4, so this table
//! only carries what the checker needs now. `string.toInt`/`toFloat`
//! return `Option` here; whether they throw instead is settled with the
//! M4 signatures.

use crate::types::{Type, unify};

/// How a builtin method's result type is computed.
pub enum RetRule {
    Fixed(Type),
    /// `Vector<T>.map((T) -> U) -> Vector<U>`: the result element is the
    /// return type of the function argument, known only after the
    /// argument is checked.
    VectorOfFnRet,
}

/// One builtin method signature: parameter types (`self` excluded) and
/// the result rule.
pub struct MethodSig {
    pub params: Vec<Type>,
    pub ret: RetRule,
}

fn sig(params: Vec<Type>, ret: Type) -> MethodSig {
    MethodSig {
        params,
        ret: RetRule::Fixed(ret),
    }
}

/// Looks up `name` on a receiver of type `recv`. Returns `None` when the
/// receiver type has no such builtin method (the checker layers the
/// universal derived `toString` and the unknown-member error on top).
pub fn method(recv: &Type, name: &str) -> Option<MethodSig> {
    match recv {
        Type::Int => int_method(name),
        Type::Float => float_method(name),
        Type::String => string_method(name),
        Type::Vector(elem) => vector_method(elem, name),
        Type::Map(key, value) => map_method(key, value, name),
        Type::Set(elem) => set_method(elem, name),
        _ => None,
    }
}

fn int_method(name: &str) -> Option<MethodSig> {
    match name {
        "toFloat" => Some(sig(vec![], Type::Float)),
        "toString" => Some(sig(vec![], Type::String)),
        _ => None,
    }
}

fn float_method(name: &str) -> Option<MethodSig> {
    match name {
        "toInt" => Some(sig(vec![], Type::Int)),
        "toString" => Some(sig(vec![], Type::String)),
        _ => None,
    }
}

fn string_method(name: &str) -> Option<MethodSig> {
    match name {
        "len" => Some(sig(vec![], Type::Int)),
        "count" => Some(sig(vec![Type::String], Type::Int)),
        "trim" | "toUpper" | "toLower" => Some(sig(vec![], Type::String)),
        "contains?" | "startsWith?" | "endsWith?" => Some(sig(vec![Type::String], Type::Bool)),
        "split" => Some(sig(vec![Type::String], Type::vector(Type::String))),
        "lines" => Some(sig(vec![], Type::vector(Type::String))),
        "chars" => Some(sig(vec![], Type::vector(Type::Char))),
        "slice" => Some(sig(vec![Type::Int, Type::Int], Type::String)),
        "repeat" => Some(sig(vec![Type::Int], Type::String)),
        "replace" => Some(sig(vec![Type::String, Type::String], Type::String)),
        "find" => Some(sig(vec![Type::String], Type::option(Type::Int))),
        "toInt" => Some(sig(vec![], Type::option(Type::Int))),
        "toFloat" => Some(sig(vec![], Type::option(Type::Float))),
        _ => None,
    }
}

fn vector_method(elem: &Type, name: &str) -> Option<MethodSig> {
    match name {
        "len" => Some(sig(vec![], Type::Int)),
        "push" => Some(sig(vec![elem.clone()], Type::Unit)),
        "pop" | "first" | "last" => Some(sig(vec![], Type::option(elem.clone()))),
        "reverse" => Some(sig(vec![], Type::vector(elem.clone()))),
        "contains?" => Some(sig(vec![elem.clone()], Type::Bool)),
        // `join` requires `Vector<string>` (decision recorded here; the
        // checker reports a dedicated error for other element types).
        "join" if unify(elem, &Type::String).is_some() => {
            Some(sig(vec![Type::String], Type::String))
        }
        "map" => Some(MethodSig {
            params: vec![Type::func(vec![elem.clone()], Type::Unknown)],
            ret: RetRule::VectorOfFnRet,
        }),
        "filter" => Some(sig(
            vec![Type::func(vec![elem.clone()], Type::Bool)],
            Type::vector(elem.clone()),
        )),
        "each" => Some(sig(
            vec![Type::func(vec![elem.clone()], Type::Unit)],
            Type::Unit,
        )),
        "sortBy" => Some(sig(
            vec![Type::func(vec![elem.clone()], Type::Unknown)],
            Type::vector(elem.clone()),
        )),
        _ => None,
    }
}

fn map_method(key: &Type, value: &Type, name: &str) -> Option<MethodSig> {
    match name {
        "len" => Some(sig(vec![], Type::Int)),
        "keys" => Some(sig(vec![], Type::vector(key.clone()))),
        "values" => Some(sig(vec![], Type::vector(value.clone()))),
        "insert" => Some(sig(vec![key.clone(), value.clone()], Type::Unit)),
        "remove" => Some(sig(vec![key.clone()], Type::option(value.clone()))),
        "has?" => Some(sig(vec![key.clone()], Type::Bool)),
        "get" => Some(sig(vec![key.clone()], Type::option(value.clone()))),
        _ => None,
    }
}

fn set_method(elem: &Type, name: &str) -> Option<MethodSig> {
    match name {
        "add" => Some(sig(vec![elem.clone()], Type::Unit)),
        "remove" => Some(sig(vec![elem.clone()], Type::Bool)),
        "has?" => Some(sig(vec![elem.clone()], Type::Bool)),
        "len" => Some(sig(vec![], Type::Int)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{RetRule, method};
    use crate::types::Type;

    #[test]
    fn join_requires_string_elements() {
        assert!(method(&Type::vector(Type::String), "join").is_some());
        assert!(method(&Type::vector(Type::Int), "join").is_none());
        assert!(method(&Type::vector(Type::Unknown), "join").is_some());
    }

    #[test]
    fn string_methods_from_the_stdlib_slice() {
        let chars = method(&Type::String, "chars").expect("chars exists");
        assert!(matches!(&chars.ret, RetRule::Fixed(t) if *t == Type::vector(Type::Char)));

        let find = method(&Type::String, "find").expect("find exists");
        assert_eq!(find.params, vec![Type::String]);
        assert!(matches!(&find.ret, RetRule::Fixed(t) if *t == Type::option(Type::Int)));

        let slice = method(&Type::String, "slice").expect("slice exists");
        assert_eq!(slice.params, vec![Type::Int, Type::Int]);
    }

    #[test]
    fn map_result_depends_on_the_function_argument() {
        let sig = method(&Type::vector(Type::Int), "map").expect("map exists");
        assert!(matches!(sig.ret, RetRule::VectorOfFnRet));
        assert_eq!(sig.params, vec![Type::func(vec![Type::Int], Type::Unknown)]);
    }
}
