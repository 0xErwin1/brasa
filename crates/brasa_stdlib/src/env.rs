//! The `std::env` member surface (`docs/spec/05-stdlib.md`, BRS-32 and
//! BRS-33).
//!
//! A free module like [`crate::fs`], and the first converted one whose
//! `throws` column names another module's errors: `cwd` and `cd` reach
//! the filesystem and fail the way every other path operation does, so
//! they borrow the `fs` lists rather than minting `env`-flavoured
//! duplicates. The guard in `brasa_typeck` that every declared error is
//! one the resolver knows as native covers a borrowed name exactly as
//! it covers an owned one.
//!
//! It is also the first module whose registry ids are not one run:
//! `get`/`set`/`vars`/`args` were minted in BRS-32, `cwd`/`cd` in
//! BRS-33, and `exit` later still, so the ids are 70-73, 97-98 and 137.
//! The table below is in surface order instead, which is the concrete
//! case for why [`crate::FREE_MODULES`] and the `brasa_bytecode`
//! registry cannot be one list: appending is the only compatible way to
//! extend the ids, and a readable table is not append-ordered.

crate::module_table! {
    /// Every `std::env` member, in surface order.
    EnvMember => ENV_MEMBERS, module "env" {
        /// `None` when the variable is unset, which is what separates
        /// it from one set to the empty string.
        Get   "get"   (string)         -> [Option<string>];
        Set   "set"   (string, string) -> unit;

        /// The whole environment as data, process overlay included.
        Vars  "vars"  ()               -> [Map<string, string>];

        /// The script's arguments, program name excluded.
        Args  "args"  ()               -> [Vector<string>];

        /// The two members that touch the filesystem, and the only two
        /// here that throw. `cwd` can fail only by having no readable
        /// current directory; `cd` looks at a path the caller chose, so
        /// it can fail every way `fs` can.
        Cwd   "cwd"   ()               -> string throws crate::fs::CWD_ERRORS;
        Cd    "cd"    (string)         -> unit   throws crate::fs::ALL_ERRORS;

        /// Declared `unit` because a caller must be allowed to write it
        /// as the last expression of a `unit` function; no call ever
        /// returns, since the VM unwinds past every handler.
        Exit  "exit"  (int)            -> unit;
    }
}

#[cfg(test)]
mod tests {
    use super::{ENV_MEMBERS, EnvMember};

    /// `decl` indexes the table by the variant's position, so a row and
    /// its variant must stay in the same order.
    #[test]
    fn every_member_resolves_to_its_own_declaration() {
        for decl in ENV_MEMBERS {
            let member = EnvMember::from_name(decl.name)
                .unwrap_or_else(|| panic!("`{}` resolves", decl.name));

            assert_eq!(member.decl().name, decl.name);
        }
    }

    #[test]
    fn unknown_names_do_not_resolve() {
        assert_eq!(EnvMember::from_name("home"), None);
    }

    /// A receiver-less table cannot mention the receiver's element
    /// type; `lower` would panic at the first call site rather than
    /// here (`crate::fs` carries the same guard).
    #[test]
    fn no_row_mentions_the_receiver_element_type() {
        fn mentions_elem(desc: &crate::TyDesc) -> bool {
            match desc {
                crate::TyDesc::Elem => true,
                crate::TyDesc::Vector(inner) | crate::TyDesc::Option(inner) => mentions_elem(inner),
                crate::TyDesc::Map(key, value) => mentions_elem(key) || mentions_elem(value),
                crate::TyDesc::Tuple(items) => items.iter().any(mentions_elem),
                crate::TyDesc::Fn(params, ret) => {
                    params.iter().any(mentions_elem) || mentions_elem(ret)
                }
                _ => false,
            }
        }

        for decl in ENV_MEMBERS {
            let types = decl
                .required
                .iter()
                .chain(decl.optional)
                .chain(std::iter::once(&decl.ret));

            for desc in types {
                assert!(
                    !mentions_elem(desc),
                    "`env.{}` mentions `elem`, but a free module has no receiver",
                    decl.name
                );
            }
        }
    }

    /// The filesystem pair is the whole of this module's error
    /// contribution. A third thrower appearing here would put a `catch`
    /// obligation on scripts that only read a variable.
    #[test]
    fn only_the_filesystem_members_throw() {
        for decl in ENV_MEMBERS {
            let throws = !decl.throws.is_empty();
            let touches_fs = matches!(decl.name, "cwd" | "cd");

            assert_eq!(
                throws, touches_fs,
                "`env.{}` disagrees with the filesystem rule",
                decl.name
            );
        }
    }

    /// `cd` fails every way a path operation can and `cwd` only by
    /// being unreadable, which is why the two borrow different `fs`
    /// lists rather than sharing one.
    #[test]
    fn the_two_throwers_borrow_the_fs_lists() {
        assert_eq!(EnvMember::Cwd.decl().throws, crate::fs::CWD_ERRORS);
        assert_eq!(EnvMember::Cd.decl().throws, crate::fs::ALL_ERRORS);
        assert!(crate::fs::CWD_ERRORS.len() < crate::fs::ALL_ERRORS.len());
    }
}
