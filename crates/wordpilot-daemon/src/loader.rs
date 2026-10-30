//! Startup loading of dictionaries and frequency lists.
//!
//! Everything here happens once. A language that fails to load is
//! logged and skipped rather than aborting the daemon: English autocomplete is
//! still worth having when the German dictionary is missing.

use std::collections::HashMap;

use wordpilot_core::config::Config;
use wordpilot_core::Language;
use wordpilot_dictionary::{FrequencyTable, HunspellDictionary};

use std::sync::Arc;

use wordpilot_autocomplete::{LanguageResources, NoPersonalDictionary, PersonalVocabulary};
use wordpilot_storage::PersonalDictionary;

/// Result of loading, including what failed and why, so `wordpilot doctor`
/// can report it.
pub struct LoadOutcome {
    pub resources: HashMap<Language, LanguageResources>,
    pub failures: Vec<(Language, String)>,
}

pub fn load_languages(config: &Config) -> LoadOutcome {
    let dictionary_dirs = config.dictionaries.resolved_search_paths();
    let frequency_dirs = config.dictionaries.resolved_frequency_paths();

    let mut resources = HashMap::new();
    let mut failures = Vec::new();

    for &language in &config.dictionaries.languages {
        match HunspellDictionary::discover(language, &dictionary_dirs) {
            Ok(dictionary) => {
                let frequency = FrequencyTable::discover(language, &frequency_dirs);
                resources.insert(
                    language,
                    LanguageResources::new(Box::new(dictionary), frequency),
                );
            }
            Err(error) => {
                tracing::warn!(language = %language, %error, "skipping language");
                failures.push((language, error.to_string()));
            }
        }
    }

    LoadOutcome {
        resources,
        failures,
    }
}

/// Loads the personal dictionary according to `privacy.learning_enabled`.
///
/// A database that fails to open (corrupted file, unwritable directory) is
/// logged and swapped for [`NoPersonalDictionary`] rather than aborting the
/// daemon: basic dictionary completion must keep working regardless.
pub fn load_personal_dictionary(config: &Config) -> Arc<dyn PersonalVocabulary> {
    if !config.privacy.learning_enabled {
        tracing::info!("personal learning is disabled in config");
        return Arc::new(NoPersonalDictionary);
    }

    let path = match wordpilot_core::paths::personal_db_path() {
        Ok(path) => path,
        Err(error) => {
            tracing::warn!(%error, "cannot resolve personal dictionary path; learning is off");
            return Arc::new(NoPersonalDictionary);
        }
    };

    match PersonalDictionary::open(&path) {
        Ok(dictionary) => {
            let dictionary = dictionary.with_half_life(config.ranking.recency_half_life_secs);
            tracing::info!(
                path = %path.display(),
                words = dictionary.word_count(),
                "loaded personal dictionary"
            );
            Arc::new(dictionary)
        }
        Err(error) => {
            tracing::warn!(
                %error,
                path = %path.display(),
                "personal dictionary unavailable; continuing without learning"
            );
            Arc::new(NoPersonalDictionary)
        }
    }
}
