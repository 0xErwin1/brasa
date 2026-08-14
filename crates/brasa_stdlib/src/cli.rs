//! The `std::cli` surface (`docs/spec/05-stdlib.md`, BRS-112): the two
//! module members and the `Args` record `parse` yields.

/// `cli.UsageError`: the command line did not match the declaration.
/// Raised by `parse` only — `help` renders the declaration and never
/// sees a command line.
pub const USAGE_ERROR: &str = "cli.UsageError";

/// The one error `cli.parse` raises.
pub const PARSE_ERRORS: &[&str] = &[USAGE_ERROR];

crate::module_table! {
    /// Every `std::cli` member, in surface order.
    ///
    /// Both take the same declaration, written as a vector of
    /// `[kind, name, short, help]` rows — a nested string vector rather
    /// than a record, because a script builds it as a literal and
    /// nothing reads it back.
    CliMember => CLI_MEMBERS, module "cli" {
        Parse "parse" ([Vector<string>], [Vector<[Vector<string>]>]) -> args
            throws PARSE_ERRORS;

        /// Renders the declaration for `--help`. It cannot fail: a
        /// malformed declaration is the author's bug and is fatal, not
        /// a usage error to report to whoever ran the script.
        Help  "help"  (string, [Vector<[Vector<string>]>])           -> string;
    }
}

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
