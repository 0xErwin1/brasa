//! Stable diagnostic codes, one constant per error kind.
//!
//! This is the machine-facing side of the public registry in
//! spec: 06 — Diagnósticos; the spec table and these constants must
//! stay in sync. Codes follow `<PhaseLetter><3 digits>` (`L` lexer, `P`
//! parser, `M` module loading, `R` resolver, `T` type checker, `E`
//! error-sets, `C` code
//! generation; `X` is
//! reserved), are append-only, and are never renumbered or reused after
//! removal. Every
//! emission site names one of these constants instead of writing the code
//! inline; a code identifies an error kind, so one code may back several
//! wordings and spans.

// --- lexer (L) ---

/// A character that starts no token.
pub const L_UNEXPECTED_CHARACTER: &str = "L001";
/// A string literal left open at a newline or end of file.
pub const L_UNTERMINATED_STRING: &str = "L002";
/// A `#{` interpolation left open at end of file.
pub const L_UNTERMINATED_INTERPOLATION: &str = "L003";
/// A character literal that is empty, unclosed, or holds more than one
/// character.
pub const L_MALFORMED_CHAR_LITERAL: &str = "L004";

// --- parser (P) ---

/// The "expected X, found Y" family: a construct required a specific
/// token or production and the cursor sat on something else.
pub const P_EXPECTED: &str = "P001";
/// Expression/statement/type/pattern nesting exceeded the parser's
/// recursion limit.
pub const P_NESTING_TOO_DEEP: &str = "P002";
/// An escape sequence outside the shared `\n \t \" \\ \#` set, in a
/// string or char literal.
pub const P_UNKNOWN_ESCAPE: &str = "P003";
/// An `enum` declaration with no variants.
pub const P_EMPTY_ENUM: &str = "P004";
/// An `interface` declaration with no members.
pub const P_EMPTY_INTERFACE: &str = "P005";
/// A `..`/`..=` chained onto another range without parentheses; ranges
/// are non-associative.
pub const P_NON_ASSOCIATIVE_RANGE: &str = "P006";
/// An integer literal that cannot be decoded: no digits after its
/// `0x`/`0b` prefix, or out of range.
pub const P_INVALID_INT_LITERAL: &str = "P007";
/// The same field named twice in one struct literal.
pub const P_DUPLICATE_FIELD: &str = "P008";
/// A `#{...}` interpolation in a string position that forbids it.
pub const P_INTERPOLATION_NOT_ALLOWED: &str = "P009";

// --- module loading (M) ---

/// An `import "path"` naming a file that cannot be read: missing, not a
/// regular file, or denied by the OS.
pub const M_UNREADABLE_IMPORT: &str = "M001";
/// An import cycle. Top-level `let`s evaluate on import
/// (spec: 01 — Sintaxis), so a cycle has no sound evaluation order.
pub const M_IMPORT_CYCLE: &str = "M002";
/// An import chain deeper than the loader follows.
pub const M_IMPORTS_TOO_DEEP: &str = "M003";
/// A `::` import naming no file under any search-path root.
pub const M_MODULE_NOT_FOUND: &str = "M004";

// --- resolver (R) ---

/// A value name that resolves to nothing in any scope or the prelude.
pub const R_UNKNOWN_NAME: &str = "R001";
/// A top-level `let` referenced from top-level code before its
/// initializer has run.
pub const R_USE_BEFORE_DEF: &str = "R002";
/// A type name that resolves to nothing: annotations, struct literals,
/// and named constraints share this kind.
pub const R_UNKNOWN_TYPE: &str = "R003";
/// A constructor name matching no enum variant and neither `Some` nor
/// `None`.
pub const R_UNKNOWN_CONSTRUCTOR: &str = "R004";
/// A constructor name matching variants of more than one enum.
pub const R_AMBIGUOUS_CONSTRUCTOR: &str = "R005";
/// The same name bound twice in one scope: items, locals, generics,
/// fields, and enum variants share this kind.
pub const R_DUPLICATE_DEFINITION: &str = "R006";
/// `self` used outside a function taking a `self` parameter.
pub const R_SELF_OUTSIDE_METHOD: &str = "R007";
/// Retired. A `::` import whose first segment is not `std` used to be
/// rejected outright; since BRS-102 any root may name a module on the
/// search path, and a root that resolves to nothing is `M004`. Codes are
/// append-only, so the constant stays and the number is never reused.
pub const R_UNKNOWN_IMPORT_ROOT: &str = "R008";
/// A `std::` import naming no known std module.
pub const R_UNKNOWN_STD_MODULE: &str = "R009";
/// A generic constraint naming a type that is not an interface.
pub const R_NOT_AN_INTERFACE: &str = "R010";
/// A `panics.`-qualified `catch` arm naming no member of the closed
/// panic union.
pub const R_UNKNOWN_PANIC: &str = "R011";
/// A `catch` arm in a landed stdlib-error namespace (`string.`) naming
/// no member of the closed native-error list.
pub const R_UNKNOWN_NATIVE_ERROR: &str = "R012";
/// A qualified name (`mod.member`) naming nothing in the imported
/// module, or naming something the module declares without `pub`.
pub const R_UNKNOWN_MODULE_MEMBER: &str = "R013";

