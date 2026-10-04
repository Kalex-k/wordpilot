//! Hunspell `.aff` parsing and affix expansion.
//!
//! Only what completion needs: the directives that decide whether a stem is a
//! standalone word, and the `PFX`/`SFX` rules that generate surface forms.
//! Compounding (`COMPOUNDRULE`, `COMPOUNDMIN`) is not implemented, so German
//! ad-hoc compounds such as `Berufserfahrung` are only found when the
//! dictionary lists them as stems. That is a documented gap, not a silent one.

use std::collections::{HashMap, HashSet};

use encoding_rs::Encoding;

/// How flags are spelled in the `.dic` file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FlagKind {
    /// One flag per character (Hunspell's default).
    #[default]
    Single,
    /// Two ASCII characters per flag.
    Long,
    /// Comma-separated decimal numbers.
    Numeric,
    /// One flag per Unicode scalar.
    Utf8,
}

/// One element of a rule condition: a single position in the stem.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ClassItem {
    /// `.` -- any character.
    Any,
    /// A literal character.
    Literal(char),
    /// `[abc]`, or `[^abc]` when negated.
    Set { chars: Vec<char>, negated: bool },
}

impl ClassItem {
    fn matches(&self, c: char) -> bool {
        match self {
            ClassItem::Any => true,
            ClassItem::Literal(expected) => *expected == c,
            ClassItem::Set { chars, negated } => chars.contains(&c) != *negated,
        }
    }
}

/// A `.aff` rule condition, e.g. `[bgkpßsz]eln` or `[^n]`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Condition(Vec<ClassItem>);

impl Condition {
    /// `.` means "always", which Hunspell writes as a single dot.
    pub fn parse(pattern: &str) -> Self {
        if pattern == "." {
            return Self::default();
        }
        let mut items = Vec::new();
        let mut chars = pattern.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '.' => items.push(ClassItem::Any),
                '[' => {
                    let negated = chars.peek() == Some(&'^');
                    if negated {
                        chars.next();
                    }
                    let mut set = Vec::new();
                    for c in chars.by_ref() {
                        if c == ']' {
                            break;
                        }
                        set.push(c);
                    }
                    items.push(ClassItem::Set {
                        chars: set,
                        negated,
                    });
                }
                other => items.push(ClassItem::Literal(other)),
            }
        }
        Self(items)
    }

    /// Whether the condition holds at the end of `stem` (for suffixes).
    fn matches_end(&self, stem: &[char]) -> bool {
        if self.0.is_empty() {
            return true;
        }
        if stem.len() < self.0.len() {
            return false;
        }
        let tail = &stem[stem.len() - self.0.len()..];
        self.0.iter().zip(tail).all(|(item, c)| item.matches(*c))
    }

    /// Whether the condition holds at the start of `stem` (for prefixes).
    fn matches_start(&self, stem: &[char]) -> bool {
        if self.0.is_empty() {
            return true;
        }
        if stem.len() < self.0.len() {
            return false;
        }
        self.0
            .iter()
            .zip(&stem[..self.0.len()])
            .all(|(item, c)| item.matches(*c))
    }
}

/// One `PFX`/`SFX` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AffixRule {
    /// Characters removed from the stem; empty for Hunspell's `0`.
    strip: Vec<char>,
    /// Characters added.
    append: String,
    condition: Condition,
}

impl AffixRule {
    /// Applies this rule as a suffix, or `None` when it does not fit.
    fn apply_suffix(&self, stem: &[char]) -> Option<String> {
        if !self.condition.matches_end(stem) {
            return None;
        }
        if stem.len() < self.strip.len() {
            return None;
        }
        let keep = stem.len() - self.strip.len();
        if !stem[keep..].iter().eq(self.strip.iter()) {
            return None;
        }
        // Stripping the entire stem requires FULLSTRIP, which these
        // dictionaries do not set; the result would be a bare affix anyway.
        if keep == 0 {
            return None;
        }
        let mut out: String = stem[..keep].iter().collect();
        out.push_str(&self.append);
        Some(out)
    }

    /// Applies this rule as a prefix, or `None` when it does not fit.
    fn apply_prefix(&self, stem: &[char]) -> Option<String> {
        if !self.condition.matches_start(stem) {
            return None;
        }
        if stem.len() <= self.strip.len() {
            return None;
        }
        if !stem[..self.strip.len()].iter().eq(self.strip.iter()) {
            return None;
        }
        let mut out = self.append.clone();
        out.extend(&stem[self.strip.len()..]);
        Some(out)
    }
}

