# CLAUDE.md

Read `AGENTS.md` first — it is the canonical agent guide for this repo
(spec locations, environment, workspace layout, architecture invariants,
conventions). Everything there applies to Claude Code sessions.

Quick facts:

- Enter the environment with `direnv allow` or `nix develop --impure`;
  `cargo check` must stay clean.
- `docs/spec/` is normative and mirrored to the Atlas workspace `brasa`
  (update both when the spec changes).
- Work items: Atlas board `Roadmap`, epics BRS-1..BRS-6 with subtasks.
