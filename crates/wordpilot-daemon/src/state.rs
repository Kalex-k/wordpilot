//! Shared daemon state.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Instant;

use wordpilot_core::config::Config;
use wordpilot_core::{Language, LanguageMode};
use wordpilot_protocol::message::LoadedLanguage;
use wordpilot_protocol::{ServerStatus, PROTOCOL_VERSION};

use wordpilot_autocomplete::AutocompleteEngine;
use wordpilot_dictionary::DictionaryProvider;

/// The active language, as the hotkeys set it (spec §2).
///
/// Encoded in an atomic rather than behind a lock: it is read on every
/// keystroke and written only when the user presses a hotkey, so a lock here
/// would be contention for nothing.
struct ActiveLanguage(AtomicU8);

impl ActiveLanguage {
    const AUTO: u8 = u8::MAX;

    fn new(mode: LanguageMode) -> Self {
        Self(AtomicU8::new(Self::encode(mode)))
    }

    fn encode(mode: LanguageMode) -> u8 {
        match mode {
            LanguageMode::Auto => Self::AUTO,
            LanguageMode::Fixed(language) => Language::ALL
                .iter()
                .position(|l| *l == language)
                .expect("every Language variant is in Language::ALL")
                as u8,
        }
    }

    fn get(&self) -> LanguageMode {
        let raw = self.0.load(Ordering::Relaxed);
        if raw == Self::AUTO {
            LanguageMode::Auto
        } else {
            // A value is only ever stored through `encode`, so the index is
            // in range; falling back to Auto keeps this panic-free regardless.
            Language::ALL
                .get(raw as usize)
                .copied()
                .map_or(LanguageMode::Auto, LanguageMode::Fixed)
        }
    }

    fn set(&self, mode: LanguageMode) {
        self.0.store(Self::encode(mode), Ordering::Relaxed);
    }
}

pub struct DaemonState {
    engine: AutocompleteEngine,
    active_language: ActiveLanguage,
    started: Instant,
    load_failures: Vec<(Language, String)>,
}

impl DaemonState {
    pub fn new(engine: AutocompleteEngine, load_failures: Vec<(Language, String)>) -> Arc<Self> {
        let initial = engine.config().initial_language_mode();
        Arc::new(Self {
            active_language: ActiveLanguage::new(initial),
            engine,
            started: Instant::now(),
            load_failures,
        })
    }

    pub fn engine(&self) -> &AutocompleteEngine {
        &self.engine
    }

    pub fn config(&self) -> &Config {
        self.engine.config()
    }

    pub fn active_language(&self) -> LanguageMode {
        self.active_language.get()
    }

    /// Returns the mode actually adopted.
    ///
    /// A request for a language with no loaded dictionary, or for `Auto` when
    /// detection is disabled, is declined rather than accepted into a state
    /// that cannot produce candidates.
    pub fn set_active_language(&self, mode: LanguageMode) -> LanguageMode {
        let accept = match mode {
            LanguageMode::Auto => self.config().general.auto_language_detection,
            LanguageMode::Fixed(language) => self.engine.resources(language).is_some(),
        };
        if accept {
            self.active_language.set(mode);
        } else {
            tracing::warn!(requested = %mode, "declining unavailable language");
        }
        self.active_language.get()
    }

    pub fn load_failures(&self) -> &[(Language, String)] {
        &self.load_failures
    }

    pub fn status(&self) -> ServerStatus {
        let config = self.config();
        let loaded_languages = self
            .engine
            .loaded_languages()
            .into_iter()
            .filter_map(|language| {
                let resources = self.engine.resources(language)?;
                Some(LoadedLanguage {
                    language,
                    dictionary_words: resources.dictionary.len(),
                    frequency_words: resources.frequency.len(),
                    dictionary_path: resources.dictionary.source_name(),
                })
            })
            .collect();

        ServerStatus {
            protocol_version: PROTOCOL_VERSION,
            daemon_version: crate::DAEMON_VERSION.to_string(),
            active_language: self.active_language(),
            loaded_languages,
            autocomplete_enabled: config.autocomplete.enabled,
            learning_enabled: config.privacy.learning_enabled
                && self.engine.personal().is_learning(),
            grammar_enabled: config.grammar.enabled,
            ai_enabled: config.ai.enabled,
            uptime_secs: self.started.elapsed().as_secs(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_language_encoding_roundtrips_every_variant() {
        for mode in Language::ALL
            .iter()
            .map(|l| LanguageMode::Fixed(*l))
            .chain(std::iter::once(LanguageMode::Auto))
        {
            let active = ActiveLanguage::new(mode);
            assert_eq!(active.get(), mode);
        }
    }

    #[test]
    fn active_language_can_be_changed() {
        let active = ActiveLanguage::new(LanguageMode::Auto);
        active.set(LanguageMode::Fixed(Language::EnglishGb));
        assert_eq!(active.get(), LanguageMode::Fixed(Language::EnglishGb));
    }
}
