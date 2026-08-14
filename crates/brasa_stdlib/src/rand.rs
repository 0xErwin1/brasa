//! The `std::rand` member surface (`docs/spec/05-stdlib.md`, BRS-35).
//!
//! A deterministic PRNG: `rand.seed` fixes the stream, so a script that
//! seeds gets the same draws on every run.
//!
//! Nothing throws. Drawing from an empty range or an empty vector is a
//! `panics.AssertionFailed` rather than a catchable error — it is a bug
//! in the caller, not a condition to handle, and the panic channel is
//! where the language puts that distinction.

crate::module_table! {
    /// Every `std::rand` member, in surface order.
    RandMember => RAND_MEMBERS, module "rand" {
        Seed    "seed"    (int)   -> unit;

        /// Takes a range rather than two ints, so the half-open and
        /// inclusive forms are the language's `..` and `..=` instead of
        /// a boolean nobody would read correctly at the call site.
        Int     "int"     (range) -> int;

        /// The unit interval, which is why it takes nothing.
        Float   "float"   ()      -> float;

        /// Generic over the element of the vector they are handed:
        /// `choice` answers one element and `shuffle` a whole new
        /// vector of them. Neither is expressible as a fixed type, and
        /// unlike a receiver method there is no `elem` to name here —
        /// a free module has no receiver to take one from.
        Choice  "choice"  custom "generic over the element of the vector argument, which a free module cannot name";
        Shuffle "shuffle" custom "generic over the element of the vector argument, which a free module cannot name";
    }
}

#[cfg(test)]
mod tests {
    use super::{RAND_MEMBERS, RandMember};
    use crate::ModuleKind;

    /// `decl` indexes the table by the variant's position, so a row and
    /// its variant must stay in the same order.
    #[test]
    fn every_member_resolves_to_its_own_declaration() {
        for decl in RAND_MEMBERS {
            let member = RandMember::from_name(decl.name)
                .unwrap_or_else(|| panic!("`{}` resolves", decl.name));

            assert_eq!(member.decl().name, decl.name);
        }
    }

    #[test]
    fn unknown_names_do_not_resolve() {
        assert_eq!(RandMember::from_name("bool"), None);
    }

    /// An empty draw panics rather than throws, so no row contributes
    /// to a caller's error-set.
    #[test]
    fn no_member_throws() {
        for decl in RAND_MEMBERS {
            assert!(
                decl.throws.is_empty(),
                "`rand.{}` declares an error, but an empty draw panics instead",
                decl.name
            );
        }
    }

    /// The two vector-generic members are the delegated ones, and
    /// nothing else is. `rand.int` takes a range and answers an int —
    /// concrete on both sides — so it stays data.
    #[test]
    fn only_the_vector_generic_members_are_delegated() {
        for decl in RAND_MEMBERS {
            let delegated = matches!(decl.kind, ModuleKind::Custom(_));

            assert_eq!(
                delegated,
                matches!(decl.name, "choice" | "shuffle"),
                "`rand.{}` disagrees with the delegation rule",
                decl.name
            );
        }
    }

    /// Nothing here is read without a call: every member draws or sets,
    /// and a constant would be a fixed number from a random source.
    #[test]
    fn nothing_is_a_constant() {
        for decl in RAND_MEMBERS {
            assert!(
                !matches!(decl.kind, ModuleKind::Constant(_)),
                "`rand.{}` is a constant, which a random source cannot have",
                decl.name
            );
        }
    }
}
