//! Lexer for Brasa, built on `logos`.
//!
//! Turns source text into `brasa_token` tokens with spans. Newlines are
//! tokens (they terminate statements); string interpolation switches the
//! lexer into a sub-mode. Implemented in BRS-8.
