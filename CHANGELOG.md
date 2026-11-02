# Changelog

All notable changes to this project are documented in this file. Format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added

- `PostgresStore`, implementing the same `Store` trait as `SqliteStore`,
  behind the `postgres` Cargo feature. Storage backend is now selected at
  runtime from `GUSS_STORE__URL`'s scheme (`sqlite:` vs
  `postgres://`/`postgresql://`) via a new `AnyStore` dispatcher.
- End-to-end CI verification of local Whisper speech-to-text against a
  real synthesized speech sample, closing out the "hasn't been verified
  end-to-end" caveat in the Status section.
- `.github/workflows/ci.yml`: fmt/clippy/test on every push and PR, plus
  dedicated jobs exercising the Postgres and local-Whisper feature-gated
  paths.
- Repository governance: `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`,
  `SECURITY.md`, issue/PR templates, `AGENTS.md`/`CLAUDE.md`.
- Mermaid diagrams in `docs/ARCHITECTURE.md` (crate dependency graph,
  inbound-message sequence, storage dialect dispatch, voice pipeline).

### Changed

- Applied `cargo fmt` across the workspace and added a format check to
  CI (previously unformatted; no behavior change).

## [0.1.0] - development history prior to this changelog

- Scaffolded the Guss workspace: domain model, SQLite store, config
  loading.
- Added the LLM structured-output provider and NLU extraction pipeline.
- Added the insights aggregation engine and bilingual (en/id) report
  rendering.
- Wired the CLI channel end-to-end with confirmation-before-commit and
  undo.
- Wired voice (STT/TTS) into the CLI channel, feature-gated for local
  Whisper.
- Added the Telegram channel with webhook-redelivery dedup and bounded
  concurrency.
- Added the WhatsApp Cloud API channel with webhook signature
  verification.
- Added the scheduler for proactive EOD/weekly/monthly report pushes.
- Added the `guss init` wizard, Docker/Compose setup, and full bilingual
  documentation.
