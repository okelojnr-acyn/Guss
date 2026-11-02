use std::collections::BTreeMap;

use serde::Serialize;

use crate::manifest::Sample;

/// What the pipeline actually produced for one sample, alongside the
/// ground truth it's scored against. `None` fields mean that stage
/// failed outright (STT error, NLU couldn't extract anything) rather
/// than extracted-but-wrong - these are counted separately from wrong
/// answers, since "failed to understand" and "confidently wrong" are
/// different failure modes a reader needs to tell apart.
#[derive(Debug, Clone)]
pub struct SampleResult {
    pub sample: Sample,
    pub transcript: Option<String>,
    pub extracted_amount: Option<f64>,
    pub extracted_currency: Option<String>,
    pub extracted_category: Option<String>,
    pub error: Option<String>,
}

impl SampleResult {
    pub fn pipeline_failed(&self) -> bool {
        self.error.is_some()
    }

    pub fn amount_correct(&self, tolerance: f64) -> bool {
        match self.extracted_amount {
            Some(got) => (got - self.sample.expected_amount).abs() <= tolerance,
            None => false,
        }
    }

    pub fn category_correct(&self) -> bool {
        match &self.extracted_category {
            Some(got) => got
                .trim()
                .eq_ignore_ascii_case(self.sample.expected_category.trim()),
            None => false,
        }
    }

    /// The right amount in the wrong currency is still a wrong ledger
    /// entry - e.g. the extractor defaulting to USD instead of ZAR
    /// because it didn't pick up a currency cue from the transcript.
    pub fn currency_correct(&self) -> bool {
        match &self.extracted_currency {
            Some(got) => got
                .trim()
                .eq_ignore_ascii_case(self.sample.expected_currency.trim()),
            None => false,
        }
    }

    /// The number that actually matters for a bookkeeping product: did
    /// the amount, currency, and category all come out right, regardless
    /// of whether the transcript itself was word-perfect.
    pub fn task_correct(&self, tolerance: f64) -> bool {
        !self.pipeline_failed()
            && self.amount_correct(tolerance)
            && self.currency_correct()
            && self.category_correct()
    }

    /// Word Error Rate against the reference transcript, if one was
    /// supplied - a diagnostic metric explaining *why* a task-level
    /// failure happened, not the headline number.
    pub fn word_error_rate(&self) -> Option<f64> {
        let reference = self.sample.reference_transcript.as_deref()?;
        let hypothesis = self.transcript.as_deref().unwrap_or("");
        Some(word_error_rate(reference, hypothesis))
    }
}

/// Word-level Levenshtein distance divided by reference word count -
/// the standard WER definition.
pub fn word_error_rate(reference: &str, hypothesis: &str) -> f64 {
    let ref_words: Vec<&str> = reference.split_whitespace().collect();
    let hyp_words: Vec<&str> = hypothesis.split_whitespace().collect();

    if ref_words.is_empty() {
        return if hyp_words.is_empty() { 0.0 } else { 1.0 };
    }

    word_edit_distance(&ref_words, &hyp_words) as f64 / ref_words.len() as f64
}

fn word_edit_distance(a: &[&str], b: &[&str]) -> usize {
    let (n, m) = (a.len(), b.len());
    let mut dp = vec![vec![0usize; m + 1]; n + 1];
    for (i, row) in dp.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, v) in dp[0].iter_mut().enumerate() {
        *v = j;
    }
    for i in 1..=n {
        for j in 1..=m {
            dp[i][j] = if a[i - 1].eq_ignore_ascii_case(b[j - 1]) {
                dp[i - 1][j - 1]
            } else {
                1 + dp[i - 1][j - 1].min(dp[i - 1][j]).min(dp[i][j - 1])
            };
        }
    }
    dp[n][m]
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct LanguageStats {
    pub total: usize,
    pub task_correct: usize,
    pub pipeline_failures: usize,
    pub mean_word_error_rate: Option<f64>,
}

