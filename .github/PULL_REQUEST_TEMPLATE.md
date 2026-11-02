## What this changes and why

<!-- The "why" matters more than the "what" - link an issue if there is one. -->

## How it was tested

<!--
- `cargo test --workspace --no-default-features`
- Feature-gated paths you touched (`--features postgres`, `--features local-whisper`), and how you verified them
- Manual steps, if any (which channel, which LLM provider, etc.)
-->

## Checklist

- [ ] `cargo fmt --all -- --check` passes
- [ ] `cargo clippy --workspace --no-default-features -- -D warnings` passes
- [ ] `cargo test --workspace --no-default-features` passes
- [ ] New/changed behavior is covered by a test
- [ ] Docs updated if behavior, config, or setup steps changed (`README.md`, `README.id.md`, `docs/ARCHITECTURE.md`, or a `docs/setup-*.md`)
- [ ] If this changes storage: both `SqliteStore` and `PostgresStore` stay behaviorally identical (see `AnyStore` in `crates/guss-store/src/any.rs`)
