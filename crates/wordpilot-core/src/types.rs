use serde::{Deserialize, Serialize};

use crate::language::{Language, LanguageMode};
use crate::text::TextContext;

/// Where a candidate came from. The frontend may style sources differently,
/// and spell corrections must stay distinguishable from completions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CandidateSource {
    /// Prefix match from a system dictionary.
    Dictionary,
    /// Surfaced primarily because of corpus frequency.
    Frequency,
    /// From the user's learned personal dictionary.
    Personal,
    /// Predicted next word from a context model.
    Context,
    /// A correction of a likely misspelling, not a completion.
    SpellCorrection,
}

/// One thing the user could insert.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    /// The full surface form, not the remaining suffix.
    pub text: String,
    pub source: CandidateSource,
    /// Final ranking score. Higher is better; not normalised to any range.
    pub score: f32,
}

impl Candidate {
    pub fn new(text: impl Into<String>, source: CandidateSource, score: f32) -> Self {
        Self {
            text: text.into(),
            source,
            score,
        }
    }
}

/// Which application the text is being typed into.
///
/// Carries identifiers the frontend can cheaply obtain from the input method;
/// the core uses them only for blacklisting and per-app ranking. Note there is
/// deliberately no window handle or X11 id here.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplicationContext {
    /// Process or desktop-file name, e.g. `firefox`, `org.keepassxc.KeePassXC`.
    #[serde(default)]
    pub name: Option<String>,
    /// True when the input method reports a field flagged as sensitive
    /// (e.g. a credit-card or one-time-code field). WordPilot disables itself
    /// for these when `privacy.sensitive_field_detection` is on.
    #[serde(default)]
    pub sensitive: bool,
    /// True when the input method reports a password field. WordPilot disables
    /// itself for these when `privacy.password_field_detection` is on.
    #[serde(default)]
    pub password: bool,
}

impl ApplicationContext {
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: Some(name.into()),
            ..Self::default()
        }
    }

    pub fn sensitive() -> Self {
        Self {
            sensitive: true,
            ..Self::default()
        }
    }

    pub fn password() -> Self {
        Self {
            password: true,
            ..Self::default()
        }
    }

    /// Whether this is a password or sensitive field at all, regardless of
    /// configuration. Nothing typed into one is ever learned.
    pub fn is_private_field(&self) -> bool {
        self.sensitive || self.password
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionRequest {
    pub context: TextContext,
    /// `Auto` leaves language choice to the detector.
    pub language: LanguageMode,
    #[serde(default)]
    pub application: ApplicationContext,
    /// Upper bound on returned candidates; the daemon also clamps this to the
    /// configured maximum so a client cannot ask for ten thousand.
    #[serde(default)]
    pub max_candidates: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionResponse {
    pub candidates: Vec<Candidate>,
    /// The language actually used, after resolving `Auto`.
    pub language: Language,
    /// The prefix the candidates complete. The frontend replaces exactly this
    /// many bytes before the caret when committing.
    pub prefix: String,
    /// Byte length of `prefix`, repeated for frontends that cannot measure
    /// UTF-8 themselves (the C++ Fcitx bridge works in bytes).
    pub prefix_len_bytes: usize,
}

impl CompletionResponse {
    pub fn empty(language: Language) -> Self {
        Self {
            candidates: Vec::new(),
            language,
            prefix: String::new(),
            prefix_len_bytes: 0,
        }
    }
}

/// Semantic transformations, performed by an LLM. Kept open for the planned
/// `Explain`, `Shorten` and formality actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AiAction {
    /// Minimal corrections only; never a stylistic rewrite.
    Grammar,
    /// Idiomatic phrasing, preserving meaning, register, person and tense.
    Natural,
    /// Free to restructure the sentence.
    Rewrite,
    /// Continue the user's thought.
    Continue,
}

impl AiAction {
    pub const DEFAULT_SET: &'static [AiAction] = &[
        AiAction::Grammar,
        AiAction::Natural,
        AiAction::Rewrite,
        AiAction::Continue,
    ];

    pub fn label(self) -> &'static str {
        match self {
            AiAction::Grammar => "Grammar",
            AiAction::Natural => "Natural",
            AiAction::Rewrite => "Rewrite",
            AiAction::Continue => "Continue",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiRequest {
    /// The text to transform: a selection or the current sentence.
    pub text: String,
    /// Bounded surrounding text for coherence. Never the whole document.
    #[serde(default)]
    pub surrounding_context: Option<String>,
    pub language: Language,
    pub action: AiAction,
    #[serde(default)]
    pub tone: Option<String>,
    #[serde(default)]
    pub application: ApplicationContext,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiResponse {
    pub action: AiAction,
    pub text: String,
    /// Optional rationale, used by the future `Explain` action.
    #[serde(default)]
    pub explanation: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrammarRequest {
    pub text: String,
    pub language: Language,
}

/// A single deterministic grammar finding, e.g. from LanguageTool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrammarMatch {
    /// Byte offset into the checked text.
    pub offset: usize,
    /// Byte length of the flagged span.
    pub length: usize,
    pub message: String,
    #[serde(default)]
    pub replacements: Vec<String>,
    #[serde(default)]
    pub rule_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrammarResponse {
    pub matches: Vec<GrammarMatch>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_source_serializes_as_kebab_case() {
        let json = serde_json::to_string(&CandidateSource::SpellCorrection).unwrap();
        assert_eq!(json, "\"spell-correction\"");
    }

    #[test]
    fn ai_action_roundtrips() {
        for action in AiAction::DEFAULT_SET {
            let json = serde_json::to_string(action).unwrap();
            let back: AiAction = serde_json::from_str(&json).unwrap();
            assert_eq!(&back, action);
        }
    }

    #[test]
    fn application_context_defaults_to_not_sensitive() {
        let ctx: ApplicationContext = serde_json::from_str("{}").unwrap();
        assert!(!ctx.sensitive);
        assert!(ctx.name.is_none());
    }
}
