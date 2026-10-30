//! XDG base-directory resolution.
//!
//! No user-specific path is ever hardcoded; everything derives from the
//! environment with the standard `$HOME`-relative fallbacks.

use std::path::PathBuf;

use crate::error::CoreError;

const APP: &str = "wordpilot";

fn env_dir(var: &str) -> Option<PathBuf> {
    match std::env::var_os(var) {
        Some(value) if !value.is_empty() => {
            let path = PathBuf::from(value);
            // The XDG spec says relative paths must be ignored.
            path.is_absolute().then_some(path)
        }
        _ => None,
    }
}

fn home() -> Option<PathBuf> {
    env_dir("HOME")
}

/// `$XDG_CONFIG_HOME/wordpilot`, else `~/.config/wordpilot`.
pub fn config_dir() -> Result<PathBuf, CoreError> {
    env_dir("XDG_CONFIG_HOME")
        .or_else(|| home().map(|h| h.join(".config")))
        .map(|base| base.join(APP))
        .ok_or(CoreError::MissingDirectory("XDG_CONFIG_HOME or HOME"))
}

/// `$XDG_DATA_HOME/wordpilot`, else `~/.local/share/wordpilot`.
pub fn data_dir() -> Result<PathBuf, CoreError> {
    env_dir("XDG_DATA_HOME")
        .or_else(|| home().map(|h| h.join(".local").join("share")))
        .map(|base| base.join(APP))
        .ok_or(CoreError::MissingDirectory("XDG_DATA_HOME or HOME"))
}

/// `$XDG_CACHE_HOME/wordpilot`, else `~/.cache/wordpilot`.
pub fn cache_dir() -> Result<PathBuf, CoreError> {
    env_dir("XDG_CACHE_HOME")
        .or_else(|| home().map(|h| h.join(".cache")))
        .map(|base| base.join(APP))
        .ok_or(CoreError::MissingDirectory("XDG_CACHE_HOME or HOME"))
}

/// `$XDG_RUNTIME_DIR/wordpilot`.
///
/// There is no `$HOME` fallback on purpose: the socket must live on a
/// user-private directory, and `/tmp` is world-readable.
pub fn runtime_dir() -> Result<PathBuf, CoreError> {
    env_dir("XDG_RUNTIME_DIR")
        .map(|base| base.join(APP))
        .ok_or(CoreError::MissingDirectory("XDG_RUNTIME_DIR"))
}

/// Default config file location.
pub fn config_file() -> Result<PathBuf, CoreError> {
    Ok(config_dir()?.join("config.toml"))
}

/// Default daemon socket path.
pub fn socket_path() -> Result<PathBuf, CoreError> {
    Ok(runtime_dir()?.join("daemon.sock"))
}

/// Default personal-dictionary database location.
pub fn personal_db_path() -> Result<PathBuf, CoreError> {
    Ok(data_dir()?.join("personal.sqlite"))
}

/// Directories searched for Hunspell dictionaries, most specific first.
///
/// The user's own data directory wins over system paths so that dictionaries
/// can be installed without root.
pub fn default_dictionary_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(data) = data_dir() {
        dirs.push(data.join("dictionaries"));
    }
    dirs.push(PathBuf::from("/usr/share/hunspell"));
    dirs.push(PathBuf::from("/usr/share/myspell/dicts"));
    dirs
}

/// Directories searched for frequency word lists.
pub fn default_frequency_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(data) = data_dir() {
        dirs.push(data.join("frequency"));
    }
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;

    // These tests mutate process-wide environment, so they are serialized
    // behind one `#[test]` rather than racing each other.
    #[test]
    fn xdg_vars_take_precedence_and_relative_paths_are_ignored() {
        let restore = |key: &str, old: Option<std::ffi::OsString>| match old {
            Some(value) => unsafe { std::env::set_var(key, value) },
            None => unsafe { std::env::remove_var(key) },
        };
        let old_config = std::env::var_os("XDG_CONFIG_HOME");
        let old_home = std::env::var_os("HOME");
        let old_runtime = std::env::var_os("XDG_RUNTIME_DIR");

        unsafe { std::env::set_var("HOME", "/home/testuser") };

        unsafe { std::env::set_var("XDG_CONFIG_HOME", "/custom/cfg") };
        assert_eq!(
            config_dir().unwrap(),
            PathBuf::from("/custom/cfg/wordpilot")
        );

        // Empty and relative values must fall back to $HOME.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", "") };
        assert_eq!(
            config_dir().unwrap(),
            PathBuf::from("/home/testuser/.config/wordpilot")
        );
        unsafe { std::env::set_var("XDG_CONFIG_HOME", "relative/cfg") };
        assert_eq!(
            config_dir().unwrap(),
            PathBuf::from("/home/testuser/.config/wordpilot")
        );

        // Runtime dir has no fallback.
        unsafe { std::env::remove_var("XDG_RUNTIME_DIR") };
        assert!(runtime_dir().is_err());
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", "/run/user/1000") };
        assert_eq!(
            socket_path().unwrap(),
            PathBuf::from("/run/user/1000/wordpilot/daemon.sock")
        );

        restore("XDG_CONFIG_HOME", old_config);
        restore("HOME", old_home);
        restore("XDG_RUNTIME_DIR", old_runtime);
    }

    #[test]
    fn dictionary_search_prefers_user_data_dir_over_system() {
        let dirs = default_dictionary_dirs();
        let system = dirs
            .iter()
            .position(|p| p == &PathBuf::from("/usr/share/hunspell"))
            .expect("system dir present");
        assert!(system > 0, "user dictionaries must be searched first");
    }
}