/// Everything parsed out of a `.aff` file.
#[derive(Debug, Clone, Default)]
pub struct AffixTable {
    pub encoding: Option<&'static Encoding>,
    flag_kind: FlagKind,
    /// Flags meaning "this stem is not a word on its own".
    needs_affix: HashSet<String>,
    /// Flags meaning "this entry is explicitly wrong" (old spellings).
    forbidden: HashSet<String>,
    suffixes: HashMap<String, Vec<AffixRule>>,
    prefixes: HashMap<String, Vec<AffixRule>>,
}

impl AffixTable {
    /// Hunspell's documented default encoding when no `SET` line is present.
    pub const DEFAULT_ENCODING: &'static Encoding = encoding_rs::WINDOWS_1252;

    pub fn encoding(&self) -> &'static Encoding {
        self.encoding.unwrap_or(Self::DEFAULT_ENCODING)
    }

    /// Reads only the `SET` line, which is always ASCII.
    ///
    /// The rest of the file cannot be parsed until the encoding is known, so
    /// this is a deliberate two-pass design.
    pub fn probe_encoding(bytes: &[u8]) -> Option<&'static Encoding> {
        for line in String::from_utf8_lossy(bytes).lines() {
            let mut parts = line.split_whitespace();
            if parts.next() == Some("SET") {
                if let Some(label) = parts.next() {
                    return lookup_encoding(label).or_else(|| {
                        tracing::warn!(encoding = label, "unknown .aff SET encoding");
                        None
                    });
                }
            }
        }
        None
    }

    /// Parses a decoded `.aff` file.
    pub fn parse(text: &str) -> Self {
        let mut table = Self {
            encoding: Self::probe_encoding(text.as_bytes()),
            ..Self::default()
        };

        let lines: Vec<&str> = text.lines().collect();
        let mut index = 0;
        while index < lines.len() {
            let fields: Vec<&str> = lines[index].split_whitespace().collect();
            index += 1;
            let Some(&keyword) = fields.first() else {
                continue;
            };
            match keyword {
                "FLAG" => {
                    table.flag_kind = match fields.get(1) {
                        Some(&"long") => FlagKind::Long,
                        Some(&"num") => FlagKind::Numeric,
                        Some(&"UTF-8") | Some(&"utf-8") => FlagKind::Utf8,
                        other => {
                            if let Some(other) = other {
                                tracing::warn!(flag = other, "unknown .aff FLAG type");
                            }
                            FlagKind::Single
                        }
                    };
                }
                "NEEDAFFIX" | "PSEUDOROOT" | "ONLYINCOMPOUND" => {
                    if let Some(&flag) = fields.get(1) {
                        table.needs_affix.insert(flag.to_owned());
                    }
                }
                "FORBIDDENWORD" => {
                    if let Some(&flag) = fields.get(1) {
                        table.forbidden.insert(flag.to_owned());
                    }
                }
                "PFX" | "SFX" => {
                    // Header: `SFX <flag> <cross> <count>`, then `count` rules.
                    let (Some(&flag), Some(&count)) = (fields.get(1), fields.get(3)) else {
                        continue;
                    };
                    let Ok(count) = count.parse::<usize>() else {
                        continue;
                    };
                    let mut rules = Vec::with_capacity(count);
                    for line in lines.iter().skip(index).take(count) {
                        let parts: Vec<&str> = line.split_whitespace().collect();
                        // `SFX <flag> <strip> <append> [<condition>]`
                        let (Some(&strip), Some(&append)) = (parts.get(2), parts.get(3)) else {
                            continue;
                        };
                        rules.push(AffixRule {
                            strip: if strip == "0" {
                                Vec::new()
                            } else {
                                strip.chars().collect()
                            },
                            // `append` may carry continuation flags after `/`;
                            // those chain further affixes, which is beyond
                            // single-level expansion.
                            append: match append.split('/').next().unwrap_or(append) {
                                "0" => String::new(),
                                text => text.to_owned(),
                            },
                            condition: Condition::parse(parts.get(4).copied().unwrap_or(".")),
                        });
                    }
                    index += count;
                    let target = if keyword == "SFX" {
                        &mut table.suffixes
                    } else {
                        &mut table.prefixes
                    };
                    target.entry(flag.to_owned()).or_default().extend(rules);
                }
                _ => {}
            }
        }
        table
    }

    /// Splits a raw flag field into individual flags.
    pub fn split_flags(&self, raw: &str) -> Vec<String> {
        match self.flag_kind {
            FlagKind::Single | FlagKind::Utf8 => raw.chars().map(|c| c.to_string()).collect(),
            FlagKind::Long => {
                let chars: Vec<char> = raw.chars().collect();
                chars.chunks(2).map(|pair| pair.iter().collect()).collect()
            }
            FlagKind::Numeric => raw
                .split(',')
                .map(str::trim)
                .filter(|flag| !flag.is_empty())
                .map(str::to_owned)
                .collect(),
        }
    }

    /// Whether an entry is explicitly wrong and must be ignored completely.
    pub fn is_forbidden(&self, flags: &[String]) -> bool {
        flags.iter().any(|flag| self.forbidden.contains(flag))
    }

    /// Whether the bare stem is a word, as opposed to only a base for affixes.
    pub fn stem_is_word(&self, flags: &[String]) -> bool {
        !flags.iter().any(|flag| self.needs_affix.contains(flag))
    }

    /// Surface forms generated from `stem` by its flags.
    ///
    /// Single level only: one prefix *or* one suffix, no cross products and no
    /// continuation flags. That is what keeps the index around 1.3M entries
    /// for German instead of tens of millions, and it already recovers the
    /// `-ung` nouns (`Erfahrung`, `Bewerbung`) that matter most for
    /// completion.
    pub fn expand(&self, stem: &str, flags: &[String], out: &mut Vec<String>) {
        if flags.is_empty() || (self.suffixes.is_empty() && self.prefixes.is_empty()) {
            return;
        }
        let chars: Vec<char> = stem.chars().collect();
        for flag in flags {
            if let Some(rules) = self.suffixes.get(flag) {
                out.extend(rules.iter().filter_map(|rule| rule.apply_suffix(&chars)));
            }
            if let Some(rules) = self.prefixes.get(flag) {
                out.extend(rules.iter().filter_map(|rule| rule.apply_prefix(&chars)));
            }
        }
    }

    pub fn has_rules(&self) -> bool {
        !(self.suffixes.is_empty() && self.prefixes.is_empty())
    }
}

