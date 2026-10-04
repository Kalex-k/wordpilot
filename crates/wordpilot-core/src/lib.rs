//! Core vocabulary of WordPilot.
//!
//! This crate deliberately knows nothing about X11, Wayland, Fcitx, HTTP or
//! SQLite. It only defines the types that flow between the input-method
//! frontend and the engines, plus configuration and XDG path resolution.

pub mod config;
pub mod error;
pub mod language;
pub mod paths;
pub mod text;
pub mod types;

pub use error::{CoreError, Result};
pub use language::{Language, LanguageMode};
pub use text::TextContext;
pub use types::{
    AiAction, AiRequest, AiResponse, ApplicationContext, Candidate, CandidateSource,
    CompletionRequest, CompletionResponse, GrammarMatch, GrammarRequest, GrammarResponse,
};
