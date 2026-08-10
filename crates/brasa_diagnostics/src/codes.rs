//! Stable diagnostic codes, one constant per error kind.
//!
//! This is the machine-facing side of the public registry in
//! `docs/spec/06-diagnostics.md`; the spec table and these constants must
//! stay in sync. Codes follow `<PhaseLetter><3 digits>` (`L` lexer, `P`
//! parser, `R` resolver, `T` type checker; `E` and `X` are reserved), are
//! append-only, and are never renumbered or reused after removal. Every
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
/// A `::` import whose first segment is not `std`.
pub const R_UNKNOWN_IMPORT_ROOT: &str = "R008";
/// A `std::` import naming no known std module.
pub const R_UNKNOWN_STD_MODULE: &str = "R009";
/// A generic constraint naming a type that is not an interface.
pub const R_NOT_AN_INTERFACE: &str = "R010";

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
/// `join` called on a `Vector` whose element type is not `string`.
pub const T_JOIN_REQUIRES_STRING_VECTOR: &str = "T008";
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
        super::T_MISMATCHED_TYPES,
        super::T_INVALID_OPERANDS,
        super::T_CANNOT_COMPARE_EQUALITY,
        super::T_UNSUPPORTED_ORDERING,
        super::T_WRONG_ARG_COUNT,
        super::T_NOT_CALLABLE,
        super::T_UNKNOWN_MEMBER,
        super::T_JOIN_REQUIRES_STRING_VECTOR,
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
    ];

    /// The spec's `^[LPRT]\d{3}$` shape, checked without a regex crate.
    fn has_valid_format(code: &str) -> bool {
        let bytes = code.as_bytes();

        bytes.len() == 4
            && matches!(bytes[0], b'L' | b'P' | b'R' | b'T')
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
                "code {code} does not match ^[LPRT][0-9]{{3}}$"
            );
        }
    }
}
