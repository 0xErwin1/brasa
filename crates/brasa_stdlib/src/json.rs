//! The `std::json` member surface (`docs/spec/05-stdlib.md`, BRS-34).
//!
//! A free module like [`crate::fs`], and the smallest one: two members
//! that convert between a `string` and the compiler-known `Json` tree.
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

/// The one error this module raises, spelled as a list so a row can
/// name it the way the `fs` rows name theirs.
pub const PARSE_ERRORS: &[&str] = &[PARSE_ERROR];

crate::module_table! {
    /// Every `std::json` member, in declaration order.
    JsonMember => JSON_MEMBERS, module "json" {
        Parse     "parse"     (string) -> json   throws PARSE_ERRORS;

        /// Takes a `Json` value rather than an arbitrary language value:
        /// serializing a struct or a vector is a v2 design, so this
        /// cannot fail — every `Json` tree renders.
        Stringify "stringify" (json)   -> string;
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
    use super::{JSON_MEMBERS, JsonMember, PARSE_ERRORS};

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

    /// `stringify` is the module's only infallible member, and `parse`
    /// its only thrower. A row that acquired the other's column would
    /// be changing a contract the spec states.
    #[test]
    fn only_parse_throws() {
        assert_eq!(JsonMember::Parse.decl().throws, PARSE_ERRORS);
        assert!(JsonMember::Stringify.decl().throws.is_empty());
    }
}
