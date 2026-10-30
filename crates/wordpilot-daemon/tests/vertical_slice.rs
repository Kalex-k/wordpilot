//! The whole pipeline in one place: client -> socket -> daemon ->
//! dictionary -> ranking -> response.
//!
//! Uses a real Unix socket in a temporary directory, so the framing,
//! permissions and connection handling are all exercised, but builds its
//! dictionaries in memory so the test does not depend on what the machine has
//! installed.

use std::collections::HashMap;
use std::sync::Arc;

use wordpilot_autocomplete::{
    AutocompleteEngine, HeuristicLanguageDetector, LanguageResources, NoContextPredictor,
    NoPersonalDictionary,
};
use wordpilot_core::config::Config;
use wordpilot_core::{ApplicationContext, CompletionRequest, Language, LanguageMode, TextContext};
use wordpilot_daemon::client::Client;
use wordpilot_daemon::server::Server;
use wordpilot_daemon::state::DaemonState;
use wordpilot_dictionary::{FrequencyTable, HunspellDictionary};
use wordpilot_protocol::{Request, Response};

const DE_DIC: &str = "\
0
Bewerben/SJ
bewerben/SJ
bewerten/SJ
bewegen/SJ
bewältigen/SJ
bewahren/SJ
Erfahren/J
Straße/S
";

const DE_AFF: &str = "\
SET UTF-8
SFX J Y 3
SFX J   en      ung        en
SFX J   en      ungen      en
SFX J   0       ung        [^n]
SFX S Y 1
SFX S   0       s          .
";

const EN_DIC: &str = "\
0
apply/S
application/S
experience/S
";

const DE_FREQ: &str = "\
bewegen 9579
bewältigen 562
bewerben 482
bewerten 421
bewerbung 1200
erfahrung 5000
";

const EN_FREQ: &str = "\
apply 8000
application 5000
";

/// A running daemon on a private socket, torn down with the test.
struct Harness {
    _dir: tempfile::TempDir,
    socket: std::path::PathBuf,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Harness {
    async fn start(config: Config) -> Self {
        Self::start_with(config, None).await
    }

    /// Starts a daemon that reloads from `config_path`, which the test
    /// rewrites to simulate the user editing their config.
    async fn start_with(config: Config, config_path: Option<std::path::PathBuf>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("daemon.sock");

        let mut resources = HashMap::new();
        resources.insert(
            Language::German,
            LanguageResources::new(
                Box::new(HunspellDictionary::from_parts(
                    Language::German,
                    DE_DIC,
                    DE_AFF,
                )),
                FrequencyTable::from_text(DE_FREQ),
            ),
        );
        resources.insert(
            Language::EnglishUs,
            LanguageResources::new(
                Box::new(HunspellDictionary::from_parts(
                    Language::EnglishUs,
                    EN_DIC,
                    DE_AFF,
                )),
                FrequencyTable::from_text(EN_FREQ),
            ),
        );

        let engine = AutocompleteEngine::new(
            config,
            resources,
            Arc::new(NoPersonalDictionary),
            Arc::new(NoContextPredictor),
            Arc::new(HeuristicLanguageDetector::default()),
        );
        let state = match config_path {
            Some(path) => DaemonState::with_config_path(engine, Vec::new(), path),
            None => DaemonState::new(engine, Vec::new()),
        };
        let server = Server::bind(&socket).unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            server
                .run(state, async {
                    let _ = rx.await;
                })
                .await
                .unwrap();
        });