/// Maps Hunspell's encoding spellings onto the WHATWG labels `encoding_rs`
/// knows: it writes `ISO8859-1`, the lookup table wants `iso-8859-1`.
fn lookup_encoding(name: &str) -> Option<&'static Encoding> {
    let normalized = name.trim().to_ascii_lowercase();
    let candidates = [
        normalized.clone(),
        normalized.replace("iso8859", "iso-8859"),
        normalized.replace("microsoft-cp", "windows-"),
    ];
    candidates
        .iter()
        .find_map(|label| Encoding::for_label(label.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expand(aff: &str, entry: &str) -> Vec<String> {
        let table = AffixTable::parse(aff);
        let (stem, raw) = match entry.split_once('/') {
            Some((stem, flags)) => (stem, flags),
            None => (entry, ""),
        };
        let flags = table.split_flags(raw);
        let mut out = Vec::new();
        table.expand(stem, &flags, &mut out);
        out.sort();
        out.dedup();
        out
    }

    // The real igerman98 rule group that produces the `-ung` nouns.
    const DE_J: &str = "\
SET ISO8859-1
SFX J Y 12
SFX J   n       ung        [bgkpßsz]eln
SFX J   n       ungen      [bgkpßsz]eln
SFX J   eln     lung       eln
SFX J   n       ung        ern
SFX J   en      ung        en
SFX J   eln     lungen     eln
SFX J   n       ungen      ern
SFX J   en      ungen      en
SFX J   0       ung        [^n]
SFX J   0       ungen      [^n]
SFX J   el      lung       el
SFX J   el      lungen     el
";

    #[test]
    fn suffix_expansion_produces_the_ung_nouns() {
        // These are exactly the words that are missing without expansion.
        let got = expand(DE_J, "Erfahren/J");
        assert!(got.contains(&"Erfahrung".to_string()), "{got:?}");
        assert!(got.contains(&"Erfahrungen".to_string()), "{got:?}");

        let got = expand(DE_J, "bewerben/J");
        assert!(got.contains(&"bewerbung".to_string()), "{got:?}");
    }

    #[test]
    fn conditions_select_the_right_rule() {
        // `Erfahren` ends in `en`, so the `en -> ung` rule applies and the
        // `[^n]` rule does not.
        let got = expand(DE_J, "Erfahren/J");
        assert!(!got.iter().any(|w| w == "Erfahrenung"), "{got:?}");

        // `Bewerb` does not end in `n`, so `0 -> ung` applies.
        let got = expand(DE_J, "Bewerb/J");
        assert_eq!(got, vec!["Bewerbung", "Bewerbungen"]);
    }

    #[test]
    fn negated_character_class_is_honoured() {
        let aff = "SFX A Y 1\nSFX A 0 s [^n]\n";
        assert_eq!(expand(aff, "Haus/A"), vec!["Hauss"]);
        // Ends in `n`, so the rule must not fire.
        assert!(expand(aff, "Kuchen/A").is_empty());
    }

    #[test]
    fn character_class_with_eszett_and_umlauts_matches() {
        let aff = "SFX A Y 1\nSFX A n ung [bgkpßsz]eln\n";
        assert_eq!(expand(aff, "wandeln/A"), Vec::<String>::new());
        assert_eq!(expand(aff, "bügeln/A"), vec!["bügelung"]);
    }

    #[test]
    fn prefix_rules_are_applied() {
        let aff = "PFX U Y 1\nPFX U 0 un .\n";
        assert_eq!(expand(aff, "möglich/U"), vec!["unmöglich"]);
    }

    #[test]
    fn prefix_can_strip() {
        let aff = "PFX A Y 1\nPFX A ge 0 ge\n";
        assert_eq!(expand(aff, "gearbeitet/A"), vec!["arbeitet"]);
    }

    #[test]
    fn dot_condition_always_matches() {
        let aff = "SFX S Y 1\nSFX S 0 s .\n";
        assert_eq!(expand(aff, "Auto/S"), vec!["Autos"]);
        assert_eq!(Condition::parse("."), Condition::default());
    }

    #[test]
    fn a_rule_that_would_consume_the_whole_stem_is_skipped() {
        // FULLSTRIP is not set, so this must not yield a bare affix.
        let aff = "SFX A Y 1\nSFX A haus hof haus\n";
        assert!(expand(aff, "haus/A").is_empty());
    }

    #[test]
    fn continuation_flags_in_the_append_field_are_stripped() {
        let aff = "SFX A Y 1\nSFX A 0 ung/XY .\n";
        assert_eq!(expand(aff, "Bewerb/A"), vec!["Bewerbung"]);
    }

    #[test]
    fn unflagged_or_unknown_flags_expand_to_nothing() {
        assert!(expand(DE_J, "Erfahren").is_empty());
        assert!(expand(DE_J, "Erfahren/Z").is_empty());
    }

    #[test]
    fn forbidden_and_needaffix_flags_are_distinguished_case_sensitively() {
        let table = AffixTable::parse("FORBIDDENWORD d\nNEEDAFFIX h\n");
        let flags = |raw: &str| table.split_flags(raw);
        assert!(table.is_forbidden(&flags("dm")));
        // `D` is a different flag from `d`; conflating them would delete
        // thousands of correct igerman98 entries.
        assert!(!table.is_forbidden(&flags("DI")));
        assert!(!table.stem_is_word(&flags("hij")));
        assert!(table.stem_is_word(&flags("ij")));
    }

    #[test]
    fn encoding_labels_are_normalised() {
        assert_eq!(
            AffixTable::parse("SET ISO8859-1\n").encoding(),
            encoding_rs::WINDOWS_1252
        );
        assert_eq!(
            AffixTable::parse("SET UTF-8\n").encoding(),
            encoding_rs::UTF_8
        );
        // No SET line falls back to Hunspell's default, not UTF-8.
        assert_eq!(AffixTable::default().encoding(), encoding_rs::WINDOWS_1252);
        assert!(AffixTable::probe_encoding(b"SET NOSUCHCODEPAGE\n").is_none());
    }

    #[test]
    fn long_and_numeric_flag_forms_parse() {
        let long = AffixTable::parse("FLAG long\nSFX Aa Y 1\nSFX Aa 0 s .\n");
        assert_eq!(long.split_flags("AaBb"), vec!["Aa", "Bb"]);
        let mut out = Vec::new();
        long.expand("Auto", &long.split_flags("Aa"), &mut out);
        assert_eq!(out, vec!["Autos"]);

        let num = AffixTable::parse("FLAG num\nSFX 1000 Y 1\nSFX 1000 0 s .\n");
        assert_eq!(num.split_flags("1,1000"), vec!["1", "1000"]);
        let mut out = Vec::new();
        num.expand("Auto", &num.split_flags("1,1000"), &mut out);
        assert_eq!(out, vec!["Autos"]);
    }

    #[test]
    fn rule_count_is_respected_so_following_directives_are_not_eaten() {
        let table = AffixTable::parse("SFX A Y 1\nSFX A 0 s .\nFORBIDDENWORD d\n");
        assert!(table.is_forbidden(&table.split_flags("d")));
        assert!(table.has_rules());
    }

    #[test]
    fn an_empty_aff_file_parses_to_no_rules() {
        let table = AffixTable::parse("");
        assert!(!table.has_rules());
        assert!(table.stem_is_word(&["X".to_string()]));
    }
}
