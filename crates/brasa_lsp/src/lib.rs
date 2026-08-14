//! The Brasa language server (spec: 00 — Visión y alcance, M5 — BRS-92).
//!
//! Minimal on purpose: diagnostics as you type, and hover showing the
//! inferred type and error-set. Both are things the compiler already
//! knows and had no way to say.

pub mod analysis;
pub mod convert;
pub mod server;
pub mod uri;

pub use analysis::{Analysis, Hover, analyze};
pub use server::run;
