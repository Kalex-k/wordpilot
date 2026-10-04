//! Latency check against the dictionaries actually installed on this machine
//! (spec §36: under 30 ms per keystroke, ideally under 10 ms).
//!
//! Ignored by default because it depends on installed dictionaries. Run with:
//! `cargo test --release -p wordpilot-daemon --test latency -- --ignored --nocapture`

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use wordpilot_autocomplete::{
    AutocompleteEngine, HeuristicLanguageDetector, NoContextPredictor, NoPersonalDictionary,
};
use wordpilot_core::config::Config;
use wordpilot_core::{ApplicationContext, CompletionRequest, LanguageMode, TextContext};
use wordpilot_daemon::loader;
use wordpilot_dictionary::DictionaryProvider;

fn engine(config: Config) -> (AutocompleteEngine, HashMap<String, usize>) {
    let started = Instant::now();
    let outcome = loader::load_languages(&config);
    let load_ms = started.elapsed().as_millis() as usize;

    let mut sizes = HashMap::new();
    sizes.insert("load_ms".to_string(), load_ms);
    for (language, resources) in &outcome.resources {
        sizes.insert(language.tag().to_string(), resources.dictionary.len());
        sizes.insert(
            format!("{}_bytes", language.tag()),
            resources.dictionary.memory_bytes(),
        );
    }

    let engine = AutocompleteEngine::new(
        config,
        outcome.resources,
        Arc::new(NoPersonalDictionary),
        Arc::new(NoContextPredictor),
        Arc::new(HeuristicLanguageDetector::default()),
    );
    (engine, sizes)
}

#[test]
#[ignore = "depends on installed dictionaries"]
fn completion_stays_inside_the_latency_budget() {
    let (engine, sizes) = engine(Config::default());
    if engine.loaded_languages().is_empty() {
        eprintln!("no dictionaries installed; nothing to measure");
        return;
    }
    println!("load: {} ms", sizes["load_ms"]);
    for language in engine.loaded_languages() {
        println!(
            "  {}: {} words, {:.1} MB",
            language,
            sizes[language.tag()],
            sizes[&format!("{}_bytes", language.tag())] as f64 / 1_048_576.0
        );
    }

    // The realistic worst case: every prefix of a long German word, typed one
    // character at a time, with a short prefix matching the most words.
    let prefixes = [
        "be",
        "bew",
        "bewe",
        "bewer",
        "bewerb",
        "bewerbu",
        "bewerbun",
        "bewerbung",
        "er",
        "erf",
        "erfa",
        "erfah",
        "erfahr",
        "erfahru",
        "st",
        "str",
        "stra",
        "straß",
        "ge",
        "ver",
        "unt",
    ];

    let mut worst_us = 0u128;
    let mut total_us = 0u128;
    let mut count = 0u128;

    // Warm up: the spec's budget is "after warm-up".
    for _ in 0..3 {
        for prefix in prefixes {
            let _ = engine.complete(&request(prefix));
        }
    }

    for _ in 0..20 {
        for prefix in prefixes {
            let request = request(prefix);
            let started = Instant::now();
            let response = engine.complete(&request);
            let elapsed = started.elapsed().as_micros();
            assert!(
                !response.candidates.is_empty() || response.prefix.is_empty(),
                "no candidates for {prefix:?}"
            );
            worst_us = worst_us.max(elapsed);
            total_us += elapsed;
            count += 1;
        }
    }

    let mean_us = total_us / count;
    println!(
        "mean {:.2} ms, worst {:.2} ms",
        mean_us as f64 / 1000.0,
        worst_us as f64 / 1000.0
    );

    assert!(
        worst_us < 30_000,
        "worst case {:.2} ms exceeds the 30 ms budget",
        worst_us as f64 / 1000.0
    );
}

fn request(prefix: &str) -> CompletionRequest {
    CompletionRequest {
        context: TextContext::from_typed(format!("Ich schreibe {prefix}")),
        language: LanguageMode::Fixed(wordpilot_core::Language::German),
        application: ApplicationContext::named("firefox"),
        max_candidates: None,
    }
}
