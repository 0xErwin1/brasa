//! The structured-concurrency surface (spec: 08 — Concurrencia
//! estructurada, BRS-133): the `Scope` a `concurrent` block receives
//! and the `Task` handles `spawn` answers with.
//!
//! `concurrent` itself is not declared here: it is a prelude function
//! like `print`, resolved by name with no import, so the resolver owns
//! it (`brasa_resolver::BuiltinValue`) and the checker owns its
//! signature — a lambda-taking free call is not expressible in the
//! module table language. What IS a table is the two receivers the
//! block works through, so every layer derives their surface from one
//! declaration like any other receiver's.

/// `concurrent.ScopeExited`: `spawn` was called on a scope whose
/// `concurrent` block already returned. The scope value itself is
/// harmless to keep — only spawning through it after the end is.
pub const SCOPE_EXITED: &str = "concurrent.ScopeExited";

/// The one error `spawn` raises.
pub const SPAWN_ERRORS: &[&str] = &[SCOPE_EXITED];

crate::method_table! {
    /// The `Scope` methods: `spawn` registers a block and answers the
    /// handle its result is read through.
    ///
    /// The result is `Task<U>` where `U` is the block's return type —
    /// the same argument-driven rule `Vector.map` uses, so the row
    /// delegates its result to the checker's `TaskOfFnRet` rule rather
    /// than stating a type.
    ScopeMember => SCOPE_METHODS, receiver "Scope" Plain {
        Spawn "spawn" ([fn() -> unknown]) -> taskOfFnRet throws SPAWN_ERRORS;
    }
}

crate::method_table! {
    /// The `Task<T>` methods: `value` runs the block if it has not run,
    /// caches the outcome, and answers the result — or rethrows the
    /// block's error, on every call.
    ///
    /// `value` declares no `throws` column: what it rethrows is the
    /// spawned block's own error-set, which flows at the `spawn` site
    /// (where the block is syntactically present), not a native error
    /// of its own.
    TaskMember => TASK_METHODS, receiver "Task" Elem {
        Value "value" () -> elem;
    }
}

#[cfg(test)]
mod tests {
    use super::{SCOPE_METHODS, ScopeMember, TASK_METHODS, TaskMember};

    /// `decl` indexes the table by the variant's position, so a row and
    /// its variant must stay in the same order.
    #[test]
    fn every_member_resolves_to_its_own_declaration() {
        for decl in SCOPE_METHODS {
            let member = ScopeMember::from_name(decl.name)
                .unwrap_or_else(|| panic!("`{}` resolves", decl.name));
            assert_eq!(member.decl().name, decl.name);
        }
        for decl in TASK_METHODS {
            let member = TaskMember::from_name(decl.name)
                .unwrap_or_else(|| panic!("`{}` resolves", decl.name));
            assert_eq!(member.decl().name, decl.name);
        }
    }

    #[test]
    fn unknown_names_do_not_resolve() {
        assert_eq!(ScopeMember::from_name("value"), None);
        assert_eq!(TaskMember::from_name("spawn"), None);
    }
}
