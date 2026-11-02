# Contributing to Guss

Thanks for considering it. Guss is a small Cargo workspace, not a huge
project, so the bar here is mostly: keep crates independently testable,
keep the storage/voice abstractions real (not leaky), and don't silently
drop the bilingual (English/Indonesian) behavior.

## Dev setup

You need a Rust toolchain (stable). That's it for the default build:

```bash
git clone https://github.com/okelojnr-acyn/Guss.git
cd Guss
cargo test --workspace --no-default-features
```

Two things are feature-gated and need extra local setup - skip them
unless you're specifically working on that area:

- **`--features postgres`** (on `guss-store`/`guss`) needs a running
  Postgres instance for its tests. Point `GUSS_TEST_POSTGRES_URL` at it
  and run `cargo test -p guss-store --features postgres -- --ignored`.
  Easiest: `docker run -e POSTGRES_PASSWORD=guss -e POSTGRES_USER=guss -e POSTGRES_DB=guss -p 5432:5432 postgres:16-alpine`.
- **`--features local-whisper`** needs a C/C++ toolchain + CMake (it
  compiles whisper.cpp via `whisper-rs-sys`) - see
  [docs/setup-elevenlabs-piper.md](docs/setup-elevenlabs-piper.md).

Both are also exercised in CI (`.github/workflows/ci.yml`) against a
real Postgres service container and a real synthesized speech sample,
respectively, so you don't strictly need either locally to contribute -
CI will tell you if something's broken.

## Project layout

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the full crate
dependency graph and design rationale. Short version: each crate has one
job (domain model, storage, LLM extraction, insights, voice, channels,
scheduling), business logic depends only on the `Store`/`Stt`/`Tts`
*traits* from `guss-core`/`guss-voice`, never on a concrete backend.

## Before opening a PR

```bash
cargo fmt --all -- --check
cargo clippy --workspace --no-default-features -- -D warnings
cargo test --workspace --no-default-features
```

These three are also what CI runs on every PR. If you touched storage,
run the same change past both `SqliteStore` and `PostgresStore` - see
`crates/guss-store/src/event_columns.rs` for the dialect-shared mapping
logic both implementations use, so a schema/behavior change usually
needs updating in one place plus both `migrations/` and
`migrations_postgres/`.

If you touched anything user-facing (CLI output, report templates,
confirmation messages), make sure the Indonesian (`id`) copy stays in
sync with the English one - Guss's bilingual behavior is a feature, not
an afterthought.

Use the PR template's checklist; it mirrors the above.

## Reporting bugs / requesting features

Use the issue templates. For security issues, see
[SECURITY.md](SECURITY.md) instead - please don't open a public issue.
