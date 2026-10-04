use serde::{Deserialize, Serialize};

use crate::language::Language;

/// The text surrounding the caret, as reported by the input-method frontend.
///
/// All offsets inside this type are *byte* offsets into the respective
/// `String`, because that is what Rust slicing needs. Never treat them as
/// character counts (spec §45).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextContext {
    /// Text to the left of the caret, within the current field/paragraph.
    #[serde(default)]
    pub before_cursor: String,
    /// Text to the right of the caret.
    #[serde(default)]
    pub after_cursor: String,
    /// The user's selection, if any (spec §27).
    #[serde(default)]
    pub selection: Option<String>,
}

impl TextContext {
    pub fn new(before_cursor: impl Into<String>, after_cursor: impl Into<String>) -> Self {
        Self {
            before_cursor: before_cursor.into(),
            after_cursor: after_cursor.into(),
            selection: None,
        }
    }

    /// Convenience for tests and for frontends that only have a keystroke
    /// buffer: everything typed so far, caret at the end.
    pub fn from_typed(before_cursor: impl Into<String>) -> Self {
        Self::new(before_cursor, "")
    }

    pub fn with_selection(mut self, selection: impl Into<String>) -> Self {
        self.selection = Some(selection.into());
        self
    }

    /// The partial word immediately left of the caret.
    ///
    /// Returns an empty string when the caret sits after a separator, which is
    /// the signal for next-word prediction rather than prefix completion.
    pub fn current_word(&self, language: Language) -> &str {
        let start = word_start(&self.before_cursor, language);
        &self.before_cursor[start..]
    }

    /// Byte offset in `before_cursor` where `current_word` begins.
    ///
    /// The frontend needs this to know how many bytes to replace on commit.
    pub fn current_word_start(&self, language: Language) -> usize {
        word_start(&self.before_cursor, language)
    }

    /// Whether the caret is in the middle of a word, i.e. the character to the
    /// right is also a word character. Completion is suppressed there, because
    /// committing would corrupt the tail of the word.
    pub fn caret_inside_word(&self, language: Language) -> bool {
        self.after_cursor
            .chars()
            .next()
            .is_some_and(|c| is_word_char(c, language))
    }

    /// The last `count` complete words before the current partial word, oldest
    /// first. Used as n-gram context and for language detection.
    pub fn preceding_words(&self, language: Language, count: usize) -> Vec<&str> {
        if count == 0 {
            return Vec::new();
        }
        let start = word_start(&self.before_cursor, language);
        let committed = &self.before_cursor[..start];
        let mut words: Vec<&str> = committed
            .split(|c: char| !is_word_char(c, language))
            .filter(|w| !w.is_empty())
            .collect();
        if words.len() > count {
            words.drain(..words.len() - count);
        }
        words
    }

    /// The sentence the caret sits in, used as the AI target when there is no
    /// selection (spec §27).
    pub fn current_sentence(&self) -> &str {
        let start = self
            .before_cursor
            .char_indices()
            .rev()
            .find(|(_, c)| matches!(c, '.' | '!' | '?' | '\n'))
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(0);
        self.before_cursor[start..].trim_start()
    }

    /// What the AI should operate on: the selection if present, otherwise the
    /// current sentence. Never the whole document (spec §27).
    pub fn ai_target(&self) -> &str {
        match self.selection.as_deref() {
            Some(selection) if !selection.trim().is_empty() => selection,
            _ => self.current_sentence(),
        }
    }
}

/// Whether `c` participates in a word for the purposes of prefix extraction.
pub fn is_word_char(c: char, language: Language) -> bool {
    c.is_alphabetic() || language.extra_word_chars().contains(&c)
}

