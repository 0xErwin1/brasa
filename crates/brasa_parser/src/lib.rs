//! Parser for Brasa.
//!
//! Recursive descent for items and statements; Pratt (binding powers) for
//! expressions, following the precedence table in
//! `docs/spec/02-grammar.md`. Produces `brasa_ast` arenas plus
//! diagnostics with recovery. Implemented in BRS-10/BRS-11.
