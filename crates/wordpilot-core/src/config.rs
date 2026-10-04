//! TOML configuration (spec §31).
//!
//! Every tunable the spec asks to keep out of code lives here: ranking
//! weights, hotkeys, endpoints, privacy lists. Missing files are not an
//! error -- the defaults are a usable configuration.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::CoreError;
use crate::language::{Language, LanguageMode};
use crate::paths;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    pub general: GeneralConfig,
    pub autocomplete: AutocompleteConfig,
    pub ranking: RankingConfig,
    pub dictionaries: DictionaryConfig,
    pub privacy: PrivacyConfig,
    pub applications: ApplicationsConfig,
    pub grammar: GrammarConfig,
    pub ai: AiConfig,
    pub hotkeys: HotkeyConfig,
    pub logging: LoggingConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct GeneralConfig {
    pub default_language: Language,
    /// Whether `Auto` language mode is offered at all.
    pub auto_language_detection: bool,
    /// Overrides the XDG socket path. Empty means "use the XDG default".
    pub socket_path: Option<PathBuf>,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            default_language: Language::German,
            auto_language_detection: true,
            socket_path: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AutocompleteConfig {
    pub enabled: bool,
    pub max_candidates: usize,
    /// Prefixes shorter than this produce no candidates, which keeps the popup
    /// from flashing on every first keystroke.
    pub min_prefix_length: usize,
    /// Debounce applied by the frontend, in milliseconds (spec §37).
    pub debounce_ms: u64,
    /// Offer corrections for prefixes that match no word at all (spec §8).
    pub spell_correction: bool,
}

impl Default for AutocompleteConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_candidates: 5,
            min_prefix_length: 2,
            debounce_ms: 25,
            spell_correction: true,
        }
    }
}

/// Ranking weights. Deliberately not normalised for the user: the ranker
/// normalises internally so that editing one weight does not silently rescale
/// the others.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RankingConfig {
    pub global_frequency_weight: f32,
    pub personal_frequency_weight: f32,
    pub context_weight: f32,
    pub recency_weight: f32,
    pub application_weight: f32,
    /// Extra weight for an exact-case prefix match, which keeps German nouns
    /// from being swamped by their lowercase verb homographs.
    pub prefix_quality_weight: f32,
    /// Half-life for the recency signal, in seconds.
    pub recency_half_life_secs: f64,
}

impl Default for RankingConfig {
    fn default() -> Self {
        Self {
            global_frequency_weight: 0.35,
            personal_frequency_weight: 0.30,
            context_weight: 0.20,
            recency_weight: 0.10,
            application_weight: 0.05,
            prefix_quality_weight: 0.15,
            recency_half_life_secs: 7.0 * 24.0 * 3600.0,
        }
    }
}

