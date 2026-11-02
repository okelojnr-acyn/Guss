# Agent instructions for Guss

This file is for AI coding agents (and is a good orientation for human
contributors too). Read [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)
first for the full design - this file is about conventions to preserve,
not a design doc.

## Build/test commands

```bash
cargo build --workspace --no-default-features   # default: sqlite only, no C toolchain needed
cargo test --workspace --no-default-features
cargo fmt --all -- --check
cargo clippy --workspace --no-default-features -- -D warnings
```

Two feature flags change what gets compiled, and both need something
beyond a plain Rust toolchain - don't assume either is available unless
you've confirmed it:

- **`postgres`** (on `guss-store`, forwarded through `guss`'s
  `Cargo.toml`): adds `PostgresStore`. Needs a live Postgres to run its
  tests (`GUSS_TEST_POSTGRES_URL`), but *compiles* fine without one.
- **`local-whisper`** (on `guss-voice`, default *at that crate's level*
  only - the `guss` binary opts in explicitly): needs a C/C++ toolchain +
  CMake to compile `whisper-rs-sys` (it builds whisper.cpp). Don't assume
  this is available in a sandboxed/CI-less environment - check first
  (`cmake --version`, `cc --version` or similar) before trying to build
  or test it.

CI (`.github/workflows/ci.yml`) runs the default build/test/fmt/clippy on
every push/PR, plus two dedicated jobs that have the Postgres service
container and the C++ toolchain respectively - that's where the
feature-gated paths actually get exercised end-to-end.

## Conventions that are load-bearing, not incidental

- **Money is `Money { minor_units: i64 }` plus a currency code, never a
  float.** (`crates/guss-core/src/money.rs`) Don't introduce a float
  anywhere in the money path, even temporarily - it's exactly the kind of
  change that looks harmless in a diff and causes rounding drift later.
- **Business logic depends on traits, never concrete types.** `Store`
  (`crates/guss-core/src/store.rs`), `Stt`/`Tts`
  (`crates/guss-voice/src/lib.rs`), `LlmProvider` (`guss-llm`). Channel
  adapters and `Orchestrator` are generic over these traits
  (`Orchestrator<P: LlmProvider, S: Store>`) - don't thread a concrete
  `SqliteStore`/`PostgresStore` type through new code; use the generic
  bound, or `AnyStore` (`crates/guss-store/src/any.rs`) at the one place
  (`apps/guss-app/src/main.rs`) that picks a concrete backend from config.
- **Storage dialects must stay behaviorally identical.** A schema or
  query change to `crates/guss-store/migrations/` needs the matching
  change in `crates/guss-store/migrations_postgres/`, and shared mapping
  logic belongs in `crates/guss-store/src/event_columns.rs`, not
  duplicated between `sqlite.rs` and `postgres.rs`.
- **Confirmation-before-commit, not validate-before-commit.** Every
  extracted event is saved immediately so the confirmation message shows
  real numbers; "no"/"undo" within a 5-minute window deletes it
  (`crates/guss-channels/src/orchestrator.rs`). Don't "fix" an extraction
  bug by adding a pre-commit confirmation step - that's a deliberate,
  already-settled tradeoff (see docs/ARCHITECTURE.md), not an oversight.
- **Bilingual is end-to-end, not docs-only.** User-facing strings (CLI
  output, report templates under `crates/guss-insights/templates/`,
  confirmation/undo wording) exist in both `en` and `id`. If you add a
  new user-facing message, add both.
- **Tests that need an external toolchain or service are `#[ignore]`d**
  and read their config from env vars (see `GUSS_TEST_POSTGRES_URL`,
  `GUSS_TEST_WHISPER_MODEL_PATH`/`GUSS_TEST_WHISPER_WAV_PATH`) - CI
  un-ignores them explicitly (`-- --ignored`) in the jobs that have the
  prerequisite set up. Don't make a test like this run by default; it'll
  break `cargo test` for anyone without that prerequisite.
- **No comments explaining *what* the code does** - identifiers should
  already make that obvious. A comment is only warranted for a
  non-obvious *why* (a workaround, an invariant, a documented upstream
  failure mode like whisper.cpp's hallucination behavior). This is the
  existing style throughout the codebase - match it.

## Before calling something done

Run the three commands at the top of this file. If you touched a
feature-gated path you can't build locally, say so explicitly rather
than claiming it's verified - this repo already had that exact honesty
gap (Postgres wiring, Whisper end-to-end verification) as its two
documented "not done yet" items, and it's worth not reintroducing a new
one silently.
