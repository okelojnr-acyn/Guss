use guss_voice::Stt;

use crate::{Args, BenchError};

/// Builds the `Stt` backend named by `--engine` for one language group.
/// Called once per distinct `language` tag in the manifest (not once per
/// sample) - model loading is the expensive part, and the loaded model
/// doesn't depend on the language hint, only how a given call is decoded.
///
/// Only "whisper" exists today, on purpose - it's the baseline that
/// demonstrates the isiZulu/Sesotho gap this whole project is about. Add
/// a candidate (Lelapa, Azure, Google, a fine-tuned MMS checkpoint) by
/// matching its name here; nothing else in this crate needs to change,
/// since everything downstream only depends on the `Stt` trait.
pub fn build_stt(args: &Args, language: &str) -> Result<Box<dyn Stt>, BenchError> {
    match args.engine.as_str() {
        "whisper" => build_whisper(args, language),
        other => Err(BenchError::UnknownEngine(format!(
            "\"{other}\" - only \"whisper\" is implemented today. See crates/guss-sttbench/README.md \
             for how to wire in a new candidate (Lelapa, Azure, Google, a fine-tuned MMS checkpoint)."
        ))),
    }
}

#[cfg(feature = "local-whisper")]
fn build_whisper(args: &Args, language: &str) -> Result<Box<dyn Stt>, BenchError> {
    let model_path = args.whisper_model_path.as_ref().ok_or_else(|| {
        BenchError::MissingConfig(
            "--whisper-model-path is required for --engine whisper".to_string(),
        )
    })?;
    let vad_path = args
        .whisper_vad_model_path
        .as_ref()
        .map(|p| p.to_string_lossy().to_string());

    let stt = guss_voice::stt::WhisperStt::new(&model_path.to_string_lossy(), language, vad_path)
        .map_err(|e| {
        BenchError::EngineInit(format!(
            "failed to load Whisper model for language \"{language}\": {e}"
        ))
    })?;
    Ok(Box::new(stt))
}

#[cfg(not(feature = "local-whisper"))]
fn build_whisper(_args: &Args, _language: &str) -> Result<Box<dyn Stt>, BenchError> {
    Err(BenchError::EngineInit(
        "guss-sttbench was built without the \"local-whisper\" feature - rebuild with \
         --features local-whisper (needs a C/C++ toolchain + CMake, see docs/setup-elevenlabs-piper.md)"
            .to_string(),
    ))
}