/// Byte offset of the start of the trailing word in `text`.
///
/// Walks backwards over `char_indices`, so it is correct for multi-byte
/// characters such as `ä`, `ö`, `ü` and `ß`.
fn word_start(text: &str, language: Language) -> usize {
    let mut start = text.len();
    for (index, c) in text.char_indices().rev() {
        if is_word_char(c, language) {
            start = index;
        } else {
            break;
        }
    }
    // A leading hyphen is a separator, not the start of a word.
    let trimmed = &text[start..];
    match trimmed.char_indices().find(|(_, c)| c.is_alphabetic()) {
        Some((offset, _)) if offset > 0 => start + offset,
        Some(_) => start,
        None => text.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DE: Language = Language::German;
    const EN: Language = Language::EnglishUs;

    #[test]
    fn extracts_simple_prefix() {
        let ctx = TextContext::from_typed("Ich möchte mich bew");
        assert_eq!(ctx.current_word(DE), "bew");
    }

    #[test]
    fn empty_prefix_after_space() {
        let ctx = TextContext::from_typed("Ich interessiere mich ");
        assert_eq!(ctx.current_word(DE), "");
    }

    #[test]
    fn empty_context_is_empty_prefix() {
        let ctx = TextContext::default();
        assert_eq!(ctx.current_word(DE), "");
        assert_eq!(ctx.current_word_start(DE), 0);
    }

    #[test]
    fn umlauts_are_word_characters_and_byte_offsets_are_respected() {
        let ctx = TextContext::from_typed("Die Bewährung über");
        assert_eq!(ctx.current_word(DE), "über");
        // "über" starts at a byte offset that is NOT its character offset.
        let start = ctx.current_word_start(DE);
        assert_eq!(&ctx.before_cursor[start..], "über");
        assert_ne!(start, ctx.before_cursor.chars().count() - 4);
    }

    #[test]
    fn eszett_and_sharp_s_prefixes() {
        for word in ["größ", "Straße", "Fußgängerübergang", "weiß"] {
            let typed = format!("ein {word}");
            let ctx = TextContext::from_typed(typed);
            assert_eq!(ctx.current_word(DE), word);
        }
    }

    #[test]
    fn prefix_boundary_is_a_char_boundary_for_every_truncation() {
        // Simulates the user typing "bewältigen" one character at a time.
        let word = "bewältigen";
        let mut typed = String::from("Ich will ");
        for c in word.chars() {
            typed.push(c);
            let ctx = TextContext::from_typed(typed.clone());
            let start = ctx.current_word_start(DE);
            assert!(ctx.before_cursor.is_char_boundary(start));
            assert!(word.starts_with(ctx.current_word(DE)));
        }
    }

    #[test]
    fn hyphen_joins_german_compounds_but_not_leading() {
        let ctx = TextContext::from_typed("Java-Erfahr");
        assert_eq!(ctx.current_word(DE), "Java-Erfahr");
        let ctx = TextContext::from_typed("Liste: -bew");
        assert_eq!(ctx.current_word(DE), "bew");
    }

    #[test]
    fn apostrophe_is_a_word_char_in_english_only() {
        assert_eq!(TextContext::from_typed("don'").current_word(EN), "don'");
        assert_eq!(TextContext::from_typed("don'").current_word(DE), "");
    }

    #[test]
    fn digits_terminate_a_word() {
        let ctx = TextContext::from_typed("version2 bew");
        assert_eq!(ctx.current_word(DE), "bew");
        let ctx = TextContext::from_typed("abc123");
        assert_eq!(ctx.current_word(DE), "");
    }

    #[test]
    fn detects_caret_inside_word() {
        let ctx = TextContext::new("bew", "erben");
        assert!(ctx.caret_inside_word(DE));
        let ctx = TextContext::new("bew", " erben");
        assert!(!ctx.caret_inside_word(DE));
        assert!(!TextContext::from_typed("bew").caret_inside_word(DE));
    }

    #[test]
    fn preceding_words_are_oldest_first_and_exclude_partial() {
        let ctx = TextContext::from_typed("Ich interessiere mich für Back");
        assert_eq!(ctx.preceding_words(DE, 2), vec!["mich", "für"]);
        assert_eq!(
            ctx.preceding_words(DE, 10),
            vec!["Ich", "interessiere", "mich", "für"]
        );
        assert!(ctx.preceding_words(DE, 0).is_empty());
    }

    #[test]
    fn preceding_words_after_separator_include_last_word() {
        let ctx = TextContext::from_typed("Ich warte ");
        assert_eq!(ctx.preceding_words(DE, 2), vec!["Ich", "warte"]);
    }

    #[test]
    fn current_sentence_stops_at_punctuation() {
        let ctx = TextContext::from_typed("Hallo. Ich habe viele Erfahrung");
        assert_eq!(ctx.current_sentence(), "Ich habe viele Erfahrung");
        let ctx = TextContext::from_typed("Nur ein Satz");
        assert_eq!(ctx.current_sentence(), "Nur ein Satz");
    }

    #[test]
    fn ai_target_prefers_selection() {
        let ctx = TextContext::from_typed("Erster Satz. Zweiter").with_selection("Erster Satz");
        assert_eq!(ctx.ai_target(), "Erster Satz");
        let blank = TextContext::from_typed("Erster Satz. Zweiter").with_selection("   ");
        assert_eq!(blank.ai_target(), "Zweiter");
    }
}
