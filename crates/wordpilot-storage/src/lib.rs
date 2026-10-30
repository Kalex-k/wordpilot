//! SQLite-backed personal dictionary.
//!
//! Implements [`wordpilot_autocomplete::PersonalVocabulary`], the interface
//! the engine already ranks against. Nothing outside this crate links
//! `rusqlite`: a corrupted or missing database degrades to
//! [`wordpilot_autocomplete::NoPersonalDictionary`] at the call site, not a
//! daemon crash.

mod schema;

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};
use wordpilot_autocomplete::{PersonalStats, PersonalVocabulary};
use wordpilot_core::Language;
use wordpilot_ranking::Signals;

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("could not open personal dictionary at {path}: {source}")]
    Open {
        path: String,
        #[source]
        source: rusqlite::Error,
    },
    #[error("could not prepare the personal dictionary schema: {0}")]
    Schema(#[source] rusqlite::Error),
    #[error("could not create {path}: {source}")]
    Directory {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

/// The empty-string application key used for a word's global (cross-app)
/// usage row. A real application name is never empty, so there is no
/// collision, and this avoids `NULL`-handling in every query.
const GLOBAL_APP: &str = "";

/// Decay half-life for the recency signal.
///
/// Fixed here rather than read from `RankingConfig` so this crate does not
/// need to parse configuration; the daemon passes the configured value in
/// through [`PersonalDictionary::with_half_life`] if it differs.
const DEFAULT_HALF_LIFE_SECS: f64 = 7.0 * 24.0 * 3600.0;

/// A learned vocabulary backed by a single SQLite file.
///
/// `&self` methods serialise through an internal [`Mutex`], matching the
/// trait's contract that a learning write never needs an exclusive borrow on
/// the keystroke path (see `PersonalVocabulary::record_usage`).
pub struct PersonalDictionary {
    conn: Mutex<Connection>,
    half_life_secs: f64,
}

impl PersonalDictionary {
    /// Opens (creating if absent) the database at `path`.
    pub fn open(path: &std::path::Path) -> Result<Self, StorageError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| StorageError::Directory {
                path: parent.display().to_string(),
                source,
            })?;
        }
        let conn = Connection::open(path).map_err(|source| StorageError::Open {
            path: path.display().to_string(),
            source,
        })?;
        Self::from_connection(conn)
    }

    /// In-memory database, for tests and for `--no-persist` style runs.
    pub fn open_in_memory() -> Result<Self, StorageError> {
        let conn = Connection::open_in_memory().map_err(|source| StorageError::Open {
            path: ":memory:".to_string(),
            source,
        })?;
        Self::from_connection(conn)
    }

    fn from_connection(conn: Connection) -> Result<Self, StorageError> {
        schema::initialize(&conn).map_err(StorageError::Schema)?;
        Ok(Self {
            conn: Mutex::new(conn),
            half_life_secs: DEFAULT_HALF_LIFE_SECS,
        })
    }

    /// Overrides the recency half-life, matching `ranking.recency_half_life_secs`.
    pub fn with_half_life(mut self, half_life_secs: f64) -> Self {
        self.half_life_secs = half_life_secs;
        self
    }

    /// Total distinct learned words, for `wordpilot dictionary stats`.
    pub fn word_count(&self) -> usize {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.query_row(
            "SELECT COUNT(DISTINCT word || '\u{1}' || language) FROM personal_words WHERE application = ?1",
            params![GLOBAL_APP],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0) as usize
    }

    /// Removes a learned word entirely (every application), for a future
    /// `wordpilot dictionary remove` and for tests.
    pub fn remove_word(&self, word: &str, language: Language) {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let folded = word.to_lowercase();
        if let Err(error) = conn.execute(
            "DELETE FROM personal_words WHERE folded_word = ?1 AND language = ?2",
            params![folded, language.tag()],
        ) {
            tracing::warn!(%error, word, "failed to remove personal word");
        }
    }

    fn now_secs() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }

    /// Current max usage count for `language`, used to normalise frequency
    /// into `[0, 1]` the same way `FrequencyTable` does for corpus counts.
    fn max_usage(conn: &Connection, language: Language) -> i64 {
        conn.query_row(
            "SELECT COALESCE(MAX(usage_count), 0) FROM personal_words \
             WHERE language = ?1 AND application = ?2",
            params![language.tag(), GLOBAL_APP],
            |row| row.get(0),
        )
        .unwrap_or(0)
    }

    fn normalize(count: i64, max_count: i64) -> f32 {
        if count <= 0 || max_count <= 0 {
            return 0.0;
        }
        if max_count <= 1 {
            return 1.0;
        }
        ((count as f64).ln() / (max_count as f64).ln()).clamp(0.0, 1.0) as f32
    }

    /// Upserts one usage row, incrementing the count and bumping `last_used`.
    fn bump(
        conn: &Connection,
        word: &str,
        folded: &str,
        language: Language,
        application: &str,
        now: i64,
    ) {
        let result = conn.execute(
            "INSERT INTO personal_words (word, folded_word, language, application, usage_count, last_used) \
             VALUES (?1, ?2, ?3, ?4, 1, ?5) \
             ON CONFLICT(folded_word, language, application) \
             DO UPDATE SET usage_count = usage_count + 1, last_used = ?5, word = ?1",
            params![word, folded, language.tag(), application, now],
        );
        if let Err(error) = result {
            tracing::warn!(%error, word, "failed to record word usage");
        }
    }
}

