pub mod convert;
pub mod tts;
pub mod wav;

#[cfg(feature = "local-whisper")]
pub mod stt;

use async_trait::async_trait;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum VoiceError {
    #[error("required external tool failed: {0}")]
    ExternalTool(String),
    #[error("HTTP request to TTS provider failed: {0}")]
    Http(String),
    #[error("transcription failed: {0}")]
    Transcription(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioFormat {
    Wav,
    Mp3,
}

#[derive(Debug, Clone)]
pub struct AudioClip {
    pub bytes: Vec<u8>,
    pub format: AudioFormat,
}

/// Speech-to-text: implementations consume 16kHz mono PCM WAV bytes
/// (convert incoming Opus/OGG voice notes with `convert::to_wav16k_mono`
/// first) and return the transcript.
#[async_trait]
pub trait Stt: Send + Sync {
    async fn transcribe(&self, wav16k_mono: &[u8]) -> Result<String, VoiceError>;
}

/// Text-to-speech: implementations synthesize spoken audio for a report
/// or confirmation string.
#[async_trait]
pub trait Tts: Send + Sync {
    async fn synthesize(&self, text: &str) -> Result<AudioClip, VoiceError>;
}
