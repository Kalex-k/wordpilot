//! Candidate ranking.
//!
//! Alphabetical order is useless for a prefix like `bew`, which matches
//! hundreds of German words (spec §4). This crate combines independent
//! signals, each normalised to `[0, 1]`, using weights that live in the
//! configuration rather than in the code.
//!
//! The ranker itself holds no data sources: callers assemble a [`Signals`]
//! value per candidate. That keeps the SQLite personal dictionary, the
//! frequency table and any future n-gram model out of this crate.

use wordpilot_core::config::RankingConfig;
use wordpilot_core::{Candidate, CandidateSource};

/// Per-candidate ranking inputs. Every field is in `[0, 1]`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Signals {
    /// Corpus frequency of the word.
    pub global_frequency: f32,
    /// How often this user has accepted the word.
    pub personal_frequency: f32,
    /// Probability from a context/n-gram model.
    pub context: f32,
    /// Decayed recency of the last use.
    pub recency: f32,
    /// Frequency within the current application.
    pub application: f32,
    /// How good a prefix match this is: exact capitalisation and a short
    /// remaining suffix both raise it.
    pub prefix_quality: f32,
}

impl Signals {
    /// Recency decayed with the configured half-life.
    ///
    /// `age_secs` is clamped at zero so a clock skew that puts a timestamp in
    /// the future cannot produce a score above 1.
    pub fn decay_recency(age_secs: f64, half_life_secs: f64) -> f32 {
        if !(half_life_secs.is_finite() && half_life_secs > 0.0) {
            return 0.0;
        }
        let age = age_secs.max(0.0);
        (0.5_f64).powf(age / half_life_secs) as f32
    }

    /// Prefix quality from the match itself.
    ///
    /// Longer completions are penalised mildly: when the user has typed `bew`,
    /// `bewegen` is a more likely intent than `bewältigungsvorschlag`.
    pub fn prefix_quality(prefix: &str, word: &str, exact_case: bool) -> f32 {
        let prefix_chars = prefix.chars().count();
        let word_chars = word.chars().count();
        if prefix_chars == 0 || word_chars == 0 {
            return 0.0;
        }
        // Char counts, not byte lengths: "bewä" is 4 characters, not 5.
        let coverage = (prefix_chars as f32 / word_chars as f32).clamp(0.0, 1.0);
        let case_bonus = if exact_case { 1.0 } else { 0.0 };
        0.5 * coverage + 0.5 * case_bonus
    }
}

/// Combines signals into a score using configured weights.
#[derive(Debug, Clone)]
pub struct CandidateRanker {
    config: RankingConfig,
    /// Sum of all weights, used to normalise the score into `[0, 1]`.
    weight_sum: f32,
}

impl CandidateRanker {
    pub fn new(config: RankingConfig) -> Self {
        let weight_sum = config.global_frequency_weight
            + config.personal_frequency_weight
            + config.context_weight
            + config.recency_weight
            + config.application_weight
            + config.prefix_quality_weight;
        Self {
            config,
            // `Config::validate` guarantees a positive sum, but a ranker built
            // directly from a hand-made config must not divide by zero.
            weight_sum: if weight_sum > 0.0 { weight_sum } else { 1.0 },
        }
    }

    pub fn config(&self) -> &RankingConfig {
        &self.config
    }

    /// Weighted score in `[0, 1]`.
    ///
    /// Normalising by the weight sum means a user who edits one weight does
    /// not implicitly rescale all scores, so thresholds stay comparable.
    pub fn score(&self, signals: &Signals) -> f32 {
        let c = &self.config;
        let weighted = c.global_frequency_weight * clamp_unit(signals.global_frequency)
            + c.personal_frequency_weight * clamp_unit(signals.personal_frequency)
            + c.context_weight * clamp_unit(signals.context)
            + c.recency_weight * clamp_unit(signals.recency)
            + c.application_weight * clamp_unit(signals.application)
            + c.prefix_quality_weight * clamp_unit(signals.prefix_quality);
        weighted / self.weight_sum
    }

    /// Sorts candidates by score, highest first.
    ///
    /// Ties break on source priority and then alphabetically, so the candidate
    /// list is deterministic for the same input -- a popup that reshuffles
    /// equal-scoring words between keystrokes is unusable.
    pub fn sort(&self, candidates: &mut [Candidate]) {
        candidates.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| source_priority(a.source).cmp(&source_priority(b.source)))
                .then_with(|| a.text.len().cmp(&b.text.len()))
                .then_with(|| a.text.cmp(&b.text))
        });
    }
}

/// Lower is better. A learned word outranks a raw dictionary hit, and a
/// correction never outranks a real completion at equal score.
fn source_priority(source: CandidateSource) -> u8 {
    match source {
        CandidateSource::Personal => 0,
        CandidateSource::Context => 1,
        CandidateSource::Frequency => 2,
        CandidateSource::Dictionary => 3,
        CandidateSource::SpellCorrection => 4,
    }
}