impl PersonalVocabulary for PersonalDictionary {
    fn stats(&self, word: &str, language: Language, application: Option<&str>) -> PersonalStats {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let folded = word.to_lowercase();

        let global: Option<(i64, i64)> = conn
            .query_row(
                "SELECT usage_count, last_used FROM personal_words \
                 WHERE folded_word = ?1 AND language = ?2 AND application = ?3",
                params![folded, language.tag(), GLOBAL_APP],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .unwrap_or(None);

        let Some((count, last_used)) = global else {
            return PersonalStats::default();
        };

        let max_count = Self::max_usage(&conn, language);
        let frequency = Self::normalize(count, max_count);
        let age_secs = (Self::now_secs() - last_used).max(0) as f64;
        let recency = Signals::decay_recency(age_secs, self.half_life_secs);

        let application_frequency = application
            .map(|app| {
                conn.query_row(
                    "SELECT usage_count FROM personal_words \
                     WHERE folded_word = ?1 AND language = ?2 AND application = ?3",
                    params![folded, language.tag(), app],
                    |row| row.get::<_, i64>(0),
                )
                .optional()
                .unwrap_or(None)
                .map(|app_count| Self::normalize(app_count, max_count))
                .unwrap_or(0.0)
            })
            .unwrap_or(0.0);

        PersonalStats {
            frequency,
            recency,
            application_frequency,
        }
    }

    fn complete(&self, prefix: &str, language: Language, limit: usize) -> Vec<String> {
        if prefix.is_empty() || limit == 0 {
            return Vec::new();
        }
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let folded_prefix = prefix.to_lowercase();
        // `x'...'` upper bound trick: appending the highest Unicode scalar as
        // a byte makes `folded_word < upper_bound` a sargable prefix range
        // scan, same trick as the in-memory dictionary's binary search.
        let mut upper_bound = folded_prefix.clone();
        upper_bound.push('\u{10FFFF}');

        let mut statement = match conn.prepare(
            "SELECT word FROM personal_words \
             WHERE language = ?1 AND application = ?2 \
               AND folded_word >= ?3 AND folded_word < ?4 \
             ORDER BY usage_count DESC \
             LIMIT ?5",
        ) {
            Ok(statement) => statement,
            Err(error) => {
                tracing::warn!(%error, "personal dictionary query failed");
                return Vec::new();
            }
        };

        let rows = statement.query_map(
            params![
                language.tag(),
                GLOBAL_APP,
                folded_prefix,
                upper_bound,
                limit as i64
            ],
            |row| row.get::<_, String>(0),
        );

        match rows {
            Ok(rows) => rows.filter_map(Result::ok).collect(),
            Err(error) => {
                tracing::warn!(%error, "personal dictionary query failed");
                Vec::new()
            }
        }
    }

    fn record_usage(&self, word: &str, language: Language, application: Option<&str>) {
        if word.is_empty() {
            return;
        }
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let folded = word.to_lowercase();
        let now = Self::now_secs();

        // Always bump the global (cross-application) row, which is what
        // ranking's `personal_frequency` signal reads.
        Self::bump(&conn, word, &folded, language, GLOBAL_APP, now);
        // A per-application row too, for the application-specific
        // ranking signal -- recorded now so no migration is needed later.
        if let Some(app) = application {
            if !app.is_empty() {
                Self::bump(&conn, word, &folded, language, app, now);
            }
        }
    }

    fn is_learning(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict() -> PersonalDictionary {
        PersonalDictionary::open_in_memory().unwrap()
    }

    #[test]
    fn unknown_word_has_default_stats() {
        let dict = dict();
        assert_eq!(
            dict.stats("bewerben", Language::German, None),
            PersonalStats::default()
        );
    }

    #[test]
    fn recording_usage_raises_frequency_and_recency() {
        let dict = dict();
        dict.record_usage("bewerben", Language::German, None);
        let stats = dict.stats("bewerben", Language::German, None);
        assert!(stats.frequency > 0.0, "{stats:?}");
        assert!(stats.recency > 0.9, "{stats:?}"); // just recorded, so ~no decay
    }

    #[test]
    fn repeated_use_outranks_a_single_use() {
        let dict = dict();
        dict.record_usage("bewerben", Language::German, None);
        for _ in 0..20 {
            dict.record_usage("bewegen", Language::German, None);
        }
        let rare = dict.stats("bewerben", Language::German, None);
        let frequent = dict.stats("bewegen", Language::German, None);
        assert!(
            frequent.frequency > rare.frequency,
            "{frequent:?} vs {rare:?}"
        );
    }

    #[test]
    fn complete_returns_learned_words_by_usage_descending() {
        let dict = dict();
        dict.record_usage("bewachung", Language::German, None);
        for _ in 0..5 {
            dict.record_usage("bewerbung", Language::German, None);
        }
        let got = dict.complete("bew", Language::German, 10);
        assert_eq!(got, vec!["bewerbung".to_string(), "bewachung".to_string()]);
    }

    #[test]
    fn complete_respects_language_and_limit() {
        let dict = dict();
        dict.record_usage("Kubernetes", Language::German, None);
        dict.record_usage("Kubernetes", Language::EnglishUs, None);
        assert_eq!(dict.complete("Kube", Language::German, 10).len(), 1);
        assert_eq!(dict.complete("Kube", Language::EnglishUs, 10).len(), 1);
        for i in 0..5 {
            dict.record_usage(&format!("bewerb{i}"), Language::German, None);
        }
        assert_eq!(dict.complete("bewerb", Language::German, 2).len(), 2);
    }

    #[test]
    fn complete_is_case_insensitive_and_preserves_original_case() {
        let dict = dict();
        dict.record_usage("Kubernetes", Language::German, None);
        assert_eq!(
            dict.complete("kube", Language::German, 5),
            vec!["Kubernetes"]
        );
        assert_eq!(
            dict.complete("KUBE", Language::German, 5),
            vec!["Kubernetes"]
        );
    }

    #[test]
    fn empty_prefix_or_zero_limit_yields_nothing() {
        let dict = dict();
        dict.record_usage("bewerben", Language::German, None);
        assert!(dict.complete("", Language::German, 5).is_empty());
        assert!(dict.complete("bew", Language::German, 0).is_empty());
    }

    #[test]
    fn application_frequency_is_tracked_separately_from_global() {
        let dict = dict();
        for _ in 0..10 {
            dict.record_usage("Bewerbung", Language::German, Some("firefox"));
        }
        let stats = dict.stats("Bewerbung", Language::German, Some("firefox"));
        assert!(stats.application_frequency > 0.0, "{stats:?}");
        // A different application never used this word.
        let stats = dict.stats("Bewerbung", Language::German, Some("vscode"));
        assert_eq!(stats.application_frequency, 0.0);
        // No application requested at all.
        let stats = dict.stats("Bewerbung", Language::German, None);
        assert_eq!(stats.application_frequency, 0.0);
        assert!(stats.frequency > 0.0, "global frequency must still be set");
    }

    #[test]
    fn same_word_different_language_is_tracked_independently() {
        let dict = dict();
        dict.record_usage("Gift", Language::German, None);
        let de = dict.stats("Gift", Language::German, None);
        let en = dict.stats("Gift", Language::EnglishUs, None);
        assert!(de.frequency > 0.0);
        assert_eq!(en.frequency, 0.0);
    }

    #[test]
    fn remove_word_deletes_every_application_row() {
        let dict = dict();
        dict.record_usage("secret", Language::German, Some("firefox"));
        dict.record_usage("secret", Language::German, None);
        dict.remove_word("secret", Language::German);
        assert_eq!(
            dict.stats("secret", Language::German, Some("firefox")),
            PersonalStats::default()
        );
    }

    #[test]
    fn word_count_counts_distinct_words_not_usage_rows() {
        let dict = dict();
        for _ in 0..3 {
            dict.record_usage("bewerben", Language::German, Some("firefox"));
        }
        dict.record_usage("bewegen", Language::German, None);
        assert_eq!(dict.word_count(), 2);
    }

    #[test]
    fn is_learning_is_true_whenever_the_database_is_open() {
        // The config-level on/off switch is the engine's job, not this
        // struct's; see `NoPersonalDictionary` for the disabled case.
        assert!(dict().is_learning());
    }

    #[test]
    fn opening_creates_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("personal.sqlite");
        let dict = PersonalDictionary::open(&path).unwrap();
        dict.record_usage("bewerben", Language::German, None);
        assert!(path.is_file());
    }

    #[test]
    fn data_survives_reopening_the_same_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("personal.sqlite");
        {
            let dict = PersonalDictionary::open(&path).unwrap();
            dict.record_usage("bewerben", Language::German, None);
        }
        let dict = PersonalDictionary::open(&path).unwrap();
        assert!(dict.stats("bewerben", Language::German, None).frequency > 0.0);
    }

    #[test]
    fn half_life_override_changes_decay_speed() {
        let dict = PersonalDictionary::open_in_memory()
            .unwrap()
            .with_half_life(1.0);
        dict.record_usage("bewerben", Language::German, None);
        // Manually age the row far beyond the 1-second half-life.
        {
            let conn = dict.conn.lock().unwrap();
            conn.execute("UPDATE personal_words SET last_used = last_used - 100", [])
                .unwrap();
        }
        let stats = dict.stats("bewerben", Language::German, None);
        assert!(stats.recency < 0.01, "{stats:?}");
    }

    #[test]
    fn unicode_words_with_umlauts_roundtrip() {
        let dict = dict();
        dict.record_usage("Bewährung", Language::German, None);
        assert_eq!(
            dict.complete("bewä", Language::German, 5),
            vec!["Bewährung"]
        );
        assert!(dict.stats("Bewährung", Language::German, None).frequency > 0.0);
    }
}
