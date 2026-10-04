//! Edit-distance corrections for a word that is not in the dictionary.
//!
//! Intentionally small: corrections are a convenience, and the result is
//! always surfaced as [`CandidateSource::SpellCorrection`] so the user decides
//! (spec §8). Nothing here is ever applied automatically.

use crate::provider::WordEntry;

/// Maximum edit distance considered a correction.
const MAX_DISTANCE: usize = 2;

/// Candidate words within [`MAX_DISTANCE`] of `word`, closest first.
///
/// `vocabulary` is scanned once. Cheap length and first-character filters cut
/// the real work to a small fraction of the dictionary before any distance is
/// computed, which keeps this inside the latency budget for the rare keystroke
/// that triggers it.
pub fn corrections<'a, I>(vocabulary: I, word: &str, limit: usize) -> Vec<WordEntry>
where
    I: Iterator<Item = &'a str>,
{
    if word.is_empty() || limit == 0 {
        return Vec::new();
    }
    let needle: Vec<char> = word.to_lowercase().chars().collect();
    let first = needle[0];

    let mut best: Vec<(usize, &str)> = Vec::new();
    for entry in vocabulary {
        let candidate: Vec<char> = entry.to_lowercase().chars().collect();
        if candidate.len().abs_diff(needle.len()) > MAX_DISTANCE {
            continue;
        }
        // A differing first letter is possible but rare in typos, and this
        // filter is what makes the scan affordable.
        if candidate[0] != first {
            continue;
        }
        let distance = damerau_levenshtein(&needle, &candidate, MAX_DISTANCE);
        if distance <= MAX_DISTANCE && distance > 0 {
            best.push((distance, entry));
        }
    }

    best.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.len().cmp(&b.1.len())));
    best.dedup_by(|a, b| a.1 == b.1);
    best.into_iter()
        .take(limit)
        .map(|(_, entry)| WordEntry::new(entry, entry.starts_with(word)))
        .collect()
}

/// Optimal-string-alignment distance, abandoned once it exceeds `cutoff`.
///
/// Operates on `char` slices, never bytes, so `ä` counts as one edit rather
/// than two (spec §45).
fn damerau_levenshtein(a: &[char], b: &[char], cutoff: usize) -> usize {
    let (n, m) = (a.len(), b.len());
    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }

    let mut prev2: Vec<usize> = vec![0; m + 1];
    let mut prev: Vec<usize> = (0..=m).collect();
    let mut current: Vec<usize> = vec![0; m + 1];

    for i in 1..=n {
        current[0] = i;
        let mut row_min = current[0];
        for j in 1..=m {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut value = (current[j - 1] + 1)
                .min(prev[j] + 1)
                .min(prev[j - 1] + cost);
            // Transposition: "Erfharung" -> "Erfahrung" is one edit.
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                value = value.min(prev2[j - 2] + 1);
            }
            current[j] = value;
            row_min = row_min.min(value);
        }
        if row_min > cutoff {
            return cutoff + 1;
        }
        std::mem::swap(&mut prev2, &mut prev);
        std::mem::swap(&mut prev, &mut current);
    }
    prev[m]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn distance(a: &str, b: &str) -> usize {
        let a: Vec<char> = a.chars().collect();
        let b: Vec<char> = b.chars().collect();
        damerau_levenshtein(&a, &b, usize::MAX)
    }

    #[test]
    fn identical_words_have_zero_distance() {
        assert_eq!(distance("Erfahrung", "Erfahrung"), 0);
    }

    #[test]
    fn transposition_counts_as_one_edit() {
        assert_eq!(distance("erfharung", "erfahrung"), 1);
    }

    #[test]
    fn substitution_insertion_deletion() {
        assert_eq!(distance("haus", "maus"), 1);
        assert_eq!(distance("haus", "hauss"), 1);
        assert_eq!(distance("hauss", "haus"), 1);
    }

    #[test]
    fn umlauts_count_as_single_characters() {
        // Byte-based distance would report 2 for each of these.
        assert_eq!(distance("bewaltigen", "bewältigen"), 1);
        assert_eq!(distance("gross", "groß"), 2);
        assert_eq!(distance("grosse", "große"), 2);
        assert_eq!(distance("uber", "über"), 1);
    }

    #[test]
    fn empty_inputs() {
        assert_eq!(distance("", ""), 0);
        assert_eq!(distance("", "abc"), 3);
        assert_eq!(distance("abc", ""), 3);
    }

    #[test]
    fn cutoff_short_circuits_without_changing_small_results() {
        let a: Vec<char> = "kurz".chars().collect();
        let b: Vec<char> = "vollkommenanders".chars().collect();
        assert_eq!(damerau_levenshtein(&a, &b, 2), 3);
        let c: Vec<char> = "haus".chars().collect();
        let d: Vec<char> = "maus".chars().collect();
        assert_eq!(damerau_levenshtein(&c, &d, 2), 1);
    }

    #[test]
    fn suggests_the_intended_word() {
        let vocab = ["Erfahrung", "Erfahrungen", "Ernährung", "Bewerbung"];
        let got = corrections(vocab.into_iter(), "Erfharung", 3);
        assert_eq!(got.first().map(|e| e.word.as_str()), Some("Erfahrung"));
    }

    #[test]
    fn never_suggests_the_input_itself() {
        let vocab = ["Erfahrung", "Erfahrungen"];
        let got = corrections(vocab.into_iter(), "Erfahrung", 5);
        assert!(got.iter().all(|e| e.word != "Erfahrung"), "{got:?}");
    }

    #[test]
    fn respects_limit_and_empty_input() {
        let vocab = ["haus", "hau", "haut", "hausen"];
        assert_eq!(corrections(vocab.into_iter(), "haus", 2).len(), 2);
        assert!(corrections(vocab.into_iter(), "", 5).is_empty());
        assert!(corrections(vocab.into_iter(), "haus", 0).is_empty());
    }

    #[test]
    fn a_typo_in_the_first_letter_is_not_corrected() {
        // A documented trade-off: the first-character filter is what keeps the
        // scan affordable over a 250k-word dictionary.
        let vocab = ["haus"];
        assert!(corrections(vocab.into_iter(), "maus", 5).is_empty());
    }

    #[test]
    fn distant_words_are_not_suggested() {
        let vocab = ["Kubernetes", "PostgreSQL"];
        assert!(corrections(vocab.into_iter(), "Erfharung", 5).is_empty());
    }
}
