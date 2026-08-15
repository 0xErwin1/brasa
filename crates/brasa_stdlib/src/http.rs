//! The `std::http` surface (spec: 05 — Stdlib de scripting, BRS-113): the two
//! request members and the `Response` record they yield.

/// `http.RequestError`: the request never produced a response. A
/// non-2xx status is NOT this — it is an answer, reported through
/// `Response::status`.
pub const REQUEST_ERROR: &str = "http.RequestError";

/// The one error either request member raises.
pub const ALL_ERRORS: &[&str] = &[REQUEST_ERROR];

crate::module_table! {
    /// Every `std::http` member, in surface order.
    HttpMember => HTTP_MEMBERS, module "http" {
        /// The optional trailing parameter is a timeout in
        /// milliseconds. Both members answer a `Response` whatever the
        /// status was, and throw only when there was no response at
        /// all.
        Get  "get"  (string)         ?(int) -> response throws ALL_ERRORS;
        Post "post" (string, string) ?(int) -> response throws ALL_ERRORS;

        /// The `With` pair adds request headers (BRS-129) — the door
        /// to every authenticated API. Separate members rather than a
        /// second optional on `get`/`post`: optionals are positional,
        /// so a caller would have to pass a timeout to reach the
        /// headers (or the shipped timeout position would move).
        /// Header names are sent as given; the RESPONSE side is where
        /// lookup is case-insensitive.
        GetWith  "getWith"  (string, [Map<string, string>])         ?(int) -> response throws ALL_ERRORS;
        PostWith "postWith" (string, string, [Map<string, string>]) ?(int) -> response throws ALL_ERRORS;
    }
}

crate::record_table! {
    /// The `Response` record: the two parts of an answer that always
    /// exist, plus the lookup for the part that may not.
    ResponseMember => RESPONSE_MEMBERS, record "Response" {
        /// A non-2xx status is an answer, not an error — which is why
        /// this is a field a caller reads rather than something the
        /// request already raised about.
        Status "status"          -> int;
        Body   "body"            -> string;

        /// A method rather than a field, because the lookup takes the
        /// name being looked up. It is case-insensitive, since HTTP
        /// header names are, and total: an absent header is `None`, so
        /// a caller writes `?? fallback` instead of guarding.
        Header "header" (string) -> [Option<string>];
    }
}

#[cfg(test)]
mod tests {
    use super::{RESPONSE_MEMBERS, ResponseMember};
    use crate::RecordKind;

    /// `decl` indexes the table by the variant's position, so a row and
    /// its variant must stay in the same order.
    #[test]
    fn every_member_resolves_to_its_own_declaration() {
        for decl in RESPONSE_MEMBERS {
            let member = ResponseMember::from_name(decl.name)
                .unwrap_or_else(|| panic!("`{}` resolves", decl.name));

            assert_eq!(member.decl().name, decl.name);
        }
    }

    #[test]
    fn unknown_names_do_not_resolve() {
        assert_eq!(ResponseMember::from_name("headers"), None);
    }

    /// The one member that takes an argument is the one whose answer
    /// depends on it. Turning `header` into a field would mean picking
    /// a header at declaration time, and turning `status` into a method
    /// would make every reader write empty parentheses.
    #[test]
    fn only_the_header_lookup_is_a_method() {
        for decl in RESPONSE_MEMBERS {
            let is_method = matches!(decl.kind, RecordKind::Method(_));

            assert_eq!(
                is_method,
                decl.name == "header",
                "`Response.{}` disagrees with the field/method split",
                decl.name
            );
        }
    }
}
