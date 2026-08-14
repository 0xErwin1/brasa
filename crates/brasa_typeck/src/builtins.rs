//! The builtin method table: what `.method(...)` means on primitive and
//! container types.
//!
//! Carries the `docs/spec/05-stdlib.md` surface as it closes module by
//! module during M4 (BRS-31 strings, BRS-35 collections).
//! `string.toInt`/`toFloat` return the parsed number directly and
//! throw `string.ParseError` on failure (BRS-41); the error
//! contribution is the error-set pass's concern, not this table's.
//!
//! No stdlib MODULE lives here any more: every free module and every
//! record is declared once in `brasa_stdlib` and lowered below
//! (BRS-96), error contributions included. What remains is the builtin
//! methods of the language's own types — string, int, float, the
//! containers, `Json` — plus the signatures the tables deliberately
//! delegate here because they are not expressible as data.

use brasa_stdlib::{ModuleKind, ParamDesc, RecordKind, RetDesc, TyDesc, VectorMember};

use crate::types::Type;

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
        // Total, unlike a `slice` the caller would have to guard: an
        // absent prefix yields the string unchanged (BRS-53).
        "removePrefix" => Some(sig(vec![Type::String], Type::String)),
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

/// Lowers a declared type against the receiver it was declared for:
/// [`TyDesc::Elem`] becomes the receiver's element type, everything else
/// is fixed.
///
/// A free module's table has no receiver and passes `None`; a row there
/// mentioning `elem` is a declaration bug that
/// `brasa_stdlib::fs::tests::no_row_mentions_the_receiver_element_type`
/// rejects before it can reach a user's call.
fn lower(desc: &TyDesc, elem: Option<&Type>) -> Type {
    match desc {
        TyDesc::Int => Type::Int,
        TyDesc::Float => Type::Float,
        TyDesc::String => Type::String,
        TyDesc::Bool => Type::Bool,
        TyDesc::Unit => Type::Unit,
        TyDesc::Range => Type::Range,
        TyDesc::Unknown => Type::Unknown,
        TyDesc::Walk => Type::Walk,
        TyDesc::Json => Type::Json,
        TyDesc::ProcOutput => Type::ProcOutput,
        TyDesc::HttpResponse => Type::HttpResponse,
        TyDesc::CliArgs => Type::CliArgs,
        TyDesc::Elem => elem
            .expect("a receiver-less declaration cannot mention the receiver's element type")
            .clone(),
        TyDesc::Vector(inner) => Type::vector(lower(inner, elem)),
        TyDesc::Option(inner) => Type::option(lower(inner, elem)),
        TyDesc::Map(key, value) => {
            Type::Map(Box::new(lower(key, elem)), Box::new(lower(value, elem)))
        }
        TyDesc::Tuple(items) => Type::Tuple(items.iter().map(|item| lower(item, elem)).collect()),
        TyDesc::Fn(params, ret) => Type::func(
            params.iter().map(|param| lower(param, elem)).collect(),
            lower(ret, elem),
        ),
    }
}

/// What a record member is, once its declaration is lowered: a field
/// the caller reads or a method the caller calls.
///
/// The two are separate because the surface separates them —
/// `output.stdout` is a read and `response.header("x")` is a call — and
/// the checker reports writing either the other way.
pub enum RecordMemberSig {
    Field(Type),
    Method(MethodSig),
}

/// The declaration table for a record type, or `None` when the receiver
/// is not one of the stdlib records.
///
/// This is the one place a `Type` is matched against a record, because
/// `brasa_stdlib` does not know what a `Type` is — it names its records
/// and the checker owns which of its own types each name denotes.
fn record_table(recv: &Type) -> Option<&'static [brasa_stdlib::RecordDecl]> {
    match recv {
        Type::ProcOutput => Some(brasa_stdlib::OUTPUT_MEMBERS),
        Type::HttpResponse => Some(brasa_stdlib::RESPONSE_MEMBERS),
        Type::CliArgs => Some(brasa_stdlib::ARGS_MEMBERS),
        Type::Walk => Some(brasa_stdlib::WALK_MEMBERS),
        _ => None,
    }
}

