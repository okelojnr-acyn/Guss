use std::process::Stdio;

use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::VoiceError;

/// Converts arbitrary input audio (WhatsApp/Telegram voice notes are
/// typically Opus-in-OGG) to 16kHz mono PCM WAV via `ffmpeg`, which Whisper
/// needs. FFmpeg auto-detects the input container/codec, so no format hint
/// is required. Requires the `ffmpeg` binary on PATH - see
/// docs/setup-elevenlabs-piper.md for install instructions; this is a
/// deliberate choice over a pure-Rust Opus decoder, since Symphonia's Opus
/// support isn't production-ready yet.
pub async fn to_wav16k_mono(input: &[u8]) -> Result<Vec<u8>, VoiceError> {
    let mut child = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            "pipe:0",
            "-f",
            "wav",
            "-ar",
            "16000",
            "-ac",
            "1",
            "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            VoiceError::ExternalTool(format!(
                "failed to start ffmpeg (is it installed and on PATH? see docs/setup-elevenlabs-piper.md): {e}"
            ))
        })?;

    let mut stdin = child.stdin.take().expect("stdin was piped");
    let input_owned = input.to_vec();
    let write_task = tokio::spawn(async move {
        let _ = stdin.write_all(&input_owned).await;
    });

    let output = child
        .wait_with_output()
        .await
        .map_err(|e| VoiceError::ExternalTool(format!("ffmpeg I/O error: {e}")))?;
    let _ = write_task.await;

    if !output.status.success() {
        return Err(VoiceError::ExternalTool(format!(
            "ffmpeg exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    Ok(output.stdout)
}
