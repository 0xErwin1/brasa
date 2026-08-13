//! The builtin method table: what `.method(...)` means on primitive and
//! container types.
//!
//! Carries the `docs/spec/05-stdlib.md` surface as it closes module by
//! module during M4 (BRS-31 strings, BRS-35 collections).
//! `string.toInt`/`toFloat` return the parsed number directly and
//! throw `string.ParseError` on failure (BRS-41); the error
//! contribution is the error-set pass's concern, not this table's.

use crate::types::{Type, unify};

/// How a builtin method's result type is computed.
pub enum RetRule {
    Fixed(Type),
    /// `Vector<T>.map((T) -> U) -> Vector<U>`: the result element is the
    /// return type of the function argument, known only after the
    /// argument is checked.
    VectorOfFnRet,
}

/// Whether `elem` can be a `sort` element or a `sortBy` key: the
/// orderable primitives (`docs/spec/05-stdlib.md`, BRS-35). Flexible
/// types stay allowed — the cause of the imprecision was already
/// reported.
fn orderable(elem: &Type) -> bool {
    matches!(elem, Type::Int | Type::Float | Type::String | Type::Char) || elem.is_flexible()
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
        Type::Json => json_method(name),
        // The `Json` accessors flatten through `Option<Json>` (`None`
        // stays `None`), so an Option-yielding indexing chain can end
        // in `.asString() ?? fallback` — `Json` values cannot be
        // constructed in the language, so a chain has no other way to
        // terminate (BRS-34, `docs/spec/05-stdlib.md`).
        Type::Option(inner) if **inner == Type::Json => json_method(name),
        _ => None,
    }
}

fn int_method(name: &str) -> Option<MethodSig> {
    match name {
        "toFloat" => Some(sig(vec![], Type::Float)),
        "toFixed" => Some(sig(vec![Type::Int], Type::String)),
        "toString" => Some(sig(vec![], Type::String)),
        _ => None,
    }
}

fn float_method(name: &str) -> Option<MethodSig> {
    match name {
        "toInt" => Some(sig(vec![], Type::Int)),
        "toFixed" => Some(sig(vec![Type::Int], Type::String)),
        "toString" => Some(sig(vec![], Type::String)),
        _ => None,
    }
}

