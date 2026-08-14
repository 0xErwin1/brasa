//! The `string` method surface (spec: 05 — Stdlib de scripting, BRS-31 and
//! BRS-53).
//!
//! A [`crate::RecvShape::Plain`] receiver: `string` has no type
//! arguments, so every row is concrete and none of them may say `elem`.
//!
//! This is the first converted receiver whose methods throw, which is
//! why [`crate::MethodDecl`] has a `throws` column at all. Before it,
//! the six throwing methods were listed in `brasa_errorset` — one table
//! away from the signatures they belong to, with nothing keeping the
//! two in agreement.
//!
//! Names shared with another receiver kind (`len`, `slice`, `find`,
//! `contains?`, `reverse`) are declared here for the `string` receiver
//! only. They share ONE `brasa_bytecode` id with the other receivers
//! that carry them, and the VM dispatches on the receiver's runtime
//! kind.

/// `string.ParseError`: the text is not a number of the requested kind.
pub const PARSE_ERROR: &str = "string.ParseError";

/// `string.RegexError`: the pattern argument is not a valid regex.
///
/// A runtime error rather than a compile-time one because the pattern
/// is an ordinary string until `std::re` lands — there is nothing for
/// the checker to look at.
pub const REGEX_ERROR: &str = "string.RegexError";

/// What a parsing method raises.
pub const PARSE_ERRORS: &[&str] = &[PARSE_ERROR];

/// What a regex method raises.
pub const REGEX_ERRORS: &[&str] = &[REGEX_ERROR];

crate::method_table! {
    /// Every `string` method, in declaration order.
    StringMember => STRING_METHODS, receiver "string" Plain {
        Len         "len"          ()                 -> int;
        Count       "count"        (string)           -> int;

        Trim        "trim"         ()                 -> string;
        TrimStart   "trimStart"    ()                 -> string;
        TrimEnd     "trimEnd"      ()                 -> string;
        ToUpper     "toUpper"      ()                 -> string;
        ToLower     "toLower"      ()                 -> string;
        Reverse     "reverse"      ()                 -> string;

        Contains    "contains?"    (string)           -> bool;
        StartsWith  "startsWith?"  (string)           -> bool;
        EndsWith    "endsWith?"    (string)           -> bool;

        Split       "split"        (string)           -> [Vector<string>];
        Lines       "lines"        ()                 -> [Vector<string>];
        Chars       "chars"        ()                 -> [Vector<char>];

        /// The UTF-8 byte values (0..=255) as ints, which is what
        /// separates it from `chars`.
        Bytes       "bytes"        ()                 -> [Vector<int>];

        Slice       "slice"        (int, int)         -> string;
        Repeat      "repeat"       (int)              -> string;
        PadStart    "padStart"     (int, string)      -> string;
        PadEnd      "padEnd"       (int, string)      -> string;
        Replace     "replace"      (string, string)   -> string;

        /// Total, unlike a `slice` the caller would have to guard: an
        /// absent prefix yields the string unchanged (BRS-53).
        RemovePrefix "removePrefix" (string)          -> string;

        Find        "find"         (string)           -> [Option<int>];

        /// The parsers answer the number directly and throw on failure
        /// rather than yielding an `Option` (BRS-41), so the common
        /// path reads as arithmetic and the failure is a `catch`.
        ToInt       "toInt"        ()                 -> int    throws PARSE_ERRORS;
        ToFloat     "toFloat"      ()                 -> float  throws PARSE_ERRORS;

        /// The regex four. Every one of them can be handed a pattern
        /// that does not compile, which is the whole of what they
        /// raise.
        Match       "match?"       (string)           -> bool                     throws REGEX_ERRORS;
        Captures    "captures"     (string)           -> [Option<[Vector<string>]>] throws REGEX_ERRORS;
        ReplaceRe   "replaceRe"    (string, string)   -> string                   throws REGEX_ERRORS;
        Scan        "scan"         (string)           -> [Vector<string>]         throws REGEX_ERRORS;
    }
}

#[cfg(test)]
mod tests {
    use super::{STRING_METHODS, StringMember};

    /// `decl` indexes the table by the variant's position, so a row and
    /// its variant must stay in the same order.
    #[test]
    fn every_member_resolves_to_its_own_declaration() {
        for decl in STRING_METHODS {
            let member = StringMember::from_name(decl.name)
                .unwrap_or_else(|| panic!("`{}` resolves", decl.name));

            assert_eq!(member.decl().name, decl.name);
        }
    }

    #[test]
    fn unknown_names_do_not_resolve() {
        assert_eq!(StringMember::from_name("format"), None);
    }

    /// The throwing set, written out rather than derived from the table
    /// it checks. What `throws` decides is whether the checker accepts
    /// a caller's `throws` clause, so a test that asked the table what
    /// the table says would pass for any answer.
    #[test]
    fn exactly_the_parsers_and_the_regex_methods_throw() {
        let expected: &[(&str, &[&str])] = &[
            ("toInt", super::PARSE_ERRORS),
            ("toFloat", super::PARSE_ERRORS),
            ("match?", super::REGEX_ERRORS),
            ("captures", super::REGEX_ERRORS),
            ("replaceRe", super::REGEX_ERRORS),
            ("scan", super::REGEX_ERRORS),
        ];

        for decl in STRING_METHODS {
            let want = expected
                .iter()
                .find(|(name, _)| *name == decl.name)
                .map(|(_, errors)| *errors)
                .unwrap_or(&[]);

            assert_eq!(
                decl.throws, want,
                "`string.{}` disagrees about what it raises",
                decl.name
            );
        }

        for (name, _) in expected {
            assert!(
                StringMember::from_name(name).is_some(),
                "`string.{name}` is expected to throw but is not declared"
            );
        }
    }
}
