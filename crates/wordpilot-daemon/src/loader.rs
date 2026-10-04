//! Startup loading of dictionaries and frequency lists.
//!
//! Everything here happens once (spec §36). A language that fails to load is
//! logged and skipped rather than aborting the daemon: English autocomplete is
//! still worth having when the German dictionary is missing.

use std::collections::HashMap;

use wordpilot_core::config::Config;
use wordpilot_core::Language;
use wordpilot_dictionary::{FrequencyTable, HunspellDictionary};

use wordpilot_autocomplete::LanguageResources;

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
