mod engine;
mod manifest;
mod report;
mod score;

use std::collections::BTreeMap;
use std::path::PathBuf;

use clap::Parser;
use guss_core::domain::{BusinessEvent, SourceChannel};
use guss_llm::OpenAiCompatibleProvider;
use guss_nlu::Nlu;
use guss_voice::Stt;
use thiserror::Error;
use uuid::Uuid;

use manifest::Sample;
use score::SampleResult;

/// Evaluates speech-to-text + extraction accuracy against a manifest of
/// labelled voice notes - built for the FinFex South-African-language
/// proof of concept, but engine-agnostic: see crates/guss-sttbench/README.md
/// for the manifest format and how the accuracy numbers are computed.
#[derive(Parser, Debug)]
#[command(name = "guss-sttbench")]
struct Args {
    /// Path to a JSON Lines manifest - see README.md for the format.
    #[arg(long)]
    manifest: PathBuf,

    /// Which STT engine to benchmark. Only "whisper" is implemented
    /// today.
    #[arg(long, default_value = "whisper")]
    engine: String,

    /// GGML/GGUF Whisper model path - required when --engine whisper.
    #[arg(long)]
    whisper_model_path: Option<PathBuf>,

    /// Optional Silero VAD model path, passed through to WhisperStt.
    #[arg(long)]
    whisper_vad_model_path: Option<PathBuf>,

    /// OpenAI-compatible endpoint for the NLU extraction step (Ollama,
    /// Groq, or OpenAI all work, same as the main `guss` binary).
    #[arg(long, default_value = "http://localhost:11434/v1")]
    llm_base_url: String,

    #[arg(long)]
    llm_api_key: Option<String>,

    #[arg(long, default_value = "llama3.1")]
    llm_model: String,

    /// Currency assumed when a sample doesn't let the extractor infer
    /// one on its own.
    #[arg(long, default_value = "ZAR")]
    default_currency: String,

    /// How close the extracted amount must be to `expected_amount`
    /// (in major units) to count as correct.
    #[arg(long, default_value_t = 0.5)]
    amount_tolerance: f64,

    /// Optional path to also write the full report as JSON.
    #[arg(long)]
    json_output: Option<PathBuf>,
}

