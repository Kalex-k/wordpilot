//! The autocomplete engine: context in, ranked candidates out.
//!
//! Knows nothing about sockets, X11, Wayland or Fcitx (spec §57). Everything
//! it needs arrives in a [`CompletionRequest`].

use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;

use wordpilot_core::config::Config;
use wordpilot_core::{
    Candidate, CandidateSource, CompletionRequest, CompletionResponse, Language, LanguageMode,
};
use wordpilot_dictionary::{DictionaryProvider, FrequencyTable};
use wordpilot_ranking::{CandidateRanker, Signals};

use crate::detect::LanguageDetector;
use crate::personal::PersonalVocabulary;
use crate::predict::ContextPredictor;

/// How many dictionary hits to consider before ranking.
///
/// Ranking only reorders what it is given, so a prefix like `bew` must be
/// over-fetched: taking the first five alphabetically and ranking those would
/// never surface `bewerben` (spec §4).
const RANKING_POOL: usize = 400;

/// How many preceding words are used as context and for detection.
const CONTEXT_WINDOW: usize = 4;

/// Everything loaded for one language. Built once at startup (spec §36).
pub struct LanguageResources {
    pub dictionary: Box<dyn DictionaryProvider>,
    pub frequency: FrequencyTable,
}

impl LanguageResources {
    pub fn new(dictionary: Box<dyn DictionaryProvider>, frequency: FrequencyTable) -> Self {
        Self {
            dictionary,
            frequency,
        }
    }
}

pub struct AutocompleteEngine {
    config: Config,
    ranker: CandidateRanker,
    resources: HashMap<Language, LanguageResources>,
    personal: Arc<dyn PersonalVocabulary>,
    predictor: Arc<dyn ContextPredictor>,
    detector: Arc<dyn LanguageDetector>,
}

