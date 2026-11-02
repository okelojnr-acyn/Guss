use crate::VoiceError;

/// Decodes the 16-bit mono PCM samples out of a WAV byte buffer (the
/// output of `convert::to_wav16k_mono`) into the f32 samples whisper.cpp's
/// `full()` expects. Scans for the `data` subchunk rather than assuming a
/// fixed header length, since some encoders insert extra chunks.
///
/// Deliberately has no dependency on whisper-rs, unlike the rest of the
/// Whisper integration in `stt.rs` - this lets the decoder (and its tests)
/// build and run even without a C/C++ toolchain/CMake available.
pub fn decode_wav16_mono_pcm(wav: &[u8]) -> Result<Vec<f32>, VoiceError> {
    if wav.len() < 12 || &wav[0..4] != b"RIFF" || &wav[8..12] != b"WAVE" {
        return Err(VoiceError::Transcription(
            "input is not a valid WAV file".to_string(),
        ));
    }

    let mut pos = 12;
    while pos + 8 <= wav.len() {
        let chunk_id = &wav[pos..pos + 4];
        let chunk_size = u32::from_le_bytes(wav[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let data_start = pos + 8;

        if chunk_id == b"data" {
            let data_end = (data_start + chunk_size).min(wav.len());
            let pcm = &wav[data_start..data_end];
            let samples = pcm
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| i16::from_le_bytes(*b) as f32 / 32768.0)
                .collect();
            return Ok(samples);
        }

        // Chunks are word-aligned; skip a padding byte for odd-sized chunks.
        pos = data_start + chunk_size + (chunk_size % 2);
    }

    Err(VoiceError::Transcription(
        "WAV file has no data chunk".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_wav(samples: &[i16]) -> Vec<u8> {
        let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVE");
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&16000u32.to_le_bytes());
        wav.extend_from_slice(&32000u32.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(data.len() as u32).to_le_bytes());
        wav.extend_from_slice(&data);
        wav
    }

    #[test]
    fn decodes_known_samples_to_normalized_f32() {
        let wav = make_wav(&[0, 16384, -16384, 32767, -32768]);
        let samples = decode_wav16_mono_pcm(&wav).unwrap();
        assert_eq!(samples.len(), 5);
        assert_eq!(samples[0], 0.0);
        assert!((samples[1] - 0.5).abs() < 1e-4);
        assert!((samples[2] + 0.5).abs() < 1e-4);
        assert_eq!(samples[4], -1.0);
    }

    #[test]
    fn rejects_non_wav_input() {
        let err = decode_wav16_mono_pcm(b"not a wav file at all").unwrap_err();
        assert!(matches!(err, VoiceError::Transcription(_)));
    }

    #[test]
    fn skips_extra_chunk_before_data() {
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&0u32.to_le_bytes()); // size placeholder, unchecked by decoder
        wav.extend_from_slice(b"WAVE");
        wav.extend_from_slice(b"LIST");
        wav.extend_from_slice(&4u32.to_le_bytes());
        wav.extend_from_slice(b"INFO");
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&4u32.to_le_bytes());
        wav.extend_from_slice(&1000i16.to_le_bytes());
        wav.extend_from_slice(&(-1000i16).to_le_bytes());

        let samples = decode_wav16_mono_pcm(&wav).unwrap();
        assert_eq!(samples.len(), 2);
    }
}
