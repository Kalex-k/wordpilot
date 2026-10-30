use serde::{Deserialize, Serialize};
use wordpilot_core::{
    AiAction, AiRequest, AiResponse, ApplicationContext, CompletionRequest, CompletionResponse,
    GrammarRequest, GrammarResponse, Language, LanguageMode,
};

/// Bumped on any incompatible change. The daemon reports it in `Status` so a
/// mismatched Fcitx addon fails loudly instead of misbehaving.
pub const PROTOCOL_VERSION: u32 = 1;

/// A client request.
///
/// `#[serde(tag = "type")]` keeps the wire form self-describing, which matters
/// because the C++ bridge builds these by hand.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Request {
    /// Fast path: local prefix completion.
    Complete(CompletionRequest),

    /// The user accepted a candidate; a positive learning signal.
    ///
    /// `language` should be the `language` of the completion response the
    /// word came from. `"auto"` is accepted and resolved to the daemon's
    /// active language, falling back to `general.default_language`.
    AcceptCandidate {
        word: String,
        language: LanguageMode,
        #[serde(default)]
        application: ApplicationContext,
    },

    /// Deterministic grammar check.
    Grammar(GrammarRequest),

    /// One LLM transformation.
    Ai(AiRequest),

    /// All four AI actions at once, so the frontend can render four loading
    /// rows and fill them as they arrive.
    AiBatch {
        text: String,
        #[serde(default)]
        surrounding_context: Option<String>,
        language: Language,
        #[serde(default)]
        actions: Vec<AiAction>,
        #[serde(default)]
        application: ApplicationContext,
    },

    /// Change the active language.
    SetLanguage { language: LanguageMode },

    /// Daemon health and loaded resources, for `wordpilot status` and `doctor`.
    Status,
    /// The subset of the config the input-method frontend acts on itself
    /// (hotkeys, limits, blacklist), so the addon never parses TOML and picks
    /// up config changes without being rebuilt.
    FrontendConfig,
    /// Re-read the config file now. The daemon also does this on its own when
    /// the file changes and on SIGHUP.
    Reload,

    /// Round-trip check with no side effects.
    Ping,
}

