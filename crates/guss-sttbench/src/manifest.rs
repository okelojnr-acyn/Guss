use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

/// One labelled voice note: ground truth prepared independently of the
/// engine(s) being benchmarked, per the blind-test methodology in the
/// FinFex proposal - see README.md in this crate.
#[derive(Debug, Clone, Deserialize)]
pub struct Sample {
    /// Path to the audio file, relative to the manifest file's own
    /// directory unless absolute. Any format `ffmpeg` reads is fine -
    /// it goes through the same `to_wav16k_mono` conversion real voice
    /// notes do.
    pub path: String,
    /// Free-text language tag, e.g. "zu", "st", "af", "en" - not
    /// validated against any engine's supported-language list, since
    /// that's exactly the thing this tool is measuring.
    pub language: String,
    /// True if this sample is genuinely code-switched (mixes languages
    /// mid-utterance) - scored as its own subset, not folded into
    /// whichever single `language` tag it's nearest to.
    #[serde(default)]
    pub code_switched: bool,
    pub expected_amount: f64,
    pub expected_currency: String,
    pub expected_category: String,
    /// Human-verified transcript, if available - enables the Word Error
    /// Rate diagnostic metric. Task-level accuracy (amount + category)
    /// doesn't need this.
    #[serde(default)]
    pub reference_transcript: Option<String>,
}

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("failed to read manifest at {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("manifest {path} line {line}: {source}")]
    Parse {
        path: PathBuf,
        line: usize,
        #[source]
        source: serde_json::Error,
    },
}

/// Loads samples from a JSON Lines manifest (one `Sample` object per
/// non-empty, non-comment line - lines starting with `//` are skipped so
/// a manifest can carry notes inline).
pub fn load_manifest(path: &Path) -> Result<Vec<Sample>, ManifestError> {
    let content = std::fs::read_to_string(path).map_err(|source| ManifestError::Read {
        path: path.to_path_buf(),
        source,
    })?;

    content
        .lines()
        .enumerate()
        .filter(|(_, line)| {
            let trimmed = line.trim();
            !trimmed.is_empty() && !trimmed.starts_with("//")
        })
        .map(|(i, line)| {
            serde_json::from_str::<Sample>(line).map_err(|source| ManifestError::Parse {
                path: path.to_path_buf(),
                line: i + 1,
                source,
            })
        })
        .collect()
}

/// Resolves a sample's audio path relative to the manifest file's own
/// directory, so a manifest and its audio can be moved together without
/// hard-coded absolute paths.
pub fn resolve_audio_path(manifest_path: &Path, sample: &Sample) -> PathBuf {
    let sample_path = Path::new(&sample.path);
    if sample_path.is_absolute() {
        return sample_path.to_path_buf();
    }
    manifest_path
        .parent()
        .map(|dir| dir.join(sample_path))
        .unwrap_or_else(|| sample_path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_manifest(dir: &Path, content: &str) -> PathBuf {
        let path = dir.join("manifest.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        path
    }

    #[test]
    fn parses_samples_and_skips_blank_and_comment_lines() {
        let dir = std::env::temp_dir();
        let path = write_manifest(
            &dir,
            r#"
// note: this is a comment, skip it
{"path": "a.wav", "language": "zu", "expected_amount": 150.0, "expected_currency": "ZAR", "expected_category": "stock"}

{"path": "b.wav", "language": "af", "code_switched": true, "expected_amount": 80.0, "expected_currency": "ZAR", "expected_category": "bills", "reference_transcript": "betaal 80 rand vir rekeninge"}
"#,
        );

        let samples = load_manifest(&path).unwrap();
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0].language, "zu");
        assert!(!samples[0].code_switched);
        assert!(samples[1].code_switched);
        assert_eq!(
            samples[1].reference_transcript.as_deref(),
            Some("betaal 80 rand vir rekeninge")
        );
    }

    #[test]
    fn resolves_relative_path_against_manifest_directory() {
        let manifest_path = Path::new("/data/benchmarks/manifest.jsonl");
        let sample = Sample {
            path: "audio/001.wav".to_string(),
            language: "en".to_string(),
            code_switched: false,
            expected_amount: 10.0,
            expected_currency: "ZAR".to_string(),
            expected_category: "stock".to_string(),
            reference_transcript: None,
        };

        let resolved = resolve_audio_path(manifest_path, &sample);
        assert_eq!(resolved, Path::new("/data/benchmarks/audio/001.wav"));
    }

    #[test]
    fn absolute_sample_path_is_used_as_is() {
        let manifest_path = Path::new("/data/benchmarks/manifest.jsonl");
        let sample = Sample {
            path: "/elsewhere/001.wav".to_string(),
            language: "en".to_string(),
            code_switched: false,
            expected_amount: 10.0,
            expected_currency: "ZAR".to_string(),
            expected_category: "stock".to_string(),
            reference_transcript: None,
        };

        let resolved = resolve_audio_path(manifest_path, &sample);
        assert_eq!(resolved, Path::new("/elsewhere/001.wav"));
    }
}
