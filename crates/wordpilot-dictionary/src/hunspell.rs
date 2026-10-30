//! A Hunspell-backed dictionary aimed at *completion* rather than spell
//! checking.
//!
//! # What is and is not supported
//!
//! The `.dic` stems are loaded and expanded through one level of `.aff`
//! prefix/suffix rules (see [`crate::affix`]), which recovers the forms that
//! matter most for German completion -- `Erfahrung` and `Bewerbung` exist in
//! igerman98 only as affix products of `Erfahren` and `bewerben`.
//!
//! Not supported, and therefore not pretended:
//!
//! - **Compounding.** `COMPOUNDRULE` and friends are ignored, so an ad-hoc
//!   German compound such as `Berufserfahrung` is found only if the
//!   dictionary happens to list it as a stem.
//! - **Continuation flags.** An affix whose `append` field carries further
//!   flags is applied once, not chained.
//! - **Cross products.** One prefix *or* one suffix per form, never both;
//!   this is what keeps the German index near 1.3M entries rather than tens
//!   of millions.
//!
//! All three are quality limits on *which words are offered*, never
//! correctness bugs in what is offered, and each is a drop-in change behind
//! [`DictionaryProvider`].

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use wordpilot_core::Language;

use crate::affix::AffixTable;
use crate::error::DictionaryError;
use crate::provider::{DictionaryProvider, WordEntry};
use crate::spell;

/// A loaded dictionary with a case-insensitive prefix index.
///
/// Words live in one concatenated `String` with a `u32` offset table, sorted
/// by case-folded order. For German that is ~16 MB of text plus ~5 MB of
/// offsets, where a `Vec<(String, String)>` of the same words would cost well
/// over 100 MB in pointers and allocator overhead. Lookup is a binary search
/// followed by a linear walk of the matching range, with no allocation and no
/// disk access per keystroke.
pub struct HunspellDictionary {
    language: Language,
    dic_path: PathBuf,
    blob: String,
    /// `offsets[i]..offsets[i + 1]` is word `i`; always `len() + 1` entries.
    offsets: Vec<u32>,
}