/// Looks up `name` on one of the stdlib records, derived from its
/// declaration (`brasa_stdlib`, BRS-96).
///
/// `None` covers both "not a record" and "not a member of this one";
/// the caller layers the universal `toString` and the unknown-member
/// error on top either way, exactly as it did when the four records
/// were four hand-written blocks.
pub fn record_member(recv: &Type, name: &str) -> Option<RecordMemberSig> {
    let table = record_table(recv)?;
    let decl = table.iter().find(|decl| decl.name == name)?;

    Some(match decl.kind {
        RecordKind::Field => RecordMemberSig::Field(lower(&decl.ret, None)),
        RecordKind::Method(params) => RecordMemberSig::Method(MethodSig {
            params: params.iter().map(|param| lower(param, None)).collect(),
            ret: RetRule::Fixed(lower(&decl.ret, None)),
        }),
    })
}

/// The `Vector<T>` methods, derived from their declarations
/// (`brasa_stdlib::vector`, BRS-96).
fn vector_method(elem: &Type, name: &str) -> Option<MethodSig> {
    let member = VectorMember::from_name(name)?;
    let decl = member.decl();

    let params = || {
        decl.params
            .iter()
            .map(|param| lower(param, Some(elem)))
            .collect()
    };

    match decl.ret {
        RetDesc::Ty(ret) => Some(MethodSig {
            params: params(),
            ret: RetRule::Fixed(lower(&ret, Some(elem))),
        }),
        RetDesc::VectorOfFnRet => Some(MethodSig {
            params: params(),
            ret: RetRule::VectorOfFnRet,
        }),
        RetDesc::Custom => vector_custom_method(elem, member),
    }
}

/// The two `Vector` members whose declaration delegates the signature
/// here, because neither their existence nor their result is data:
/// `sort` exists only for orderable elements, and `flatten` exists only
/// for nested vectors and yields the receiver's inner element type
/// (BRS-35).
fn vector_custom_method(elem: &Type, member: VectorMember) -> Option<MethodSig> {
    match member {
        VectorMember::Sort if orderable(elem) => Some(sig(vec![], Type::vector(elem.clone()))),
        VectorMember::Sort => None,
        VectorMember::Flatten => match elem {
            Type::Vector(inner) => Some(sig(vec![], Type::vector((**inner).clone()))),
            flexible if flexible.is_flexible() => Some(sig(vec![], Type::vector(Type::Unknown))),
            _ => None,
        },
        _ => unreachable!("`{member:?}` does not delegate its signature to the checker"),
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

/// The stdlib-native errors a std module member raises, by canonical
/// qualified name (`docs/spec/05-stdlib.md`).
///
/// This lives beside the signature table on purpose. Before BRS-96 the
/// error-set pass carried its own copy of this knowledge, and adding a
/// throwing member here without remembering to add it there made the
/// contract silently unverifiable: a caller could declare
/// `throws never` over a body that throws and the checker would agree.
/// That is not hypothetical — `cli.parse` shipped that way for exactly
/// one commit.
///
/// Covers delegated members and constants too, which is why it is a
/// function rather than a field on [`ModuleSig`] — those two have no
/// `ModuleSig` to carry it.
///
/// Every free module answers from its declaration table's `throws`
/// column now (`brasa_stdlib`, BRS-96), so a name this does not know
/// is a name no module declares.
pub fn module_throws(module: &str, name: &str) -> &'static [&'static str] {
    match brasa_stdlib::free_member(module, name) {
        Some(decl) => decl.throws,
        None => &[],
    }
}

/// Looks up the signature of `module.name`, for every std module —
/// all of them are declared in `brasa_stdlib` now (BRS-96).
///
/// The two members this does NOT answer for are the two the
/// declaration says it should not: a constant is read rather than
/// called ([`module_constant`]) and a delegated member's signature is
/// the checker's own ([`module_member_special`]).
pub fn module_member(module: &str, name: &str) -> Option<ModuleSig> {
    free_member(module, name)
}

/// The members of a free module, derived from their declarations
/// (`brasa_stdlib`, BRS-96).
///
/// Answers for [`ModuleKind::Call`] rows only. A constant is not called
/// and a delegated member has no signature to derive, so both are
/// `None` here and reached through [`module_constant`] and
/// [`module_member_special`] instead — the same three-way split the
/// declaration draws.
fn free_member(module: &str, name: &str) -> Option<ModuleSig> {
    let decl = brasa_stdlib::free_member(module, name)?;

    let ModuleKind::Call {
        required,
        optional,
        ret,
    } = decl.kind
    else {
        return None;
    };

    let params = |descs: &'static [ParamDesc]| {
        descs
            .iter()
            .map(|desc| match desc {
                ParamDesc::Ty(ty) => ModuleParam::Ty(lower(ty, None)),
                ParamDesc::Command => ModuleParam::Command,
            })
            .collect()
    };

    Some(ModuleSig {
        required: params(required),
        optional: params(optional),
        ret: lower(&ret, None),
    })
}

