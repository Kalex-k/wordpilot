//! Next-word prediction, as an interface (spec §6).
//!
//! The core must not depend on KenLM or any particular n-gram library, so the
//! engine talks only to this trait. No statistical model ships in the MVP;
//! [`NoContextPredictor`] is the honest stand-in until one does, and it makes
//! the engine's behaviour with an empty prefix explicit rather than accidental.

use wordpilot_core::Language;

/// One predicted continuation.
#[derive(Debug, Clone, PartialEq)]
pub struct Prediction {
    pub word: String,
    /// Model probability in `[0, 1]`.
    pub probability: f32,
}

impl Prediction {
    pub fn new(word: impl Into<String>, probability: f32) -> Self {
        Self {
            word: word.into(),
            probability,
        }
    }
}

/// Predicts the next word, and scores how well a candidate fits the context.
pub trait ContextPredictor: Send + Sync {
    /// Likely next words given the preceding words (oldest first).
    ///
    /// Called when the current prefix is empty, e.g. after `Ich warte `.
    fn predict(&self, preceding: &[&str], language: Language, limit: usize) -> Vec<Prediction>;

    /// Context probability for an already-known candidate, used as the
    /// `context` ranking signal rather than as a candidate source.
    fn score(&self, preceding: &[&str], candidate: &str, language: Language) -> f32;
}

/// A predictor that knows nothing.
///
/// Deliberately not a fabricated hardcoded table of German prepositions: the
/// spec asks for this to come from n-grams or user statistics, not from a list
/// baked into the binary (spec §7).
#[derive(Debug, Default, Clone, Copy)]
pub struct NoContextPredictor;

impl ContextPredictor for NoContextPredictor {
    fn predict(&self, _preceding: &[&str], _language: Language, _limit: usize) -> Vec<Prediction> {
        Vec::new()
    }

    fn score(&self, _preceding: &[&str], _candidate: &str, _language: Language) -> f32 {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_predictor_predicts_nothing_and_scores_zero() {
        let predictor = NoContextPredictor;
        assert!(predictor
            .predict(&["ich", "warte"], Language::German, 5)
            .is_empty());
        assert_eq!(
            predictor.score(&["ich", "warte"], "auf", Language::German),
            0.0
        );
    }
}
