use async_trait::async_trait;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use crate::wav::decode_wav16_mono_pcm;
use crate::{Stt, VoiceError};

/// Local, offline speech-to-text via whisper.cpp (through `whisper-rs`).
/// Requires a GGML/GGUF whisper model file downloaded separately (see
/// docs/setup-elevenlabs-piper.md) - no API key, no network call per
/// transcription.
///
/// Tuned against known whisper.cpp failure modes (hallucinated stock
/// phrases on silence/non-speech audio): greedy decoding at temperature 0
/// (no exploratory sampling), `logprob_thold`/`entropy_thold` tuned rather
/// than left at library defaults, and optional VAD pre-filtering when a
/// VAD model path is configured. `no_speech_thold` is set for forward
/// compatibility but is a documented no-op in whisper.cpp as of the
/// version whisper-rs 0.16 vendors - logprob/entropy thresholds are the
/// levers that actually work today.
pub struct WhisperStt {
    ctx: WhisperContext,
    /// ISO 639-1 code ("en"/"id") - conveniently the same codes Guss's
    /// business.language field already uses.
    language: String,
    vad_model_path: Option<String>,
}

impl WhisperStt {
    pub fn new(
        model_path: &str,
        language: impl Into<String>,
        vad_model_path: Option<String>,
    ) -> Result<Self, VoiceError> {
        let ctx = WhisperContext::new_with_params(model_path, WhisperContextParameters::default())
            .map_err(|e| {
                VoiceError::Transcription(format!(
                    "failed to load whisper model at \"{model_path}\": {e}"
                ))
            })?;

        if vad_model_path.is_none() {
            tracing::warn!(
                "no VAD model configured for WhisperStt - silence/non-speech hallucination mitigation \
                 is relying on logprob/entropy thresholds alone. See docs/setup-elevenlabs-piper.md to add one."
            );
        }

        Ok(Self {
            ctx,
            language: language.into(),
            vad_model_path,
        })
    }
}

#[async_trait]
impl Stt for WhisperStt {
    async fn transcribe(&self, wav16k_mono: &[u8]) -> Result<String, VoiceError> {
        let samples = decode_wav16_mono_pcm(wav16k_mono)?;

        let mut state = self.ctx.create_state().map_err(|e| {
            VoiceError::Transcription(format!("failed to create whisper state: {e}"))
        })?;

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_temperature(0.0);
        params.set_entropy_thold(2.4); // whisper.cpp's analogue of OpenAI's compression_ratio_threshold
        params.set_logprob_thold(-1.0);
        params.set_no_speech_thold(0.6); // currently a no-op upstream, set anyway for forward-compat
        params.set_language(Some(&self.language));
        params.set_translate(false);

        if let Some(vad_path) = &self.vad_model_path {
            params.enable_vad(true);
            params.set_vad_model_path(Some(vad_path));
        } else {
            params.enable_vad(false);
        }

        state
            .full(params, &samples)
            .map_err(|e| VoiceError::Transcription(format!("whisper transcription failed: {e}")))?;

        let mut text = String::new();
        for i in 0..state.full_n_segments() {
            if let Some(segment) = state.get_segment(i) {
                match segment.to_str_lossy() {
                    Ok(s) => {
                        if !text.is_empty() {
                            text.push(' ');
                        }
                        text.push_str(s.trim());
                    }
                    Err(e) => {
                        tracing::warn!(segment = i, error = %e, "skipping unreadable whisper segment")
                    }
                }
            }
        }

        Ok(text.trim().to_string())
    }
}
