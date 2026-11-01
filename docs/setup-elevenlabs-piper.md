# Setting up voice (text-to-speech and speech-to-text)

Guss's voice features are entirely optional and independent of each
other - you can run text-only, TTS-only, STT-only, or both. Nothing
below is required to use Guss.

## Text-to-speech (TTS)

### Option A: Piper (free, local, default)

[Piper](https://github.com/rhasspy/piper) is a fast, free, local TTS
engine. Guss shells out to the `piper` CLI binary rather than using a
Rust binding, since no mature pure-Rust Piper binding exists yet.

1. Download a release binary for your OS from
   [github.com/rhasspy/piper/releases](https://github.com/rhasspy/piper/releases).
2. Put it on your `PATH`, or note its full path.
3. Download a voice model (an `.onnx` file plus its matching `.onnx.json`)
   from [the Piper voices list](https://github.com/rhasspy/piper/blob/master/VOICES.md) -
   pick one in your business's language (English or Indonesian voices
   both exist).

In your `.env`:

```
GUSS_TTS__PROVIDER=piper
GUSS_TTS__PIPER_BINARY=piper
GUSS_TTS__PIPER_VOICE_MODEL_PATH=models/voice.onnx
```

(`PIPER_BINARY` can be a bare name if it's on `PATH`, or a full path
otherwise.)

### Option B: ElevenLabs (higher quality, needs a key, paid)

In your `.env`:

```
GUSS_TTS__PROVIDER=elevenlabs
GUSS_TTS__ELEVENLABS_API_KEY=your-key
GUSS_TTS__ELEVENLABS_VOICE_ID=your-chosen-voice-id
```

Get a key and pick a voice ID at [elevenlabs.io](https://elevenlabs.io/app).
**If this key expires or you cancel**, just switch `GUSS_TTS__PROVIDER`
back to `piper` - no code changes needed, both implement the same `Tts`
trait.

## Speech-to-text (STT) - for voice note input

Local Whisper is the only STT option today, and it's **not on by
default** - it needs a C/C++ toolchain to build, which plain-text and
TTS-only setups don't require.

### Why a toolchain is needed

STT is powered by [`whisper-rs`](https://github.com/tazz4843/whisper-rs),
which wraps whisper.cpp (C++) and compiles it via CMake as part of the
Rust build. This is true for any whisper.cpp binding in any language -
it's not something Guss can avoid, since whisper.cpp itself is C++.

### 1. Install a C/C++ toolchain + CMake

- **Windows**: install
  [Visual Studio Build Tools](https://visualstudio.microsoft.com/downloads/#build-tools-for-visual-studio-)
  (the "Desktop development with C++" workload) and
  [CMake](https://cmake.org/download/).
- **macOS**: `xcode-select --install`, then `brew install cmake`.
- **Linux**: `sudo apt install build-essential cmake` (Debian/Ubuntu) or
  your distro's equivalent.

### 2. Download a Whisper model

```bash
mkdir -p models
curl -L -o models/ggml-base.en.bin \
  https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin
```

For Indonesian, use a multilingual model instead (not an `.en.bin` one) -
see the [whisper.cpp model list](https://github.com/ggml-org/whisper.cpp/blob/master/models/README.md)
for options. Bigger models (`small`, `medium`) transcribe more
accurately at the cost of slower, heavier inference - `base` is a
reasonable default to start with.

### 3. (Recommended) Download a VAD model

Voice activity detection reduces Whisper's well-documented tendency to
hallucinate stock phrases ("thanks for watching...") during silence or
non-speech audio. Download a Silero VAD model per
[whisper.cpp's VAD documentation](https://github.com/ggml-org/whisper.cpp/blob/master/examples/addon.node/README.md)
and point `GUSS_STT__WHISPER_VAD_MODEL_PATH` at it. Skipping this still
works - Guss also sets conservative decoding parameters (temperature 0,
tuned log-probability thresholds) as a second line of defense - but VAD
is the more reliable fix.

### 4. Build with the feature enabled

```bash
cargo build -p guss --features local-whisper
```

### 5. Configure Guss

```
GUSS_STT__WHISPER_MODEL_PATH=models/ggml-base.en.bin
GUSS_STT__WHISPER_VAD_MODEL_PATH=models/silero_vad.onnx
```

## Audio format conversion (ffmpeg)

WhatsApp and Telegram voice notes arrive as Opus-in-OGG, which Guss
converts to the 16kHz mono WAV Whisper needs via `ffmpeg`, rather than a
pure-Rust decoder (Symphonia's Opus support isn't production-ready as
of this writing). Install `ffmpeg` and make sure it's on `PATH`:

- **Windows**: `winget install ffmpeg` or download from
  [ffmpeg.org](https://ffmpeg.org/download.html).
- **macOS**: `brew install ffmpeg`.
- **Linux**: `sudo apt install ffmpeg` or your distro's equivalent.

This is required for voice *input* on any channel (CLI's `voice <path>`
command, Telegram voice notes, WhatsApp voice notes) regardless of which
STT backend you use.
