//! Word sources for completion.
//!
//! The engine depends on the [`DictionaryProvider`] trait only; Hunspell is
//! one implementation behind it, so it can be replaced by an
//! aff-expanding reader, a trie file or a remote service later.

pub mod affix;
pub mod error;
pub mod frequency;
pub mod hunspell;
pub mod provider;
pub mod spell;

pub use affix::AffixTable;
pub use error::DictionaryError;
pub use frequency::FrequencyTable;
pub use hunspell::HunspellDictionary;
pub use provider::{DictionaryProvider, WordEntry};
