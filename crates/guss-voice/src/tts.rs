use std::process::Stdio;

use async_trait::async_trait;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::{AudioClip, AudioFormat, Tts, VoiceError};

/// ElevenLabs TTS. Uses the caller's API key, which is expected to expire
/// eventually - swap for `PiperTts` (or any other `Tts` impl) with zero
/// changes to calling code when that happens.
pub struct ElevenLabsTts {
    api_key: String,
    voice_id: String,
    client: reqwest::Client,
}

impl ElevenLabsTts {
    pub fn new(api_key: impl Into<String>, voice_id: impl Into<String>) -> Self {
        Self { api_key: api_key.into(), voice_id: voice_id.into(), client: reqwest::Client::new() }
    }
}

#[async_trait]
impl Tts for ElevenLabsTts {
    async fn synthesize(&self, text: &str) -> Result<AudioClip, VoiceError> {
        let url = format!("https://api.elevenlabs.io/v1/text-to-speech/{}", self.voice_id);

        let response = self
            .client
            .post(&url)
            .header("xi-api-key", &self.api_key)
            .json(&serde_json::json!({
                "text": text,
                "model_id": "eleven_multilingual_v2",
            }))
            .send()
            .await
            .map_err(|e| VoiceError::Http(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(VoiceError::Http(format!("ElevenLabs returned HTTP {status}: {body}")));
        }

        let bytes = response.bytes().await.map_err(|e| VoiceError::Http(e.to_string()))?;
        Ok(AudioClip { bytes: bytes.to_vec(), format: AudioFormat::Mp3 })
    }
}

/// Local, free TTS via the official `piper` CLI binary (shelled out to,
/// not an FFI binding - no mature pure-Rust Piper binding exists yet).
/// This is the fallback so Guss works end-to-end with zero paid keys -
/// see docs/setup-elevenlabs-piper.md for installing the `piper` binary
/// and a voice model.
pub struct PiperTts {
    piper_binary: String,
    voice_model_path: String,
}

impl PiperTts {
    pub fn new(piper_binary: impl Into<String>, voice_model_path: impl Into<String>) -> Self {
        Self { piper_binary: piper_binary.into(), voice_model_path: voice_model_path.into() }
    }
}

#[async_trait]
impl Tts for PiperTts {
    async fn synthesize(&self, text: &str) -> Result<AudioClip, VoiceError> {
        let mut child = Command::new(&self.piper_binary)
            .args(["--model", &self.voice_model_path, "--output-raw"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                VoiceError::ExternalTool(format!(
                    "failed to start piper at \"{}\" (see docs/setup-elevenlabs-piper.md): {e}",
                    self.piper_binary
                ))
            })?;

        let mut stdin = child.stdin.take().expect("stdin was piped");
        let text_owned = text.to_string();
        let write_task = tokio::spawn(async move {
            let _ = stdin.write_all(text_owned.as_bytes()).await;
        });

        let output = child
            .wait_with_output()
            .await
            .map_err(|e| VoiceError::ExternalTool(format!("piper I/O error: {e}")))?;
        let _ = write_task.await;

        if !output.status.success() {
            return Err(VoiceError::ExternalTool(format!(
                "piper exited with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        // --output-raw gives headerless 16-bit PCM at the voice model's
        // sample rate; wrap it in a minimal WAV header so downstream
        // consumers (channels sending a voice note) get a standard file.
        let wav_bytes = wrap_pcm_as_wav(&output.stdout, 22050);
        Ok(AudioClip { bytes: wav_bytes, format: AudioFormat::Wav })
    }
}

fn wrap_pcm_as_wav(pcm: &[u8], sample_rate: u32) -> Vec<u8> {
    let data_len = pcm.len() as u32;
    let byte_rate = sample_rate * 2; // 16-bit mono
    let mut wav = Vec::with_capacity(44 + pcm.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes()); // block align
    wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend_from_slice(pcm);
    wav
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_wrapper_produces_valid_header_lengths() {
        let pcm = vec![0u8; 100];
        let wav = wrap_pcm_as_wav(&pcm, 22050);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(wav.len(), 44 + 100);
    }
}
