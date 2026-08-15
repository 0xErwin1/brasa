//! The record a native stdlib error binds to in a `catch` arm
//! (spec: 04 — Sistema de errores, BRS-135).
//!
//! Every error in [`crate`]'s namespaces — `string.ParseError`,
//! `fs.Denied`, `proc.NonZeroExit`, … — is one runtime value carrying
//! its canonical qualified name and a message. The name is what a
//! `catch` arm matches on; the message is what this record exposes.
//!
//! A record rather than the bare message string, which is what an arm
//! used to bind: a string has nowhere to put the data an error is
//! about, so `proc.NonZeroExit` had to embed the command, the exit
//! code and the child's stderr INTO its message text, and a caller that
//! wanted the code back had to parse English. Binding a record leaves
//! room for those fields to become members without disturbing the
//! arms that only ever read the message.
//!
//! The one invariant that keeps that promise cheap: every error record
//! carries `message`. An error that later declares a richer record of
//! its own still answers `e.message`, so gaining a payload never breaks
//! an arm that had one.
//!
//! Panics are deliberately NOT this shape. A panic arm still binds its
//! detail string (spec: 04): a panic is not a value the program is
//! meant to inspect and recover from, and giving it members would
//! invite exactly that.

crate::record_table! {
    /// Every `NativeError` member, in declaration order.
    ErrorMember => ERROR_MEMBERS, record "NativeError" {
        /// The human-readable message, identical to what rendering the
        /// error yields — `"#{e}"` and `e.message` agree by
        /// construction, so an arm that only printed the binding reads
        /// the same before and after BRS-135.
        Message "message" -> string;
    }
}

#[cfg(test)]
mod tests {
    use super::{ERROR_MEMBERS, ErrorMember};

    /// `decl` indexes the table by the variant's position, so a row and
    /// its variant must stay in the same order.
    #[test]
    fn every_member_resolves_to_its_own_declaration() {
        for decl in ERROR_MEMBERS {
            let member = ErrorMember::from_name(decl.name)
                .unwrap_or_else(|| panic!("`{}` resolves", decl.name));

            assert_eq!(member.decl().name, decl.name);
        }
    }

    #[test]
    fn unknown_names_do_not_resolve() {
        assert_eq!(ErrorMember::from_name("name"), None);
    }
}