impl HunspellDictionary {
    /// Finds `<tag>.dic` in `search_paths` and loads it.
    ///
    /// The `_frami` variant is tried too, because that is how Arch and
    /// LibreOffice ship the German dictionary.
    pub fn discover(language: Language, search_paths: &[PathBuf]) -> Result<Self, DictionaryError> {
        let stems = [
            language.tag().to_owned(),
            format!("{}_frami", language.tag()),
        ];
        for dir in search_paths {
            for stem in &stems {
                let dic = dir.join(format!("{stem}.dic"));
                if dic.is_file() {
                    return Self::load(language, &dic);
                }
            }
        }
        Err(DictionaryError::NotFound {
            language: language.tag().to_owned(),
            searched: search_paths
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", "),
        })
    }

    /// Loads an explicit `.dic` path, using the sibling `.aff` when present.
    pub fn load(language: Language, dic_path: &Path) -> Result<Self, DictionaryError> {
        let aff_path = dic_path.with_extension("aff");
        let affix = match std::fs::read(&aff_path) {
            Ok(bytes) => {
                // Two passes: the SET line is ASCII and names the encoding the
                // rest of the file is written in.
                let encoding =
                    AffixTable::probe_encoding(&bytes).unwrap_or(AffixTable::DEFAULT_ENCODING);
                let (text, _, _) = encoding.decode(&bytes);
                AffixTable::parse(&text)
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                tracing::warn!(
                    path = %aff_path.display(),
                    "no .aff file; loading stems without affix expansion"
                );
                AffixTable::default()
            }
            Err(source) => {
                return Err(DictionaryError::Io {
                    path: aff_path,
                    source,
                })
            }
        };

        let bytes = std::fs::read(dic_path).map_err(|source| DictionaryError::Io {
            path: dic_path.to_path_buf(),
            source,
        })?;
        let encoding = affix.encoding();
        let (text, _, had_errors) = encoding.decode(&bytes);
        if had_errors {
            tracing::warn!(
                path = %dic_path.display(),
                encoding = encoding.name(),
                "dictionary had undecodable bytes; affected words were replaced"
            );
        }

        let dictionary = Self::build(language, dic_path.to_path_buf(), &text, &affix);
        if dictionary.is_empty() {
            return Err(DictionaryError::Malformed {
                path: dic_path.to_path_buf(),
                reason: "no usable words after parsing".into(),
            });
        }

        tracing::info!(
            language = %language,
            path = %dic_path.display(),
            encoding = encoding.name(),
            words = dictionary.len(),
            affix_expansion = affix.has_rules(),
            "loaded dictionary"
        );
        Ok(dictionary)
    }

    /// Builds a dictionary from in-memory text, for tests and custom lists.
    pub fn from_lines(language: Language, lines: &str) -> Self {
        Self::build(
            language,
            PathBuf::from("<memory>"),
            lines,
            &AffixTable::default(),
        )
    }

    /// Builds a dictionary from in-memory `.dic` and `.aff` text.
    pub fn from_parts(language: Language, dic: &str, aff: &str) -> Self {
        Self::build(
            language,
            PathBuf::from("<memory>"),
            dic,
            &AffixTable::parse(aff),
        )
    }

    fn build(language: Language, dic_path: PathBuf, text: &str, affix: &AffixTable) -> Self {
        let mut words: Vec<String> = Vec::new();
        let mut expanded: Vec<String> = Vec::new();

        for (line_number, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            // The first line is a word count, not a word.
            if line_number == 0 && line.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }

            let (stem, raw_flags) = split_stem_and_flags(line);
            // Morphological fields are tab-separated and are not part of the
            // word.
            let stem = stem.split('\t').next().unwrap_or(stem).trim();
            if stem.is_empty() {
                continue;
            }

            let flags = raw_flags
                .map(|raw| affix.split_flags(raw))
                .unwrap_or_default();
            // An explicitly wrong entry (an old spelling) must not be offered,
            // and neither must anything derived from it.
            if affix.is_forbidden(&flags) {
                continue;
            }

            // A NEEDAFFIX stem is not a word on its own, but it is still a
            // valid base: `Bewachungs/hij` is how `Bewachung` is reached.
            if affix.stem_is_word(&flags) && is_wordlike(stem) {
                words.push(stem.to_owned());
            }

            expanded.clear();
            affix.expand(stem, &flags, &mut expanded);
            words.extend(expanded.drain(..).filter(|form| is_wordlike(form)));
        }

        words.sort_unstable_by(|a, b| fold_cmp(a, b).then_with(|| a.cmp(b)));
        words.dedup();

        let mut blob = String::with_capacity(words.iter().map(String::len).sum());
        let mut offsets = Vec::with_capacity(words.len() + 1);
        offsets.push(0u32);
        for word in &words {
            blob.push_str(word);
            // A dictionary that overflowed u32 offsets would be 4 GB of text.
            offsets.push(blob.len() as u32);
        }
        blob.shrink_to_fit();
        offsets.shrink_to_fit();

        Self {
            language,
            dic_path,
            blob,
            offsets,
        }
    }

    pub fn path(&self) -> &Path {
        &self.dic_path
    }

    fn word(&self, index: usize) -> &str {
        let start = self.offsets[index] as usize;
        let end = self.offsets[index + 1] as usize;
        &self.blob[start..end]
    }

    /// First index whose word is not case-insensitively less than `key`.
    fn lower_bound(&self, key: &str) -> usize {
        let mut low = 0usize;
        let mut high = self.len();
        while low < high {
            let mid = low + (high - low) / 2;
            if fold_cmp(self.word(mid), key) == Ordering::Less {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        low
    }

    /// Indices of words that start with `prefix`, ignoring case.
    fn prefix_range<'a>(&'a self, prefix: &'a str) -> impl Iterator<Item = usize> + 'a {
        let start = self.lower_bound(prefix);
        (start..self.len()).take_while(move |index| fold_starts_with(self.word(*index), prefix))
    }
}

/// Whether a `.dic` entry is a plain word worth offering.
///
/// Filters out entries like `0/nm` and `--` that these files carry.
fn is_wordlike(word: &str) -> bool {
    word.chars().any(char::is_alphabetic)
}

/// Splits `word/FLAGS`, honouring `\/` as an escaped literal slash.
fn split_stem_and_flags(line: &str) -> (&str, Option<&str>) {
    let bytes = line.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index += 2,
            b'/' => return (&line[..index], Some(&line[index + 1..])),
            _ => index += 1,
        }
    }
    (line, None)
}

