//! The `std::io` member surface (`docs/spec/05-stdlib.md`, BRS-34).
//!
//! A free module like [`crate::fs`], and the only converted one whose
//! `throws` column is empty on every row: a closed stream ends the run
//! rather than raising, so no member contributes to a caller's
//! error-set. The column is still written — as nothing — which is the
//! point of declaring it in the table instead of a side list.

crate::module_table! {
    /// Every `std::io` member, in declaration order.
    IoMember => IO_MEMBERS, module "io" {
        /// The printers take any single value and render it through the
        /// universal `toString`, like the prelude `puts`/`print`, which
        /// is what `unknown` says: the call site decides the type.
        Puts     "puts"     (unknown) -> unit;
        Print    "print"    (unknown) -> unit;
        EPrint   "eprint"   (unknown) -> unit;

        /// `None` at end of input, which is what separates it from an
        /// empty line.
        ReadLine "readLine" ()        -> [Option<string>];
        ReadAll  "readAll"  ()        -> string;
    }
}

#[cfg(test)]
mod tests {
    use super::{IO_MEMBERS, IoMember};

    /// `decl` indexes the table by the variant's position, so a row and
    /// its variant must stay in the same order.
    #[test]
    fn every_member_resolves_to_its_own_declaration() {
        for decl in IO_MEMBERS {
            let member = IoMember::from_name(decl.name)
                .unwrap_or_else(|| panic!("`{}` resolves", decl.name));

            assert_eq!(member.decl().name, decl.name);
        }
    }

    #[test]
    fn unknown_names_do_not_resolve() {
        assert_eq!(IoMember::from_name("readByte"), None);
    }

    /// Nothing here throws (BRS-34). A row that acquired an error list
    /// would put a `catch` obligation on every script that prints.
    #[test]
    fn no_member_throws() {
        for decl in IO_MEMBERS {
            assert!(
                decl.throws.is_empty(),
                "`io.{}` declares an error, but the surface is infallible",
                decl.name
            );
        }
    }
}