/// A daemon response.
///
/// Errors are a response variant rather than a transport failure: a failed AI
/// call must not tear down the connection that autocomplete is using.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Response {
    Completion(CompletionResponse),
    Accepted,
    Grammar(GrammarResponse),
    Ai(AiResponse),
    /// One entry per requested action, in request order. An action that failed
    /// carries its own error so the others still render.
    AiBatch {
        results: Vec<AiBatchEntry>,
    },
    LanguageSet {
        language: LanguageMode,
    },
    Status(ServerStatus),
    FrontendConfig(FrontendConfig),
    Reloaded(ReloadOutcome),
    Pong,
    Error {
        /// Stable machine-readable code; see [`error_codes`].
        code: String,
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiBatchEntry {
    pub action: AiAction,
    #[serde(flatten)]
    pub outcome: AiOutcome,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum AiOutcome {
    Ok { text: String },
    Failed { message: String },
}

/// Stable error codes. Clients branch on these, never on the message text.
pub mod error_codes {
    pub const BAD_REQUEST: &str = "bad-request";
    pub const UNSUPPORTED: &str = "unsupported";
    pub const AI_DISABLED: &str = "ai-disabled";
    pub const AI_FAILED: &str = "ai-failed";
    pub const GRAMMAR_DISABLED: &str = "grammar-disabled";
    pub const GRAMMAR_FAILED: &str = "grammar-failed";
    pub const INTERNAL: &str = "internal";
}

/// What the Fcitx addon needs from the config. Hotkeys are already in the
/// spelling `fcitx::Key` parses; an empty string means "no hotkey".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrontendConfig {
    /// Bumped on every successful reload, so a client can tell whether what
    /// it cached is current.
    pub generation: u64,
    pub autocomplete_enabled: bool,
    pub min_prefix_length: usize,
    pub max_candidates: usize,
    pub password_field_detection: bool,
    pub sensitive_field_detection: bool,
    pub disabled_applications: Vec<String>,
    pub hotkeys: FrontendHotkeys,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrontendHotkeys {
    pub language_german: String,
    pub language_english: String,
    pub language_auto: String,
    pub ai: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReloadOutcome {
    pub generation: u64,
    /// False when the file was unchanged and nothing was rebuilt.
    pub changed: bool,
    /// True when `[dictionaries]` changed and the word lists were re-read.
    pub dictionaries_reloaded: bool,
    /// Settings that changed in the file but only take effect after a
    /// daemon restart, e.g. `general.socket_path`.
    pub restart_required: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerStatus {
    pub protocol_version: u32,
    pub daemon_version: String,
    pub active_language: LanguageMode,
    pub loaded_languages: Vec<LoadedLanguage>,
    pub autocomplete_enabled: bool,
    pub learning_enabled: bool,
    pub grammar_enabled: bool,
    pub ai_enabled: bool,
    pub uptime_secs: u64,
    /// See [`FrontendConfig::generation`].
    #[serde(default)]
    pub config_generation: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadedLanguage {
    pub language: Language,
    pub dictionary_words: usize,
    pub frequency_words: usize,
    pub dictionary_path: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use wordpilot_core::{Candidate, CandidateSource, TextContext};

    fn roundtrip_request(request: &Request) -> Request {
        let json = serde_json::to_string(request).unwrap();
        serde_json::from_str(&json).unwrap()
    }

    #[test]
    fn complete_request_roundtrips_with_umlauts() {
        let request = Request::Complete(CompletionRequest {
            context: TextContext::from_typed("Ich möchte mich bewä"),
            language: LanguageMode::Fixed(Language::German),
            application: ApplicationContext::named("firefox"),
            max_candidates: Some(5),
        });
        let back = roundtrip_request(&request);
        match back {
            Request::Complete(req) => {
                assert_eq!(req.context.before_cursor, "Ich möchte mich bewä");
                assert_eq!(req.context.current_word(Language::German), "bewä");
                assert_eq!(req.language, LanguageMode::Fixed(Language::German));
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn request_is_tagged_so_the_cpp_bridge_can_build_it_by_hand() {
        let json = serde_json::to_value(&Request::Ping).unwrap();
        assert_eq!(json["type"], "ping");
        let json = serde_json::to_value(&Request::SetLanguage {
            language: LanguageMode::Auto,
        })
        .unwrap();
        assert_eq!(json["type"], "set-language");
    }

    #[test]
    fn a_hand_written_minimal_complete_request_parses() {
        // Exactly what the thin C++ adapter is expected to emit: no optional
        // fields, short language alias.
        let json = r#"{"type":"complete","context":{"before_cursor":"bew"},"language":"de"}"#;
        let request: Request = serde_json::from_str(json).unwrap();
        match request {
            Request::Complete(req) => {
                assert_eq!(req.context.before_cursor, "bew");
                assert_eq!(req.context.after_cursor, "");
                assert!(req.max_candidates.is_none());
                assert!(!req.application.sensitive);
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn language_mode_auto_parses_from_a_bare_string() {
        let json = r#"{"type":"set-language","language":"auto"}"#;
        let request: Request = serde_json::from_str(json).unwrap();
        match request {
            Request::SetLanguage { language } => assert_eq!(language, LanguageMode::Auto),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn completion_response_roundtrips_byte_length() {
        let response = Response::Completion(CompletionResponse {
            candidates: vec![Candidate::new(
                "bewältigen",
                CandidateSource::Frequency,
                0.8,
            )],
            language: Language::German,
            prefix: "bewä".into(),
            prefix_len_bytes: 5,
        });
        let json = serde_json::to_string(&response).unwrap();
        let back: Response = serde_json::from_str(&json).unwrap();
        match back {
            Response::Completion(res) => {
                assert_eq!(res.prefix_len_bytes, 5);
                assert_eq!(res.prefix.chars().count(), 4);
                assert_eq!(res.candidates[0].text, "bewältigen");
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn ai_batch_entries_carry_per_action_failures() {
        let response = Response::AiBatch {
            results: vec![
                AiBatchEntry {
                    action: AiAction::Grammar,
                    outcome: AiOutcome::Ok {
                        text: "Ich habe viel Erfahrung.".into(),
                    },
                },
                AiBatchEntry {
                    action: AiAction::Rewrite,
                    outcome: AiOutcome::Failed {
                        message: "timeout".into(),
                    },
                },
            ],
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"status\":\"ok\""), "{json}");
        assert!(json.contains("\"status\":\"failed\""), "{json}");
        let back: Response = serde_json::from_str(&json).unwrap();
        match back {
            Response::AiBatch { results } => {
                assert_eq!(results.len(), 2);
                assert!(matches!(results[1].outcome, AiOutcome::Failed { .. }));
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn error_response_uses_stable_codes() {
        let json = serde_json::to_value(&Response::Error {
            code: error_codes::AI_DISABLED.into(),
            message: "AI is disabled in config".into(),
        })
        .unwrap();
        assert_eq!(json["type"], "error");
        assert_eq!(json["code"], "ai-disabled");
    }

    #[test]
    fn accept_candidate_takes_auto_or_a_concrete_language() {
        // Exactly what the Fcitx addon sends.
        let json = r#"{"type":"accept-candidate","word":"bewerben","language":"auto"}"#;
        match serde_json::from_str::<Request>(json).unwrap() {
            Request::AcceptCandidate { language, .. } => assert_eq!(language, LanguageMode::Auto),
            other => panic!("wrong variant: {other:?}"),
        }
        let json = r#"{"type":"accept-candidate","word":"bewerben","language":"de_DE"}"#;
        match serde_json::from_str::<Request>(json).unwrap() {
            Request::AcceptCandidate { language, .. } => {
                assert_eq!(language, LanguageMode::Fixed(Language::German))
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn frontend_config_and_reload_are_bare_tags() {
        for (json, expected) in [
            (r#"{"type":"frontend-config"}"#, "FrontendConfig"),
            (r#"{"type":"reload"}"#, "Reload"),
        ] {
            let request: Request = serde_json::from_str(json).unwrap();
            assert!(format!("{request:?}").starts_with(expected), "{request:?}");
        }
    }

    #[test]
    fn unknown_request_type_is_an_error_not_a_silent_default() {
        let err = serde_json::from_str::<Request>(r#"{"type":"teleport"}"#);
        assert!(err.is_err());
    }
}
