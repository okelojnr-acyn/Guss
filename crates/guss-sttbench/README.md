# guss-sttbench

Evaluates speech-to-text + extraction accuracy against a manifest of
labelled voice notes. Built for the FinFex South-African-language proof
of concept (isiZulu / Sesotho / Afrikaans / English, including
code-switched speech), but the tool itself is engine-agnostic and not
specific to that project - it's a reusable way to answer "how accurate
is engine X on my real audio" for any `guss-voice::Stt` implementation.

It runs the **real pipeline**, not a shortcut: `ffmpeg` conversion
(`guss_voice::convert::to_wav16k_mono`, the same code path real
WhatsApp/Telegram voice notes go through) → the chosen `Stt` backend →
`guss-nlu`'s LLM-based extraction → a structured amount/category. What
it measures is whether that end result is correct, not just whether the
transcript looks right.

## Why task-level accuracy, not just Word Error Rate

A transcript can be imperfect while the extracted ledger entry is still
right (and vice versa). This tool's headline metric is **task-level
accuracy**: did the pipeline output the correct amount, currency, *and*
category - a right amount in the wrong currency (e.g. defaulting to USD
instead of ZAR) is still a wrong ledger entry. Transcription Word Error
Rate is reported too, but only as a
secondary, diagnostic number - it explains *why* a task-level failure
happened, it isn't the thing to optimize in isolation.

Every metric is reported **per language, and for the code-switched
subset separately** - never a single blended average, since an engine
that's 95% accurate overall but fails specifically on code-switched
audio is not production-ready, and a blended number would hide exactly
that.

## Usage

```bash
cargo run -p guss-sttbench --features local-whisper -- \
  --manifest samples/manifest.jsonl \
  --engine whisper \
  --whisper-model-path models/ggml-base.en.bin \
  --llm-base-url http://localhost:11434/v1 \
  --llm-model llama3.1 \
  --json-output report.json
```

`--engine whisper` needs the `local-whisper` Cargo feature (a C/C++
toolchain + CMake to build whisper.cpp - see
`docs/setup-elevenlabs-piper.md` in the repo root). Without that
feature, `--engine whisper` fails at runtime with a clear message rather
than being unavailable at compile time.

## Manifest format

JSON Lines - one JSON object per non-empty line. Lines starting with
`//` are treated as comments and skipped, so a manifest can carry notes
inline.

```jsonl
// isiZulu, no code-switching
{"path": "audio/001.wav", "language": "zu", "expected_amount": 150.0, "expected_currency": "ZAR", "expected_category": "stock"}
// Afrikaans/English code-switched, with a human-verified reference transcript for WER
{"path": "audio/002.wav", "language": "af", "code_switched": true, "expected_amount": 80.0, "expected_currency": "ZAR", "expected_category": "bills", "reference_transcript": "ek het 80 rand betaal for bills today"}
```

| Field                  | Required | Meaning |
|-------------------------|----------|---------|
| `path`                  | yes      | Audio file path, resolved relative to the manifest file's own directory unless absolute. Any format `ffmpeg` reads. |
| `language`               | yes      | Free-text tag (`zu`, `st`, `af`, `en`, ...) - not validated against any engine's supported-language list, since that's exactly what this tool measures. |
| `code_switched`          | no (default `false`) | Scored as its own subset regardless of the `language` tag. |
| `expected_amount`        | yes      | Ground truth, major units (e.g. rand, not cents). |
| `expected_currency`      | yes      | ISO currency code. |
| `expected_category`      | yes      | Ground truth category, matched case-insensitively. |
| `reference_transcript`   | no       | Human-verified transcript - enables the WER diagnostic metric for that sample. Not required for task-level accuracy. |

**Ground truth should be prepared independently of whoever runs this
tool**, ideally by the data owner (FinFex) rather than whoever's being
evaluated - that's what makes the resulting accuracy number credible
rather than self-graded.

## Adding a new engine

Only `whisper` is implemented today - deliberately, since it's the
baseline that demonstrates the isiZulu/Sesotho gap (Whisper's training
data doesn't cover either language; Afrikaans and English are covered).
To add a candidate (Lelapa AI's Vulavula, Azure/Google Cloud Speech, a
fine-tuned Meta MMS checkpoint, ...):

1. Implement `guss_voice::Stt` for it (in `guss-voice`, or a new crate if
   it needs its own dependencies/API client).
2. Add a match arm in `src/engine.rs`'s `build_stt` for its `--engine`
   name.

Nothing else in this crate changes - `main.rs`, `score.rs`, and
`report.rs` only depend on the `Stt` trait.

## Output

Prints a per-sample line as it runs, then a summary: overall stats, a
per-language breakdown, the code-switched subset, and a category
confusion matrix (`expected -> got: count`, with pipeline failures
shown as `<none>`). Pass `--json-output <path>` to also write the full
`AccuracyReport` as JSON for further analysis.