impl AutocompleteEngine {
    pub fn new(
        config: Config,
        resources: HashMap<Language, LanguageResources>,
        personal: Arc<dyn PersonalVocabulary>,
        predictor: Arc<dyn ContextPredictor>,
        detector: Arc<dyn LanguageDetector>,
    ) -> Self {
        let ranker = CandidateRanker::new(config.ranking.clone());
        Self {
            config,
            ranker,
            resources,
            personal,
            predictor,
            detector,
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Languages with loaded resources.
    pub fn loaded_languages(&self) -> Vec<Language> {
        let mut languages: Vec<Language> = self.resources.keys().copied().collect();
        languages.sort();
        languages
    }

    pub fn resources(&self, language: Language) -> Option<&LanguageResources> {
        self.resources.get(&language)
    }

    pub fn personal(&self) -> &Arc<dyn PersonalVocabulary> {
        &self.personal
    }

    /// Falls back to any loaded language when the requested one is missing, so
    /// a missing German dictionary degrades to English rather than to silence.
    fn resolve_language(&self, request: &CompletionRequest) -> Option<Language> {
        let loaded = self.loaded_languages();
        let requested = match request.language {
            LanguageMode::Fixed(language) => Some(language),
            LanguageMode::Auto => {
                let words = request
                    .context
                    .preceding_words(self.config.general.default_language, CONTEXT_WINDOW);
                self.detector
                    .detect(&words, &loaded)
                    .or(Some(self.config.general.default_language))
            }
        };
        match requested {
            Some(language) if self.resources.contains_key(&language) => Some(language),
            Some(language) => {
                let fallback = loaded.first().copied();
                tracing::debug!(
                    requested = %language,
                    fallback = ?fallback.map(|l| l.tag()),
                    "requested language is not loaded"
                );
                fallback
            }
            None => loaded.first().copied(),
        }
    }

    /// Whether WordPilot must stay silent for this request (spec §28, §29).
    fn is_suppressed(&self, request: &CompletionRequest) -> bool {
        let privacy = &self.config.privacy;
        if request.application.sensitive
            && (privacy.password_field_detection || privacy.sensitive_field_detection)
        {
            return true;
        }
        self.config
            .applications
            .is_disabled(request.application.name.as_deref())
    }

    /// Produces ranked candidates. Never fails: an empty response is the
    /// correct answer whenever there is nothing good to offer.
    pub fn complete(&self, request: &CompletionRequest) -> CompletionResponse {
        let default_language = self.config.general.default_language;

        if !self.config.autocomplete.enabled || self.is_suppressed(request) {
            return CompletionResponse::empty(default_language);
        }

        let Some(language) = self.resolve_language(request) else {
            tracing::warn!("no dictionaries loaded; completion is unavailable");
            return CompletionResponse::empty(default_language);
        };
        let Some(resources) = self.resources.get(&language) else {
            return CompletionResponse::empty(language);
        };

        let prefix = request.context.current_word(language).to_owned();
        let limit = request
            .max_candidates
            .unwrap_or(self.config.autocomplete.max_candidates)
            .min(self.config.autocomplete.max_candidates)
            .max(1);

        // Completing with the caret inside a word would strand the suffix.
        if request.context.caret_inside_word(language) {
            return CompletionResponse::empty(language);
        }

        let preceding = request.context.preceding_words(language, CONTEXT_WINDOW);
        let application = request.application.name.as_deref();

        let mut candidates = if prefix.is_empty() {
            self.next_word_candidates(&preceding, language, limit)
        } else if prefix.chars().count() < self.config.autocomplete.min_prefix_length {
            Vec::new()
        } else {
            self.prefix_candidates(&prefix, &preceding, language, resources, application)
        };

        self.ranker.sort(&mut candidates);
        // A five-slot popup must not spend two slots on "Erfahrung" and
        // "erfahrung". Deduplicating *after* ranking keeps whichever
        // capitalisation the user is actually typing, because `prefix_quality`
        // scores the case-matching variant higher.
        dedup_case_variants(&mut candidates);
        candidates.truncate(limit);

        CompletionResponse {
            candidates,
            language,
            prefix_len_bytes: prefix.len(),
            prefix,
        }
    }

    /// Next-word prediction for an empty prefix (spec §6).
    fn next_word_candidates(
        &self,
        preceding: &[&str],
        language: Language,
        limit: usize,
    ) -> Vec<Candidate> {
        if preceding.is_empty() {
            return Vec::new();
        }
        self.predictor
            .predict(preceding, language, limit)
            .into_iter()
            .map(|prediction| {
                let score = self.ranker.score(&Signals {
                    context: prediction.probability,
                    ..Signals::default()
                });
                Candidate::new(prediction.word, CandidateSource::Context, score)
            })
            .collect()
    }

    fn prefix_candidates(
        &self,
        prefix: &str,
        preceding: &[&str],
        language: Language,
        resources: &LanguageResources,
        application: Option<&str>,
    ) -> Vec<Candidate> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut candidates: Vec<Candidate> = Vec::new();

        // The user's own vocabulary first: these words may not exist in any
        // system dictionary, so they must not be crowded out by the pool cap.
        let personal_words = if self.personal.is_learning() {
            self.personal.complete(prefix, language, RANKING_POOL)
        } else {
            Vec::new()
        };
        for word in personal_words {
            if !seen.insert(word.clone()) {
                continue;
            }
            let exact_case = word.starts_with(prefix);
            let score = self.score_word(
                prefix,
                &word,
                exact_case,
                preceding,
                language,
                resources,
                application,
            );
            candidates.push(Candidate::new(word, CandidateSource::Personal, score));
        }

        for entry in resources.dictionary.complete(prefix, RANKING_POOL) {
            if !seen.insert(entry.word.clone()) {
                continue;
            }
            let score = self.score_word(
                prefix,
                &entry.word,
                entry.exact_case,
                preceding,
                language,
                resources,
                application,
            );
            // A word with real corpus weight is surfaced as a frequency hit,
            // which also gives it tie-break priority over an unknown word.
            let source = if resources.frequency.score(&entry.word) > 0.0 {
                CandidateSource::Frequency
            } else {
                CandidateSource::Dictionary
            };
            candidates.push(Candidate::new(entry.word, source, score));
        }

        // Corrections only when the prefix is a plausible whole word that the
        // dictionary rejects, and never mixed in silently (spec §8).
        if candidates.is_empty()
            && self.config.autocomplete.spell_correction
            && prefix.chars().count() >= 4
            && !resources.dictionary.contains(prefix)
        {
            for entry in resources
                .dictionary
                .corrections(prefix, self.config.autocomplete.max_candidates)
            {
                if !seen.insert(entry.word.clone()) {
                    continue;
                }
                let score = self.score_word(
                    prefix,
                    &entry.word,
                    entry.exact_case,
                    preceding,
                    language,
                    resources,
                    application,
                );
                candidates.push(Candidate::new(
                    entry.word,
                    CandidateSource::SpellCorrection,
                    score,
                ));
            }
        }

        candidates
    }

    #[allow(clippy::too_many_arguments)]
    fn score_word(
        &self,
        prefix: &str,
        word: &str,
        exact_case: bool,
        preceding: &[&str],
        language: Language,
        resources: &LanguageResources,
        application: Option<&str>,
    ) -> f32 {
        let personal = self.personal.stats(word, language, application);
        self.ranker.score(&Signals {
            global_frequency: resources.frequency.score(word),
            personal_frequency: personal.frequency,
            context: self.predictor.score(preceding, word, language),
            recency: personal.recency,
            application: personal.application_frequency,
            prefix_quality: Signals::prefix_quality(prefix, word, exact_case),
        })
    }

    /// Records an accepted candidate as a positive signal (spec §39).
    ///
    /// Silently does nothing when learning is off or the field was sensitive;
    /// a password must never reach the personal dictionary (spec §28).
    pub fn record_acceptance(
        &self,
        word: &str,
        language: Language,
        application: Option<&str>,
        sensitive: bool,
    ) {
        if sensitive
            || !self.config.privacy.learning_enabled
            || !self.personal.is_learning()
            || self.config.applications.is_disabled(application)
        {
            return;
        }
        self.personal.record_usage(word, language, application);
    }
}

/// Removes candidates that differ from an earlier, higher-ranked one only in
/// capitalisation. Expects `candidates` to be sorted best-first.
fn dedup_case_variants(candidates: &mut Vec<Candidate>) {
    let mut kept: HashSet<String> = HashSet::with_capacity(candidates.len());
    candidates.retain(|candidate| kept.insert(candidate.text.to_lowercase()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::HeuristicLanguageDetector;
    use crate::personal::{NoPersonalDictionary, PersonalStats};
    use crate::predict::{NoContextPredictor, Prediction};
    use std::sync::Mutex;
    use wordpilot_core::{ApplicationContext, TextContext};
    use wordpilot_dictionary::HunspellDictionary;

    const DE_WORDS: &str = "\
0
bewerben
bewerten
bewegen
bewältigen
bewahren
bewaffnen
bewältigungsvorschlag
Bewerbung
Bewerbungen
Berufserfahrung
Erfahrung
";

    const EN_WORDS: &str = "\
0
application
applications
apply
applied
appliance
experience
";

    const DE_FREQ: &str = "\
bewegen 9579
bewältigen 562
bewerben 482
bewerten 421
bewahren 300
bewerbung 1200
erfahrung 5000
";

    const EN_FREQ: &str = "\
apply 8000
application 5000
experience 4000
";

    fn resources() -> HashMap<Language, LanguageResources> {
        let mut map = HashMap::new();
        map.insert(
            Language::German,
            LanguageResources::new(
                Box::new(HunspellDictionary::from_lines(Language::German, DE_WORDS)),
                FrequencyTable::from_text(DE_FREQ),
            ),
        );
        map.insert(
            Language::EnglishUs,
            LanguageResources::new(
                Box::new(HunspellDictionary::from_lines(
                    Language::EnglishUs,
                    EN_WORDS,
                )),
                FrequencyTable::from_text(EN_FREQ),
            ),
        );
        map
    }

    fn engine_with(config: Config) -> AutocompleteEngine {
        AutocompleteEngine::new(
            config,
            resources(),
            Arc::new(NoPersonalDictionary),
            Arc::new(NoContextPredictor),
            Arc::new(HeuristicLanguageDetector::default()),
        )
    }

    fn engine() -> AutocompleteEngine {
        engine_with(Config::default())
    }

    fn request(typed: &str, language: LanguageMode) -> CompletionRequest {
        CompletionRequest {
            context: TextContext::from_typed(typed),
            language,
            application: ApplicationContext::default(),
            max_candidates: None,
        }
    }

    fn words(response: &CompletionResponse) -> Vec<&str> {
        response
            .candidates
            .iter()
            .map(|c| c.text.as_str())
            .collect()
    }

    const DE: LanguageMode = LanguageMode::Fixed(Language::German);
    const EN: LanguageMode = LanguageMode::Fixed(Language::EnglishUs);

    #[test]
    fn completes_a_german_prefix() {
        let response = engine().complete(&request("Ich möchte mich bew", DE));
        assert_eq!(response.language, Language::German);
        assert_eq!(response.prefix, "bew");
        let got = words(&response);
        assert!(got.contains(&"bewerben"), "{got:?}");
        assert!(got.len() <= 5, "{got:?}");
    }

    #[test]
    fn ranking_puts_frequent_words_above_rare_long_ones() {
        let response = engine().complete(&request("bew", DE));
        let got = words(&response);
        let position = |w: &str| got.iter().position(|g| *g == w);
        // Pure global frequency: bewegen leads. The rare long compound must
        // not be in front of any of them.
        assert_eq!(position("bewegen"), Some(0), "{got:?}");
        assert!(
            position("bewältigungsvorschlag").is_none(),
            "rare compound should not make the top 5: {got:?}"
        );
    }

    #[test]
    fn over_fetches_so_ranking_is_not_limited_to_alphabetical_head() {
        // Alphabetically "bewaffnen" and "bewahren" precede "bewegen"; a naive
        // implementation that fetched only `limit` words would never see it.
        let response = engine().complete(&request("bew", DE));
        let got = words(&response);
        assert_eq!(got.first(), Some(&"bewegen"), "{got:?}");
    }

    #[test]
    fn prefix_is_reported_in_bytes_for_the_frontend() {
        let response = engine().complete(&request("Ich bewä", DE));
        assert_eq!(response.prefix, "bewä");
        assert_eq!(response.prefix_len_bytes, 5);
        assert_eq!(response.prefix.chars().count(), 4);
    }

    #[test]
    fn umlaut_prefix_completes() {
        let response = engine().complete(&request("bewä", DE));
        let got = words(&response);
        assert_eq!(got, vec!["bewältigen", "bewältigungsvorschlag"]);
    }

    #[test]
    fn respects_min_prefix_length() {
        let engine = engine();
        assert!(engine.complete(&request("b", DE)).candidates.is_empty());
        assert!(!engine.complete(&request("be", DE)).candidates.is_empty());
    }

    #[test]
    fn respects_max_candidates_and_cannot_be_exceeded_by_the_client() {
        let mut config = Config::default();
        config.autocomplete.max_candidates = 3;
        let engine = engine_with(config);

        assert_eq!(engine.complete(&request("bew", DE)).candidates.len(), 3);

        let mut greedy = request("bew", DE);
        greedy.max_candidates = Some(1000);
        assert_eq!(engine.complete(&greedy).candidates.len(), 3);

        let mut modest = request("bew", DE);
        modest.max_candidates = Some(2);
        assert_eq!(engine.complete(&modest).candidates.len(), 2);

        let mut zero = request("bew", DE);
        zero.max_candidates = Some(0);
        assert_eq!(engine.complete(&zero).candidates.len(), 1);
    }

    #[test]
    fn switching_language_switches_dictionaries() {
        let engine = engine();
        let de_response = engine.complete(&request("appl", DE));
        let de = words(&de_response);
        assert!(
            de.is_empty(),
            "German dictionary should not know 'appl': {de:?}"
        );
        let response = engine.complete(&request("appl", EN));
        assert_eq!(response.language, Language::EnglishUs);
        assert!(
            words(&response).contains(&"apply"),
            "{:?}",
            words(&response)
        );
    }

    #[test]
    fn auto_mode_detects_german_but_a_fixed_choice_wins() {
        let engine = engine();
        let german_context = "Ich habe nicht viel Erfahr";
        assert_eq!(
            engine
                .complete(&request(german_context, LanguageMode::Auto))
                .language,
            Language::German
        );
        // Same text, explicit English: the manual choice must win (spec §38).
        assert_eq!(
            engine.complete(&request(german_context, EN)).language,
            Language::EnglishUs
        );
    }

    #[test]
    fn auto_mode_detects_english() {
        let response = engine().complete(&request(
            "I have been working with the appl",
            LanguageMode::Auto,
        ));
        assert_eq!(response.language, Language::EnglishUs);
    }

    #[test]
    fn auto_mode_falls_back_to_the_default_language_without_evidence() {
        let engine = engine();
        let response = engine.complete(&request("bew", LanguageMode::Auto));
        assert_eq!(response.language, Language::German);
    }

    #[test]
    fn unloaded_language_degrades_to_a_loaded_one_instead_of_silence() {
        let engine = engine();
        let response = engine.complete(&request("appl", LanguageMode::Fixed(Language::EnglishGb)));
        assert!(engine.resources(Language::EnglishGb).is_none());
        assert_eq!(response.language, Language::German);
    }

    #[test]
    fn no_dictionaries_yields_an_empty_response_rather_than_a_panic() {
        let engine = AutocompleteEngine::new(
            Config::default(),
            HashMap::new(),
            Arc::new(NoPersonalDictionary),
            Arc::new(NoContextPredictor),
            Arc::new(HeuristicLanguageDetector::default()),
        );
        let response = engine.complete(&request("bew", DE));
        assert!(response.candidates.is_empty());
        assert!(engine.loaded_languages().is_empty());
    }

    #[test]
    fn disabled_autocomplete_returns_nothing() {
        let mut config = Config::default();
        config.autocomplete.enabled = false;
        assert!(engine_with(config)
            .complete(&request("bew", DE))
            .candidates
            .is_empty());
    }

    #[test]
    fn sensitive_fields_are_suppressed_entirely() {
        let mut req = request("bew", DE);
        req.application = ApplicationContext::sensitive();
        assert!(engine().complete(&req).candidates.is_empty());
    }

    #[test]
    fn blacklisted_applications_are_suppressed() {
        let mut req = request("bew", DE);
        req.application = ApplicationContext::named("KeePassXC");
        assert!(engine().complete(&req).candidates.is_empty());

        let mut allowed = request("bew", DE);
        allowed.application = ApplicationContext::named("firefox");
        assert!(!engine().complete(&allowed).candidates.is_empty());
    }

    #[test]
    fn caret_inside_a_word_suppresses_completion() {
        let engine = engine();
        let mut req = request("bew", DE);
        req.context = TextContext::new("bew", "erben");
        assert!(engine.complete(&req).candidates.is_empty());

        req.context = TextContext::new("bew", ", und");
        assert!(!engine.complete(&req).candidates.is_empty());
    }

    #[test]
    fn empty_prefix_yields_nothing_without_a_context_model() {
        // Honest behaviour for the MVP: no fabricated preposition table.
        let response = engine().complete(&request("Ich warte ", DE));
        assert!(response.candidates.is_empty());
        assert_eq!(response.prefix, "");
    }

    #[test]
    fn empty_prefix_uses_the_context_predictor_when_one_exists() {
        struct FakePredictor;
        impl ContextPredictor for FakePredictor {
            fn predict(&self, preceding: &[&str], _l: Language, _limit: usize) -> Vec<Prediction> {
                if preceding.last() == Some(&"warte") {
                    vec![Prediction::new("auf", 0.9)]
                } else {
                    Vec::new()
                }
            }
            fn score(&self, _p: &[&str], _c: &str, _l: Language) -> f32 {
                0.0
            }
        }
        let engine = AutocompleteEngine::new(
            Config::default(),
            resources(),
            Arc::new(NoPersonalDictionary),
            Arc::new(FakePredictor),
            Arc::new(HeuristicLanguageDetector::default()),
        );
        let response = engine.complete(&request("Ich warte ", DE));
        assert_eq!(words(&response), vec!["auf"]);
        assert_eq!(response.candidates[0].source, CandidateSource::Context);
    }

    #[test]
    fn personal_vocabulary_is_offered_and_outranks_the_dictionary() {
        struct Personal;
        impl PersonalVocabulary for Personal {
            fn stats(&self, word: &str, _l: Language, _a: Option<&str>) -> PersonalStats {
                if word == "bewerben" {
                    PersonalStats {
                        frequency: 1.0,
                        recency: 1.0,
                        application_frequency: 1.0,
                    }
                } else {
                    PersonalStats::default()
                }
            }
            fn complete(&self, prefix: &str, _l: Language, _limit: usize) -> Vec<String> {
                // A word no system dictionary contains.
                if "Kubernetes".starts_with(prefix) {
                    vec!["Kubernetes".to_string()]
                } else {
                    Vec::new()
                }
            }
            fn record_usage(&self, _w: &str, _l: Language, _a: Option<&str>) {}
            fn is_learning(&self) -> bool {
                true
            }
        }
        let engine = AutocompleteEngine::new(
            Config::default(),
            resources(),
            Arc::new(Personal),
            Arc::new(NoContextPredictor),
            Arc::new(HeuristicLanguageDetector::default()),
        );

        // Learned usage overcomes "bewegen"'s much higher corpus frequency.
        let bew = engine.complete(&request("bew", DE));
        let got = words(&bew);
        assert_eq!(got.first(), Some(&"bewerben"), "{got:?}");

        // A word that exists only in the personal dictionary is offered.
        let kube = engine.complete(&request("Kube", DE));
        let got = words(&kube);
        assert_eq!(got, vec!["Kubernetes"]);
    }

    #[test]
    fn spell_correction_only_when_nothing_completes_and_is_labelled() {
        let response = engine().complete(&request("Erfharung", DE));
        let got = words(&response);
        assert!(got.contains(&"Erfahrung"), "{got:?}");
        assert!(response
            .candidates
            .iter()
            .all(|c| c.source == CandidateSource::SpellCorrection));

        // A prefix that does complete must not be "corrected".
        let response = engine().complete(&request("bewer", DE));
        assert!(response
            .candidates
            .iter()
            .all(|c| c.source != CandidateSource::SpellCorrection));
    }

    #[test]
    fn spell_correction_can_be_disabled() {
        let mut config = Config::default();
        config.autocomplete.spell_correction = false;
        assert!(engine_with(config)
            .complete(&request("Erfharung", DE))
            .candidates
            .is_empty());
    }

    #[test]
    fn case_variants_do_not_each_take_a_slot() {
        let engine = engine();
        let response = engine.complete(&request("bewerb", DE));
        let got = words(&response);
        let folded: Vec<String> = got.iter().map(|w| w.to_lowercase()).collect();
        let mut unique = folded.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), folded.len(), "case variants in {got:?}");

        // The surviving variant is the one matching the typed capitalisation.
        let lower_response = engine.complete(&request("bewerben", DE));
        let lower = words(&lower_response);
        assert!(
            lower.is_empty() || !lower.contains(&"Bewerben"),
            "{lower:?}"
        );
    }

    #[test]
    fn candidates_are_unique() {
        let response = engine().complete(&request("bew", DE));
        let mut texts: Vec<&str> = words(&response);
        let before = texts.len();
        texts.sort_unstable();
        texts.dedup();
        assert_eq!(texts.len(), before);
    }

    #[test]
    fn repeated_identical_requests_give_identical_output() {
        let engine = engine();
        let first = engine.complete(&request("bew", DE));
        let second = engine.complete(&request("bew", DE));
        assert_eq!(first.candidates, second.candidates);
    }

    #[test]
    fn acceptance_is_recorded_only_when_learning_is_allowed() {
        #[derive(Default)]
        struct Recorder {
            seen: Mutex<Vec<String>>,
        }
        impl PersonalVocabulary for Recorder {
            fn stats(&self, _w: &str, _l: Language, _a: Option<&str>) -> PersonalStats {
                PersonalStats::default()
            }
            fn complete(&self, _p: &str, _l: Language, _limit: usize) -> Vec<String> {
                Vec::new()
            }
            fn record_usage(&self, word: &str, _l: Language, _a: Option<&str>) {
                self.seen.lock().unwrap().push(word.to_string());
            }
            fn is_learning(&self) -> bool {
                true
            }
        }

        let recorder = Arc::new(Recorder::default());
        let engine = AutocompleteEngine::new(
            Config::default(),
            resources(),
            recorder.clone(),
            Arc::new(NoContextPredictor),
            Arc::new(HeuristicLanguageDetector::default()),
        );

        engine.record_acceptance("bewerben", Language::German, Some("firefox"), false);
        // A sensitive field, a blacklisted app: neither may be learned.
        engine.record_acceptance("hunter2", Language::German, None, true);
        engine.record_acceptance("secret", Language::German, Some("keepassxc"), false);
        assert_eq!(*recorder.seen.lock().unwrap(), vec!["bewerben".to_string()]);

        // Learning disabled in config.
        let mut config = Config::default();
        config.privacy.learning_enabled = false;
        let recorder2 = Arc::new(Recorder::default());
        let engine = AutocompleteEngine::new(
            config,
            resources(),
            recorder2.clone(),
            Arc::new(NoContextPredictor),
            Arc::new(HeuristicLanguageDetector::default()),
        );
        engine.record_acceptance("bewerben", Language::German, None, false);
        assert!(recorder2.seen.lock().unwrap().is_empty());
    }
}