/// Looks up a plain-value module member (`math.pi`): the members
/// declared [`ModuleKind::Constant`], read without a call
/// (`docs/spec/05-stdlib.md`, BRS-35).
pub fn module_constant(module: &str, name: &str) -> Option<Type> {
    match brasa_stdlib::free_member(module, name)?.kind {
        ModuleKind::Constant(ty) => Some(lower(&ty, None)),
        _ => None,
    }
}

/// Whether this member's signature is the checker's own rather than the
/// table's ([`ModuleKind::Custom`]): `math.abs`/`min`/`max` are
/// polymorphic over `int`/`float`, and `rand.choice`/`shuffle` are
/// generic over the vector element (BRS-35).
///
/// The table still says these members exist; only the signature is
/// delegated, so this cannot drift out of agreement with the surface
/// the way a second hand-written list could.
pub fn module_member_special(module: &str, name: &str) -> bool {
    matches!(
        brasa_stdlib::free_member(module, name).map(|decl| decl.kind),
        Some(ModuleKind::Custom(_))
    )
}

/// Whether `module` is a std module whose member signatures have
/// closed: an unknown member on a closed module is an error, while
/// open modules stay `Unknown`-typed until they close.
///
/// Having a declaration table IS being closed, which is why this asks
/// `brasa_stdlib` rather than carrying a list. The two were the same
/// ten names by the end of BRS-96, and a hand-kept list would have been
/// free to disagree with the tables the moment a module was added.
pub fn module_closed(module: &str) -> bool {
    brasa_stdlib::is_free_module(module)
}

#[cfg(test)]
mod tests {
    use super::{RetRule, method};
    use crate::types::Type;

    #[test]
    fn join_accepts_any_element_type() {
        for elem in [
            Type::String,
            Type::Int,
            Type::Bool,
            Type::Unknown,
            Type::vector(Type::Int),
        ] {
            let sig = method(&Type::vector(elem), "join").expect("join exists");
            assert_eq!(sig.params, vec![Type::String]);
            assert!(matches!(&sig.ret, RetRule::Fixed(t) if *t == Type::String));
        }
    }

    #[test]
    fn vector_slice_matches_the_string_signature() {
        let string_slice = method(&Type::String, "slice").expect("string slice exists");

        let vector_slice = method(&Type::vector(Type::Int), "slice").expect("vector slice exists");
        assert_eq!(vector_slice.params, string_slice.params);
        assert!(
            matches!(&vector_slice.ret, RetRule::Fixed(t) if *t == Type::vector(Type::Int)),
            "slice preserves the element type"
        );
    }

