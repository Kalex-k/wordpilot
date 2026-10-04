//! Database schema for the personal dictionary.

use rusqlite::{Connection, Result};

/// Creates the schema if it does not exist yet.
///
/// There is exactly one table for the MVP, so this is a single
/// `CREATE TABLE IF NOT EXISTS` rather than a migration framework. If the
/// schema ever needs to change shape, a `user_version` pragma check belongs
/// here.
pub fn initialize(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        PRAGMA journal_mode = WAL;
        PRAGMA synchronous = NORMAL;
        PRAGMA foreign_keys = ON;

        CREATE TABLE IF NOT EXISTS personal_words (
            id           INTEGER PRIMARY KEY,
            word         TEXT NOT NULL,
            folded_word  TEXT NOT NULL,
            language     TEXT NOT NULL,
            -- '' means the cross-application (global) row for this word.
            application  TEXT NOT NULL DEFAULT '',
            usage_count  INTEGER NOT NULL DEFAULT 0,
            last_used    INTEGER NOT NULL,
            UNIQUE(folded_word, language, application)
        );

        CREATE INDEX IF NOT EXISTS personal_words_prefix
            ON personal_words (language, application, folded_word);
        ",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        initialize(&conn).unwrap();
        initialize(&conn).unwrap();
        conn.execute(
            "INSERT INTO personal_words (word, folded_word, language, last_used) \
             VALUES ('bewerben', 'bewerben', 'de_DE', 0)",
            [],
        )
        .unwrap();
    }

    #[test]
    fn unique_constraint_rejects_duplicate_rows() {
        let conn = Connection::open_in_memory().unwrap();
        initialize(&conn).unwrap();
        let insert =
            "INSERT INTO personal_words (word, folded_word, language, application, last_used) \
                      VALUES ('bewerben', 'bewerben', 'de_DE', '', 0)";
        conn.execute(insert, []).unwrap();
        assert!(conn.execute(insert, []).is_err());
    }
}