#[derive(Debug, Error)]
enum BenchError {
    #[error("failed to load manifest: {0}")]
    Manifest(#[from] manifest::ManifestError),
    #[error("unknown --engine {0}")]
    UnknownEngine(String),
    #[error("missing configuration: {0}")]
    #[cfg_attr(not(feature = "local-whisper"), allow(dead_code))]
    MissingConfig(String),
    #[error("failed to initialize engine: {0}")]
    EngineInit(String),
    #[error("failed to write JSON report to {path}: {source}")]
    WriteReport {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    if let Err(e) = run(args).await {
        eprintln!("guss-sttbench failed: {e}");
        std::process::exit(1);
    }
}

async fn run(args: Args) -> Result<(), BenchError> {
    let samples = manifest::load_manifest(&args.manifest)?;
    if samples.is_empty() {
        eprintln!("manifest at {} has no samples", args.manifest.display());
        return Ok(());
    }

    let provider = OpenAiCompatibleProvider::new(
        args.llm_base_url.clone(),
        args.llm_api_key.clone(),
        args.llm_model.clone(),
    );
    // The extraction prompt itself stays in English regardless of the
    // sample's spoken language - the LLM reads whatever language the
    // transcript comes out in. Localized SA-language prompts (the same
    // kind of addition guss-nlu already has for "en"/"id") are future
    // work once a transcription engine for isiZulu/Sesotho is chosen -
    // see README.md.
    let nlu = Nlu::new(provider, "en");

    let mut by_language: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, sample) in samples.iter().enumerate() {
        by_language
            .entry(sample.language.clone())
            .or_default()
            .push(i);
    }

    let mut results: Vec<Option<SampleResult>> = (0..samples.len()).map(|_| None).collect();

    for (language, indices) in &by_language {
        let stt = engine::build_stt(&args, language)?;
        for &i in indices {
            let sample = &samples[i];
            let result = run_one(stt.as_ref(), &nlu, &args, sample).await;
            println!(
                "{:<28} {:<6} {}",
                sample.path,
                sample.language,
                summarize(&result)
            );
            results[i] = Some(result);
        }
    }

    let results: Vec<SampleResult> = results
        .into_iter()
        .map(|r| r.expect("every sample was processed"))
        .collect();
    let accuracy_report = score::build_report(&results, args.amount_tolerance);

    println!();
    println!("{}", report::render_text(&accuracy_report));

    if let Some(json_path) = &args.json_output {
        let json = serde_json::to_string_pretty(&accuracy_report)
            .expect("AccuracyReport always serializes");
        std::fs::write(json_path, json).map_err(|source| BenchError::WriteReport {
            path: json_path.clone(),
            source,
        })?;
        println!("Wrote JSON report to {}", json_path.display());
    }

    Ok(())
}

async fn run_one<P: guss_llm::LlmProvider>(
    stt: &dyn Stt,
    nlu: &Nlu<P>,
    args: &Args,
    sample: &Sample,
) -> SampleResult {
    let fail = |error: String| SampleResult {
        sample: sample.clone(),
        transcript: None,
        extracted_amount: None,
        extracted_currency: None,
        extracted_category: None,
        error: Some(error),
    };

    let audio_path = manifest::resolve_audio_path(&args.manifest, sample);
    let raw_audio = match std::fs::read(&audio_path) {
        Ok(bytes) => bytes,
        Err(e) => return fail(format!("failed to read {}: {e}", audio_path.display())),
    };

    // The same conversion real WhatsApp/Telegram voice notes go through
    // (crates/guss-voice/src/convert.rs) - not a bench-only shortcut.
    let wav16k_mono = match guss_voice::convert::to_wav16k_mono(&raw_audio).await {
        Ok(w) => w,
        Err(e) => return fail(format!("ffmpeg conversion failed: {e}")),
    };

    let transcript = match stt.transcribe(&wav16k_mono).await {
        Ok(t) => t,
        Err(e) => return fail(format!("transcription failed: {e}")),
    };

    let events = match nlu
        .extract(
            Uuid::new_v4(),
            SourceChannel::Cli,
            &transcript,
            &args.default_currency,
            &[],
        )
        .await
    {
        Ok(events) => events,
        Err(e) => {
            return SampleResult {
                sample: sample.clone(),
                transcript: Some(transcript),
                extracted_amount: None,
                extracted_currency: None,
                extracted_category: None,
                error: Some(format!("NLU extraction failed: {e}")),
            };
        }
    };

    let (amount, currency, category) = first_amount_and_category(&events);
    let error = if events.is_empty() {
        Some("NLU extracted no events from the transcript".to_string())
    } else {
        None
    };

    SampleResult {
        sample: sample.clone(),
        transcript: Some(transcript),
        extracted_amount: amount,
        extracted_currency: currency,
        extracted_category: category,
        error,
    }
}

/// A single voice note is expected to describe one transaction, so the
/// first extracted event is what gets scored - matches how FinFex's own
/// deck frames this (one note, one structured entry).
fn first_amount_and_category(
    events: &[BusinessEvent],
) -> (Option<f64>, Option<String>, Option<String>) {
    match events.first() {
        Some(BusinessEvent::Sale(s)) => {
            let total = s.total();
            (
                Some(total.major_units()),
                Some(total.currency),
                s.category.clone(),
            )
        }
        Some(BusinessEvent::Expense(e)) => (
            Some(e.amount.major_units()),
            Some(e.amount.currency.clone()),
            e.category.clone(),
        ),
        Some(BusinessEvent::StockUpdate(_)) | Some(BusinessEvent::Note(_)) | None => {
            (None, None, None)
        }
    }
}

fn summarize(result: &SampleResult) -> String {
    match &result.error {
        Some(e) => format!("FAILED: {e}"),
        None => format!(
            "amount={:?} category={:?}",
            result.extracted_amount,
            result.extracted_category.as_deref().unwrap_or("<none>")
        ),
    }
}
