use wordpilot_core::{Candidate, Language};

/// One dictionary word, before ranking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WordEntry {
    /// Surface form as the dictionary spells it, including capitalisation.
    pub word: String,
    /// True when the stored capitalisation matches the user's prefix exactly.
    /// German needs this: `bewerben` and `Bewerben` are different words.
    pub exact_case: bool,
}

/// A source of words for prefix completion.
///
/// Implementations are expected to be cheap to query (well under
/// 10 ms) and to do all loading up front, so this trait takes `&self` and is
/// not async.
pub trait DictionaryProvider: Send + Sync {
    fn language(&self) -> Language;

    /// Words starting with `prefix`, case-insensitively, at most `limit` of
    /// them. Order is unspecified: ranking is a separate concern.
    fn complete(&self, prefix: &str, limit: usize) -> Vec<WordEntry>;

    /// Whether `word` is spelled correctly. Used to decide whether to offer
    /// corrections at all.
    fn contains(&self, word: &str) -> bool;

    /// Corrections for a likely misspelling. These must be reported
    /// with `CandidateSource::SpellCorrection`, never silently applied.
    fn corrections(&self, word: &str, limit: usize) -> Vec<WordEntry>;

    /// Number of loaded words, for `wordpilot dictionary stats`.
    fn len(&self) -> usize;

    /// Where the words came from, for `status` and `doctor`.
    fn source_name(&self) -> String;

    /// Approximate heap footprint, for `wordpilot dictionary stats`.
    fn memory_bytes(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl WordEntry {
    pub fn new(word: impl Into<String>, exact_case: bool) -> Self {
        Self {
            word: word.into(),
            exact_case,
        }
    }

    /// Convenience for tests: turn an entry into an unranked candidate.
    pub fn into_candidate(self, source: wordpilot_core::CandidateSource) -> Candidate {
        Candidate::new(self.word, source, 0.0)
    }
}

/// Blanket impl so `Box<dyn DictionaryProvider>` is itself a provider.
impl<T: DictionaryProvider + ?Sized> DictionaryProvider for Box<T> {
    fn language(&self) -> Language {
        (**self).language()
    }
    fn complete(&self, prefix: &str, limit: usize) -> Vec<WordEntry> {
        (**self).complete(prefix, limit)
    }
    fn contains(&self, word: &str) -> bool {
        (**self).contains(word)
    }
    fn corrections(&self, word: &str, limit: usize) -> Vec<WordEntry> {
        (**self).corrections(word, limit)
    }
    fn len(&self) -> usize {
        (**self).len()
    }
    fn source_name(&self) -> String {
        (**self).source_name()
    }
    fn memory_bytes(&self) -> usize {
        (**self).memory_bytes()
    }
}