fn string_method(name: &str) -> Option<MethodSig> {
    match name {
        "len" => Some(sig(vec![], Type::Int)),
        "count" => Some(sig(vec![Type::String], Type::Int)),
        "trim" | "trimStart" | "trimEnd" | "toUpper" | "toLower" | "reverse" => {
            Some(sig(vec![], Type::String))
        }
        "contains?" | "startsWith?" | "endsWith?" => Some(sig(vec![Type::String], Type::Bool)),
        "split" => Some(sig(vec![Type::String], Type::vector(Type::String))),
        "lines" => Some(sig(vec![], Type::vector(Type::String))),
        "chars" => Some(sig(vec![], Type::vector(Type::Char))),
        // `bytes` yields the UTF-8 byte values (0..=255) as ints
        // (`docs/spec/05-stdlib.md`).
        "bytes" => Some(sig(vec![], Type::vector(Type::Int))),
        "slice" => Some(sig(vec![Type::Int, Type::Int], Type::String)),
        "repeat" => Some(sig(vec![Type::Int], Type::String)),
        "padStart" | "padEnd" => Some(sig(vec![Type::Int, Type::String], Type::String)),
        "replace" => Some(sig(vec![Type::String, Type::String], Type::String)),
        "find" => Some(sig(vec![Type::String], Type::option(Type::Int))),
        "toInt" => Some(sig(vec![], Type::Int)),
        "toFloat" => Some(sig(vec![], Type::Float)),
        // Built-in regex (`docs/spec/05-stdlib.md`): the pattern is a
        // plain string until `std::re` lands; an invalid pattern throws
        // the native `string.RegexError` at runtime.
        "match?" => Some(sig(vec![Type::String], Type::Bool)),
        "captures" => Some(sig(
            vec![Type::String],
            Type::option(Type::vector(Type::String)),
        )),
        "replaceRe" => Some(sig(vec![Type::String, Type::String], Type::String)),
        "scan" => Some(sig(vec![Type::String], Type::vector(Type::String))),
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
        // `reduce(init, f)` folds left: `(U, (U, T) -> U) -> U`. This
        // table cannot see the `init` argument, so the entry here only
        // serves the bound-value form; the call form is special-cased
        // in the checker, which infers `U` from `init` (BRS-35).
        "reduce" => Some(sig(
            vec![
                Type::Unknown,
                Type::func(vec![Type::Unknown, elem.clone()], Type::Unknown),
            ],
            Type::Unknown,
        )),
        "find" => Some(sig(
            vec![Type::func(vec![elem.clone()], Type::Bool)],
            Type::option(elem.clone()),
        )),
        "any?" | "all?" => Some(sig(
            vec![Type::func(vec![elem.clone()], Type::Bool)],
            Type::Bool,
        )),
        // `sort` is natural ascending order, so it only exists on
        // vectors of orderable elements — the same key rule `sortBy`
        // enforces at runtime (decision recorded here, BRS-35).
        "sort" if orderable(elem) => Some(sig(vec![], Type::vector(elem.clone()))),
        // `zip(other)`'s pair type depends on the argument; like
        // `reduce`, the entry serves the bound-value form and the call
        // form is special-cased in the checker (BRS-35).
        "zip" => Some(sig(
            vec![Type::vector(Type::Unknown)],
            Type::vector(Type::Tuple(vec![elem.clone(), Type::Unknown])),
        )),
        // `flatten` removes exactly one nesting level, so it only
        // exists on `Vector<Vector<T>>` (decision recorded here).
        "flatten" => match elem {
            Type::Vector(inner) => Some(sig(vec![], Type::vector((**inner).clone()))),
            flexible if flexible.is_flexible() => Some(sig(vec![], Type::vector(Type::Unknown))),
            _ => None,
        },
        "uniq" => Some(sig(vec![], Type::vector(elem.clone()))),
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
        "entries" => Some(sig(
            vec![],
            Type::vector(Type::Tuple(vec![key.clone(), value.clone()])),
        )),
        "merge" => Some(sig(
            vec![Type::Map(Box::new(key.clone()), Box::new(value.clone()))],
            Type::Map(Box::new(key.clone()), Box::new(value.clone())),
        )),
        "each" => Some(sig(
            vec![Type::func(vec![key.clone(), value.clone()], Type::Unit)],
            Type::Unit,
        )),
        _ => None,
    }
}

/// The `Json` accessors (BRS-34, `docs/spec/05-stdlib.md`): every
/// `as*` accessor yields `Option` — `None` when the node is not that
/// JSON kind. `asInt` succeeds only for integral numbers representable
/// as `int`; `asFloat` succeeds for every number. `null?` distinguishes
/// an explicit JSON `null` from an absent member (which indexing
/// already reported as `None`).
fn json_method(name: &str) -> Option<MethodSig> {
    match name {
        "asString" => Some(sig(vec![], Type::option(Type::String))),
        "asInt" => Some(sig(vec![], Type::option(Type::Int))),
        "asFloat" => Some(sig(vec![], Type::option(Type::Float))),
        "asBool" => Some(sig(vec![], Type::option(Type::Bool))),
        "asArray" => Some(sig(vec![], Type::option(Type::vector(Type::Json)))),
        "asObject" => Some(sig(
            vec![],
            Type::option(Type::Map(Box::new(Type::String), Box::new(Type::Json))),
        )),
        "null?" => Some(sig(vec![], Type::Bool)),
        _ => None,
    }
}

