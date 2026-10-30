//! Corpus frequency, the primary ranking signal before the user has taught
//! WordPilot anything.
//!
//! The on-disk format is the widely available `word<space>count` list (one
//! entry per line, descending). Counts are converted to a score in `[0, 1]`
//! via `log(count) / log(max_count)`: raw counts span five orders of
//! magnitude, so a linear scale would make everything below the top hundred
//! words indistinguishable.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use wordpilot_core::Language;

use crate::error::DictionaryError;

#[derive(Debug, Default)]
pub struct FrequencyTable {
    scores: HashMap<String, f32>,
}

impl FrequencyTable {
    /// Looks for `<short_tag>_50k.txt`, then `<short_tag>.txt`, in each
    /// directory. A missing list is not an error: ranking degrades to the
    /// other signals.
    pub fn discover(language: Language, search_paths: &[PathBuf]) -> Self {
        let names = [
            format!("{}_50k.txt", language.short_tag()),
            format!("{}.txt", language.short_tag()),
            format!("{}_full.txt", language.short_tag()),
        ];
        for dir in search_paths {
            for name in &names {
                let path = dir.join(name);
                if !path.is_file() {
                    continue;
                }
                match Self::load(&path) {
                    Ok(table) => {
                        tracing::info!(
                            language = %language,
                            path = %path.display(),
                            words = table.len(),
                            "loaded frequency list"
                        );
                        return table;
                    }
                    Err(error) => {
                        tracing::warn!(
                            path = %path.display(),
                            %error,
                            "ignoring unreadable frequency list"
                        );
                    }
                }
            }
        }
        tracing::info!(language = %language, "no frequency list found; ranking will rely on other signals");
        Self::default()
    }

    pub fn load(path: &Path) -> Result<Self, DictionaryError> {
        let text = std::fs::read_to_string(path).map_err(|source| DictionaryError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(Self::from_text(&text))
    }

    pub fn from_text(text: &str) -> Self {
        let mut counts: Vec<(String, f64)> = Vec::new();
        let mut max_count = 0.0_f64;

        for line in text.lines() {
            let mut parts = line.split_whitespace();
            let (Some(word), Some(raw_count)) = (parts.next(), parts.next()) else {
                continue;
            };
            let Ok(count) = raw_count.parse::<f64>() else {
                continue;
            };
            if count <= 0.0 {
                continue;
            }
            max_count = max_count.max(count);
            counts.push((word.to_lowercase(), count));
        }

        let denominator = if max_count > 1.0 { max_count.ln() } else { 1.0 };
        let mut scores: HashMap<String, f32> = HashMap::with_capacity(counts.len());
        for (word, count) in counts {
            let score = (count.ln() / denominator).clamp(0.0, 1.0) as f32;
            // Keep the highest score when a list repeats a word in different
            // capitalisations.
            scores
                .entry(word)
                .and_modify(|existing| *existing = existing.max(score))
                .or_insert(score);
        }
        Self { scores }
    }

    /// Frequency score in `[0, 1]`; `0.0` for an unknown word.
    pub fn score(&self, word: &str) -> f32 {
        self.scores
            .get(&word.to_lowercase())
            .copied()
            .unwrap_or(0.0)
    }

    pub fn len(&self) -> usize {
        self.scores.len()
    }

    pub fn is_empty(&self) -> bool {
        self.scores.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
ich 5890279
bewegen 9579
bewältigen 562
bewerben 482
";

    #[test]
    fn frequent_words_score_higher() {
        let table = FrequencyTable::from_text(SAMPLE);
        assert!(table.score("ich") > table.score("bewegen"));
        assert!(table.score("bewegen") > table.score("bewältigen"));
        assert!(table.score("bewältigen") > table.score("bewerben"));
    }

    #[test]
    fn scores_are_normalised_to_the_unit_interval() {
        let table = FrequencyTable::from_text(SAMPLE);
        for word in ["ich", "bewegen", "bewerben"] {
            let score = table.score(word);
            assert!((0.0..=1.0).contains(&score), "{word} scored {score}");
        }
        assert!((table.score("ich") - 1.0).abs() < 1e-6);
    }

    #[test]
    fn lookup_is_case_insensitive_and_unknown_words_score_zero() {
        let table = FrequencyTable::from_text(SAMPLE);
        assert_eq!(table.score("Bewerben"), table.score("bewerben"));
        assert_eq!(table.score("Kubernetes"), 0.0);
    }

    #[test]
    fn umlauts_survive_case_folding() {
        let table = FrequencyTable::from_text(SAMPLE);
        assert!(table.score("Bewältigen") > 0.0);
    }

    #[test]
    fn malformed_lines_are_skipped() {
        let table = FrequencyTable::from_text("good 100\nbad\nworse notanumber\nzero 0\n\n");
        assert_eq!(table.len(), 1);
        assert!(table.score("good") > 0.0);
        assert_eq!(table.score("zero"), 0.0);
    }

    #[test]
    fn empty_table_scores_everything_zero() {
        let table = FrequencyTable::default();
        assert!(table.is_empty());
        assert_eq!(table.score("anything"), 0.0);
    }

    #[test]
    fn missing_list_is_not_an_error() {
        let table = FrequencyTable::discover(Language::German, &[PathBuf::from("/nonexistent")]);
        assert!(table.is_empty());
    }
}