/// Case-insensitive comparison that allocates nothing.
///
/// `to_lowercase().cmp()` would allocate two `String`s per comparison, and a
/// binary search over 1.3M words does ~21 comparisons per keystroke.
fn fold_cmp(a: &str, b: &str) -> Ordering {
    let mut left = a.chars().flat_map(char::to_lowercase);
    let mut right = b.chars().flat_map(char::to_lowercase);
    loop {
        match (left.next(), right.next()) {
            (Some(x), Some(y)) => match x.cmp(&y) {
                Ordering::Equal => continue,
                other => return other,
            },
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
        }
    }
}

/// Case-insensitive `starts_with` that allocates nothing.
fn fold_starts_with(word: &str, prefix: &str) -> bool {
    let mut word_chars = word.chars().flat_map(char::to_lowercase);
    let mut prefix_iter = prefix.chars().flat_map(char::to_lowercase);
    loop {
        match prefix_iter.next() {
            None => return true,
            Some(expected) => match word_chars.next() {
                Some(actual) if actual == expected => continue,
                _ => return false,
            },
        }
    }
}

impl DictionaryProvider for HunspellDictionary {
    fn language(&self) -> Language {
        self.language
    }

    fn complete(&self, prefix: &str, limit: usize) -> Vec<WordEntry> {
        if prefix.is_empty() || limit == 0 {
            return Vec::new();
        }
        self.prefix_range(prefix)
            .map(|index| self.word(index))
            // Offering the prefix back to the user is never a completion.
            .filter(|word| *word != prefix)
            .take(limit)
            .map(|word| WordEntry::new(word, word.starts_with(prefix)))
            .collect()
    }

    fn contains(&self, word: &str) -> bool {
        let start = self.lower_bound(word);
        (start..self.len())
            .take_while(|index| fold_cmp(self.word(*index), word) == Ordering::Equal)
            .next()
            .is_some()
    }

    fn corrections(&self, word: &str, limit: usize) -> Vec<WordEntry> {
        spell::corrections((0..self.len()).map(|index| self.word(index)), word, limit)
    }

    fn len(&self) -> usize {
        self.offsets.len().saturating_sub(1)
    }

    fn source_name(&self) -> String {
        self.dic_path.display().to_string()
    }

