//! The `std::json` member surface (`docs/spec/05-stdlib.md`, BRS-34).
//!
//! A free module like [`crate::fs`], and the smallest one: two members
//! that convert between a `string` and the compiler-known `Json` tree.
//!
//! The `Json` ACCESSORS (`asInt`, `get`, …) are not here. They are
//! methods on a `Json` receiver, not members of the module, and their
//! result types flatten through `Option<Json>` in a way the receiver
//! table's `elem` column does not express — so they stay hand-written
//! in the checker until a receiver table earns that column.

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
