//! The `std::proc` member surface (spec: 05 — Stdlib de scripting, BRS-32).
//!
//! A free module like [`crate::fs`], and the one that needed both of
//! the table language's newest pieces.
//!
//! Its result is the `Output` record ([`crate::TyDesc::ProcOutput`]),
//! which is a type and so goes where `walk` and `json` went. Its
//! command parameter is not a type at all: `run`, `tryRun` and `shell`
//! accept an argv `Vector<string>` or the whitespace-split `string`
//! sugar, and no single type of the checker's means either-of-those.
//! That one is [`crate::ParamDesc::Command`], a rule in parameter
//! position rather than a case inside the type language.
//!
//! `tryRunAll` deliberately does not take it. The split-on-whitespace
//! sugar exists for a command an author typed literally; a batch is
//! built from data, where a string that happens to contain a space
//! would be a silent re-parse rather than a convenience.
//!
//! The `Output` record every runner yields is declared here too, in
//! the third table shape ([`crate::record_table!`]).

/// `proc.NonZeroExit`: the child ran and exited non-zero. The tolerant
/// runners do not raise it — a non-zero exit is their result.
pub const NON_ZERO_EXIT: &str = "proc.NonZeroExit";

/// `proc.SpawnError`: the child never started. Every runner raises it,
/// tolerant or not, because it is a failure of the environment rather
/// than of the command.
pub const SPAWN_ERROR: &str = "proc.SpawnError";

/// What a strict runner raises: either failure can reach the caller.
pub const STRICT_ERRORS: &[&str] = &[NON_ZERO_EXIT, SPAWN_ERROR];

/// What a tolerant runner raises: only the failure that leaves it with
/// no `Output` to return.
pub const TOLERANT_ERRORS: &[&str] = &[SPAWN_ERROR];

crate::module_table! {
    /// Every `std::proc` member, in surface order.
    ProcMember => PROC_MEMBERS, module "proc" {
        /// The runners take an optional trailing stdin string. `run`
        /// and `tryRun` differ only in whether a non-zero exit is an
        /// error or an answer, which is why their signatures are
        /// identical and their error lists are not.
        Run       "run"       (command) ?(string) -> procOutput throws STRICT_ERRORS;
        TryRun    "tryRun"    (command) ?(string) -> procOutput throws TOLERANT_ERRORS;

        /// The shell form takes a command line rather than a command:
        /// it is handed to `/bin/sh -c` whole, so the argv reading
        /// would be wrong here even though the sugar looks the same.
        Shell     "shell"     (string)  ?(string) -> procOutput throws STRICT_ERRORS;

        /// Argv arrays only, and an optional concurrency limit.
        TryRunAll "tryRunAll" ([Vector<[Vector<string>]>]) ?(int)
            -> [Vector<procOutput>] throws TOLERANT_ERRORS;
    }
}

crate::record_table! {
    /// The `Output` record: a finished child process, all three fields
    /// already captured. Nothing here is a method, because nothing
    /// about reading a captured value needs an argument.
    OutputMember => OUTPUT_MEMBERS, record "Output" {
        Stdout "stdout" -> string;
        Stderr "stderr" -> string;
        Code   "code"   -> int;
    }
}

#[cfg(test)]
mod tests {
    use super::{PROC_MEMBERS, ProcMember};
    use crate::ParamDesc;

    /// `decl` indexes the table by the variant's position, so a row and
    /// its variant must stay in the same order.
    #[test]
    fn every_member_resolves_to_its_own_declaration() {
        for decl in PROC_MEMBERS {
            let member = ProcMember::from_name(decl.name)
                .unwrap_or_else(|| panic!("`{}` resolves", decl.name));

            assert_eq!(member.decl().name, decl.name);
        }
    }

    #[test]
    fn unknown_names_do_not_resolve() {
        assert_eq!(ProcMember::from_name("spawn"), None);
    }

    /// The command rule belongs to the two runners that take a command,
    /// and to nothing else. `shell` takes a command *line* and
    /// `tryRunAll` takes argv arrays, so a `Command` appearing on
    /// either would widen what they accept without anyone saying so.
    #[test]
    fn only_the_argv_runners_take_the_command_rule() {
        for decl in PROC_MEMBERS {
            let crate::ModuleKind::Call {
                required, optional, ..
            } = decl.kind
            else {
                panic!("`proc.{}` is not a plain call", decl.name)
            };

            let takes_command = required
                .iter()
                .chain(optional)
                .any(|param| matches!(param, ParamDesc::Command));

            assert_eq!(
                takes_command,
                matches!(decl.name, "run" | "tryRun"),
                "`proc.{}` disagrees with the command rule",
                decl.name
            );
        }
    }

    /// Every runner can fail to start; only the strict pair reports a
    /// non-zero exit. Losing the difference would make `tryRun`
    /// catchable for a case it is defined to swallow.
    #[test]
    fn tolerance_is_exactly_the_non_zero_exit_difference() {
        for decl in PROC_MEMBERS {
            assert!(
                decl.throws.contains(&super::SPAWN_ERROR),
                "`proc.{}` cannot fail to spawn, which no runner can promise",
                decl.name
            );

            let strict = decl.throws.contains(&super::NON_ZERO_EXIT);
            assert_eq!(
                strict,
                matches!(decl.name, "run" | "shell"),
                "`proc.{}` disagrees with the tolerance rule",
                decl.name
            );
        }
    }
}
