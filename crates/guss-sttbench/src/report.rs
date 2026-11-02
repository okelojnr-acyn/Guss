use std::fmt::Write as _;

use crate::score::{AccuracyReport, LanguageStats};

/// Renders the report as the per-language breakdown the FinFex proposal
/// promises - never a single blended number, since that's exactly what
/// would hide a language or the code-switched subset quietly failing.
pub fn render_text(report: &AccuracyReport) -> String {
    let mut out = String::new();

    writeln!(out, "=== Overall ===").unwrap();
    writeln!(out, "{}", format_stats_line(&report.overall)).unwrap();
    writeln!(out).unwrap();

    writeln!(out, "=== By language ===").unwrap();
    for (language, stats) in &report.by_language {
        writeln!(out, "{language:<8} {}", format_stats_line(stats)).unwrap();
    }
    writeln!(out).unwrap();

    writeln!(out, "=== Code-switched subset ===").unwrap();
    writeln!(out, "{}", format_stats_line(&report.code_switched)).unwrap();
    writeln!(out).unwrap();

    writeln!(out, "=== Category confusion (expected -> got: count) ===").unwrap();
    for ((expected, actual), count) in &report.category_confusion {
        let marker = if expected.eq_ignore_ascii_case(actual) {
            " "
        } else {
            "!"
        };
        writeln!(out, "{marker} {expected:<16} -> {actual:<16} : {count}").unwrap();
    }

    out
}

fn format_stats_line(stats: &LanguageStats) -> String {
    let wer = stats
        .mean_word_error_rate
        .map(|w| format!("{:.1}% WER", w * 100.0))
        .unwrap_or_else(|| "no reference transcripts".to_string());
    format!(
        "n={:<4} task-correct={:<4} ({:>5.1}%)  pipeline-failures={:<4}  {}",
        stats.total,
        stats.task_correct,
        stats.task_accuracy() * 100.0,
        stats.pipeline_failures,
        wer,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Sample;
    use crate::score::SampleResult;
    use crate::score::build_report;

    #[test]
    fn render_text_includes_every_language_and_the_code_switched_subset() {
        let sample = |language: &str, code_switched: bool| Sample {
            path: "x.wav".into(),
            language: language.into(),
            code_switched,
            expected_amount: 100.0,
            expected_currency: "ZAR".into(),
            expected_category: "stock".into(),
            reference_transcript: None,
        };
        let results = vec![
            SampleResult {
                sample: sample("zu", false),
                transcript: None,
                extracted_amount: None,
                extracted_category: None,
                extracted_currency: None,
                error: Some("unsupported".into()),
            },
            SampleResult {
                sample: sample("en", true),
                transcript: Some("sold stock for a hundred rand".into()),
                extracted_amount: Some(100.0),
                extracted_category: Some("stock".into()),
                extracted_currency: Some("ZAR".into()),
                error: None,
            },
        ];

        let report = build_report(&results, 0.01);
        let text = render_text(&report);

        assert!(text.contains("=== Overall ==="));
        assert!(text.contains("zu"));
        assert!(text.contains("en"));
        assert!(text.contains("Code-switched subset"));
        assert!(text.contains("Category confusion"));
    }
}
