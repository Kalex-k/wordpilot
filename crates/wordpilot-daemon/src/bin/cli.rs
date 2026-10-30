//! `wordpilot`: the diagnostic CLI.

use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};
use wordpilot_core::config::Config;
use wordpilot_core::{Language, LanguageMode};
use wordpilot_daemon::client::Client;
use wordpilot_protocol::{Request, Response};

#[derive(Parser, Debug)]
#[command(name = "wordpilot", version, about = "Control and diagnose WordPilot")]
struct Args {
    #[arg(long, value_name = "PATH", global = true)]
    config: Option<PathBuf>,
    #[arg(long, value_name = "PATH", global = true)]
    socket: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Daemon status and loaded dictionaries.
    Status,
    /// Show or set the active language (de, en, en-GB, auto).
    Language { lang: Option<String> },
    /// Dictionary and frequency-list statistics.
    Dictionary {
        #[command(subcommand)]
        command: DictionaryCommand,
    },
    /// AI subsystem status.
    Ai {
        #[command(subcommand)]
        command: AiCommand,
    },
    /// Make the running daemon re-read its config file now.
    ///
    /// The daemon also notices edits on its own within a second; this is for
    /// scripts and for seeing whether the new file was accepted.
    Reload,
    /// Check every dependency and report what is wrong.
    Doctor,
}

#[derive(Subcommand, Debug)]
enum DictionaryCommand {
    Stats,
}

#[derive(Subcommand, Debug)]
enum AiCommand {
    Status,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let config_path = match &args.config {
        Some(path) => path.clone(),
        None => wordpilot_core::paths::config_file()?,
    };
    // A broken config must not lock the user out of the CLI: `reload` is how
    // they learn what the daemon thinks of the file, and `doctor` reports it.
    let (config, config_error) = match Config::load(&config_path) {
        Ok(config) => (config, None),
        Err(error) => {
            let message = error.to_string();
            if !matches!(args.command, Command::Doctor) {
                eprintln!("warning: {message}\nwarning: continuing with built-in defaults");
            }
            (Config::default(), Some(message))
        }
    };
    let socket = match &args.socket {
        Some(path) => path.clone(),
        None => config.socket_path()?,
    };

    match args.command {
        Command::Status => status(&socket).await,
        Command::Language { lang } => language(&socket, lang.as_deref()).await,
        Command::Dictionary {
            command: DictionaryCommand::Stats,
        } => dictionary_stats(&socket).await,
        Command::Ai {
            command: AiCommand::Status,
        } => {
            ai_status(&config);
            Ok(())
        }
        Command::Reload => reload(&socket).await,
        Command::Doctor => doctor(&config, &config_path, config_error.as_deref(), &socket).await,
    }
}

async fn request(socket: &std::path::Path, request: Request) -> anyhow::Result<Response> {
    Client::connect(socket).await?.send(&request).await
}

async fn status(socket: &std::path::Path) -> anyhow::Result<()> {
    match request(socket, Request::Status).await? {
        Response::Status(status) => {
            println!("daemon       running (version {})", status.daemon_version);
            println!("protocol     {}", status.protocol_version);
            println!("uptime       {}s", status.uptime_secs);
            println!("language     {}", status.active_language);
            println!("autocomplete {}", onoff(status.autocomplete_enabled));
            println!("learning     {}", onoff(status.learning_enabled));
            // Both switches exist in the config, but neither feature is in
            // this build; printing "enabled" alone would suggest otherwise.
            println!(
                "grammar      {} in config, not implemented yet",
                onoff(status.grammar_enabled)
            );
            println!(
                "ai           {} in config, not implemented yet",
                onoff(status.ai_enabled)
            );
            println!("config gen   {}", status.config_generation);
            Ok(())
        }
        other => unexpected(other),
    }
}