fn set_method(elem: &Type, name: &str) -> Option<MethodSig> {
    match name {
        "add" => Some(sig(vec![elem.clone()], Type::Unit)),
        "remove" => Some(sig(vec![elem.clone()], Type::Bool)),
        "has?" => Some(sig(vec![elem.clone()], Type::Bool)),
        "len" => Some(sig(vec![], Type::Int)),
        "union" | "intersect" | "diff" => Some(sig(
            vec![Type::Set(Box::new(elem.clone()))],
            Type::Set(Box::new(elem.clone())),
        )),
        _ => None,
    }
}

/// One parameter of a stdlib module member.
pub enum ModuleParam {
    /// An ordinary parameter of a fixed type.
    Ty(Type),
    /// A `std::proc` command: `Vector<string>` (the primary argv form)
    /// or `string` (whitespace-split sugar) —
    /// `docs/spec/05-stdlib.md`.
    Command,
}

/// One stdlib module member signature: required parameters, optional
/// trailing parameters, and the fixed result type.
pub struct ModuleSig {
    pub required: Vec<ModuleParam>,
    pub optional: Vec<ModuleParam>,
    pub ret: Type,
}

/// Looks up `module.name` for the std modules whose signatures have
/// closed (`docs/spec/05-stdlib.md` — BRS-32: `proc` and `env`;
/// BRS-33: `fs` plus `env.cwd`/`env.cd`; BRS-34: `json` and `io`;
/// BRS-35: `math`, `time`, and `rand`). Polymorphic members
/// ([`module_member_special`]) and constants ([`module_constant`])
/// resolve outside this fixed-type table.
pub fn module_member(module: &str, name: &str) -> Option<ModuleSig> {
    let msig = |required: Vec<ModuleParam>, optional: Vec<ModuleParam>, ret: Type| ModuleSig {
        required,
        optional,
        ret,
    };

    match (module, name) {
        // Every runner takes an optional trailing stdin string and
        // returns `Output`.
        ("proc", "run" | "tryRun") => Some(msig(
            vec![ModuleParam::Command],
            vec![ModuleParam::Ty(Type::String)],
            Type::ProcOutput,
        )),
        // The commands are argv arrays only: the whitespace-split
        // string sugar exists for a literal command an author typed,
        // and a batch is built from data.
        ("proc", "tryRunAll") => Some(msig(
            vec![ModuleParam::Ty(Type::vector(Type::vector(Type::String)))],
            vec![ModuleParam::Ty(Type::Int)],
            Type::vector(Type::ProcOutput),
        )),
        ("cli", "parse") => Some(msig(
            vec![
                ModuleParam::Ty(Type::vector(Type::String)),
                ModuleParam::Ty(Type::vector(Type::vector(Type::String))),
            ],
            vec![],
            Type::CliArgs,
        )),
        ("cli", "help") => Some(msig(
            vec![
                ModuleParam::Ty(Type::String),
                ModuleParam::Ty(Type::vector(Type::vector(Type::String))),
            ],
            vec![],
            Type::String,
        )),
        ("http", "get") => Some(msig(
            vec![ModuleParam::Ty(Type::String)],
            vec![ModuleParam::Ty(Type::Int)],
            Type::HttpResponse,
        )),
        ("http", "post") => Some(msig(
            vec![ModuleParam::Ty(Type::String), ModuleParam::Ty(Type::String)],
            vec![ModuleParam::Ty(Type::Int)],
            Type::HttpResponse,
        )),
        ("proc", "shell") => Some(msig(
            vec![ModuleParam::Ty(Type::String)],
            vec![ModuleParam::Ty(Type::String)],
            Type::ProcOutput,
        )),
        ("env", "get") => Some(msig(
            vec![ModuleParam::Ty(Type::String)],
            vec![],
            Type::option(Type::String),
        )),
        ("env", "set") => Some(msig(
            vec![ModuleParam::Ty(Type::String), ModuleParam::Ty(Type::String)],
            vec![],
            Type::Unit,
        )),
        ("env", "vars") => Some(msig(
            vec![],
            vec![],
            Type::Map(Box::new(Type::String), Box::new(Type::String)),
        )),
        ("env", "args") => Some(msig(vec![], vec![], Type::vector(Type::String))),
        ("env", "cwd") => Some(msig(vec![], vec![], Type::String)),
        ("env", "exit") => Some(msig(vec![ModuleParam::Ty(Type::Int)], vec![], Type::Unit)),
        ("env", "cd") => Some(msig(
            vec![ModuleParam::Ty(Type::String)],
            vec![],
            Type::Unit,
        )),
        // `std::fs` (BRS-33): every member takes string paths. The
        // path helpers (`join`, `base`, `dir`, `ext`, `abs`) are `fs`
        // members per the spec's "`path` helpers" bullet.
        ("fs", "read" | "base" | "dir" | "ext" | "abs" | "resolve") => Some(msig(
            vec![ModuleParam::Ty(Type::String)],
            vec![],
            Type::String,
        )),
        ("fs", "join" | "write" | "append" | "cp" | "mv") => {
            let ret = if name == "join" {
                Type::String
            } else {
                Type::Unit
            };
            Some(msig(
                vec![ModuleParam::Ty(Type::String), ModuleParam::Ty(Type::String)],
                vec![],
                ret,
            ))
        }
        ("fs", "exists?" | "isFile?" | "isDir?" | "isSymlink?") => Some(msig(
            vec![ModuleParam::Ty(Type::String)],
            vec![],
            Type::Bool,
        )),
        ("fs", "ls" | "glob") => Some(msig(
            vec![ModuleParam::Ty(Type::String)],
            vec![],
            Type::vector(Type::String),
        )),
        // The optional trailing list is directory names to prune,
        // following `proc.run`'s optional-stdin shape.
        ("fs", "walk") => Some(msig(
            vec![ModuleParam::Ty(Type::String)],
            vec![ModuleParam::Ty(Type::vector(Type::String))],
            Type::vector(Type::String),
        )),
        // The tolerant form (BRS-66): same arguments, but a directory
        // below the root that cannot be read is reported rather than
        // thrown, so the result carries both halves.
        ("fs", "tryWalk") => Some(msig(
            vec![ModuleParam::Ty(Type::String)],
            vec![ModuleParam::Ty(Type::vector(Type::String))],
            Type::Walk,
        )),
        ("fs", "mkdir" | "mkdirAll" | "rm" | "rmAll") => Some(msig(
            vec![ModuleParam::Ty(Type::String)],
            vec![],
            Type::Unit,
        )),
        // `std::json` (BRS-34): `parse` yields the compiler-known
        // `Json` tree (throwing `json.ParseError`); `stringify` takes a
        // `Json` value — serializing arbitrary language values is a v2
        // design.
        ("json", "parse") => Some(msig(
            vec![ModuleParam::Ty(Type::String)],
            vec![],
            Type::Json,
        )),
        ("json", "stringify") => Some(msig(
            vec![ModuleParam::Ty(Type::Json)],
            vec![],
            Type::String,
        )),
        // `std::io` (BRS-34): the printers take any single value via
        // the universal `toString`, like the prelude `puts`/`print`
        // (`Unknown` unifies with everything).
        ("io", "puts" | "print" | "eprint") => Some(msig(
            vec![ModuleParam::Ty(Type::Unknown)],
            vec![],
            Type::Unit,
        )),
        ("io", "readLine") => Some(msig(vec![], vec![], Type::option(Type::String))),
        ("io", "readAll") => Some(msig(vec![], vec![], Type::String)),
        // `std::math` (BRS-35): the float members. `abs`/`min`/`max`
        // are polymorphic over `int`/`float` and are special-cased in
        // the checker; the constants live in [`module_constant`].
        ("math", "sqrt" | "floor" | "ceil" | "round") => Some(msig(
            vec![ModuleParam::Ty(Type::Float)],
            vec![],
            Type::Float,
        )),
        ("math", "pow") => Some(msig(
            vec![ModuleParam::Ty(Type::Float), ModuleParam::Ty(Type::Float)],
            vec![],
            Type::Float,
        )),
        // `std::time` (BRS-35): epoch timestamps plus sleep and basic
        // ISO-8601 formatting.
        ("time", "now") => Some(msig(vec![], vec![], Type::Float)),
        ("time", "nowMillis") => Some(msig(vec![], vec![], Type::Int)),
        ("time", "sleep") => Some(msig(vec![ModuleParam::Ty(Type::Int)], vec![], Type::Unit)),
        ("time", "iso") => Some(msig(vec![ModuleParam::Ty(Type::Int)], vec![], Type::String)),
        // `std::rand` (BRS-35): the deterministic PRNG surface.
        // `choice`/`shuffle` are generic over the vector element and
        // are special-cased in the checker.
        ("rand", "seed") => Some(msig(vec![ModuleParam::Ty(Type::Int)], vec![], Type::Unit)),
        ("rand", "int") => Some(msig(vec![ModuleParam::Ty(Type::Range)], vec![], Type::Int)),
        ("rand", "float") => Some(msig(vec![], vec![], Type::Float)),
        _ => None,
    }
}

