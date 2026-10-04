use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

use crate::error::CoreError;

/// A concrete language WordPilot can complete in.
///
/// Deliberately a closed enum for the MVP: every variant needs dictionary,
/// frequency and (later) context-model wiring, so a stringly-typed language
/// would only move the failure to runtime. New languages are added here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Language {
    #[serde(alias = "de", alias = "de-DE", alias = "de_DE", alias = "german")]
    German,
    #[serde(alias = "en", alias = "en-US", alias = "en_US", alias = "english")]
    EnglishUs,
    #[serde(alias = "en-GB", alias = "en_GB")]
    EnglishGb,
}

impl Language {
    /// All languages known at compile time.
    pub const ALL: &'static [Language] =
        &[Language::German, Language::EnglishUs, Language::EnglishGb];

    /// The BCP-47-ish tag used in config, CLI and dictionary file names.
    pub fn tag(self) -> &'static str {
        match self {
            Language::German => "de_DE",
            Language::EnglishUs => "en_US",
            Language::EnglishGb => "en_GB",
        }
    }

    /// Short tag used by frequency lists and LanguageTool's coarse codes.
    pub fn short_tag(self) -> &'static str {
        match self {
            Language::German => "de",
            Language::EnglishUs | Language::EnglishGb => "en",
        }
    }

    /// Characters that count as part of a word in this language, beyond
    /// Unicode alphabetics. Used by prefix extraction.
    pub fn extra_word_chars(self) -> &'static [char] {
        match self {
            // Hyphen is a word character in German compounds ("Java-Erfahrung").
            Language::German => &['-'],
            Language::EnglishUs | Language::EnglishGb => &['-', '\''],
        }
    }
}

impl fmt::Display for Language {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.tag())
    }
}

impl FromStr for Language {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let normalized = s.trim().to_ascii_lowercase().replace('-', "_");
        match normalized.as_str() {
            "de" | "de_de" | "german" | "deutsch" => Ok(Language::German),
            "en" | "en_us" | "english" => Ok(Language::EnglishUs),
            "en_gb" | "english_gb" => Ok(Language::EnglishGb),
            _ => Err(CoreError::UnknownLanguage(s.to_owned())),
        }
    }
}

/// What the user selected, which is not the same thing as a `Language`.
///
/// `Auto` is resolved by a `LanguageDetector` at request time; a manual choice
/// always wins over detection (spec §38).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LanguageMode {
    Auto,
    Fixed(Language),
}

// Serialised as a plain string -- `"auto"`, `"de"`, `"en_US"` -- rather than
// as a tagged enum. The thin C++ Fcitx adapter and the CLI both write this
// field by hand, and `{"fixed":"de"}` would be a trap for both.
impl Serialize for LanguageMode {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for LanguageMode {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

impl LanguageMode {
    pub fn fixed(self) -> Option<Language> {
        match self {
            LanguageMode::Auto => None,
            LanguageMode::Fixed(language) => Some(language),
        }
    }
}

impl fmt::Display for LanguageMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LanguageMode::Auto => f.write_str("auto"),
            LanguageMode::Fixed(language) => language.fmt(f),
        }
    }
}

impl FromStr for LanguageMode {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.trim().eq_ignore_ascii_case("auto") {
            Ok(LanguageMode::Auto)
        } else {
            s.parse().map(LanguageMode::Fixed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_spellings() {
        for input in ["de", "DE", "de-DE", "de_DE", "German", " deutsch "] {
            assert_eq!(
                input.parse::<Language>().unwrap(),
                Language::German,
                "{input}"
            );
        }
        assert_eq!("en".parse::<Language>().unwrap(), Language::EnglishUs);
        assert_eq!("en-GB".parse::<Language>().unwrap(), Language::EnglishGb);
    }

    #[test]
    fn rejects_unknown_language() {
        assert!("klingon".parse::<Language>().is_err());
    }

    #[test]
    fn language_mode_roundtrip() {
        assert_eq!("auto".parse::<LanguageMode>().unwrap(), LanguageMode::Auto);
        assert_eq!(
            "de".parse::<LanguageMode>().unwrap(),
            LanguageMode::Fixed(Language::German)
        );
        assert_eq!(LanguageMode::Fixed(Language::German).to_string(), "de_DE");
    }

    #[test]
    fn language_mode_is_a_plain_string_on_the_wire() {
        // The C++ adapter writes this field by hand, so the shape matters.
        assert_eq!(
            serde_json::to_string(&LanguageMode::Auto).unwrap(),
            "\"auto\""
        );
        assert_eq!(
            serde_json::to_string(&LanguageMode::Fixed(Language::German)).unwrap(),
            "\"de_DE\""
        );
        for (json, expected) in [
            ("\"auto\"", LanguageMode::Auto),
            ("\"de\"", LanguageMode::Fixed(Language::German)),
            ("\"de-DE\"", LanguageMode::Fixed(Language::German)),
            ("\"en\"", LanguageMode::Fixed(Language::EnglishUs)),
            ("\"en_GB\"", LanguageMode::Fixed(Language::EnglishGb)),
        ] {
            let got: LanguageMode = serde_json::from_str(json).unwrap();
            assert_eq!(got, expected, "{json}");
        }
        assert!(serde_json::from_str::<LanguageMode>("\"klingon\"").is_err());
    }

    #[test]
    fn tag_is_stable_for_all_languages() {
        // Guards against a new variant being added without file-name wiring.
        for language in Language::ALL {
            assert!(!language.tag().is_empty());
            assert_eq!(language.tag().parse::<Language>().unwrap(), *language);
        }
    }
}