impl RankingConfig {
    /// Rejects a configuration that would make every score zero or undefined.
    pub fn validate(&self) -> Result<(), CoreError> {
        let weights = [
            ("global_frequency_weight", self.global_frequency_weight),
            ("personal_frequency_weight", self.personal_frequency_weight),
            ("context_weight", self.context_weight),
            ("recency_weight", self.recency_weight),
            ("application_weight", self.application_weight),
            ("prefix_quality_weight", self.prefix_quality_weight),
        ];
        for (name, value) in weights {
            if !value.is_finite() || value < 0.0 {
                return Err(CoreError::ConfigInvalid(format!(
                    "ranking.{name} must be a non-negative finite number, got {value}"
                )));
            }
        }
        if weights.iter().all(|(_, v)| *v == 0.0) {
            return Err(CoreError::ConfigInvalid(
                "at least one ranking weight must be greater than zero".into(),
            ));
        }
        if !(self.recency_half_life_secs.is_finite() && self.recency_half_life_secs > 0.0) {
            return Err(CoreError::ConfigInvalid(
                "ranking.recency_half_life_secs must be positive".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DictionaryConfig {
    /// Languages loaded at startup. Loading is the expensive part, so this is
    /// kept small by default (spec §36).
    pub languages: Vec<Language>,
    /// Extra directories prepended to the XDG/system search path.
    pub search_paths: Vec<PathBuf>,
    /// Frequency-list directories, prepended to the XDG default.
    pub frequency_paths: Vec<PathBuf>,
}

impl Default for DictionaryConfig {
    fn default() -> Self {
        Self {
            languages: vec![Language::German, Language::EnglishUs],
            search_paths: Vec::new(),
            frequency_paths: Vec::new(),
        }
    }
}

impl DictionaryConfig {
    /// Full dictionary search path: configured directories first, then XDG and
    /// system defaults.
    pub fn resolved_search_paths(&self) -> Vec<PathBuf> {
        let mut dirs = self.search_paths.clone();
        dirs.extend(paths::default_dictionary_dirs());
        dirs
    }

    pub fn resolved_frequency_paths(&self) -> Vec<PathBuf> {
        let mut dirs = self.frequency_paths.clone();
        dirs.extend(paths::default_frequency_dirs());
        dirs
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct PrivacyConfig {
    /// Master switch for the personal dictionary (spec §5, §28).
    pub learning_enabled: bool,
    /// Trust the input method's "password field" hint and disable entirely.
    pub password_field_detection: bool,
    /// Also disable for fields the input method marks as otherwise sensitive.
    pub sensitive_field_detection: bool,
}

impl Default for PrivacyConfig {
    fn default() -> Self {
        Self {
            learning_enabled: true,
            password_field_detection: true,
            sensitive_field_detection: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ApplicationsConfig {
    /// Applications WordPilot is entirely inactive in (spec §29).
    pub disabled: Vec<String>,
}

impl Default for ApplicationsConfig {
    fn default() -> Self {
        Self {
            disabled: vec![
                "keepassxc".into(),
                "org.keepassxc.KeePassXC".into(),
                "1password".into(),
                "bitwarden".into(),
            ],
        }
    }
}

impl ApplicationsConfig {
    /// Case-insensitive match, so `Firefox` and `firefox` behave the same.
    pub fn is_disabled(&self, application: Option<&str>) -> bool {
        let Some(name) = application else {
            return false;
        };
        self.disabled
            .iter()
            .any(|entry| entry.eq_ignore_ascii_case(name))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct GrammarConfig {
    pub enabled: bool,
    pub provider: String,
    pub endpoint: String,
    pub timeout_ms: u64,
}

impl Default for GrammarConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            provider: "languagetool".into(),
            endpoint: "http://127.0.0.1:8081".into(),
            timeout_ms: 2_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AiConfig {
    /// Remote AI is opt-in (spec §30); this defaults to off.
    pub enabled: bool,
    pub provider: String,
    pub endpoint: String,
    pub model: String,
    /// Name of the environment variable holding the API key. The key itself is
    /// never stored in, or read from, the config file (spec §30).
    pub api_key_env: String,
    pub timeout_ms: u64,
    pub max_context_chars: usize,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: "openai-compatible".into(),
            endpoint: "http://127.0.0.1:11434/v1".into(),
            model: "mistral:latest".into(),
            api_key_env: "WORDPILOT_AI_API_KEY".into(),
            timeout_ms: 30_000,
            max_context_chars: 1_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct HotkeyConfig {
    pub ai: String,
    pub language_german: String,
    pub language_english: String,
    pub language_auto: String,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            ai: "Ctrl+Alt+Space".into(),
            language_german: "Super+Alt+D".into(),
            language_english: "Super+Alt+E".into(),
            language_auto: "Super+Alt+A".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LoggingConfig {
    pub level: String,
    /// Logs the user's actual text. Off by default; this is a debugging
    /// escape hatch, not a normal setting (spec §42).
    pub log_user_text: bool,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "info".into(),
            log_user_text: false,
        }
    }
}

impl Config {
    /// Loads the config file, falling back to defaults when it is absent.
    pub fn load(path: &Path) -> Result<Self, CoreError> {
        match std::fs::read_to_string(path) {
            Ok(contents) => Self::from_toml(&contents, path),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                tracing::info!(path = %path.display(), "no config file, using defaults");
                Ok(Self::default())
            }
            Err(source) => Err(CoreError::ConfigRead {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    /// Loads from the default XDG location.
    pub fn load_default() -> Result<Self, CoreError> {
        let path = paths::config_file()?;
        Self::load(&path)
    }

    pub fn from_toml(contents: &str, path: &Path) -> Result<Self, CoreError> {
        let config: Config = toml::from_str(contents).map_err(|source| CoreError::ConfigParse {
            path: path.to_path_buf(),
            source,
        })?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), CoreError> {
        self.ranking.validate()?;
        if self.autocomplete.max_candidates == 0 {
            return Err(CoreError::ConfigInvalid(
                "autocomplete.max_candidates must be at least 1".into(),
            ));
        }
        if self.autocomplete.min_prefix_length == 0 {
            return Err(CoreError::ConfigInvalid(
                "autocomplete.min_prefix_length must be at least 1".into(),
            ));
        }
        if self.dictionaries.languages.is_empty() {
            return Err(CoreError::ConfigInvalid(
                "dictionaries.languages must list at least one language".into(),
            ));
        }
        if !self
            .dictionaries
            .languages
            .contains(&self.general.default_language)
        {
            return Err(CoreError::ConfigInvalid(format!(
                "general.default_language {} is not in dictionaries.languages",
                self.general.default_language
            )));
        }
        Ok(())
    }

    /// The initial language mode, honouring `auto_language_detection`.
    pub fn initial_language_mode(&self) -> LanguageMode {
        if self.general.auto_language_detection {
            LanguageMode::Auto
        } else {
            LanguageMode::Fixed(self.general.default_language)
        }
    }

    /// Resolved socket path: explicit config overrides the XDG default.
    pub fn socket_path(&self) -> Result<PathBuf, CoreError> {
        match &self.general.socket_path {
            Some(path) => Ok(path.clone()),
            None => paths::socket_path(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(contents: &str) -> Result<Config, CoreError> {
        Config::from_toml(contents, Path::new("<test>"))
    }

    #[test]
    fn empty_config_equals_defaults() {
        assert_eq!(parse("").unwrap(), Config::default());
    }

    #[test]
    fn parses_the_documented_example_shape() {
        let config = parse(
            r#"
            [general]
            default_language = "de"
            auto_language_detection = true

            [autocomplete]
            enabled = true
            max_candidates = 7
            min_prefix_length = 3

            [ranking]
            global_frequency_weight = 0.35
            personal_frequency_weight = 0.30
            context_weight = 0.20
            recency_weight = 0.10
            application_weight = 0.05

            [grammar]
            provider = "languagetool"
            endpoint = "http://127.0.0.1:8081"

            [ai]
            enabled = true
            provider = "openai-compatible"
            endpoint = "http://127.0.0.1:11434/v1"
            model = "mistral:latest"

            [hotkeys]
            ai = "Ctrl+Alt+Space"
            "#,
        )
        .unwrap();

        assert_eq!(config.general.default_language, Language::German);
        assert_eq!(config.autocomplete.max_candidates, 7);
        assert_eq!(config.autocomplete.min_prefix_length, 3);
        // Unset keys inside a present table keep their defaults.
        assert_eq!(config.autocomplete.debounce_ms, 25);
        assert_eq!(config.hotkeys.language_german, "Super+Alt+D");
        assert!(config.ai.enabled);
    }

    #[test]
    fn language_aliases_work_in_config() {
        for tag in ["de", "de-DE", "german"] {
            let config = parse(&format!(
                "[general]\ndefault_language = \"{tag}\"\n[dictionaries]\nlanguages = [\"de\"]\n"
            ))
            .unwrap();
            assert_eq!(config.general.default_language, Language::German);
        }
    }

    #[test]
    fn rejects_unknown_keys_rather_than_ignoring_a_typo() {
        let err = parse("[autocomplete]\nmax_candidate = 5\n").unwrap_err();
        assert!(matches!(err, CoreError::ConfigParse { .. }), "{err:?}");
    }

    #[test]
    fn rejects_zero_max_candidates() {
        let err = parse("[autocomplete]\nmax_candidates = 0\n").unwrap_err();
        assert!(matches!(err, CoreError::ConfigInvalid(_)), "{err:?}");
    }

    #[test]
    fn rejects_all_zero_ranking_weights() {
        let err = parse(
            r#"
            [ranking]
            global_frequency_weight = 0.0
            personal_frequency_weight = 0.0
            context_weight = 0.0
            recency_weight = 0.0
            application_weight = 0.0
            prefix_quality_weight = 0.0
            "#,
        )
        .unwrap_err();
        assert!(matches!(err, CoreError::ConfigInvalid(_)), "{err:?}");
    }

    #[test]
    fn rejects_negative_weight() {
        let err = parse("[ranking]\nrecency_weight = -0.5\n").unwrap_err();
        assert!(matches!(err, CoreError::ConfigInvalid(_)), "{err:?}");
    }

    #[test]
    fn rejects_default_language_not_loaded() {
        let err = parse("[general]\ndefault_language = \"en-GB\"\n").unwrap_err();
        assert!(matches!(err, CoreError::ConfigInvalid(_)), "{err:?}");
    }

    #[test]
    fn missing_file_yields_defaults() {
        let config = Config::load(Path::new("/nonexistent/wordpilot/config.toml")).unwrap();
        assert_eq!(config, Config::default());
    }

    #[test]
    fn ai_is_opt_in_and_key_comes_from_the_environment() {
        let config = Config::default();
        assert!(!config.ai.enabled);
        assert_eq!(config.ai.api_key_env, "WORDPILOT_AI_API_KEY");
    }

    #[test]
    fn blacklist_matching_ignores_case_and_absence() {
        let apps = ApplicationsConfig::default();
        assert!(apps.is_disabled(Some("KeePassXC")));
        assert!(apps.is_disabled(Some("org.keepassxc.keepassxc")));
        assert!(!apps.is_disabled(Some("firefox")));
        assert!(!apps.is_disabled(None));
    }

    #[test]
    fn logging_does_not_include_user_text_by_default() {
        assert!(!Config::default().logging.log_user_text);
    }
}
