//! `wordpilot-daemon`: the long-running engine process.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use tokio::signal::unix::{signal, SignalKind};
use wordpilot_autocomplete::{AutocompleteEngine, HeuristicLanguageDetector, NoContextPredictor};
use wordpilot_core::config::Config;
use wordpilot_daemon::{loader, server::Server, state::DaemonState};

#[derive(Parser, Debug)]
#[command(
    name = "wordpilot-daemon",
    version,
    about = "WordPilot engine daemon (autocomplete, grammar, AI)"
)]
struct Args {
    /// Config file. Defaults to $XDG_CONFIG_HOME/wordpilot/config.toml.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Socket path. Overrides the config and the XDG default.
    #[arg(long, value_name = "PATH")]
    socket: Option<PathBuf>,

    /// Log level, overriding config and RUST_LOG.
    #[arg(long, value_name = "LEVEL")]
    log_level: Option<String>,
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

    init_tracing(args.log_level.as_deref().unwrap_or(&config.logging.level));

    tracing::info!(
        version = wordpilot_daemon::DAEMON_VERSION,
        config = %config_path.display(),
        "starting wordpilot-daemon"
    );
    if config.logging.log_user_text {
        tracing::warn!("logging.log_user_text is on: logs will contain what you type");
    }

    let outcome = loader::load_languages(&config);
    if outcome.resources.is_empty() {
        // Not fatal: the daemon still answers status and doctor, which is how
        // the user finds out what to install.
        tracing::error!(
            "no dictionaries loaded; run `wordpilot doctor` -- autocomplete will return nothing"
        );
    }

    let socket_path = match &args.socket {
        Some(path) => path.clone(),
        None => config.socket_path()?,
    };

    let personal = loader::load_personal_dictionary(&config);
    let engine = AutocompleteEngine::new(
        config,
        outcome.resources,
        personal,
        Arc::new(NoContextPredictor),
        Arc::new(HeuristicLanguageDetector::default()),
    );
    let state = DaemonState::with_config_path(engine, outcome.failures, config_path.clone());

    let server = Server::bind(&socket_path)?;
    tokio::spawn(watch_config(Arc::clone(&state), config_path));
    server.run(state, shutdown_signal()).await
}

/// How often the config file is checked for changes.
const CONFIG_POLL: Duration = Duration::from_secs(1);

/// Applies config edits while the daemon runs, so changing a setting never
/// needs a restart, let alone a rebuild.
///
/// Polls the file's metadata instead of using inotify: one `stat` a second is
/// free, needs no extra dependency, and survives editors that save by
/// writing a new file and renaming it over the old one. SIGHUP forces a
/// reload immediately (`systemctl --user reload wordpilot`).
async fn watch_config(state: Arc<DaemonState>, path: PathBuf) {
    let fingerprint = |path: &PathBuf| {
        std::fs::metadata(path)
            .ok()
            .map(|meta| (meta.modified().ok(), meta.len()))
    };
    let mut hangup = match signal(SignalKind::hangup()) {
        Ok(signal) => Some(signal),
        Err(error) => {
            tracing::warn!(%error, "cannot listen for SIGHUP; relying on file polling");
            None
        }
    };
    let mut last = fingerprint(&path);
    let mut ticker = tokio::time::interval(CONFIG_POLL);
    loop {
        let forced = tokio::select! {
            _ = ticker.tick() => false,
            Some(()) = async { match hangup.as_mut() { Some(h) => h.recv().await, None => std::future::pending().await } } => true,
        };
        let current = fingerprint(&path);
        if !forced && current == last {
            continue;
        }
        last = current;
        let state = Arc::clone(&state);
        match tokio::task::spawn_blocking(move || state.reload()).await {
            Ok(Ok(outcome)) if outcome.changed => {
                tracing::info!(
                    generation = outcome.generation,
                    dictionaries = outcome.dictionaries_reloaded,
                    "config reloaded"
                );
                if !outcome.restart_required.is_empty() {
                    tracing::warn!(
                        settings = ?outcome.restart_required,
                        "these settings only take effect after a daemon restart"
                    );
                }
            }
            Ok(Ok(_)) => tracing::debug!("config file touched but unchanged"),
            Ok(Err(message)) => {
                tracing::error!(%message, "config reload failed; keeping the running config")
            }
            Err(error) => tracing::error!(%error, "config reload task failed"),
        }
    }
}

/// Resolves on SIGINT or SIGTERM, so systemd can stop the daemon cleanly and
/// the socket gets removed.
async fn shutdown_signal() {
    let mut terminate = match tokio::signal::unix::signal(
        tokio::signal::unix::SignalKind::terminate(),
    ) {
        Ok(signal) => signal,
        Err(error) => {
            tracing::warn!(%error, "cannot listen for SIGTERM; only Ctrl-C will stop the daemon");
            let _ = tokio::signal::ctrl_c().await;
            return;
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = terminate.recv() => {}
    }
}

fn init_tracing(level: &str) {
    use tracing_subscriber::EnvFilter;
    // RUST_LOG still wins, so a user debugging one subsystem does not have to
    // edit the config file.
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(level))
        .unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}
