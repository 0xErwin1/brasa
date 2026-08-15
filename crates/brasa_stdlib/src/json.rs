//! The `std::json` member surface (spec: 05 — Stdlib de scripting, BRS-34).
//!
//! A free module like [`crate::fs`], and one of the smallest: three
//! members that convert between the language's values, a `string`, and
//! the compiler-known `Json` tree.
//!
//! The module reads AND writes. `parse` is the read side; `of` is its
//! mirror, building a `Json` tree out of a language value, and
//! `stringify` renders one. Because `of` exists, `stringify` takes any
//! value rather than only a `Json`: `stringify(x)` is `stringify(of(x))`
//! for everything that is not already a tree, so a script that produces
//! JSON never has to hand-roll escaping.
//!
//! Not every language value has a JSON representation, so both writers
//! raise [`VALUE_ERROR`]. What converts and what does not is fixed by
//! the backend that implements the walk (`brasa_vm`), which is the only
//! layer that can read a value's contents; this table only declares
//! that the two members can fail.
//!
//! The `Json` ACCESSORS are methods on a `Json` receiver rather than
//! members of the module, so they are a second table below.
//!
//! They were held back once on the grounds that they "flatten through
//! `Option<Json>` in a way a receiver table cannot express". That
//! conflated two things. The flattening decides which TABLE a receiver
//! selects — the checker's job, exactly like its map from a `Type` to a
//! record's table — not what any row says. Every row here is concrete.

/// `json.ParseError`: the input is not valid JSON.
pub const PARSE_ERROR: &str = "json.ParseError";

/// The read side's error, spelled as a list so a row can name it the
/// way the `fs` rows name theirs.
pub const PARSE_ERRORS: &[&str] = &[PARSE_ERROR];

/// `json.ValueError`: a language value that has no JSON representation.
pub const VALUE_ERROR: &str = "json.ValueError";

/// The write side's error, as a list for the reason [`PARSE_ERRORS`] is
/// one.
pub const VALUE_ERRORS: &[&str] = &[VALUE_ERROR];

crate::module_table! {
    /// Every `std::json` member, in declaration order.
    JsonMember => JSON_MEMBERS, module "json" {
        Parse     "parse"     (string)  -> json   throws PARSE_ERRORS;

        /// The write-side mirror of `parse`: builds a tree out of a
        /// language value. `unknown` because the accepted set is a
        /// runtime question — the mapping covers whole families
        /// (`Vector`, `Map`, `Struct`) whose element types the table
        /// has no way to constrain.
        Of        "of"        (unknown) -> json   throws VALUE_ERRORS;

        /// Takes any value, not only a `Json`: a tree renders as it
        /// always did, and anything else is built first, so this can
        /// fail exactly where `of` does.
        Stringify "stringify" (unknown) -> string throws VALUE_ERRORS;
    }
}

crate::method_table! {
    /// Every `Json` accessor, in declaration order.
    ///
    /// All of them answer `Option`: a node is asked what kind it is,
    /// and `None` means it is a different kind. Since `Json` values
    /// cannot be constructed in the language, `?? fallback` at the end
    /// of an indexing chain is how a chain terminates.
    JsonAccessor => JSON_ACCESSORS, receiver "Json" Plain {
        AsString "asString" () -> [Option<string>];

        /// Succeeds only for an integral number representable as an
        /// `int`; `asFloat` succeeds for every number, so a caller that
        /// wants either asks for the float.
        AsInt    "asInt"    () -> [Option<int>];
        AsFloat  "asFloat"  () -> [Option<float>];
        AsBool   "asBool"   () -> [Option<bool>];
        AsArray  "asArray"  () -> [Option<[Vector<json>]>];
        AsObject "asObject" () -> [Option<[Map<string, json>]>];

        /// Distinguishes an explicit JSON `null` from an absent member,
        /// which indexing already reported as `None`. A bool rather
        /// than an `Option` because the question always has an answer.
        Null     "null?"    () -> bool;
    }
}

#[cfg(test)]
mod tests {
    use super::{JSON_MEMBERS, JsonMember, PARSE_ERRORS, VALUE_ERRORS};

    /// `decl` indexes the table by the variant's position, so a row and
    /// its variant must stay in the same order.
    #[test]
    fn every_member_resolves_to_its_own_declaration() {
        for decl in JSON_MEMBERS {
            let member = JsonMember::from_name(decl.name)
                .unwrap_or_else(|| panic!("`{}` resolves", decl.name));

            assert_eq!(member.decl().name, decl.name);
        }
    }

    #[test]
    fn unknown_names_do_not_resolve() {
        assert_eq!(JsonMember::from_name("asInt"), None);
    }

    /// The read side raises the read side's error and the write side
    /// the write side's: a row that acquired the other's column would
    /// be changing a contract the spec states, and would put a name in
    /// a caller's error-set that the member can never raise.
    #[test]
    fn each_side_throws_its_own_error() {
        assert_eq!(JsonMember::Parse.decl().throws, PARSE_ERRORS);
        assert_eq!(JsonMember::Of.decl().throws, VALUE_ERRORS);
        assert_eq!(JsonMember::Stringify.decl().throws, VALUE_ERRORS);
    }

    /// Every declared error is one of this module's own, so a member
    /// cannot name an error `catch` is unable to reach under `json.`
    /// (the `fs` table guards its two lists the same way).
    #[test]
    fn every_declared_error_belongs_to_this_module() {
        for decl in JSON_MEMBERS {
            assert!(
                decl.throws.is_empty()
                    || decl.throws == PARSE_ERRORS
                    || decl.throws == VALUE_ERRORS,
                "`json.{}` declares an error list this module does not define",
                decl.name
            );
        }
    }
}