// --- type checker (T) ---

/// The "expected X, found Y" family: a value's type failed to unify
/// with what its context requires.
pub const T_MISMATCHED_TYPES: &str = "T001";
/// Invalid operand types for an arithmetic operator (`+ - * / % **`,
/// unary `-`).
pub const T_INVALID_OPERANDS: &str = "T002";
/// `==`/`!=` over two sides of different types.
pub const T_CANNOT_COMPARE_EQUALITY: &str = "T003";
/// `< <= > >=` over unordered types or two sides of different types.
pub const T_UNSUPPORTED_ORDERING: &str = "T004";
/// An argument count that differs from the callee's parameter count:
/// functions, builtin calls, and constructor arities (expression and
/// pattern position) share this kind.
pub const T_WRONG_ARG_COUNT: &str = "T005";
/// A call whose callee is not a function value.
pub const T_NOT_CALLABLE: &str = "T006";
/// A member access naming no field or method on the receiver's type;
/// the message names the receiver kind.
pub const T_UNKNOWN_MEMBER: &str = "T007";
// "T008" (`join` requires `Vector<string>`) was retired when
// `Vector.join` started accepting any element type; the number is
// burned and must not be reassigned.
/// Assignment to something that cannot be assigned: an immutable
/// binding, `self`, or a non-assignable name.
pub const T_CANNOT_ASSIGN: &str = "T009";
/// An assignment whose target expression is not a name, field, or
/// index.
pub const T_INVALID_ASSIGNMENT_TARGET: &str = "T010";
/// Indexing a `string`, which is not indexable.
pub const T_STRINGS_NOT_INDEXABLE: &str = "T011";
/// Indexing a type that supports no indexing at all.
pub const T_CANNOT_INDEX: &str = "T012";
/// `for` over a type that is not iterable.
pub const T_CANNOT_ITERATE: &str = "T013";
/// An empty vector or map literal with no context to infer its
/// element/key/value types from.
pub const T_EMPTY_LITERAL_NO_TYPE: &str = "T014";
/// A lambda parameter with no annotation and no expected function type
/// to take one from.
pub const T_LAMBDA_PARAM_NEEDS_ANNOTATION: &str = "T015";
/// `if` branches or `match` arms producing different types in value
/// position.
pub const T_BRANCH_TYPE_MISMATCH: &str = "T016";
/// A `match` that does not cover every case of its scrutinee.
pub const T_NON_EXHAUSTIVE_MATCH: &str = "T017";
/// A pattern whose shape does not match the scrutinee's type:
/// `Some`/`None` against a non-`Option`, an enum pattern against
/// another type, a tuple pattern against a non-tuple or a tuple of a
/// different length.
pub const T_PATTERN_TYPE_MISMATCH: &str = "T018";
/// `return` in top-level code, where there is no function to return
/// from.
pub const T_RETURN_OUTSIDE_FUNCTION: &str = "T019";
/// A struct literal field that the struct does not declare.
pub const T_STRUCT_LIT_UNKNOWN_FIELD: &str = "T020";
/// A struct literal providing the same field twice.
pub const T_STRUCT_LIT_DUPLICATE_FIELD: &str = "T021";
/// A struct literal missing a declared field.
pub const T_STRUCT_LIT_MISSING_FIELD: &str = "T022";
/// A struct literal whose type name resolves to something that is not
/// a struct.
pub const T_NOT_A_STRUCT: &str = "T023";
/// A type argument count that differs from the type's generic
/// parameter count.
pub const T_WRONG_TYPE_ARG_COUNT: &str = "T024";
/// An interface used in type position; interfaces only constrain
/// generics in v1.
pub const T_INTERFACE_AS_TYPE: &str = "T025";
/// A generic call or literal where no argument determines some type
/// parameter.
pub const T_CANNOT_INFER_TYPE_PARAM: &str = "T026";
/// A solved type argument that does not satisfy its declared
/// constraint.
pub const T_CONSTRAINT_NOT_SATISFIED: &str = "T027";
/// `?.` applied to a receiver that is not an `Option`.
pub const T_SAFE_NAV_NEEDS_OPTION: &str = "T028";
/// `??` whose left side is not an `Option`.
pub const T_COALESCE_NEEDS_OPTION: &str = "T029";
/// `??` whose fallback's type does not match the type the `Option`
/// carries.
pub const T_COALESCE_TYPE_MISMATCH: &str = "T030";
/// A `Map` key or `Set` element type outside the closed `Hashable`
/// list (`int`, `string`, `char`, `bool`, and tuples of those).
pub const T_KEY_NOT_HASHABLE: &str = "T031";
/// `break` or `continue` with no enclosing loop in the same function or
/// lambda.
pub const T_LOOP_JUMP_OUTSIDE_LOOP: &str = "T032";
/// A name that resolves to something that is not a first-class value —
/// a module handle, or a prelude function — used as one.
pub const T_NOT_A_VALUE: &str = "T033";
/// A `toString` override declaring `throws`. Rendering has to be
/// infallible: `toString` is reached from the paths that report a
/// failure, so a throw there has nowhere left to go.
pub const T_TO_STRING_CANNOT_THROW: &str = "T034";

