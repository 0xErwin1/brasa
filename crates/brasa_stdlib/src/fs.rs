//! The `std::fs` member surface (`docs/spec/05-stdlib.md`, BRS-33).
//!
//! `fs` is a free module: every member is called as `fs.read(path)`,
//! with no receiver, so its rows are written in [`crate::module_table!`]
//! rather than the receiver shape `Vector` uses. Types are written in
//! the [`crate::ty!`] language; `elem` has no meaning here, since there
//! is no receiver to take an element type from.
//!
//! The path helpers (`join`, `base`, `dir`, `ext`, `abs`, `resolve`)
//! are `fs` members per the spec's "`path` helpers" bullet rather than a
//! module of their own.

/// `fs.NotFound`: the path does not exist.
pub const NOT_FOUND: &str = "fs.NotFound";

/// `fs.Denied`: the path exists but the process may not touch it.
pub const DENIED: &str = "fs.Denied";

/// `fs.IoError`: every other filesystem failure.
pub const IO_ERROR: &str = "fs.IoError";

/// The three filesystem errors, raised together by every member that
/// touches the filesystem (BRS-33). No member distinguishes a subset:
/// the same `open` can answer any of the three, so a narrower list
/// would be a promise the OS does not make.
pub const ALL_ERRORS: &[&str] = &[NOT_FOUND, DENIED, IO_ERROR];

/// An unreadable current directory is the only way `fs.abs` fails: it
/// resolves against the cwd and never looks at the path itself.
pub const CWD_ERRORS: &[&str] = &[IO_ERROR];

crate::module_table! {
    /// Every `std::fs` member, in declaration order.
    FsMember => FS_MEMBERS, module "fs" {
        Read      "read"       (string)         -> string           throws ALL_ERRORS;
        Write     "write"      (string, string) -> unit             throws ALL_ERRORS;
        Append    "append"     (string, string) -> unit             throws ALL_ERRORS;

        /// The predicates answer about the filesystem rather than
        /// reading it, so an inaccessible path is `false` rather than a
        /// throw — a guard that could itself fail would need a guard.
        Exists    "exists?"    (string)         -> bool;
        IsFile    "isFile?"    (string)         -> bool;
        IsDir     "isDir?"     (string)         -> bool;
        IsSymlink "isSymlink?" (string)         -> bool;

        Ls        "ls"         (string)         -> [Vector<string>] throws ALL_ERRORS;
        Glob      "glob"       (string)         -> [Vector<string>] throws ALL_ERRORS;

        /// The optional trailing list is directory names to prune.
        Walk      "walk"       (string) ?([Vector<string>]) -> [Vector<string>] throws ALL_ERRORS;

        /// The tolerant form (BRS-66): a directory BELOW the root that
        /// cannot be read is reported in the result rather than thrown,
        /// but the root itself still throws, so the list is unchanged.
        TryWalk   "tryWalk"    (string) ?([Vector<string>]) -> walk throws ALL_ERRORS;

        Mkdir     "mkdir"      (string)         -> unit             throws ALL_ERRORS;
        MkdirAll  "mkdirAll"   (string)         -> unit             throws ALL_ERRORS;
        Rm        "rm"         (string)         -> unit             throws ALL_ERRORS;
        RmAll     "rmAll"      (string)         -> unit             throws ALL_ERRORS;
        Cp        "cp"         (string, string) -> unit             throws ALL_ERRORS;
        Mv        "mv"         (string, string) -> unit             throws ALL_ERRORS;

        /// The path helpers below are string arithmetic: they never
        /// look at the filesystem, so they cannot fail.
        Join      "join"       (string, string) -> string;
        Base      "base"       (string)         -> string;
        Dir       "dir"        (string)         -> string;
        Ext       "ext"        (string)         -> string;

        Abs       "abs"        (string)         -> string           throws CWD_ERRORS;

        /// Unlike `abs`, this one follows the path on disk, so it fails
        /// the way any other read does.
        Resolve   "resolve"    (string)         -> string           throws ALL_ERRORS;
    }
}

#[cfg(test)]
mod tests {
    use super::{ALL_ERRORS, CWD_ERRORS, FS_MEMBERS, FsMember};
    use crate::TyDesc;

    /// `decl` indexes the table by the variant's position, so a row and
    /// its variant must stay in the same order. A table whose rows and
    /// variants drifted apart would hand every layer the wrong
    /// signature silently.
    #[test]
    fn every_member_resolves_to_its_own_declaration() {
        for decl in FS_MEMBERS {
            let member = FsMember::from_name(decl.name)
                .unwrap_or_else(|| panic!("`{}` resolves", decl.name));

            assert_eq!(member.decl().name, decl.name);
        }
    }

    #[test]
    fn names_are_unique() {
        for decl in FS_MEMBERS {
            let declared = FS_MEMBERS.iter().filter(|d| d.name == decl.name).count();
            assert_eq!(declared, 1, "`{}` is declared twice", decl.name);
        }
    }

    #[test]
    fn unknown_names_do_not_resolve() {
        assert_eq!(FsMember::from_name("definitelyNotAMember"), None);
    }

    /// A free module has no receiver, so a row that mentioned the
    /// receiver's element type would have nothing to lower against and
    /// would only be discovered when a user called that member.
    #[test]
    fn no_row_mentions_the_receiver_element_type() {
        fn mentions_elem(desc: &TyDesc) -> bool {
            match desc {
                TyDesc::Elem => true,
                TyDesc::Vector(inner) | TyDesc::Option(inner) => mentions_elem(inner),
                TyDesc::Tuple(items) => items.iter().any(mentions_elem),
                TyDesc::Fn(params, ret) => params.iter().any(mentions_elem) || mentions_elem(ret),
                _ => false,
            }
        }

        for decl in FS_MEMBERS {
            let types = decl
                .required
                .iter()
                .chain(decl.optional)
                .chain(std::iter::once(&decl.ret));

            assert!(
                !types.into_iter().any(mentions_elem),
                "`fs.{}` mentions `elem`, but a free module has no receiver",
                decl.name
            );
        }
    }

    /// Every declared error is one of this module's own, and the two
    /// lists are the only shapes the surface uses: a member that raised
    /// something else would be raising an error `catch` cannot name
    /// under `fs.`.
    #[test]
    fn every_declared_error_belongs_to_this_module() {
        for decl in FS_MEMBERS {
            assert!(
                decl.throws.is_empty() || decl.throws == ALL_ERRORS || decl.throws == CWD_ERRORS,
                "`fs.{}` declares an error list this module does not define",
                decl.name
            );
        }
    }
}