impl LanguageStats {
    pub fn task_accuracy(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.task_correct as f64 / self.total as f64
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AccuracyReport {
    pub overall: LanguageStats,
    /// Keyed by the sample's `language` tag.
    pub by_language: BTreeMap<String, LanguageStats>,
    /// The code-switched subset, scored separately regardless of each
    /// sample's primary `language` tag.
    pub code_switched: LanguageStats,
    /// (expected_category, actual_category) -> count, actual_category
    /// is "<none>" for pipeline failures.
    pub category_confusion: BTreeMap<(String, String), usize>,
}

pub fn build_report(results: &[SampleResult], amount_tolerance: f64) -> AccuracyReport {
    let mut overall = LanguageStats::default();
    let mut by_language: BTreeMap<String, LanguageStats> = BTreeMap::new();
    let mut code_switched = LanguageStats::default();
    let mut category_confusion: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut overall_wers = Vec::new();
    let mut lang_wers: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let mut code_switched_wers = Vec::new();

    for result in results {
        let correct = result.task_correct(amount_tolerance);
        let wer = result.word_error_rate();

        accumulate(&mut overall, correct, result.pipeline_failed());
        if let Some(w) = wer {
            overall_wers.push(w);
        }

        let lang_stats = by_language
            .entry(result.sample.language.clone())
            .or_default();
        accumulate(lang_stats, correct, result.pipeline_failed());
        if let Some(w) = wer {
            lang_wers
                .entry(result.sample.language.clone())
                .or_default()
                .push(w);
        }

        if result.sample.code_switched {
            accumulate(&mut code_switched, correct, result.pipeline_failed());
            if let Some(w) = wer {
                code_switched_wers.push(w);
            }
        }

        let actual_category = result
            .extracted_category
            .clone()
            .unwrap_or_else(|| "<none>".to_string());
        *category_confusion
            .entry((result.sample.expected_category.clone(), actual_category))
            .or_insert(0) += 1;
    }

    overall.mean_word_error_rate = mean(&overall_wers);
    code_switched.mean_word_error_rate = mean(&code_switched_wers);
    for (lang, stats) in by_language.iter_mut() {
        stats.mean_word_error_rate = mean(lang_wers.get(lang).map(Vec::as_slice).unwrap_or(&[]));
    }

    AccuracyReport {
        overall,
        by_language,
        code_switched,
        category_confusion,
    }
}

fn accumulate(stats: &mut LanguageStats, task_correct: bool, pipeline_failed: bool) {
    stats.total += 1;
    if task_correct {
        stats.task_correct += 1;
    }
    if pipeline_failed {
        stats.pipeline_failures += 1;
    }
}

fn mean(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        None
    } else {
        Some(values.iter().sum::<f64>() / values.len() as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(language: &str, code_switched: bool, amount: f64, category: &str) -> Sample {
        Sample {
            path: "x.wav".into(),
            language: language.into(),
            code_switched,
            expected_amount: amount,
            expected_currency: "ZAR".into(),
            expected_category: category.into(),
            reference_transcript: Some("sold two bags of maize for one fifty rand".into()),
        }
    }

    #[test]
    fn word_error_rate_is_zero_for_identical_transcripts() {
        assert_eq!(
            word_error_rate("the quick brown fox", "the quick brown fox"),
            0.0
        );
    }

    #[test]
    fn word_error_rate_counts_one_substitution_out_of_four() {
        assert_eq!(
            word_error_rate("the quick brown fox", "the slow brown fox"),
            0.25
        );
    }

    #[test]
    fn task_correct_requires_both_amount_and_category_within_tolerance() {
        let result = SampleResult {
            sample: sample("af", false, 150.0, "stock"),
            transcript: Some("verkoop twee sakke mielies vir een vyftig rand".into()),
            extracted_amount: Some(150.0),
            extracted_currency: Some("ZAR".into()),
            extracted_category: Some("Stock".into()),
            error: None,
        };
        assert!(result.task_correct(0.01));

        let wrong_category = SampleResult {
            extracted_category: Some("bills".into()),
            ..result.clone()
        };
        assert!(!wrong_category.task_correct(0.01));

        let wrong_amount = SampleResult {
            extracted_amount: Some(999.0),
            ..result
        };
        assert!(!wrong_amount.task_correct(0.01));
    }

    #[test]
    fn pipeline_failure_is_never_task_correct_even_if_fields_happen_to_match() {
        let result = SampleResult {
            sample: sample("zu", false, 150.0, "stock"),
            transcript: None,
            extracted_amount: None,
            extracted_currency: None,
            extracted_category: None,
            error: Some("whisper transcription failed".into()),
        };
        assert!(!result.task_correct(0.01));
        assert!(result.pipeline_failed());
    }

    #[test]
    fn build_report_separates_code_switched_subset_from_its_primary_language() {
        let results = vec![
            SampleResult {
                sample: sample("zu", false, 150.0, "stock"),
                transcript: None,
                extracted_amount: None,
                extracted_category: None,
                extracted_currency: None,
                error: Some("unsupported language".into()),
            },
            SampleResult {
                sample: sample("af", true, 80.0, "bills"),
                transcript: Some("betaal tagtig rand vir rekeninge".into()),
                extracted_amount: Some(80.0),
                extracted_category: Some("bills".into()),
                extracted_currency: Some("ZAR".into()),
                error: None,
            },
        ];

        let report = build_report(&results, 0.01);

        assert_eq!(report.overall.total, 2);
        assert_eq!(report.overall.task_correct, 1);
        assert_eq!(report.by_language["zu"].pipeline_failures, 1);
        assert_eq!(report.by_language["af"].task_correct, 1);
        assert_eq!(report.code_switched.total, 1);
        assert_eq!(report.code_switched.task_correct, 1);
    }

    #[test]
    fn category_confusion_tracks_none_for_pipeline_failures() {
        let results = vec![SampleResult {
            sample: sample("st", false, 150.0, "stock"),
            transcript: None,
            extracted_amount: None,
            extracted_category: None,
            extracted_currency: None,
            error: Some("unsupported language".into()),
        }];

        let report = build_report(&results, 0.01);
        assert_eq!(
            report.category_confusion[&("stock".to_string(), "<none>".to_string())],
            1
        );
    }
}