// --- error-sets (E) ---

/// A `catch`/`catch!` arm that can never match: a named type the
/// subject's (closed) error-set does not contain, or a `_` arm in a
/// `catch!` whose named arms already handle every error.
pub const E_UNREACHABLE_ARM: &str = "E001";
/// A `catch!` whose arms (plus `_`, if any) do not cover every type
/// in the subject's closed error-set.
pub const E_CATCH_ALL_NOT_EXHAUSTIVE: &str = "E002";
/// A `catch!` whose subject's error-set is open, so exhaustiveness
/// cannot be verified.
pub const E_UNVERIFIABLE_EXHAUSTIVENESS: &str = "E003";
/// A declared `throws` list the body does not honor: the inferred
/// error-set contains a type the list does not name, or it is open
/// (and the list, being a claim about everything the body can throw,
/// is therefore unverifiable).
pub const E_UNDECLARED_THROW: &str = "E004";
/// A `throws never` contract the body violates: the inferred error-set
/// is non-empty, or open (and therefore unverifiable).
pub const E_THROWS_NEVER_VIOLATED: &str = "E005";
/// A `throws` list naming a member of the `panics.` union. A panic is
/// not an error: it never enters an error-set, so no body can ever
/// honor such a declaration.
pub const E_PANIC_IN_THROWS: &str = "E006";
/// A `toString` whose INFERRED error-set is not provably empty:
/// non-empty, or open. `T034` states the same rule over the written
/// clause; `throws` is inferred, so this is the half a declaration-site
/// check cannot see.
pub const E_TO_STRING_CAN_THROW: &str = "E007";

/// A struct accepted as satisfying an interface has a method that
/// throws more than the interface member declares (BRS-141).
///
/// Satisfaction is structural (spec: 03 — Sistema de tipos): a struct
/// never declares that it implements an interface, so there is no
/// declaration site at which to hold it to the contract. The check
/// happens where the pairing is first demanded — a call that solves a
/// constrained generic — because that is both the only moment the two
/// halves are known and the only moment the mismatch can hurt anyone.
pub const E_IFACE_THROWS_VIOLATED: &str = "E008";

// --- code generation (C) ---