        Self {
            _dir: dir,
            socket,
            shutdown: Some(tx),
            task: Some(task),
        }
    }

    async fn client(&self) -> Client {
        Client::connect(&self.socket).await.unwrap()
    }

    async fn complete(&self, typed: &str, language: &str) -> wordpilot_core::CompletionResponse {
        let mut client = self.client().await;
        let request = Request::Complete(CompletionRequest {
            context: TextContext::from_typed(typed),
            language: language.parse().unwrap(),
            application: ApplicationContext::default(),
            max_candidates: None,
        });
        match client.send(&request).await.unwrap() {
            Response::Completion(completion) => completion,
            other => panic!("unexpected response: {other:?}"),
        }
    }

    async fn stop(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

#[tokio::test]
async fn the_motivating_example_works_end_to_end() {
    // Type "bew" in German, get candidates back.
    let harness = Harness::start(Config::default()).await;
    let response = harness.complete("Ich möchte mich bew", "de").await;

    assert_eq!(response.language, Language::German);
    assert_eq!(response.prefix, "bew");
    assert_eq!(response.prefix_len_bytes, 3);

    let words: Vec<&str> = response
        .candidates
        .iter()
        .map(|c| c.text.as_str())
        .collect();
    assert!(!words.is_empty(), "no candidates");
    assert!(
        words.iter().all(|w| w.to_lowercase().starts_with("bew")),
        "{words:?}"
    );
    assert!(words.contains(&"bewerben"), "{words:?}");
    // Scores must be ordered, since the frontend renders them as given.
    for pair in response.candidates.windows(2) {
        assert!(pair[0].score >= pair[1].score, "{words:?} not ordered");
    }
    harness.stop().await;
}

#[tokio::test]
async fn affix_expansion_is_reachable_over_the_socket() {
    let harness = Harness::start(Config::default()).await;
    let response = harness.complete("meine Erfahru", "de").await;
    let words: Vec<&str> = response
        .candidates
        .iter()
        .map(|c| c.text.as_str())
        .collect();
    assert!(words.contains(&"Erfahrung"), "{words:?}");
    harness.stop().await;
}

#[tokio::test]
async fn language_switching_changes_results_for_the_same_prefix() {
    let harness = Harness::start(Config::default()).await;

    let german = harness.complete("appl", "de").await;
    assert!(german.candidates.is_empty(), "{:?}", german.candidates);

    let english = harness.complete("appl", "en").await;
    assert_eq!(english.language, Language::EnglishUs);
    let words: Vec<&str> = english.candidates.iter().map(|c| c.text.as_str()).collect();
    assert!(words.contains(&"apply"), "{words:?}");
    harness.stop().await;
}

#[tokio::test]
async fn set_language_is_remembered_and_used_for_auto_requests() {
    let harness = Harness::start(Config::default()).await;
    let mut client = harness.client().await;

    let response = client
        .send(&Request::SetLanguage {
            language: LanguageMode::Fixed(Language::EnglishUs),
        })
        .await
        .unwrap();
    match response {
        Response::LanguageSet { language } => {
            assert_eq!(language, LanguageMode::Fixed(Language::EnglishUs))
        }
        other => panic!("unexpected response: {other:?}"),
    }

    // "auto" on the wire now means the language the user selected.
    let response = harness.complete("appl", "auto").await;
    assert_eq!(response.language, Language::EnglishUs);

    // And it survives across connections, since it is daemon state.
    let mut client = harness.client().await;
    match client.send(&Request::Status).await.unwrap() {
        Response::Status(status) => assert_eq!(
            status.active_language,
            LanguageMode::Fixed(Language::EnglishUs)
        ),
        other => panic!("unexpected response: {other:?}"),
    }
    harness.stop().await;
}

#[tokio::test]
async fn an_unavailable_language_is_declined_rather_than_accepted() {
    let harness = Harness::start(Config::default()).await;
    let mut client = harness.client().await;
    let response = client
        .send(&Request::SetLanguage {
            language: LanguageMode::Fixed(Language::EnglishGb),
        })
        .await
        .unwrap();
    match response {
        Response::LanguageSet { language } => {
            assert_ne!(language, LanguageMode::Fixed(Language::EnglishGb))
        }
        other => panic!("unexpected response: {other:?}"),
    }
    harness.stop().await;
}

#[tokio::test]
async fn blacklisted_and_sensitive_fields_return_nothing() {
    let harness = Harness::start(Config::default()).await;
    let mut client = harness.client().await;

    for application in [
        ApplicationContext::named("keepassxc"),
        ApplicationContext::sensitive(),
    ] {
        let request = Request::Complete(CompletionRequest {
            context: TextContext::from_typed("bew"),
            language: LanguageMode::Fixed(Language::German),
            application,
            max_candidates: None,
        });
        match client.send(&request).await.unwrap() {
            Response::Completion(completion) => {
                assert!(
                    completion.candidates.is_empty(),
                    "{:?}",
                    completion.candidates
                )
            }
            other => panic!("unexpected response: {other:?}"),
        }
    }
    harness.stop().await;
}

#[tokio::test]
async fn many_requests_share_one_connection() {
    // This is the real usage pattern: the Fcitx addon connects once and sends
    // a request per keystroke.
    let harness = Harness::start(Config::default()).await;
    let mut client = harness.client().await;

    let mut typed = String::new();
    for c in "bewerben".chars() {
        typed.push(c);
        let request = Request::Complete(CompletionRequest {
            context: TextContext::from_typed(typed.clone()),
            language: LanguageMode::Fixed(Language::German),
            application: ApplicationContext::default(),
            max_candidates: None,
        });
        match client.send(&request).await.unwrap() {
            Response::Completion(completion) => {
                assert_eq!(completion.prefix, typed);
            }
            other => panic!("unexpected response at {typed:?}: {other:?}"),
        }
    }
    harness.stop().await;
}

#[tokio::test]
async fn concurrent_clients_are_served_independently() {
    let harness = Harness::start(Config::default()).await;
    let mut first = harness.client().await;
    let mut second = harness.client().await;

    assert!(matches!(
        first.send(&Request::Ping).await.unwrap(),
        Response::Pong
    ));
    assert!(matches!(
        second.send(&Request::Ping).await.unwrap(),
        Response::Pong
    ));
    assert!(matches!(
        first.send(&Request::Ping).await.unwrap(),
        Response::Pong
    ));
    harness.stop().await;
}

#[tokio::test]
async fn accepting_a_candidate_is_acknowledged_even_without_learning() {
    let harness = Harness::start(Config::default()).await;
    let mut client = harness.client().await;
    let response = client
        .send(&Request::AcceptCandidate {
            word: "bewerben".into(),
            language: LanguageMode::Fixed(Language::German),
            application: ApplicationContext::named("firefox"),
        })
        .await
        .unwrap();
    assert!(matches!(response, Response::Accepted), "{response:?}");
    harness.stop().await;
}

#[tokio::test]
async fn accept_with_auto_language_is_what_the_fcitx_addon_sends() {
    let harness = Harness::start(Config::default()).await;
    let mut client = harness.client().await;
    let response = client
        .send(&Request::AcceptCandidate {
            word: "bewerben".into(),
            language: LanguageMode::Auto,
            application: ApplicationContext::default(),
        })
        .await
        .unwrap();
    assert!(matches!(response, Response::Accepted), "{response:?}");
    harness.stop().await;
}

#[tokio::test]
async fn config_changes_apply_on_reload_without_a_restart() {
    let config_dir = tempfile::tempdir().unwrap();
    let config_path = config_dir.path().join("config.toml");
    std::fs::write(&config_path, "").unwrap();
    let harness = Harness::start_with(Config::default(), Some(config_path.clone())).await;
    assert!(harness.complete("bew", "de").await.candidates.len() > 1);

    std::fs::write(
        &config_path,
        "[autocomplete]\nmax_candidates = 1\n[hotkeys]\nlanguage_german = \"Ctrl+Alt+G\"\n",
    )
    .unwrap();
    let mut client = harness.client().await;
    match client.send(&Request::Reload).await.unwrap() {
        Response::Reloaded(outcome) => {
            assert!(outcome.changed);
            assert!(
                !outcome.dictionaries_reloaded,
                "dictionaries were untouched"
            );
            assert_eq!(outcome.generation, 1);
        }
        other => panic!("unexpected response: {other:?}"),
    }
    assert_eq!(harness.complete("bew", "de").await.candidates.len(), 1);

    match client.send(&Request::FrontendConfig).await.unwrap() {
        Response::FrontendConfig(frontend) => {
            assert_eq!(frontend.generation, 1);
            assert_eq!(frontend.max_candidates, 1);
            assert_eq!(frontend.hotkeys.language_german, "Control+Alt+G");
        }
        other => panic!("unexpected response: {other:?}"),
    }

    // A broken file is reported and the running config stays in effect.
    std::fs::write(&config_path, "[autocomplete]\nmax_candidates = 0\n").unwrap();
    match client.send(&Request::Reload).await.unwrap() {
        Response::Error { code, .. } => assert_eq!(code, "bad-request"),
        other => panic!("unexpected response: {other:?}"),
    }
    assert_eq!(harness.complete("bew", "de").await.candidates.len(), 1);
    harness.stop().await;
}

#[tokio::test]
async fn unimplemented_features_report_an_error_instead_of_a_fake_answer() {
    // Unimplemented features must report an error, not a fake answer.
    let harness = Harness::start(Config::default()).await;
    let mut client = harness.client().await;
    let response = client
        .send(&Request::Grammar(wordpilot_core::GrammarRequest {
            text: "Ich habe viele Erfahrung.".into(),
            language: Language::German,
        }))
        .await
        .unwrap();
    match response {
        Response::Error { code, .. } => assert_eq!(code, "unsupported"),
        other => panic!("unexpected response: {other:?}"),
    }
    harness.stop().await;
}

#[tokio::test]
async fn disabling_autocomplete_does_not_break_the_daemon() {
    // Switching a subsystem off leaves the rest alive.
    let mut config = Config::default();
    config.autocomplete.enabled = false;
    let harness = Harness::start(config).await;

    let response = harness.complete("bew", "de").await;
    assert!(response.candidates.is_empty());

    let mut client = harness.client().await;
    assert!(matches!(
        client.send(&Request::Ping).await.unwrap(),
        Response::Pong
    ));
    harness.stop().await;
}

#[tokio::test]
async fn a_malformed_frame_does_not_drop_the_connection() {
    use tokio::io::AsyncWriteExt;

    let harness = Harness::start(Config::default()).await;
    let mut stream = tokio::net::UnixStream::connect(&harness.socket)
        .await
        .unwrap();
    stream.write_all(b"{ this is not json }\n").await.unwrap();

    let (reader, writer) = stream.into_split();
    let mut framed = wordpilot_protocol::Framed::new(reader, writer);
    match framed.read::<Response>().await.unwrap().unwrap() {
        Response::Error { code, .. } => assert_eq!(code, "bad-request"),
        other => panic!("unexpected response: {other:?}"),
    }
    // The same connection still works.
    framed.write(&Request::Ping).await.unwrap();
    assert!(matches!(
        framed.read::<Response>().await.unwrap().unwrap(),
        Response::Pong
    ));
    harness.stop().await;
}

#[tokio::test]
async fn the_daemon_survives_a_client_that_disconnects_mid_request() {
    let harness = Harness::start(Config::default()).await;
    {
        let stream = tokio::net::UnixStream::connect(&harness.socket)
            .await
            .unwrap();
        drop(stream);
    }
    // A later client is unaffected.
    let mut client = harness.client().await;
    assert!(matches!(
        client.send(&Request::Ping).await.unwrap(),
        Response::Pong
    ));
    harness.stop().await;
}

#[tokio::test]
async fn the_socket_is_removed_on_shutdown_so_a_restart_is_clean() {
    let harness = Harness::start(Config::default()).await;
    let socket = harness.socket.clone();
    assert!(socket.exists());
    harness.stop().await;
    assert!(!socket.exists(), "socket should be cleaned up");
}
