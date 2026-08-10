//! AST for Brasa: index arenas, typed node IDs, and span side tables.
//!
//! Nodes live in per-kind `Vec`s and reference each other through `Copy`
//! IDs (`ExprId(u32)`, ...) instead of boxes — the rustc/rust-analyzer
//! pattern. The tree is immutable after parsing; later phases attach
//! information through side tables keyed by the same IDs. Implemented in
//! BRS-9.
