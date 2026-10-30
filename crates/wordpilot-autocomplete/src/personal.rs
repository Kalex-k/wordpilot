//! The personal-learning signal, as an interface.
//!
//! The SQLite implementation lives in the storage crate.
//! The engine only needs these three numbers, so it does not link SQLite and
//! keeps working when the personal database is missing or corrupt.

use wordpilot_core::Language;

/// What the engine needs to know about the user's history with one word.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PersonalStats {
    /// Usage count mapped into `[0, 1]`.
    pub frequency: f32,
    /// Recency in `[0, 1]`, already decayed.
    pub recency: f32,
    /// Usage count within the current application, in `[0, 1]`.
    pub application_frequency: f32,
}

/// A learned vocabulary that influences ranking.
pub trait PersonalVocabulary: Send + Sync {
    /// Stats for one word. Must be cheap: this is called once per candidate on
    /// the keystroke path.
    fn stats(&self, word: &str, language: Language, application: Option<&str>) -> PersonalStats;

    /// Words the user has typed that start with `prefix` but are not in any
    /// system dictionary -- `Kubernetes`, `rusqlite`, a surname.
    fn complete(&self, prefix: &str, language: Language, limit: usize) -> Vec<String>;

    /// Records that the user accepted `word`.
    ///
    /// Takes `&self` because implementations serialise internally; a learning
    /// write must never require an exclusive borrow on the keystroke path.
    fn record_usage(&self, word: &str, language: Language, application: Option<&str>);

    /// False when learning is disabled, so callers can skip the work entirely.
    fn is_learning(&self) -> bool;
}

/// Used when learning is disabled or the database is unavailable.
///
/// Not a stub for unfinished work: "no personal dictionary" is a supported
/// configuration.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoPersonalDictionary;

impl PersonalVocabulary for NoPersonalDictionary {
    fn stats(&self, _word: &str, _language: Language, _application: Option<&str>) -> PersonalStats {
        PersonalStats::default()
    }

    fn complete(&self, _prefix: &str, _language: Language, _limit: usize) -> Vec<String> {
        Vec::new()
    }

    fn record_usage(&self, _word: &str, _language: Language, _application: Option<&str>) {}

    fn is_learning(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_vocabulary_contributes_nothing() {
        let vocab = NoPersonalDictionary;
        assert!(!vocab.is_learning());
        assert_eq!(
            vocab.stats("bewerben", Language::German, Some("firefox")),
            PersonalStats::default()
        );
        assert!(vocab.complete("bew", Language::German, 5).is_empty());
        // Recording must be a no-op rather than a panic.
        vocab.record_usage("bewerben", Language::German, None);
    }
}
