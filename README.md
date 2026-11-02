# Guss

[![CI](https://github.com/okelojnr-acyn/Guss/actions/workflows/ci.yml/badge.svg)](https://github.com/okelojnr-acyn/Guss/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-orange.svg)](https://www.rust-lang.org)

A self-hosted business co-pilot you talk to, not a dashboard you check.

Tell Guss what happened in your shop today - by typing or by voice, over
CLI, Telegram, or WhatsApp - and it logs sales, expenses, and stock
changes. Ask for a report, or let it push one to you automatically at the
end of the day, every week, and every month: what sold, what's running
low, what's trending up or down, profit/loss, and a simple recurring-revenue
view.

Read this in [Bahasa Indonesia](README.id.md).

## Why this exists

Most small business "AI" tools are either a chatbot bolted onto a
spreadsheet, or a SaaS dashboard that locks your data behind a
subscription. Guss is neither: it's a Rust binary you run yourself, against
API keys and credentials *you* provide, with your data in a SQLite file on
your own disk. Nothing phones home. Nothing is hosted for you. Clone it,
answer a few setup questions, and it's yours.

## What it actually does

- **Logs business events from natural language.** "Sold 3 bags of rice at
  500 each" becomes a structured sale record - item, quantity, price,
  total - not a vague note. Works the same for expenses, restocks, and
  free-form notes you just want remembered.
- **Confirms before it commits.** Every logged entry is echoed back
  immediately, and you have 5 minutes to reply "no" (or "tidak") to undo
  it. An LLM extracting numbers from speech will occasionally get it
  wrong - Guss is built so a mistake is always catchable, not silently
  baked into your books.
- **Reports on demand or on a schedule.** Ask "report" (or "report week",
  "report month") any time, or configure Guss to push one automatically
  at the end of the day, every Monday, and on the 1st of the month -
  revenue, expenses, best/worst sellers, low stock, and a simple
  recurring-revenue projection for subscription-style income.
- **Talks back.** Replies can be spoken, not just printed - locally via
  the free [Piper](https://github.com/rhasspy/piper) TTS engine, or
  through ElevenLabs if you have a key.
- **Works in English or Bahasa Indonesia**, end to end - prompts, replies,
  and reports, not just the docs.

## Pick your channel

| Channel | Setup effort | Notes |
|---|---|---|
| **CLI** | None | Type in a terminal. Best way to try Guss out first. |
| **Telegram** | ~2 minutes | Free bot token, supports text and voice notes. |
| **WhatsApp** | ~30 minutes | Official Meta Cloud API - the ToS-compliant path, not an unofficial bridge that risks your number getting banned. |

## Pick your LLM

One provider implementation talks to all three - just change a URL:

| Provider | Cost | Setup |
|---|---|---|
| **[Ollama](docs/setup-ollama.md)** | Free, runs on your machine | No API key, fully offline |
| **[Groq](docs/setup-groq.md)** | Free tier, very fast | Needs a free API key |
| **OpenAI** | Paid | Needs an API key |

## Quickstart

```bash
git clone https://github.com/okelojnr-acyn/Guss.git
cd Guss
cargo run -p guss -- init > .env   # answer a few questions
cargo run -p guss                   # starts the CLI channel by default
```

Then just type what happened:

```
> sold 3 bags of rice at 500 each
Sold: 3 rice at 500.00 USD = 1500.00 USD

Reply "no" within 5 minutes to undo.
> report
Here is your report for My Shop, covering the last 24 hours.

Revenue was 1500.00 USD and expenses were 0.00 USD, for a net of 1500.00 USD.
Best sellers by quantity: rice (3 sold).
```

Want Telegram or WhatsApp instead of the CLI? See
[docs/setup-telegram.md](docs/setup-telegram.md) or
[docs/setup-whatsapp-cloud-api.md](docs/setup-whatsapp-cloud-api.md), then
set `GUSS_CHANNEL__KIND` accordingly in your `.env`.

Want voice? See [docs/setup-elevenlabs-piper.md](docs/setup-elevenlabs-piper.md)
for free local text-to-speech (and speech-to-text, which needs a C/C++
toolchain to build - see that doc for why and how).

### Docker

```bash
docker compose up --build
```

Add `--profile ollama` to also run a local Ollama instance in the same
compose stack, or `--profile postgres` to run Postgres instead of the
default SQLite file (build with `GUSS_FEATURES=postgres` and set
`GUSS_STORE__URL` accordingly - see
[docker-compose.yml](docker-compose.yml) and
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)).

## How it's built

Guss is a Cargo workspace of focused crates - domain model, storage,
LLM extraction, insights/reporting, voice, channels, scheduling - each
independently tested. See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)
for the full design, including the reliability choices (confirmation-
before-commit, webhook dedup, bounded concurrency, structured-output
extraction) that came out of researching how similar systems actually
fail in production.

## Status

This is a real, working Phase 1: every channel, the LLM extraction
pipeline, insights/reporting, the scheduler, and both storage backends
are implemented and covered by automated tests (run `cargo test
--workspace --no-default-features` for the default sqlite-only suite;
CI also runs the Postgres and Whisper suites below).

- **Postgres storage** is wired in behind `--features postgres`
  (`PostgresStore`, selected at runtime from `GUSS_STORE__URL`'s scheme -
  see [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)), with its own test
  suite running in CI against a real Postgres service container.
- **Local Whisper speech-to-text** is feature-gated behind
  `local-whisper` (needs a C/C++ toolchain + CMake to build - see
  [docs/setup-elevenlabs-piper.md](docs/setup-elevenlabs-piper.md)) and
  is verified end-to-end in CI against a real synthesized speech sample
  (see the `whisper-e2e` job in
  [.github/workflows/ci.yml](.github/workflows/ci.yml)).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for dev setup and the checks CI
runs on every PR, and [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) for how
we expect people to treat each other here. Found a security issue? See
[SECURITY.md](SECURITY.md) instead of opening a public issue.

## License

[MIT](LICENSE)
