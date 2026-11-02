# CLAUDE.md

Agent instructions for this repo live in [AGENTS.md](AGENTS.md) - read
that first. It covers build/test commands, the two toolchain-gated
feature flags (`postgres`, `local-whisper`) and what each needs, and the
conventions (money-as-integer-minor-units, trait-based storage/voice
abstractions, confirmation-before-commit, bilingual en/id coverage,
`#[ignore]`d tests for anything needing an external toolchain/service)
that are load-bearing rather than incidental - don't change those
without a reason that's actually about the task at hand.

Nothing below is Claude-Code-specific beyond that pointer; everything
else in `AGENTS.md` applies equally to any AI coding agent working in
this repo.
