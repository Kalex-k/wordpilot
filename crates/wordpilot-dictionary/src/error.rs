use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum DictionaryError {
    #[error("no dictionary for {language} found; searched: {searched}")]
    NotFound { language: String, searched: String },

    #[error("could not read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{path} is not a usable Hunspell file: {reason}")]
    Malformed { path: PathBuf, reason: String },
}