fn clamp_unit(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ranker() -> CandidateRanker {
        CandidateRanker::new(RankingConfig::default())
    }

    #[test]
    fn score_is_within_the_unit_interval() {
        let r = ranker();
        assert_eq!(r.score(&Signals::default()), 0.0);
        let all_max = Signals {
            global_frequency: 1.0,
            personal_frequency: 1.0,
            context: 1.0,
            recency: 1.0,
            application: 1.0,
            prefix_quality: 1.0,
        };
        assert!((r.score(&all_max) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn personal_frequency_beats_global_frequency() {
        // The motivating case: "bewegen" is far more common in German overall,
        // but this user keeps writing "bewerben".
        let r = ranker();
        let common = Signals {
            global_frequency: 1.0,
            ..Signals::default()
        };
        let learned = Signals {
            global_frequency: 0.3,
            personal_frequency: 1.0,
            recency: 1.0,
            ..Signals::default()
        };
        assert!(r.score(&learned) > r.score(&common));
    }

    #[test]
    fn out_of_range_and_nan_signals_do_not_poison_the_score() {
        let r = ranker();
        let bad = Signals {
            global_frequency: f32::NAN,
            personal_frequency: 5.0,
            context: -3.0,
            recency: f32::INFINITY,
            application: 0.5,
            prefix_quality: 0.5,
        };
        let score = r.score(&bad);
        assert!(score.is_finite() && (0.0..=1.0).contains(&score), "{score}");
    }

    #[test]
    fn weights_actually_come_from_config() {
        let only_context = RankingConfig {
            global_frequency_weight: 0.0,
            personal_frequency_weight: 0.0,
            context_weight: 1.0,
            recency_weight: 0.0,
            application_weight: 0.0,
            prefix_quality_weight: 0.0,
            ..RankingConfig::default()
        };
        let r = CandidateRanker::new(only_context);
        let frequent_only = Signals {
            global_frequency: 1.0,
            ..Signals::default()
        };
        let contextual_only = Signals {
            context: 1.0,
            ..Signals::default()
        };
        assert_eq!(r.score(&frequent_only), 0.0);
        assert!((r.score(&contextual_only) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn zero_weight_sum_does_not_divide_by_zero() {
        let zeroed = RankingConfig {
            global_frequency_weight: 0.0,
            personal_frequency_weight: 0.0,
            context_weight: 0.0,
            recency_weight: 0.0,
            application_weight: 0.0,
            prefix_quality_weight: 0.0,
            ..RankingConfig::default()
        };
        let score = CandidateRanker::new(zeroed).score(&Signals {
            global_frequency: 1.0,
            ..Signals::default()
        });
        assert!(score.is_finite());
    }

    #[test]
    fn recency_decays_by_half_each_half_life() {
        let half_life = 100.0;
        assert!((Signals::decay_recency(0.0, half_life) - 1.0).abs() < 1e-6);
        assert!((Signals::decay_recency(100.0, half_life) - 0.5).abs() < 1e-6);
        assert!((Signals::decay_recency(200.0, half_life) - 0.25).abs() < 1e-6);
        // A timestamp from the future must not score above 1.
        assert!(Signals::decay_recency(-1_000.0, half_life) <= 1.0);
        assert_eq!(Signals::decay_recency(10.0, 0.0), 0.0);
    }

    #[test]
    fn prefix_quality_rewards_exact_case_and_short_completions() {
        let exact = Signals::prefix_quality("Bew", "Bewerben", true);
        let folded = Signals::prefix_quality("Bew", "bewerben", false);
        assert!(exact > folded);

        let short = Signals::prefix_quality("bew", "bewegen", true);
        let long = Signals::prefix_quality("bew", "bewältigungsvorschlag", true);
        assert!(short > long);
    }

    #[test]
    fn prefix_quality_counts_characters_not_bytes() {
        // "bewä" is 4 chars / 5 bytes. Byte arithmetic would score these
        // differently despite identical character shapes.
        let with_umlaut = Signals::prefix_quality("bewä", "bewältige", true);
        let without = Signals::prefix_quality("bewa", "bewaltige", true);
        assert!((with_umlaut - without).abs() < 1e-6);
        assert_eq!(Signals::prefix_quality("", "wort", true), 0.0);
        assert_eq!(Signals::prefix_quality("wort", "", true), 0.0);
    }

    #[test]
    fn sort_is_descending_and_deterministic_on_ties() {
        let r = ranker();
        let mut candidates = vec![
            Candidate::new("bewerten", CandidateSource::Dictionary, 0.5),
            Candidate::new("bewegen", CandidateSource::Dictionary, 0.9),
            Candidate::new("bewerben", CandidateSource::Personal, 0.5),
            Candidate::new("bewahren", CandidateSource::Dictionary, 0.5),
        ];
        r.sort(&mut candidates);
        let order: Vec<&str> = candidates.iter().map(|c| c.text.as_str()).collect();
        // bewegen by score; then the Personal source wins the three-way tie;
        // the remaining two break alphabetically at equal length.
        assert_eq!(order, vec!["bewegen", "bewerben", "bewahren", "bewerten"]);

        let mut shuffled = vec![
            Candidate::new("bewahren", CandidateSource::Dictionary, 0.5),
            Candidate::new("bewerten", CandidateSource::Dictionary, 0.5),
        ];
        r.sort(&mut shuffled);
        let mut reversed = vec![
            Candidate::new("bewerten", CandidateSource::Dictionary, 0.5),
            Candidate::new("bewahren", CandidateSource::Dictionary, 0.5),
        ];
        r.sort(&mut reversed);
        assert_eq!(shuffled, reversed);
    }

    #[test]
    fn corrections_rank_below_completions_at_equal_score() {
        let r = ranker();
        let mut candidates = vec![
            Candidate::new("Erfahrung", CandidateSource::SpellCorrection, 0.7),
            Candidate::new("Erfahrungen", CandidateSource::Dictionary, 0.7),
        ];
        r.sort(&mut candidates);
        assert_eq!(candidates[0].text, "Erfahrungen");
    }

    #[test]
    fn sort_tolerates_nan_scores_without_panicking() {
        let r = ranker();
        let mut candidates = vec![
            Candidate::new("a", CandidateSource::Dictionary, f32::NAN),
            Candidate::new("b", CandidateSource::Dictionary, 0.5),
        ];
        r.sort(&mut candidates);
        assert_eq!(candidates.len(), 2);
    }
}