    #[test]
    fn remove_prefix_takes_and_returns_a_string() {
        let sig = method(&Type::String, "removePrefix").expect("removePrefix exists");
        assert_eq!(sig.params, vec![Type::String]);
        assert!(matches!(&sig.ret, RetRule::Fixed(t) if *t == Type::String));
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

    /// Every row the declaration marks `custom` must be handled here.
    /// A new custom row nobody taught the checker about would otherwise
    /// panic on a user's first call instead of failing in this test.
    #[test]
    fn every_custom_vector_row_has_a_checker_rule() {
        let flexible = Type::vector(Type::Unknown);

        for decl in brasa_stdlib::VECTOR_METHODS {
            if decl.ret != brasa_stdlib::RetDesc::Custom {
                continue;
            }

            assert!(
                method(&flexible, decl.name).is_some(),
                "`{}` is declared `custom` but the checker has no rule for it",
                decl.name
            );
        }
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

#[cfg(test)]
mod stdlib_declaration_tests {
    use super::*;

    /// BRS-96: every error a std module member declares must be one the
    /// resolver knows, or a `catch` arm naming it would be rejected as
    /// unknown while the error-set says the member throws it — the two
    /// halves of the language disagreeing about the same name.
    #[test]
    fn every_declared_throw_is_a_known_native_error() {
        for module in brasa_resolver::STD_MODULES {
            for name in MEMBERS_THAT_THROW {
                for thrown in module_throws(module, name) {
                    assert!(
                        brasa_resolver::NATIVE_ERRORS.contains(thrown),
                        "`{module}.{name}` declares `{thrown}`, which is not in NATIVE_ERRORS"
                    );
                }
            }
        }
    }

    /// Every member `module_throws` answers non-empty for. Written out
    /// rather than derived, so adding a throwing member without adding
    /// it here leaves the test above unable to see it — and the
    /// coverage check below is what catches that.
    const MEMBERS_THAT_THROW: &[&str] = &[
        "run",
        "shell",
        "tryRun",
        "tryRunAll",
        "get",
        "post",
        "parse",
        "read",
        "write",
        "append",
        "ls",
        "glob",
        "walk",
        "tryWalk",
        "mkdir",
        "mkdirAll",
        "rm",
        "rmAll",
        "cp",
        "mv",
        "resolve",
        "cd",
        "abs",
        "cwd",
    ];

    /// The list above must name every throwing member, so the check
    /// cannot silently stop covering one.
    #[test]
    fn the_throwing_member_list_is_complete() {
        // Every member name the fixed-signature table knows, per module.
        const CANDIDATES: &[&str] = &[
            "run",
            "tryRun",
            "tryRunAll",
            "shell",
            "get",
            "set",
            "vars",
            "args",
            "cwd",
            "exit",
            "cd",
            "read",
            "write",
            "append",
            "exists?",
            "isFile?",
            "isDir?",
            "isSymlink?",
            "ls",
            "glob",
            "walk",
            "tryWalk",
            "mkdir",
            "mkdirAll",
            "rm",
            "rmAll",
            "cp",
            "mv",
            "abs",
            "resolve",
            "join",
            "dir",
            "base",
            "ext",
            "parse",
            "stringify",
            "puts",
            "print",
            "eprint",
            "readLine",
            "readAll",
            "sqrt",
            "floor",
            "ceil",
            "round",
            "pow",
            "min",
            "max",
            "now",
            "nowMillis",
            "iso",
            "sleep",
            "seed",
            "int",
            "float",
            "choice",
            "shuffle",
            "post",
            "help",
        ];

        for module in brasa_resolver::STD_MODULES {
            for name in CANDIDATES {
                if module_throws(module, name).is_empty() {
                    continue;
                }

                assert!(
                    MEMBERS_THAT_THROW.contains(name),
                    "`{module}.{name}` throws but is missing from MEMBERS_THAT_THROW"
                );
            }
        }
    }

    /// The tolerant/strict pairing the spec draws: `tryRun` keeps the
    /// spawn failure and drops the non-zero exit, because a non-zero
    /// exit is its result.
    #[test]
    fn the_tolerant_runners_drop_only_the_non_zero_exit() {
        let strict = module_throws("proc", "run");
        let tolerant = module_throws("proc", "tryRun");

        assert!(strict.contains(&brasa_resolver::PROC_NON_ZERO_EXIT));
        assert!(!tolerant.contains(&brasa_resolver::PROC_NON_ZERO_EXIT));
        assert_eq!(
            module_throws("proc", "tryRunAll"),
            tolerant,
            "the parallel form is the tolerant one"
        );
    }

    #[test]
    fn a_member_that_cannot_fail_declares_nothing() {
        assert!(module_throws("json", "stringify").is_empty());
        assert!(module_throws("fs", "exists?").is_empty());
        assert!(module_throws("fs", "join").is_empty());
        assert!(module_throws("cli", "help").is_empty());
        assert!(module_throws("math", "sqrt").is_empty());
    }

    /// The whole `fs` error contribution, member by member. Written out
    /// rather than derived from the table it checks: a test that asks
    /// the table what the table says would pass for any answer, and
    /// what `throws` decides is whether E004/E005 accept a caller's
    /// declaration.
    #[test]
    fn every_fs_member_contributes_exactly_this() {
        use brasa_resolver::{FS_DENIED, FS_IO_ERROR, FS_NOT_FOUND};

        const ALL: &[&str] = &[FS_NOT_FOUND, FS_DENIED, FS_IO_ERROR];
        const NONE: &[&str] = &[];

        let expected: &[(&str, &[&str])] = &[
            ("read", ALL),
            ("write", ALL),
            ("append", ALL),
            ("exists?", NONE),
            ("isFile?", NONE),
            ("isDir?", NONE),
            ("isSymlink?", NONE),
            ("ls", ALL),
            ("glob", ALL),
            ("walk", ALL),
            ("tryWalk", ALL),
            ("mkdir", ALL),
            ("mkdirAll", ALL),
            ("rm", ALL),
            ("rmAll", ALL),
            ("cp", ALL),
            ("mv", ALL),
            ("join", NONE),
            ("base", NONE),
            ("dir", NONE),
            ("ext", NONE),
            ("abs", &[FS_IO_ERROR]),
            ("resolve", ALL),
        ];

        for (name, throws) in expected {
            assert_eq!(
                module_throws("fs", name),
                *throws,
                "`fs.{name}` contributes something other than its pinned error list"
            );
        }

        // And the list above covers the surface, so a new member cannot
        // arrive with an unexamined contribution.
        for decl in brasa_stdlib::FS_MEMBERS {
            assert!(
                expected.iter().any(|(name, _)| *name == decl.name),
                "`fs.{}` is declared but its error list is not pinned here",
                decl.name
            );
        }
    }

    /// The table spells its error names out, since the declaration
    /// crate is a leaf and cannot see the resolver's constants. The two
    /// spellings must be the same string, or a `catch` arm naming the
    /// error would not match what the member contributes.
    #[test]
    fn the_declared_error_names_are_the_resolvers() {
        assert_eq!(brasa_stdlib::fs::NOT_FOUND, brasa_resolver::FS_NOT_FOUND);
        assert_eq!(brasa_stdlib::fs::DENIED, brasa_resolver::FS_DENIED);
        assert_eq!(brasa_stdlib::fs::IO_ERROR, brasa_resolver::FS_IO_ERROR);
        assert_eq!(
            brasa_stdlib::json::PARSE_ERROR,
            brasa_resolver::JSON_PARSE_ERROR
        );
        assert_eq!(
            brasa_stdlib::proc::NON_ZERO_EXIT,
            brasa_resolver::PROC_NON_ZERO_EXIT
        );
        assert_eq!(
            brasa_stdlib::proc::SPAWN_ERROR,
            brasa_resolver::PROC_SPAWN_ERROR
        );
    }

    /// And the general form of the same rule, which is what keeps a
    /// module converted later from spelling an error the resolver never
    /// heard of: an unlisted name is one no `catch` arm can match and
    /// no `throws` clause can declare, so the member would be
    /// uncatchable rather than merely misnamed.
    #[test]
    fn every_declared_error_is_a_known_native_error() {
        for (module, members) in brasa_stdlib::FREE_MODULES {
            for decl in *members {
                for error in decl.throws {
                    assert!(
                        brasa_resolver::NATIVE_ERRORS.contains(error),
                        "`{module}.{}` raises `{error}`, which is not a native error",
                        decl.name
                    );
                }
            }
        }
    }

    /// `env` is the first table whose `throws` column names another
    /// module's errors, so the borrow is pinned end to end rather than
    /// only at the declaration: what the error-set pass reads for
    /// `env.cd` must still be the `fs` list itself, not a copy that can
    /// drift from it.
    #[test]
    fn env_borrows_the_fs_error_lists_through_its_table() {
        assert_eq!(module_throws("env", "cd"), brasa_stdlib::fs::ALL_ERRORS);
        assert_eq!(module_throws("env", "cwd"), brasa_stdlib::fs::CWD_ERRORS);

        for name in ["get", "set", "vars", "args", "exit"] {
            assert!(
                module_throws("env", name).is_empty(),
                "`env.{name}` contributes an error, but only the filesystem members throw"
            );
        }
    }

    /// Every record member the tables declare resolves on its own
    /// record's type, as the right kind. Written against `RECORDS` so a
    /// record converted later is covered, and asserting the kind
    /// because that is what the four hand-written blocks encoded in
    /// their choice of `Member::Value` versus `Member::Sig`.
    #[test]
    fn every_declared_record_member_resolves_on_its_own_type() {
        use brasa_stdlib::RecordKind;

        let type_of = |record: &str| match record {
            "Output" => Type::ProcOutput,
            "Response" => Type::HttpResponse,
            "Args" => Type::CliArgs,
            "Walk" => Type::Walk,
            other => panic!("`{other}` has no checker type; add it to `record_table`"),
        };

        for (record, members) in brasa_stdlib::RECORDS {
            let recv = type_of(record);

            for decl in *members {
                let found = record_member(&recv, decl.name)
                    .unwrap_or_else(|| panic!("`{record}.{}` resolves", decl.name));

                match (decl.kind, found) {
                    (RecordKind::Field, RecordMemberSig::Field(_)) => {}
                    (RecordKind::Method(_), RecordMemberSig::Method(_)) => {}
                    _ => panic!("`{record}.{}` resolved as the wrong kind", decl.name),
                }
            }

            assert!(
                record_member(&recv, "definitelyNotAMember").is_none(),
                "`{record}` answered for a name it does not declare"
            );
        }
    }

    /// A record's members belong to that record. `Output.stdout` on a
    /// `Walk` was impossible when each record had its own block and
    /// must stay impossible now that one function serves all four.
    #[test]
    fn a_record_does_not_answer_for_another_records_member() {
        assert!(record_member(&Type::Walk, "stdout").is_none());
        assert!(record_member(&Type::ProcOutput, "paths").is_none());
        assert!(record_member(&Type::CliArgs, "status").is_none());
        assert!(record_member(&Type::HttpResponse, "flag").is_none());
    }

    /// A type that is not a record answers for nothing, which is what
    /// leaves the ordinary method lookup and the unknown-member error
    /// downstream of this one.
    #[test]
    fn a_non_record_receiver_is_not_a_record() {
        for recv in [Type::Int, Type::String, Type::vector(Type::Int), Type::Json] {
            assert!(record_member(&recv, "stdout").is_none());
            assert!(record_member(&recv, "toString").is_none());
        }
    }

    /// The three row forms stay three answers, end to end. This is the
    /// property the split exists for: a constant must NOT come back
    /// from the signature lookup, or `math.pi` would typecheck as a
    /// call; a delegated member must not either, or the checker's own
    /// rule would be shadowed by a signature the table cannot state.
    #[test]
    fn each_row_form_is_answered_by_exactly_one_lookup() {
        for (module, members) in brasa_stdlib::FREE_MODULES {
            for decl in *members {
                let name = decl.name;
                let called = module_member(module, name).is_some();
                let constant = module_constant(module, name).is_some();
                let delegated = module_member_special(module, name);

                let answers = [called, constant, delegated];
                assert_eq!(
                    answers.iter().filter(|answered| **answered).count(),
                    1,
                    "`{module}.{name}` is answered by {} lookups, not exactly one",
                    answers.iter().filter(|answered| **answered).count()
                );

                match decl.kind {
                    brasa_stdlib::ModuleKind::Call { .. } => assert!(called),
                    brasa_stdlib::ModuleKind::Constant(_) => assert!(constant),
                    brasa_stdlib::ModuleKind::Custom(_) => assert!(delegated),
                }
            }
        }
    }

    /// The constants, lowered through the table rather than named in
    /// the checker. Both are floats: `math.pi` as an int would be a
    /// value no one should be able to use as one.
    #[test]
    fn the_math_constants_come_from_the_declaration() {
        assert_eq!(module_constant("math", "pi"), Some(Type::Float));
        assert_eq!(module_constant("math", "e"), Some(Type::Float));

        assert_eq!(module_constant("math", "sqrt"), None);
        assert_eq!(module_constant("time", "now"), None);
    }

    /// `module_closed` now means "has a declaration table", so every
    /// module the language ships answers yes and nothing else does.
    /// A module that is open would leave unknown members `Unknown`
    /// instead of reporting them.
    #[test]
    fn every_declared_module_is_closed() {
        for (module, _) in brasa_stdlib::FREE_MODULES {
            assert!(module_closed(module), "`{module}` has a table but is open");
        }

        assert!(!module_closed("re"));
        assert!(!module_closed("definitelyNotAModule"));
    }

    /// The command rule survives the table, which is the whole reason
    /// it is a parameter rule rather than a type: `run` and `tryRun`
    /// must still reach the checker's two-candidate acceptance test and
    /// its bespoke wording, not a single expected type.
    #[test]
    fn the_proc_runners_keep_the_command_rule_through_the_table() {
        for name in ["run", "tryRun"] {
            let sig = module_member("proc", name).expect("the runner exists");

            assert!(
                matches!(sig.required.as_slice(), [ModuleParam::Command]),
                "`proc.{name}` lost the command rule"
            );
            assert!(matches!(
                sig.optional.as_slice(),
                [ModuleParam::Ty(Type::String)]
            ));
            assert_eq!(sig.ret, Type::ProcOutput);
        }

        // `shell` takes a command LINE and `tryRunAll` takes argv
        // arrays; a command rule on either would widen what they accept.
        let shell = module_member("proc", "shell").expect("shell exists");
        assert!(matches!(
            shell.required.as_slice(),
            [ModuleParam::Ty(Type::String)]
        ));

        let all = module_member("proc", "tryRunAll").expect("tryRunAll exists");
        assert!(
            matches!(all.required.as_slice(), [ModuleParam::Ty(ty)] if
                *ty == Type::vector(Type::vector(Type::String))),
            "`proc.tryRunAll` takes argv arrays only"
        );
        assert_eq!(all.ret, Type::vector(Type::ProcOutput));
    }

    /// The one signature on the `env` surface that needed a type the
    /// table language did not have (`[Map<string, string>]`).
    #[test]
    fn env_vars_lowers_the_declared_map_type() {
        let vars = module_member("env", "vars").expect("vars exists");

        assert!(vars.required.is_empty());
        assert!(vars.optional.is_empty());
        assert_eq!(
            vars.ret,
            Type::Map(Box::new(Type::String), Box::new(Type::String))
        );
    }

    /// The `json` and `io` contributions, pinned the way the `fs` ones
    /// are: `parse` is the one thrower on either surface.
    #[test]
    fn json_and_io_contribute_from_their_declarations() {
        assert_eq!(
            module_throws("json", "parse"),
            &[brasa_resolver::JSON_PARSE_ERROR]
        );
        assert!(module_throws("json", "stringify").is_empty());

        for name in ["puts", "print", "eprint", "readLine", "readAll"] {
            assert!(
                module_throws("io", name).is_empty(),
                "`io.{name}` contributes an error, but the surface is infallible"
            );
        }
    }

    /// The signatures the two new tables answer, including the only
    /// member on either whose result is an `Option`.
    #[test]
    fn json_and_io_signatures_come_from_the_declaration() {
        let parse = module_member("json", "parse").expect("parse exists");
        assert!(matches!(
            parse.required.as_slice(),
            [ModuleParam::Ty(Type::String)]
        ));
        assert_eq!(parse.ret, Type::Json);

        let stringify = module_member("json", "stringify").expect("stringify exists");
        assert!(matches!(
            stringify.required.as_slice(),
            [ModuleParam::Ty(Type::Json)]
        ));
        assert_eq!(stringify.ret, Type::String);

        for name in ["puts", "print", "eprint"] {
            let sig = module_member("io", name).expect("the printers exist");
            assert!(matches!(
                sig.required.as_slice(),
                [ModuleParam::Ty(Type::Unknown)]
            ));
            assert_eq!(sig.ret, Type::Unit);
        }

        let read_line = module_member("io", "readLine").expect("readLine exists");
        assert!(read_line.required.is_empty());
        assert_eq!(read_line.ret, Type::option(Type::String));
        assert_eq!(
            module_member("io", "readAll").expect("readAll exists").ret,
            Type::String
        );

        assert!(module_member("json", "asInt").is_none());
        assert!(module_member("io", "readByte").is_none());
    }

    /// The `fs` signatures the declaration table now answers, including
    /// the only two members with an optional trailing parameter.
    #[test]
    fn fs_signatures_come_from_the_declaration() {
        let string_param = |param: &ModuleParam| matches!(param, ModuleParam::Ty(Type::String));

        let read = module_member("fs", "read").expect("read exists");
        assert!(read.required.iter().all(string_param));
        assert_eq!(read.required.len(), 1);
        assert!(read.optional.is_empty());
        assert_eq!(read.ret, Type::String);

        for name in ["walk", "tryWalk"] {
            let sig = module_member("fs", name).expect("the walkers exist");
            assert_eq!(sig.required.len(), 1);
            assert!(matches!(
                sig.optional.as_slice(),
                [ModuleParam::Ty(Type::Vector(elem))] if **elem == Type::String
            ));
        }

        assert_eq!(
            module_member("fs", "walk").expect("walk exists").ret,
            Type::vector(Type::String)
        );
        assert_eq!(
            module_member("fs", "tryWalk").expect("tryWalk exists").ret,
            Type::Walk
        );
        assert_eq!(
            module_member("fs", "mv").expect("mv exists").required.len(),
            2
        );
        assert_eq!(
            module_member("fs", "exists?").expect("exists").ret,
            Type::Bool
        );
        assert!(module_member("fs", "definitelyNotAMember").is_none());
    }
}
