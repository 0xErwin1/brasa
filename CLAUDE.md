# CLAUDE.md

Read `AGENTS.md` first — it is the agent guide for this repo
(environment, workspace layout, architecture invariants, conventions).
Everything there applies to Claude Code sessions.

Quick facts:

- Enter the environment with `direnv allow` or `nix develop --impure`;
  `cargo check` must stay clean.
- `.bras` sources are formatted by the language's own formatter, and the
  bundled examples are its corpus: `cargo run -- fmt --check examples`
  must pass.
