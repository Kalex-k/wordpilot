//! Shared daemon state.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use wordpilot_core::config::{to_fcitx_key, Config};
use wordpilot_core::{Language, LanguageMode};
use wordpilot_protocol::message::LoadedLanguage;
use wordpilot_protocol::{
    FrontendConfig, FrontendHotkeys, ReloadOutcome, ServerStatus, PROTOCOL_VERSION,
};

use crate::loader;

use wordpilot_autocomplete::AutocompleteEngine;
use wordpilot_dictionary::DictionaryProvider;

/// The active language, as the hotkeys set it.
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
    // Swapped whole on reload. Readers clone the `Arc` and keep using the
    // engine they got, so a reload never blocks or tears an in-flight
    // completion; the old engine is dropped when its last request finishes.
    engine: RwLock<Arc<AutocompleteEngine>>,
    active_language: ActiveLanguage,
    started: Instant,
    load_failures: RwLock<Vec<(Language, String)>>,
    /// Where `reload` reads from. `None` means reload is unavailable (tests
    /// that build the state from an in-memory engine).
    config_path: Option<PathBuf>,
    generation: AtomicU64,
    // Serialises reloads: two overlapping ones would both re-read the
    // dictionaries and race to install their engine.
    reload_lock: Mutex<()>,
}

impl DaemonState {
    pub fn new(engine: AutocompleteEngine, load_failures: Vec<(Language, String)>) -> Arc<Self> {
        Self::build(engine, load_failures, None)
    }

    /// Like [`DaemonState::new`], but able to [`reload`](Self::reload) from
    /// `config_path`.
    pub fn with_config_path(
        engine: AutocompleteEngine,
        load_failures: Vec<(Language, String)>,
        config_path: PathBuf,
    ) -> Arc<Self> {
        Self::build(engine, load_failures, Some(config_path))
    }

    fn build(
        engine: AutocompleteEngine,
        load_failures: Vec<(Language, String)>,
        config_path: Option<PathBuf>,
    ) -> Arc<Self> {
        let initial = engine.config().initial_language_mode();
        Arc::new(Self {
            active_language: ActiveLanguage::new(initial),
            engine: RwLock::new(Arc::new(engine)),
            started: Instant::now(),
            load_failures: RwLock::new(load_failures),
            config_path,
            generation: AtomicU64::new(0),
            reload_lock: Mutex::new(()),
        })
    }

    /// The current engine. Hold on to it for the duration of one request.
    pub fn engine(&self) -> Arc<AutocompleteEngine> {
        Arc::clone(&self.engine.read().unwrap_or_else(|e| e.into_inner()))
    }

    pub fn config_path(&self) -> Option<&Path> {
        self.config_path.as_deref()
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
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
        if self.language_available(&self.engine(), mode) {
            self.active_language.set(mode);
        } else {
            tracing::warn!(requested = %mode, "declining unavailable language");
        }
        self.active_language.get()
    }

    fn language_available(&self, engine: &AutocompleteEngine, mode: LanguageMode) -> bool {
        match mode {
            LanguageMode::Auto => engine.config().general.auto_language_detection,
            LanguageMode::Fixed(language) => engine.resources(language).is_some(),
        }
    }

    /// The concrete language a learning signal is filed under when the
    /// client said `auto`: the user's manual choice if there is one,
    /// otherwise the configured default.
    pub fn resolve_for_learning(&self, mode: LanguageMode) -> Language {
        mode.fixed()
            .or_else(|| self.active_language().fixed())
            .unwrap_or_else(|| self.engine().config().general.default_language)
    }

    pub fn load_failures(&self) -> Vec<(Language, String)> {
        self.load_failures
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Re-reads the config file and installs an engine built from it.
    ///
    /// Blocking: when `[dictionaries]` changed this re-reads every word list,
    /// which takes about a second, so async callers run it on a blocking
    /// thread. Everything else is applied in microseconds by reusing the
    /// loaded dictionaries. A config that fails to parse or validate leaves
    /// the running engine untouched and is reported as an error.
    pub fn reload(&self) -> Result<ReloadOutcome, String> {
        let _guard = self.reload_lock.lock().unwrap_or_else(|e| e.into_inner());
        let path = self
            .config_path
            .as_deref()
            .ok_or_else(|| "this daemon was started without a config path".to_string())?;
        let config = Config::load(path).map_err(|error| error.to_string())?;

        let current = self.engine();
        let old = current.config();
        if &config == old {
            return Ok(ReloadOutcome {
                generation: self.generation(),
                changed: false,
                dictionaries_reloaded: false,
                restart_required: Vec::new(),
            });
        }

        let mut restart_required = Vec::new();
        if config.general.socket_path != old.general.socket_path {
            restart_required.push("general.socket_path".to_string());
        }
        if config.logging.level != old.logging.level {
            restart_required.push("logging.level".to_string());
        }

        let dictionaries_reloaded = config.dictionaries != old.dictionaries;
        let resources = dictionaries_reloaded.then(|| {
            let outcome = loader::load_languages(&config);
            *self
                .load_failures
                .write()
                .unwrap_or_else(|e| e.into_inner()) = outcome.failures;
            outcome.resources
        });

        // The personal dictionary is cheap to reopen but only needs it when
        // the settings it was opened with changed.
        let personal = if config.privacy.learning_enabled != old.privacy.learning_enabled
            || config.ranking.recency_half_life_secs != old.ranking.recency_half_life_secs
        {
            loader::load_personal_dictionary(&config)
        } else {
            Arc::clone(current.personal())
        };

        let engine = Arc::new(current.reconfigured(config, resources, personal));

        // Keep the user's language choice across a reload unless the new
        // config can no longer serve it.
        let active = self.active_language();
        if !self.language_available(&engine, active) {
            let fallback = engine.config().initial_language_mode();
            tracing::info!(previous = %active, now = %fallback, "active language reset by reload");
            self.active_language.set(fallback);
        }

        *self.engine.write().unwrap_or_else(|e| e.into_inner()) = engine;
        let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        Ok(ReloadOutcome {
            generation,
            changed: true,
            dictionaries_reloaded,
            restart_required,
        })
    }

    pub fn frontend_config(&self) -> FrontendConfig {
        let engine = self.engine();
        let config = engine.config();
        // Validated when the config was loaded, so these cannot fail; an
        // empty string (no hotkey) is the safe fallback regardless.
        let key = |spec: &str| to_fcitx_key(spec).unwrap_or_default();
        FrontendConfig {
            generation: self.generation(),
            autocomplete_enabled: config.autocomplete.enabled,
            min_prefix_length: config.autocomplete.min_prefix_length,
            max_candidates: config.autocomplete.max_candidates,
            password_field_detection: config.privacy.password_field_detection,
            sensitive_field_detection: config.privacy.sensitive_field_detection,
            disabled_applications: config.applications.disabled.clone(),
            hotkeys: FrontendHotkeys {
                language_german: key(&config.hotkeys.language_german),
                language_english: key(&config.hotkeys.language_english),
                language_auto: key(&config.hotkeys.language_auto),
                ai: key(&config.hotkeys.ai),
            },
        }
    }

    pub fn status(&self) -> ServerStatus {
        let engine = self.engine();
        let config = engine.config();
        let loaded_languages = engine
            .loaded_languages()
            .into_iter()
            .filter_map(|language| {
                let resources = engine.resources(language)?;
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
            learning_enabled: config.privacy.learning_enabled && engine.personal().is_learning(),
            grammar_enabled: config.grammar.enabled,
            ai_enabled: config.ai.enabled,
            uptime_secs: self.started.elapsed().as_secs(),
            config_generation: self.generation(),
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
