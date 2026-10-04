use std::path::PathBuf;

/// Errors produced by core configuration and path handling.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("config file {path} could not be read: {source}")]
    ConfigRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("config file {path} is not valid TOML: {source}")]
    ConfigParse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("invalid configuration: {0}")]
    ConfigInvalid(String),

    #[error("unknown language tag {0:?}")]
    UnknownLanguage(String),

    #[error("required directory {0} is unavailable")]
    MissingDirectory(&'static str),
}

pub type Result<T> = std::result::Result<T, CoreError>;