async fn reload(socket: &std::path::Path) -> anyhow::Result<()> {
    match request(socket, Request::Reload).await? {
        Response::Reloaded(outcome) => {
            if !outcome.changed {
                println!("config unchanged (generation {})", outcome.generation);
                return Ok(());
            }
            println!("config reloaded (generation {})", outcome.generation);
            if outcome.dictionaries_reloaded {
                println!("dictionaries re-read");
            }
            for setting in &outcome.restart_required {
                println!("restart the daemon to apply {setting}");
            }
            Ok(())
        }
        Response::Error { message, .. } => {
            anyhow::bail!("config rejected, the daemon keeps the previous one: {message}")
        }
        other => unexpected(other),
    }
}

async fn language(socket: &std::path::Path, lang: Option<&str>) -> anyhow::Result<()> {
    match lang {
        None => match request(socket, Request::Status).await? {
            Response::Status(status) => {
                println!("{}", status.active_language);
                Ok(())
            }
            other => unexpected(other),
        },
        Some(value) => {
            let language: LanguageMode = value.parse().with_context(|| {
                format!("unknown language {value:?}; try de, en, en-GB or auto")
            })?;
            match request(socket, Request::SetLanguage { language }).await? {
                Response::LanguageSet { language: active } => {
                    if active != language {
                        // The daemon declines a language it cannot serve, and
                        // saying so is more useful than printing the old value.
                        println!("{active} (requested {language} is not available)");
                    } else {
                        println!("{active}");
                    }
                    Ok(())
                }
                other => unexpected(other),
            }
        }
    }
}

async fn dictionary_stats(socket: &std::path::Path) -> anyhow::Result<()> {
    match request(socket, Request::Status).await? {
        Response::Status(status) => {
            if status.loaded_languages.is_empty() {
                println!("no dictionaries loaded");
                return Ok(());
            }
            println!("{:<8} {:>10} {:>12}", "language", "words", "frequencies");
            for loaded in &status.loaded_languages {
                println!(
                    "{:<8} {:>10} {:>12}",
                    loaded.language.tag(),
                    loaded.dictionary_words,
                    loaded.frequency_words
                );
            }
            Ok(())
        }
        other => unexpected(other),
    }
}

fn ai_status(config: &Config) {
    println!("enabled   {}", onoff(config.ai.enabled));
    println!("provider  {}", config.ai.provider);
    println!("endpoint  {}", config.ai.endpoint);
    println!("model     {}", config.ai.model);
    // The key itself is never printed.
    let key_present = std::env::var_os(&config.ai.api_key_env).is_some();
    println!(
        "api key   {} (from ${})",
        if key_present { "set" } else { "not set" },
        config.ai.api_key_env
    );
    println!("note      AI actions are not implemented yet");
}