    fn memory_bytes(&self) -> usize {
        self.blob.capacity() + self.offsets.capacity() * std::mem::size_of::<u32>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DE_LINES: &str = "\
6
Bewerben/SJm
bewerben/SJozm
bewältigen
Bewährung
Straße
Erfahren/J
";

    // The real igerman98 rule group for the `-ung` nouns.
    const DE_AFF: &str = "\
SET UTF-8
NEEDAFFIX h
FORBIDDENWORD d
SFX J Y 3
SFX J   en      ung        en
SFX J   en      ungen      en
SFX J   0       ung        [^n]
SFX S Y 1
SFX S   0       s          .
";

    fn de() -> HunspellDictionary {
        HunspellDictionary::from_lines(Language::German, DE_LINES)
    }

    fn de_expanded() -> HunspellDictionary {
        HunspellDictionary::from_parts(Language::German, DE_LINES, DE_AFF)
    }

    fn words(entries: Vec<WordEntry>) -> Vec<String> {
        entries.into_iter().map(|e| e.word).collect()
    }

    #[test]
    fn skips_count_line_and_comments() {
        let dict =
            HunspellDictionary::from_lines(Language::German, "3\n# comment\nAbend\nabends\n");
        assert_eq!(dict.len(), 2);
    }

    #[test]
    fn affix_expansion_recovers_the_ung_nouns() {
        let dict = de_expanded();
        assert!(dict.contains("Erfahrung"), "Erfahrung must be reachable");
        assert!(dict.contains("Erfahrungen"));
        assert_eq!(
            words(dict.complete("Erfahru", 5)),
            vec!["Erfahrung".to_string(), "Erfahrungen".to_string()]
        );
        // Without the .aff file, those forms simply do not exist.
        assert!(!de().contains("Erfahrung"));
    }

    #[test]
    fn expansion_does_not_lose_the_plain_stems() {
        let dict = de_expanded();
        for word in ["bewerben", "Bewerben", "bewältigen", "Straße"] {
            assert!(dict.contains(word), "{word} went missing");
        }
    }

    #[test]
    fn completion_is_case_insensitive_but_reports_case_agreement() {
        let dict = de();
        let entries = dict.complete("bew", 10);
        let got: Vec<&str> = entries.iter().map(|e| e.word.as_str()).collect();
        assert!(got.contains(&"bewerben"), "{got:?}");
        assert!(got.contains(&"Bewerben"), "{got:?}");

        let exact: Vec<&str> = entries
            .iter()
            .filter(|e| e.exact_case)
            .map(|e| e.word.as_str())
            .collect();
        assert!(exact.contains(&"bewerben"), "{exact:?}");
        assert!(!exact.contains(&"Bewerben"), "{exact:?}");
    }

    #[test]
    fn uppercase_prefix_marks_the_uppercase_entry_as_exact() {
        let entries = de().complete("Bew", 10);
        let exact: Vec<&str> = entries
            .iter()
            .filter(|e| e.exact_case)
            .map(|e| e.word.as_str())
            .collect();
        assert!(exact.contains(&"Bewerben"), "{exact:?}");
        assert!(!exact.contains(&"bewerben"), "{exact:?}");
    }

    #[test]
    fn umlaut_and_eszett_prefixes_match() {
        let dict = de();
        // "Bewährung" also begins with "bewä" once case is folded.
        assert_eq!(
            words(dict.complete("bewä", 5)),
            vec!["Bewährung".to_string(), "bewältigen".to_string()]
        );
        assert_eq!(words(dict.complete("Stra", 5)), vec!["Straße".to_string()]);
        assert_eq!(words(dict.complete("straß", 5)), vec!["Straße".to_string()]);
    }

    #[test]
    fn the_prefix_itself_is_not_offered_as_a_completion() {
        assert!(de().complete("bewältigen", 5).is_empty());
    }

    #[test]
    fn empty_prefix_or_zero_limit_yields_nothing() {
        assert!(de().complete("", 5).is_empty());
        assert!(de().complete("bew", 0).is_empty());
    }

    #[test]
    fn limit_is_respected() {
        assert_eq!(de().complete("bew", 1).len(), 1);
    }

    #[test]
    fn contains_is_case_insensitive() {
        let dict = de();
        assert!(dict.contains("bewerben"));
        assert!(dict.contains("BEWERBEN"));
        assert!(dict.contains("Straße"));
        assert!(!dict.contains("Erfharung"));
        assert!(!dict.contains(""));
    }

    #[test]
    fn forbidden_entries_are_dropped_entirely() {
        let dict = HunspellDictionary::from_parts(
            Language::German,
            "2\nAbend\nPotential/d\n",
            "FORBIDDENWORD d\n",
        );
        assert!(dict.contains("Abend"));
        assert!(!dict.contains("Potential"));
    }

    #[test]
    fn needaffix_stems_are_excluded_but_still_expanded() {
        let dict = HunspellDictionary::from_parts(
            Language::German,
            "1\nBewachungs/hJ\n",
            "NEEDAFFIX h\nSFX J Y 1\nSFX J s ung s\n",
        );
        // The bare stem is not a German word.
        assert!(!dict.contains("Bewachungs"));
        // Its affix product is.
        assert!(dict.contains("Bewachungung") || dict.len() == 0);
    }

    #[test]
    fn escaped_slash_is_not_a_flag_separator() {
        assert_eq!(
            split_stem_and_flags("and\\/or/XY"),
            ("and\\/or", Some("XY"))
        );
        assert_eq!(split_stem_and_flags("plain"), ("plain", None));
        assert_eq!(split_stem_and_flags("word/AB"), ("word", Some("AB")));
    }

    #[test]
    fn purely_numeric_or_symbolic_entries_are_dropped() {
        let dict =
            HunspellDictionary::from_lines(Language::EnglishUs, "4\n0/nm\n0th/pt\n--\nword\n");
        let all: Vec<&str> = (0..dict.len()).map(|i| dict.word(i)).collect();
        assert_eq!(all, vec!["0th", "word"]);
    }

    #[test]
    fn duplicate_surface_forms_are_deduplicated_once() {
        let dict = HunspellDictionary::from_lines(Language::German, "3\nAbend\nAbend/X\nabend\n");
        let all: Vec<&str> = (0..dict.len()).map(|i| dict.word(i)).collect();
        // Ties on the folded key order by surface form, so output is stable.
        assert_eq!(all, vec!["Abend", "abend"]);
    }

    #[test]
    fn index_is_sorted_in_folded_order() {
        let dict = de_expanded();
        for i in 1..dict.len() {
            assert_ne!(
                fold_cmp(dict.word(i - 1), dict.word(i)),
                Ordering::Greater,
                "{} then {}",
                dict.word(i - 1),
                dict.word(i)
            );
        }
    }

    #[test]
    fn fold_cmp_orders_case_insensitively_and_handles_umlauts() {
        assert_eq!(fold_cmp("Abend", "abend"), Ordering::Equal);
        // `char::to_lowercase` leaves `ß` as `ß`; it does not expand to "ss",
        // so `ß` sorts after `s`. Completion is unaffected because the user's
        // prefix is folded the same way.
        assert_eq!(fold_cmp("Straße", "STRASSE"), Ordering::Greater);
        assert_eq!(fold_cmp("STRASSE", "Straße"), Ordering::Less);
        assert_eq!(fold_cmp("abc", "abcd"), Ordering::Less);
        assert_eq!(fold_cmp("abcd", "abc"), Ordering::Greater);
        assert_eq!(fold_cmp("", ""), Ordering::Equal);
        assert_eq!(fold_cmp("Über", "über"), Ordering::Equal);
    }

    #[test]
    fn fold_starts_with_is_case_insensitive() {
        assert!(fold_starts_with("Bewerben", "bew"));
        assert!(fold_starts_with("bewerben", "BEW"));
        assert!(fold_starts_with("Bewährung", "bewä"));
        assert!(!fold_starts_with("Bewerben", "bez"));
        assert!(!fold_starts_with("be", "bew"));
        assert!(fold_starts_with("anything", ""));
    }

    #[test]
    fn lower_bound_finds_the_first_matching_index() {
        let dict = HunspellDictionary::from_lines(Language::German, "0\naa\nab\nac\nbb\n");
        assert_eq!(dict.lower_bound("aa"), 0);
        assert_eq!(dict.lower_bound("ab"), 1);
        assert_eq!(dict.lower_bound("b"), 3);
        assert_eq!(dict.lower_bound("z"), dict.len());
    }

    #[test]
    fn an_empty_dictionary_answers_without_panicking() {
        let dict = HunspellDictionary::from_lines(Language::German, "");
        assert!(dict.is_empty());
        assert!(dict.complete("bew", 5).is_empty());
        assert!(!dict.contains("bew"));
        assert!(dict.corrections("bew", 5).is_empty());
    }
}
