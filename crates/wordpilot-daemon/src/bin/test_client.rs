//! `wordpilot-test-client`: drives the daemon without Fcitx (spec §46).
//!
//! This is the loop the Fcitx addon will perform, reduced to a command line,
//! so the engine can be developed and demonstrated on its own.

use std::path::PathBuf;

use anyhow::{bail, Context};
use clap::{Parser, Subcommand};
use wordpilot_core::{ApplicationContext, CompletionRequest, LanguageMode, TextContext};
use wordpilot_daemon::client::Client;
use wordpilot_protocol::{Request, Response};

#[derive(Parser, Debug)]
#[command(
    name = "wordpilot-test-client",
    version,
    about = "Send requests to wordpilot-daemon as a fake input method"
)]
struct Args {
    /// Socket path. Defaults to $XDG_RUNTIME_DIR/wordpilot/daemon.sock.
    #[arg(long, value_name = "PATH", global = true)]
    socket: Option<PathBuf>,

    /// Print the raw JSON response instead of a formatted list.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Request completions for a prefix, as if it had just been typed.
    Complete {
        /// Language: de, en, en-GB or auto.
        #[arg(long, default_value = "auto")]
        lang: String,
        /// Reported application name, for blacklist and ranking tests.
        #[arg(long)]
        app: Option<String>,
        /// Treat the field as a password field.
        #[arg(long)]
        sensitive: bool,
        #[arg(long, value_name = "N")]
        limit: Option<usize>,
        /// Text to the right of the caret.
        #[arg(long, default_value = "")]
        after: String,
        /// Everything typed before the caret. The last partial word is the
        /// prefix, so `complete "Ich moechte mich bew"` and `complete bew`
        /// differ only in available context.
        text: Vec<String>,
    },
    /// Report that a candidate was accepted (a learning signal).
    Accept {
        #[arg(long, default_value = "de")]
        lang: String,
        #[arg(long)]
        app: Option<String>,
        word: String,
    },
    /// Switch the daemon's active language.
    Language { lang: String },
    /// Print daemon status.
    Status,
    /// Round-trip check.
    Ping,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let socket = match args.socket {
        Some(path) => path,
        None => wordpilot_core::paths::socket_path()?,
    };
    let mut client = Client::connect(&socket).await?;

    let request = build_request(&args.command)?;
    let response = client.send(&request).await?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }
    render(&response);
    // A daemon-side error is a failure for scripting purposes.
    if matches!(response, Response::Error { .. }) {
        std::process::exit(1);
    }
    Ok(())
}

fn build_request(command: &Command) -> anyhow::Result<Request> {
    Ok(match command {
        Command::Complete {
            lang,
            app,
            sensitive,
            limit,
            after,
            text,
        } => {
            let before = text.join(" ");
            if before.is_empty() {
                bail!("nothing to complete: pass the text typed before the caret");
            }
            Request::Complete(CompletionRequest {
                context: TextContext::new(before, after.clone()),
                language: parse_language(lang)?,
                application: ApplicationContext {
                    name: app.clone(),
                    sensitive: *sensitive,
                },
                max_candidates: *limit,
            })
        }
        Command::Accept { lang, app, word } => {
            let language = match parse_language(lang)? {
                LanguageMode::Fixed(language) => language,
                LanguageMode::Auto => bail!("accept needs a concrete language, not 'auto'"),
            };
            Request::AcceptCandidate {
                word: word.clone(),
                language,
                application: ApplicationContext {
                    name: app.clone(),
                    sensitive: false,
                },
            }
        }
        Command::Language { lang } => Request::SetLanguage {
            language: parse_language(lang)?,
        },
        Command::Status => Request::Status,
        Command::Ping => Request::Ping,
    })
}

fn parse_language(value: &str) -> anyhow::Result<LanguageMode> {
    value
        .parse::<LanguageMode>()
        .with_context(|| format!("unknown language {value:?}; try de, en, en-GB or auto"))
}

fn render(response: &Response) {
    match response {
        Response::Completion(completion) => {
            if completion.candidates.is_empty() {
                println!(
                    "no candidates for {:?} ({})",
                    completion.prefix, completion.language
                );
                return;
            }
            println!(
                "{} candidates for {:?} ({})",
                completion.candidates.len(),
                completion.prefix,
                completion.language
            );
            for (index, candidate) in completion.candidates.iter().enumerate() {
                println!(
                    "{:>2}. {:<28} {:>6.3}  {:?}",
                    index + 1,
                    candidate.text,
                    candidate.score,
                    candidate.source
                );
            }
        }
        Response::Accepted => println!("recorded"),
        Response::LanguageSet { language } => println!("active language: {language}"),
        Response::Status(status) => print_status(status),
        Response::Pong => println!("pong"),
        Response::Error { code, message } => eprintln!("error [{code}]: {message}"),
        other => println!("{other:?}"),
    }
}

fn print_status(status: &wordpilot_protocol::ServerStatus) {
    println!("wordpilot-daemon {}", status.daemon_version);
    println!("  protocol        {}", status.protocol_version);
    println!("  uptime          {}s", status.uptime_secs);
    println!("  active language {}", status.active_language);
    println!("  autocomplete    {}", onoff(status.autocomplete_enabled));
    println!("  learning        {}", onoff(status.learning_enabled));
    println!("  grammar         {}", onoff(status.grammar_enabled));
    println!("  ai              {}", onoff(status.ai_enabled));
    if status.loaded_languages.is_empty() {
        println!("  dictionaries    none loaded");
    }
    for loaded in &status.loaded_languages {
        println!(
            "  dictionary      {} ({} words, {} frequencies)",
            loaded.language, loaded.dictionary_words, loaded.frequency_words
        );
    }
}

fn onoff(value: bool) -> &'static str {
    if value {
        "enabled"
    } else {
        "disabled"
    }
}