/// `wordpilot doctor`.
///
/// Checks everything and reports all findings rather than stopping at the
/// first failure: the point is a complete picture of what is missing.
async fn doctor(
    config: &Config,
    config_path: &std::path::Path,
    config_error: Option<&str>,
    socket: &std::path::Path,
) -> anyhow::Result<()> {
    let mut problems = 0usize;

    let mut check = |ok: bool, label: &str, detail: String| {
        if ok {
            println!("  ok    {label:<16} {detail}");
        } else {
            problems += 1;
            println!("  FAIL  {label:<16} {detail}");
        }
    };

    println!("config");
    if let Some(error) = config_error {
        check(
            false,
            "file",
            format!("{error} (checks below use defaults)"),
        );
    } else if config_path.exists() {
        check(true, "file", format!("{}", config_path.display()));
    } else {
        // Not a failure: defaults are a working configuration.
        println!(
            "  ok    {:<16} {} (absent, using defaults)",
            "file",
            config_path.display()
        );
    }

    println!("xdg");
    for (label, result) in [
        ("config dir", wordpilot_core::paths::config_dir()),
        ("data dir", wordpilot_core::paths::data_dir()),
        ("cache dir", wordpilot_core::paths::cache_dir()),
        ("runtime dir", wordpilot_core::paths::runtime_dir()),
    ] {
        match result {
            Ok(path) => check(true, label, format!("{}", path.display())),
            Err(error) => check(false, label, error.to_string()),
        }
    }

    println!("dictionaries");
    let dirs = config.dictionaries.resolved_search_paths();
    for &language in &config.dictionaries.languages {
        match wordpilot_dictionary::HunspellDictionary::discover(language, &dirs) {
            Ok(dictionary) => {
                use wordpilot_dictionary::DictionaryProvider;
                check(
                    true,
                    language.tag(),
                    format!(
                        "{} words from {}",
                        dictionary.len(),
                        dictionary.path().display()
                    ),
                );
            }
            Err(error) => check(false, language.tag(), error.to_string()),
        }
    }

    println!("frequency lists");
    let frequency_dirs = config.dictionaries.resolved_frequency_paths();
    for &language in &config.dictionaries.languages {
        let table = wordpilot_dictionary::FrequencyTable::discover(language, &frequency_dirs);
        if table.is_empty() {
            // Degraded, not broken: ranking falls back to other signals.
            println!(
                "  warn  {:<16} no list found; ranking quality will be reduced",
                language.tag()
            );
        } else {
            check(true, language.tag(), format!("{} words", table.len()));
        }
    }

    println!("daemon");
    match request(socket, Request::Status).await {
        Ok(Response::Status(status)) => {
            check(
                true,
                "socket",
                format!(
                    "{} (protocol {})",
                    socket.display(),
                    status.protocol_version
                ),
            );
            if status.protocol_version != wordpilot_protocol::PROTOCOL_VERSION {
                check(
                    false,
                    "protocol",
                    format!(
                        "daemon speaks {}, this CLI speaks {}",
                        status.protocol_version,
                        wordpilot_protocol::PROTOCOL_VERSION
                    ),
                );
            }
            if status.loaded_languages.is_empty() {
                check(false, "dictionaries", "daemon loaded none".into());
            }
        }
        Ok(other) => check(false, "socket", format!("unexpected reply: {other:?}")),
        Err(error) => check(false, "socket", format!("{error:#}")),
    }

    println!("fcitx 5");
    let fcitx = which("fcitx5");
    match &fcitx {
        Some(path) => check(true, "binary", path.clone()),
        None => check(false, "binary", "fcitx5 not found in PATH".into()),
    }
    if process_running("fcitx5") {
        check(true, "process", "running".into());
    } else {
        check(false, "process", "fcitx5 is not running".into());
    }
    match fcitx_addon_path() {
        Some(path) => check(true, "addon", path),
        None => check(
            false,
            "addon",
            "libwordpilot.so not found in $FCITX_ADDON_DIRS or /usr/lib/fcitx5".into(),
        ),
    }

    // Which variables an application needs depends on the display server:
    // on X11 everything goes through the IM modules and XIM; on Wayland the
    // compositor's text-input protocol is preferred and GTK_IM_MODULE is
    // only a fallback for compositors without it.
    let session = std::env::var("XDG_SESSION_TYPE").unwrap_or_default();
    let wayland = session == "wayland" || std::env::var_os("WAYLAND_DISPLAY").is_some();
    println!(
        "  info  {:<16} {}",
        "session",
        if session.is_empty() {
            "unknown"
        } else {
            session.as_str()
        }
    );
    for var in ["GTK_IM_MODULE", "QT_IM_MODULE", "XMODIFIERS"] {
        let value = std::env::var(var).unwrap_or_default();
        match (value.is_empty(), wayland, var) {
            (false, _, _) => check(
                value.contains("fcitx"),
                var,
                if value.contains("fcitx") {
                    value
                } else {
                    format!("{value} (not fcitx)")
                },
            ),
            // XMODIFIERS still matters for XWayland applications.
            (true, true, "XMODIFIERS") => check(
                false,
                var,
                "not set; XWayland apps will not use Fcitx".into(),
            ),
            (true, true, _) => println!(
                "  ok    {var:<16} not set (fine on Wayland if the compositor supports text-input)"
            ),
            (true, false, _) => check(
                false,
                var,
                "not set in this shell; X11 apps started from it will not use Fcitx".into(),
            ),
        }
    }

    // Grammar checking is not implemented yet, so an unreachable
    // LanguageTool is reported but not counted as a problem: nothing in this
    // build would use it.
    println!("languagetool (not used by this build yet)");
    if config.grammar.enabled {
        match check_http(&config.grammar.endpoint).await {
            Ok(()) => check(true, "endpoint", config.grammar.endpoint.clone()),
            Err(error) => println!(
                "  warn  {:<16} {} unreachable: {error}",
                "endpoint", config.grammar.endpoint
            ),
        }
    } else {
        println!("  ok    {:<16} disabled in config", "enabled");
    }

    println!("ai provider");
    if config.ai.enabled {
        match check_http(&config.ai.endpoint).await {
            Ok(()) => check(true, "endpoint", config.ai.endpoint.clone()),
            Err(error) => check(
                false,
                "endpoint",
                format!("{} unreachable: {error}", config.ai.endpoint),
            ),
        }
    } else {
        println!("  ok    {:<16} disabled in config", "enabled");
    }

    println!();
    if problems == 0 {
        println!("no problems found");
        Ok(())
    } else {
        println!("{problems} problem(s) found");
        std::process::exit(1);
    }
}

