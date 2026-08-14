//! The `std::cli` record surface (`docs/spec/05-stdlib.md`, BRS-112).
//!
//! Only the record so far, for the same reason as [`crate::http`]:
//! `cli.parse` and `cli.help` still declare their signatures and
//! `cli.parse`'s `cli.UsageError` contribution by hand in
//! `brasa_typeck::builtins`, but the record they produce does not
//! depend on that.

crate::record_table! {
    /// The `Args` record: a parsed command line. Both lookups are
    /// total — the parse already rejected anything the spec did not
    /// allow, so by the time a caller holds an `Args` the only
    /// question left is whether a permitted flag was passed.
    ArgsMember => ARGS_MEMBERS, record "Args" {
        /// The positional arguments, in order, flags removed.
        Rest   "rest"            -> [Vector<string>];

        /// Absent is `false` rather than `None`: a flag that was not
        /// passed is off, and there is no third state to report.
        Flag   "flag"   (string) -> bool;

        /// Absent is `None` here, because an option that was not
        /// passed has no value to stand in for it.
        Option "option" (string) -> [Option<string>];
    }
}

#[cfg(test)]
mod tests {
    use super::{ARGS_MEMBERS, ArgsMember};
    use crate::{RecordKind, TyDesc};

    /// `decl` indexes the table by the variant's position, so a row and
    /// its variant must stay in the same order.
    #[test]
    fn every_member_resolves_to_its_own_declaration() {
        for decl in ARGS_MEMBERS {
            let member = ArgsMember::from_name(decl.name)
                .unwrap_or_else(|| panic!("`{}` resolves", decl.name));

            assert_eq!(member.decl().name, decl.name);
        }
    }

    #[test]
    fn unknown_names_do_not_resolve() {
        assert_eq!(ArgsMember::from_name("flags"), None);
    }

    /// The two lookups take the name being looked up; the positional
    /// list does not, because there is nothing to name.
    #[test]
    fn the_two_lookups_take_a_name() {
        for decl in ARGS_MEMBERS {
            match decl.kind {
                RecordKind::Method(params) => {
                    assert_eq!(
                        params,
                        &[TyDesc::String],
                        "`Args.{}` looks something up, so it takes a name",
                        decl.name
                    );
                }
                RecordKind::Field => assert_eq!(decl.name, "rest"),
            }
        }
    }

    /// The difference the two lookups draw, which is the reason they
    /// are two members rather than one: an absent flag is `false` and
    /// an absent option is `None`. Collapsing them would force a caller
    /// to unwrap a flag that is simply off.
    #[test]
    fn a_missing_flag_is_false_and_a_missing_option_is_none() {
        assert_eq!(ArgsMember::Flag.decl().ret, TyDesc::Bool);
        assert!(matches!(
            ArgsMember::Option.decl().ret,
            TyDesc::Option(inner) if *inner == TyDesc::String
        ));
    }
}