/// A call with more arguments than an instruction's `argc` operand can
/// carry, receiver included.
pub const C_TOO_MANY_ARGUMENTS: &str = "C001";
/// A function, method, lambda, or enum variant declaring more
/// parameters than a frame's `arity` operand can carry.
pub const C_TOO_MANY_PARAMETERS: &str = "C002";
/// A vector, map, or tuple literal with more elements than the
/// construction instruction's count operand can carry.
pub const C_TOO_MANY_ELEMENTS: &str = "C003";
/// A struct with more fields, or an enum with more variants, than the
/// field/variant operands can index.
pub const C_TOO_MANY_MEMBERS: &str = "C004";
/// More bindings than the slot operands can address: local slots in one
/// function, module globals, or values captured by one closure.
pub const C_TOO_MANY_BINDINGS: &str = "C005";
/// A function body needing more operand-stack slots than a frame can
/// reserve.
pub const C_EXPRESSION_TOO_COMPLEX: &str = "C006";

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    /// Every constant in this module, in declaration order. A new code
    /// must be added here for the uniqueness/format checks to cover it.
    const ALL: &[&str] = &[
        super::L_UNEXPECTED_CHARACTER,
        super::L_UNTERMINATED_STRING,
        super::L_UNTERMINATED_INTERPOLATION,
        super::L_MALFORMED_CHAR_LITERAL,
        super::P_EXPECTED,
        super::P_NESTING_TOO_DEEP,
        super::P_UNKNOWN_ESCAPE,
        super::P_EMPTY_ENUM,
        super::P_EMPTY_INTERFACE,
        super::P_NON_ASSOCIATIVE_RANGE,
        super::P_INVALID_INT_LITERAL,
        super::P_DUPLICATE_FIELD,
        super::P_INTERPOLATION_NOT_ALLOWED,
        super::M_UNREADABLE_IMPORT,
        super::M_IMPORT_CYCLE,
        super::M_IMPORTS_TOO_DEEP,
        super::M_MODULE_NOT_FOUND,
        super::R_UNKNOWN_NAME,
        super::R_USE_BEFORE_DEF,
        super::R_UNKNOWN_TYPE,
        super::R_UNKNOWN_CONSTRUCTOR,
        super::R_AMBIGUOUS_CONSTRUCTOR,
        super::R_DUPLICATE_DEFINITION,
        super::R_SELF_OUTSIDE_METHOD,
        super::R_UNKNOWN_IMPORT_ROOT,
        super::R_UNKNOWN_STD_MODULE,
        super::R_NOT_AN_INTERFACE,
        super::R_UNKNOWN_PANIC,
        super::R_UNKNOWN_NATIVE_ERROR,
        super::R_UNKNOWN_MODULE_MEMBER,
        super::T_MISMATCHED_TYPES,
        super::T_INVALID_OPERANDS,
        super::T_CANNOT_COMPARE_EQUALITY,
        super::T_UNSUPPORTED_ORDERING,
        super::T_WRONG_ARG_COUNT,
        super::T_NOT_CALLABLE,
        super::T_UNKNOWN_MEMBER,
        super::T_CANNOT_ASSIGN,
        super::T_INVALID_ASSIGNMENT_TARGET,
        super::T_STRINGS_NOT_INDEXABLE,
        super::T_CANNOT_INDEX,
        super::T_CANNOT_ITERATE,
        super::T_EMPTY_LITERAL_NO_TYPE,
        super::T_LAMBDA_PARAM_NEEDS_ANNOTATION,
        super::T_BRANCH_TYPE_MISMATCH,
        super::T_NON_EXHAUSTIVE_MATCH,
        super::T_PATTERN_TYPE_MISMATCH,
        super::T_RETURN_OUTSIDE_FUNCTION,
        super::T_STRUCT_LIT_UNKNOWN_FIELD,
        super::T_STRUCT_LIT_DUPLICATE_FIELD,
        super::T_STRUCT_LIT_MISSING_FIELD,
        super::T_NOT_A_STRUCT,
        super::T_WRONG_TYPE_ARG_COUNT,
        super::T_INTERFACE_AS_TYPE,
        super::T_CANNOT_INFER_TYPE_PARAM,
        super::T_CONSTRAINT_NOT_SATISFIED,
        super::T_SAFE_NAV_NEEDS_OPTION,
        super::T_COALESCE_NEEDS_OPTION,
        super::T_COALESCE_TYPE_MISMATCH,
        super::T_KEY_NOT_HASHABLE,
        super::T_LOOP_JUMP_OUTSIDE_LOOP,
        super::T_NOT_A_VALUE,
        super::T_TO_STRING_CANNOT_THROW,
        super::E_UNREACHABLE_ARM,
        super::E_CATCH_ALL_NOT_EXHAUSTIVE,
        super::E_UNVERIFIABLE_EXHAUSTIVENESS,
        super::E_UNDECLARED_THROW,
        super::E_THROWS_NEVER_VIOLATED,
        super::E_PANIC_IN_THROWS,
        super::E_TO_STRING_CAN_THROW,
        super::E_IFACE_THROWS_VIOLATED,
        super::C_TOO_MANY_ARGUMENTS,
        super::C_TOO_MANY_PARAMETERS,
        super::C_TOO_MANY_ELEMENTS,
        super::C_TOO_MANY_MEMBERS,
        super::C_TOO_MANY_BINDINGS,
        super::C_EXPRESSION_TOO_COMPLEX,
    ];

    /// The spec's `^[LPMRTEC]\d{3}$` shape, checked without a regex
    /// crate.
    fn has_valid_format(code: &str) -> bool {
        let bytes = code.as_bytes();

        bytes.len() == 4
            && matches!(bytes[0], b'L' | b'P' | b'M' | b'R' | b'T' | b'E' | b'C')
            && bytes[1..].iter().all(u8::is_ascii_digit)
    }

    #[test]
    fn codes_are_unique() {
        let mut seen = HashSet::new();

        for code in ALL {
            assert!(seen.insert(*code), "duplicate diagnostic code {code}");
        }
    }

    #[test]
    fn codes_match_the_phase_letter_plus_three_digits_format() {
        for code in ALL {
            assert!(
                has_valid_format(code),
                "code {code} does not match ^[LPMRTEC][0-9]{{3}}$"
            );
        }
    }
}