/// Looks up a plain-value module member (`math.pi`): the constants of
/// the closed std modules (`docs/spec/05-stdlib.md`, BRS-35).
pub fn module_constant(module: &str, name: &str) -> Option<Type> {
    match (module, name) {
        ("math", "pi" | "e") => Some(Type::Float),
        _ => None,
    }
}

/// The module members whose signatures the checker resolves outside
/// [`module_member`]'s fixed-type table: `math.abs`/`min`/`max` are
/// polymorphic over `int`/`float`, and `rand.choice`/`shuffle` are
/// generic over the vector element (BRS-35).
pub fn module_member_special(module: &str, name: &str) -> bool {
    matches!(
        (module, name),
        ("math", "abs" | "min" | "max") | ("rand", "choice" | "shuffle")
    )
}

/// Whether `module` is a std module whose member signatures have
/// closed: an unknown member on a closed module is an error, while
/// open modules stay `Unknown`-typed until they close during M4.
pub fn module_closed(module: &str) -> bool {
    matches!(
        module,
        "proc" | "env" | "fs" | "json" | "io" | "math" | "time" | "rand" | "http" | "cli"
    )
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
    fn string_methods_from_the_m4_surface() {
        let bytes = method(&Type::String, "bytes").expect("bytes exists");
        assert!(bytes.params.is_empty());
        assert!(matches!(&bytes.ret, RetRule::Fixed(t) if *t == Type::vector(Type::Int)));

        let reverse = method(&Type::String, "reverse").expect("reverse exists");
        assert!(reverse.params.is_empty());
        assert!(matches!(&reverse.ret, RetRule::Fixed(t) if *t == Type::String));

        for name in ["trimStart", "trimEnd"] {
            let m = method(&Type::String, name).expect("trim variants exist");
            assert!(m.params.is_empty());
            assert!(matches!(&m.ret, RetRule::Fixed(t) if *t == Type::String));
        }

        for name in ["padStart", "padEnd"] {
            let m = method(&Type::String, name).expect("pad variants exist");
            assert_eq!(m.params, vec![Type::Int, Type::String]);
            assert!(matches!(&m.ret, RetRule::Fixed(t) if *t == Type::String));
        }
    }

    #[test]
    fn regex_methods_signatures() {
        let matches = method(&Type::String, "match?").expect("match? exists");
        assert_eq!(matches.params, vec![Type::String]);
        assert!(matches!(&matches.ret, RetRule::Fixed(t) if *t == Type::Bool));

        let captures = method(&Type::String, "captures").expect("captures exists");
        assert_eq!(captures.params, vec![Type::String]);
        assert!(
            matches!(&captures.ret, RetRule::Fixed(t) if *t == Type::option(Type::vector(Type::String)))
        );

        let replace_re = method(&Type::String, "replaceRe").expect("replaceRe exists");
        assert_eq!(replace_re.params, vec![Type::String, Type::String]);
        assert!(matches!(&replace_re.ret, RetRule::Fixed(t) if *t == Type::String));

        let scan = method(&Type::String, "scan").expect("scan exists");
        assert_eq!(scan.params, vec![Type::String]);
        assert!(matches!(&scan.ret, RetRule::Fixed(t) if *t == Type::vector(Type::String)));
    }

    #[test]
    fn parsing_methods_return_the_number_directly() {
        let to_int = method(&Type::String, "toInt").expect("toInt exists");
        assert!(to_int.params.is_empty());
        assert!(matches!(&to_int.ret, RetRule::Fixed(t) if *t == Type::Int));

        let to_float = method(&Type::String, "toFloat").expect("toFloat exists");
        assert!(to_float.params.is_empty());
        assert!(matches!(&to_float.ret, RetRule::Fixed(t) if *t == Type::Float));
    }

    #[test]
    fn map_result_depends_on_the_function_argument() {
        let sig = method(&Type::vector(Type::Int), "map").expect("map exists");
        assert!(matches!(sig.ret, RetRule::VectorOfFnRet));
        assert_eq!(sig.params, vec![Type::func(vec![Type::Int], Type::Unknown)]);
    }

    #[test]
    fn sort_requires_orderable_elements() {
        for elem in [
            Type::Int,
            Type::Float,
            Type::String,
            Type::Char,
            Type::Unknown,
        ] {
            assert!(method(&Type::vector(elem), "sort").is_some());
        }
        assert!(method(&Type::vector(Type::Bool), "sort").is_none());
        assert!(method(&Type::vector(Type::vector(Type::Int)), "sort").is_none());
    }

    #[test]
    fn flatten_requires_nested_vectors() {
        let sig = method(&Type::vector(Type::vector(Type::Int)), "flatten").expect("flatten");
        assert!(matches!(&sig.ret, RetRule::Fixed(t) if *t == Type::vector(Type::Int)));

        assert!(method(&Type::vector(Type::Unknown), "flatten").is_some());
        assert!(method(&Type::vector(Type::Int), "flatten").is_none());
    }

    #[test]
    fn predicate_hofs_take_bool_functions() {
        for name in ["find", "any?", "all?"] {
            let sig = method(&Type::vector(Type::Int), name).expect("predicate HOF exists");
            assert_eq!(sig.params, vec![Type::func(vec![Type::Int], Type::Bool)]);
        }
    }

    #[test]
    fn map_and_set_surfaces_from_brs35() {
        let map = Type::Map(Box::new(Type::String), Box::new(Type::Int));
        let entries = method(&map, "entries").expect("entries exists");
        assert!(matches!(
            &entries.ret,
            RetRule::Fixed(t) if *t == Type::vector(Type::Tuple(vec![Type::String, Type::Int]))
        ));

        let each = method(&map, "each").expect("each exists");
        assert_eq!(
            each.params,
            vec![Type::func(vec![Type::String, Type::Int], Type::Unit)]
        );

        let set = Type::Set(Box::new(Type::Int));
        for name in ["union", "intersect", "diff"] {
            let sig = method(&set, name).expect("set algebra exists");
            assert_eq!(sig.params, vec![set.clone()]);
            assert!(matches!(&sig.ret, RetRule::Fixed(t) if *t == set));
        }
    }

    #[test]
    fn closed_modules_and_constants() {
        use super::{module_closed, module_constant, module_member, module_member_special};

        for module in ["math", "time", "rand"] {
            assert!(module_closed(module));
        }

        assert_eq!(module_constant("math", "pi"), Some(Type::Float));
        assert_eq!(module_constant("math", "e"), Some(Type::Float));
        assert_eq!(module_constant("math", "tau"), None);

        assert!(module_member_special("math", "abs"));
        assert!(module_member_special("rand", "shuffle"));
        assert!(!module_member_special("math", "sqrt"));

        assert!(module_member("time", "iso").is_some());
        assert!(module_member("rand", "int").is_some());
        assert!(module_member("math", "sqrt").is_some());
    }
}
