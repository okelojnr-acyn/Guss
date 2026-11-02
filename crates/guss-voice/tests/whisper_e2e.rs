#![cfg(feature = "local-whisper")]

use std::fs;

use guss_voice::Stt;
use guss_voice::convert::to_wav16k_mono;
use guss_voice::stt::WhisperStt;

/// Verifies the real local-Whisper pipeline end-to-end against a real
/// speech sample - not a compile check, not a tone/silence fixture.
/// Needs two things CI's `whisper-e2e` job (.github/workflows/ci.yml)
/// sets up and this sandbox/a plain dev machine won't have by default:
/// a downloaded GGML Whisper model, and `ffmpeg`/the audio file itself.
/// `#[ignore]`d so a plain `cargo test` never needs either.
#[tokio::test]
#[ignore]
async fn whisper_transcribes_real_speech() {
    let model_path = std::env::var("GUSS_TEST_WHISPER_MODEL_PATH").expect(
        "GUSS_TEST_WHISPER_MODEL_PATH must point at a GGML Whisper model - \
         see docs/setup-elevenlabs-piper.md, or run this in CI (whisper-e2e job).",
    );
    let wav_path = std::env::var("GUSS_TEST_WHISPER_WAV_PATH").expect(
        "GUSS_TEST_WHISPER_WAV_PATH must point at a speech sample (any format ffmpeg reads) - \
         CI's whisper-e2e job synthesizes one with espeak-ng.",
    );

    let raw_audio =
        fs::read(&wav_path).unwrap_or_else(|e| panic!("failed to read {wav_path}: {e}"));

    // The same conversion real WhatsApp/Telegram voice notes go through
    // (crates/guss-voice/src/convert.rs), not a hand-rolled test-only path.
    let wav16k_mono = to_wav16k_mono(&raw_audio)
        .await
        .expect("ffmpeg conversion to 16kHz mono WAV failed");

    let stt = WhisperStt::new(&model_path, "en", None)
        .unwrap_or_else(|e| panic!("failed to load Whisper model at {model_path}: {e}"));

    let transcript = stt
        .transcribe(&wav16k_mono)
        .await
        .expect("transcription failed");
    let lower = transcript.to_lowercase();

    assert!(
        lower.contains("fox") && lower.contains("dog"),
        "expected the transcript of \"the quick brown fox jumps over the lazy dog\" to contain \
         \"fox\" and \"dog\", got: {transcript:?}"
    );
}