/// A bare TCP connect, deliberately not an HTTP request.
///
/// `doctor` must not pull in an HTTP client just to answer "is anything
/// listening there", and a TCP connect answers exactly the question the user
/// is asking at this point.
async fn check_http(endpoint: &str) -> Result<(), String> {
    let rest = endpoint
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(endpoint);
    let authority = rest.split('/').next().unwrap_or(rest);
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (
            host,
            port.parse::<u16>()
                .map_err(|_| format!("bad port in {endpoint:?}"))?,
        ),
        None if endpoint.starts_with("https") => (authority, 443),
        None => (authority, 80),
    };

    let connect = tokio::net::TcpStream::connect((host, port));
    match tokio::time::timeout(std::time::Duration::from_millis(1500), connect).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(error)) => Err(error.to_string()),
        Err(_) => Err("timed out".into()),
    }
}

fn which(binary: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(binary))
        .find(|candidate| candidate.is_file())
        .map(|candidate| candidate.display().to_string())
}

/// Where Fcitx would load the addon from: `$FCITX_ADDON_DIRS` when set (it
/// replaces the default), otherwise the distribution's library directory.
fn fcitx_addon_path() -> Option<String> {
    let dirs: Vec<PathBuf> = match std::env::var_os("FCITX_ADDON_DIRS") {
        Some(value) if !value.is_empty() => std::env::split_paths(&value).collect(),
        _ => vec![
            PathBuf::from("/usr/lib/fcitx5"),
            PathBuf::from("/usr/lib64/fcitx5"),
            PathBuf::from("/usr/local/lib/fcitx5"),
        ],
    };
    dirs.into_iter()
        .map(|dir| dir.join("libwordpilot.so"))
        .find(|path| path.is_file())
        .map(|path| path.display().to_string())
}

/// Whether a process with this exact command name exists, read from /proc so
/// `doctor` does not depend on `pgrep` being installed.
fn process_running(name: &str) -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return false;
    };
    entries.flatten().any(|entry| {
        std::fs::read_to_string(entry.path().join("comm")).is_ok_and(|comm| comm.trim_end() == name)
    })
}

fn onoff(value: bool) -> &'static str {
    if value {
        "enabled"
    } else {
        "disabled"
    }
}

fn unexpected(response: Response) -> anyhow::Result<()> {
    anyhow::bail!("unexpected response from daemon: {response:?}")
}

// Silences an unused-import warning when `Language` is only used via parsing.
const _: Option<Language> = None;
