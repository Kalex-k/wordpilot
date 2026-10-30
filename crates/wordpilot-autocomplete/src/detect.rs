//! Language detection for `Auto` mode.
//!
//! The rules: a manual choice always wins (enforced by the
//! engine, which only calls a detector for `LanguageMode::Auto`), short
//! function words like `in`, `die` and `war` are ambiguous, and the language
//! must not flip after a single word.

use wordpilot_core::Language;

pub trait LanguageDetector: Send + Sync {
    /// Best guess for `words` (oldest first), or `None` when the evidence is
    /// too weak. `None` means "keep the current language".
    fn detect(&self, words: &[&str], candidates: &[Language]) -> Option<Language>;
}

/// Character- and stopword-based detection.
///
/// Two cheap signals, no model file: German-only characters (`ä ö ü ß`) are
/// near-conclusive, and otherwise unambiguous stopwords are counted. A single
/// matching word is never enough.
#[derive(Debug, Clone, Copy)]
pub struct HeuristicLanguageDetector {
    /// How many more votes the winner needs than the runner-up.
    min_margin: usize,
    /// Minimum number of votes before any decision is made.
    min_votes: usize,
}

impl Default for HeuristicLanguageDetector {
    fn default() -> Self {
        Self {
            min_margin: 1,
            min_votes: 2,
        }
    }
}

/// Stopwords that occur in one language and effectively not the other.
/// `in`, `die`, `war`, `is`, `so` and `an` are deliberately absent: they are
/// either shared or a common word in both languages.
const GERMAN_MARKERS: &[&str] = &[
    "ich", "nicht", "und", "mit", "auch", "sich", "aber", "oder", "noch", "schon", "sehr", "habe",
    "haben", "hatte", "werden", "wurde", "kann", "muss", "möchte", "für", "über", "durch", "ohne",
    "wenn", "dass", "weil", "beim", "vom", "zum", "zur", "eine", "einen", "einem", "eines", "der",
    "dem", "den", "des", "auf", "aus", "bei", "nach", "seit", "von", "vor", "sind", "wir", "sie",
    "ihr", "mir", "mich", "dich", "euch", "immer", "schnell", "bitte", "danke", "gerne",
];

const ENGLISH_MARKERS: &[&str] = &[
    "the",
    "and",
    "with",
    "not",
    "but",
    "have",
    "has",
    "had",
    "would",
    "could",
    "should",
    "will",
    "there",
    "their",
    "they",
    "them",
    "what",
    "which",
    "when",
    "where",
    "because",
    "about",
    "already",
    "please",
    "thanks",
    "very",
    "also",
    "been",
    "were",
    "that",
    "this",
    "these",
    "those",
    "from",
    "into",
    "than",
    "then",
    "some",
    "much",
    "many",
    "more",
    "most",
    "just",
    "only",
    "really",
    "think",
    "know",
    "working",
    "experience",
    "since",
    "years",
];

impl HeuristicLanguageDetector {
    pub fn new(min_votes: usize, min_margin: usize) -> Self {
        Self {
            min_votes,
            min_margin,
        }
    }

    fn is_german_only_char(c: char) -> bool {
        matches!(c, 'ä' | 'ö' | 'ü' | 'ß' | 'Ä' | 'Ö' | 'Ü')
    }
}

impl LanguageDetector for HeuristicLanguageDetector {
    fn detect(&self, words: &[&str], candidates: &[Language]) -> Option<Language> {
        if words.is_empty() || candidates.is_empty() {
            return None;
        }
        let german_allowed = candidates.contains(&Language::German);
        let english = candidates
            .iter()
            .copied()
            .find(|l| matches!(l, Language::EnglishUs | Language::EnglishGb));

        let mut german_votes = 0usize;
        let mut english_votes = 0usize;

        for word in words {
            let lower = word.to_lowercase();
            if lower.chars().any(Self::is_german_only_char) {
                // Orthography is strong evidence, so it counts double and
                // alone can clear `min_votes`.
                german_votes += 2;
                continue;
            }
            if GERMAN_MARKERS.contains(&lower.as_str()) {
                german_votes += 1;
            } else if ENGLISH_MARKERS.contains(&lower.as_str()) {
                english_votes += 1;
            }
        }

        if !german_allowed {
            german_votes = 0;
        }
        if english.is_none() {
            english_votes = 0;
        }

        let (winner, winning, losing) = if german_votes >= english_votes {
            (
                german_allowed.then_some(Language::German),
                german_votes,
                english_votes,
            )
        } else {
            (english, english_votes, german_votes)
        };

        if winning >= self.min_votes && winning >= losing + self.min_margin {
            winner
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOTH: &[Language] = &[Language::German, Language::EnglishUs];

    fn detect(text: &str, candidates: &[Language]) -> Option<Language> {
        let words: Vec<&str> = text.split_whitespace().collect();
        HeuristicLanguageDetector::default().detect(&words, candidates)
    }

    #[test]
    fn detects_german_from_stopwords() {
        assert_eq!(
            detect("Ich habe mit dem Backend", BOTH),
            Some(Language::German)
        );
    }

    #[test]
    fn detects_english_from_stopwords() {
        assert_eq!(
            detect("I have been working with the backend", BOTH),
            Some(Language::EnglishUs)
        );
    }

    #[test]
    fn umlauts_alone_are_enough() {
        assert_eq!(detect("Bewährung", BOTH), Some(Language::German));
        assert_eq!(detect("Straße", BOTH), Some(Language::German));
        assert_eq!(detect("größer", BOTH), Some(Language::German));
    }

    #[test]
    fn does_not_switch_after_a_single_ambiguous_word() {
        // Short function words that exist in both languages.
        for word in ["in", "die", "war", "is", "an", "so"] {
            assert_eq!(detect(word, BOTH), None, "{word} must not decide");
        }
    }

    #[test]
    fn a_single_marker_word_is_not_enough() {
        assert_eq!(detect("und", BOTH), None);
        assert_eq!(detect("the", BOTH), None);
        // Two independent markers are.
        assert_eq!(detect("und nicht", BOTH), Some(Language::German));
    }

    #[test]
    fn mixed_text_without_a_clear_margin_is_undecided() {
        assert_eq!(detect("the und", BOTH), None);
    }

    #[test]
    fn empty_and_unknown_input_is_undecided() {
        assert_eq!(detect("", BOTH), None);
        assert_eq!(detect("Kubernetes PostgreSQL", BOTH), None);
        assert_eq!(detect("Ich habe", &[]), None);
    }

    #[test]
    fn never_returns_a_language_that_is_not_loaded() {
        // German evidence, but only English is available.
        let only_english = &[Language::EnglishUs];
        assert_eq!(detect("Ich habe und nicht", only_english), None);
        let only_german = &[Language::German];
        assert_eq!(detect("the and with", only_german), None);
        assert_eq!(detect("Ich habe und", only_german), Some(Language::German));
    }

    #[test]
    fn english_gb_is_honoured_when_it_is_the_loaded_variant() {
        let gb = &[Language::German, Language::EnglishGb];
        assert_eq!(detect("the and with there", gb), Some(Language::EnglishGb));
    }

    #[test]
    fn stricter_thresholds_require_more_evidence() {
        let strict = HeuristicLanguageDetector::new(4, 2);
        let words: Vec<&str> = "und nicht".split_whitespace().collect();
        assert_eq!(strict.detect(&words, BOTH), None);
        let words: Vec<&str> = "und nicht aber oder noch".split_whitespace().collect();
        assert_eq!(strict.detect(&words, BOTH), Some(Language::German));
    }
}
