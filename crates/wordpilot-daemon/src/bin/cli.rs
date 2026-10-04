//! `wordpilot`: the diagnostic CLI (spec §41).

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
    let config =
        Config::load(&config_path).with_context(|| format!("loading {}", config_path.display()))?;
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
        Command::Doctor => doctor(&config, &config_path, &socket).await,
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
            println!("grammar      {}", onoff(status.grammar_enabled));
            println!("ai           {}", onoff(status.ai_enabled));
            Ok(())
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
    // The key itself is never printed (spec §30, §42).
    let key_present = std::env::var_os(&config.ai.api_key_env).is_some();
    println!(
        "api key   {} (from ${})",
        if key_present { "set" } else { "not set" },
        config.ai.api_key_env
    );
    println!("note      AI actions are not implemented in this build (Phase 1C)");
}

/// `wordpilot doctor` (spec §41).
///
/// Checks everything and reports all findings rather than stopping at the
/// first failure: the point is a complete picture of what is missing.
async fn doctor(
    config: &Config,
    config_path: &std::path::Path,
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
    if config_path.exists() {
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
    let addon = fcitx_addon_path();
    match &addon {
        Some(path) => check(true, "addon", path.clone()),
        None => check(
            false,
            "addon",
            "wordpilot addon not installed (Phase 1B)".into(),
        ),
    }
    for var in ["GTK_IM_MODULE", "QT_IM_MODULE", "XMODIFIERS"] {
        match std::env::var(var) {
            Ok(value) if !value.is_empty() => check(true, var, value),
            _ => check(false, var, "not set; Fcitx will not be used by apps".into()),
        }
    }

    println!("languagetool");
    match check_http(&config.grammar.endpoint).await {
        Ok(()) => check(true, "endpoint", config.grammar.endpoint.clone()),
        Err(error) => check(
            false,
            "endpoint",
            format!("{} unreachable: {error}", config.grammar.endpoint),
        ),
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

fn fcitx_addon_path() -> Option<String> {
    let candidates = [
        PathBuf::from("/usr/lib/fcitx5/libwordpilot.so"),
        PathBuf::from("/usr/lib64/fcitx5/libwordpilot.so"),
    ];
    candidates
        .into_iter()
        .chain(
            wordpilot_core::paths::data_dir()
                .ok()
                .map(|d| d.join("fcitx5/libwordpilot.so")),
        )
        .find(|path| path.is_file())
        .map(|path| path.display().to_string())
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
