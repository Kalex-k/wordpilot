//! The fast path (spec §56): dictionary and frequency only, no network, no
//! LLM, no per-keystroke disk access.

pub mod detect;
pub mod engine;
pub mod personal;
pub mod predict;

pub use detect::{HeuristicLanguageDetector, LanguageDetector};
pub use engine::{AutocompleteEngine, LanguageResources};
pub use personal::{NoPersonalDictionary, PersonalStats, PersonalVocabulary};
pub use predict::{ContextPredictor, NoContextPredictor, Prediction};
