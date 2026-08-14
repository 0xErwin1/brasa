//! The `std::time` member surface (spec: 05 — Stdlib de scripting, BRS-35).
//!
//! Epoch timestamps, sleep, and basic ISO-8601 formatting. The plainest
//! table left: four calls, no constants, nothing delegated, and nothing
//! that throws — a clock that cannot be read is not a condition a
//! script can do anything about.

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
    }
}

#[cfg(test)]
mod tests {
    use super::{TIME_MEMBERS, TimeMember};
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

    #[test]
    fn no_member_throws() {
        for decl in TIME_MEMBERS {
            assert!(
                decl.throws.is_empty(),
                "`time.{}` declares an error, but the surface is infallible",
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
