//! The `std::time` member surface (spec: 05 — Stdlib de scripting, BRS-35).
//!
//! Epoch timestamps, sleep, and basic ISO-8601 formatting and parsing.
//! A plain table: five calls, no constants, nothing delegated, and one
//! error. Reading the clock cannot fail — a clock that cannot be read
//! is not a condition a script can do anything about — but reading a
//! timestamp out of a string can, and `parseIso` is where that shows.

/// `time.ParseError`: the string handed to `time.parseIso` is not an
/// RFC 3339 timestamp. Every rejection lands here — a malformed shape,
/// a field outside the calendar, and a missing UTC offset alike —
/// because a caller can do exactly one thing about any of them, and
/// the message says which it was.
pub const PARSE_ERROR: &str = "time.ParseError";

/// The one error the parsing side raises.
pub const PARSE_ERRORS: &[&str] = &[PARSE_ERROR];

crate::module_table! {
    /// Every `std::time` member, in surface order.
    TimeMember => TIME_MEMBERS, module "time" {
        /// Seconds since the epoch as a float, so sub-second precision
        /// survives; `nowMillis` is the integer form for code that
        /// wants to do arithmetic without float error.
        Now       "now"       ()      -> float;
        NowMillis "nowMillis" ()      -> int;

        /// Takes milliseconds, like `nowMillis` answers them.
        Sleep     "sleep"     (int)   -> unit;

        /// Formats a millisecond timestamp, not a second one — the same
        /// unit `nowMillis` produces, so the two compose without a
        /// conversion nobody would remember to write.
        Iso       "iso"       (int)   -> string;

        /// The read side of `iso`, answering the same unit it takes, so
        /// a timestamp from an API becomes a number two of them can be
        /// subtracted as. The UTC offset is required: a string that
        /// does not name an instant has no epoch millisecond, and
        /// guessing one would be a wrong answer rather than a failure.
        ParseIso  "parseIso"  (string) -> int throws PARSE_ERRORS;
    }
}

#[cfg(test)]
mod tests {
    use super::{PARSE_ERRORS, TIME_MEMBERS, TimeMember};
    use crate::ModuleKind;

    /// `decl` indexes the table by the variant's position, so a row and
    /// its variant must stay in the same order.
    #[test]
    fn every_member_resolves_to_its_own_declaration() {
        for decl in TIME_MEMBERS {
            let member = TimeMember::from_name(decl.name)
                .unwrap_or_else(|| panic!("`{}` resolves", decl.name));

            assert_eq!(member.decl().name, decl.name);
        }
    }

    #[test]
    fn unknown_names_do_not_resolve() {
        assert_eq!(TimeMember::from_name("parse"), None);
    }

    /// The clock side of the module is still infallible, and the one
    /// error the parsing side raises is this module's own: a member
    /// declaring anything else would be raising something `catch`
    /// cannot name under `time.`.
    #[test]
    fn every_declared_error_belongs_to_this_module() {
        for decl in TIME_MEMBERS {
            assert!(
                decl.throws.is_empty() || decl.throws == PARSE_ERRORS,
                "`time.{}` declares an error list this module does not define",
                decl.name
            );
        }
    }

    /// Every row is an ordinary call: no constant to read, nothing
    /// delegated to the checker. Worth asserting because `time` is the
    /// baseline the other two M4 modules are the exception to.
    #[test]
    fn every_member_is_an_ordinary_call() {
        for decl in TIME_MEMBERS {
            assert!(
                matches!(decl.kind, ModuleKind::Call { .. }),
                "`time.{}` is not a plain call",
                decl.name
            );
        }
    }
}
