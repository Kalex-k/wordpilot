# WordPilot

System-wide multilingual autocomplete and AI writing assistant for Linux.

Two deliberately separate paths (spec §56):

```
typing  ->  dictionary + ranking  ->  candidate popup      (local, <1 ms)
hotkey  ->  LanguageTool / LLM    ->  Grammar/Natural/...  (0.2-several s)
```

The autocomplete path never touches the network and never calls an LLM.

## Status

**Phase 1A and Phase 1D (personal dictionary) are complete and working.**
`wordpilot-test-client` drives the daemon over a Unix socket and gets ranked
German and English candidates back, and accepted candidates are learned and
persisted across restarts.

| Phase | Scope | State |
| --- | --- | --- |
| 1A | workspace, daemon, socket protocol, de/en dictionaries, prefix autocomplete, ranking, test CLI | **done** |
| 1B | Fcitx 5 addon, candidate popup, keyboard selection, commit | **not started** — needs `fcitx5` + headers installed |
| 1C | LanguageTool, Grammar/Natural/Rewrite/Continue, AI provider, selection UI | protocol defined, engines not implemented |
| 1D | SQLite personal dictionary, language hotkeys, privacy blacklist, systemd | **personal dictionary done**; blacklist and systemd unit done; language hotkeys are Fcitx-side (Phase 1B) |

Requests for Phase 1C features return a `unsupported` error rather than a
fabricated answer.

```sh
$ wordpilot-test-client complete --lang de bew
 1. bewegung  0.274  Frequency
 2. bewegt    0.273  Frequency
 3. bewegen   0.272  Frequency

$ for i in $(seq 1 15); do wordpilot-test-client accept --lang de --app firefox bewerben; done

$ wordpilot-test-client complete --lang de bew
 1. bewerben  0.558  Personal     <- learned, outranks raw corpus frequency
 2. bewegung  0.274  Frequency
 3. bewegt    0.273  Frequency
```

This survives a daemon restart: the personal dictionary is a SQLite file at
`$XDG_DATA_HOME/wordpilot/personal.sqlite`.

## Build

Requires a stable Rust toolchain. No Python runtime, at build time or at run
time.

```sh
cargo build --release
```

Binaries land in `target/release`: `wordpilot-daemon`, `wordpilot`,
`wordpilot-test-client`.

## Install (Arch / EndeavourOS)

```sh
# Dictionaries. Prefer the distribution packages:
sudo pacman -S hunspell hunspell-de hunspell-en_us

# Frequency lists are not packaged anywhere, and this also works without root
# if you cannot install the above:
./scripts/fetch-dictionaries.sh

# Binaries
install -Dm755 target/release/wordpilot-daemon ~/.local/bin/wordpilot-daemon
install -Dm755 target/release/wordpilot        ~/.local/bin/wordpilot
install -Dm755 target/release/wordpilot-test-client ~/.local/bin/wordpilot-test-client

# Config (optional; the defaults are a working configuration)
install -Dm644 config/wordpilot.example.toml ~/.config/wordpilot/config.toml

# Autostart. Enabling the service is your decision, not the installer's.
mkdir -p ~/.config/systemd/user
cp scripts/wordpilot.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now wordpilot.service
```

Check everything:

```sh
wordpilot doctor
```

## Try it without Fcitx

```sh
wordpilot-daemon &                       # or: systemctl --user start wordpilot

wordpilot-test-client complete --lang de bew
#  1. bewegt       0.273  Frequency
#  2. bewegen      0.272  Frequency
#  3. beweisen     0.272  Frequency
#  ...

wordpilot-test-client complete --lang de Bewerbu
#  1. Bewerbung    0.240  Frequency
#  2. Bewerbungen  0.209  Frequency

wordpilot-test-client complete --lang en appl
wordpilot-test-client complete --lang auto "Ich habe nicht viel Erfahr"
wordpilot-test-client complete --lang de Erfharung        # spell correction
wordpilot-test-client complete --lang de --app keepassxc bew   # blacklisted: silent
wordpilot-test-client --json complete --lang de bew       # raw protocol
```

`wordpilot status`, `wordpilot language de`, `wordpilot dictionary stats` and
`wordpilot ai status` all work against a running daemon.

## Architecture

```
          APPLICATION
              |
           FCITX 5                  <- Phase 1B
              |
      thin C++ adapter              <- no logic, only translation
              |
     Unix Domain Socket             <- newline-delimited JSON
              |
      WORDPILOT DAEMON  [Rust, tokio]
              |
   +----------+-----------+
   |          |           |
AUTOCOMPLETE GRAMMAR      AI        <- the latter two are Phase 1C
   |
Dictionary / Frequency / Personal / Context
   |
 RANKER -> CANDIDATES
```

The core knows nothing about X11, Wayland or Fcitx. Everything it needs arrives
in a `CompletionRequest`, which carries text and an application name but no
window handle and no coordinates. Moving from X11 to Wayland changes the
frontend only.

### Crates

| Crate | Responsibility |
| --- | --- |
| `wordpilot-core` | types, config, XDG paths. No I/O beyond reading the config. |
| `wordpilot-dictionary` | `DictionaryProvider` trait; Hunspell reader with affix expansion; frequency lists; spell correction |
| `wordpilot-ranking` | `CandidateRanker` over configurable weighted signals |
| `wordpilot-autocomplete` | the engine, plus the `PersonalVocabulary`, `ContextPredictor` and `LanguageDetector` interfaces |
| `wordpilot-protocol` | wire messages and newline-delimited JSON framing |
| `wordpilot-storage` | SQLite-backed `PersonalVocabulary` implementation (Phase 1D) |
| `wordpilot-daemon` | socket server, state, and the three binaries |

Hunspell, LanguageTool, SQLite and any particular LLM all sit behind traits;
none of them is visible to the engine.

## Measured behaviour

On this machine, with the German and English dictionaries installed:

```
load: 1050 ms   (once, at startup)
  de_DE: 1030031 words, 17.1 MB
  en_US:  120853 words,  1.5 MB
mean 0.12 ms, worst 0.30 ms per completion
peak RSS: 105 MB
```

Against a 30 ms budget and a 10 ms goal. Reproduce with:

```sh
cargo test --release -p wordpilot-daemon --test latency -- --ignored --nocapture
```

## Known limitations

These are real gaps, documented rather than faked (spec §60):

- **No Fcitx addon yet.** Phase 1B. `fcitx5` and its development headers are
  not installed on this machine, so the C++ adapter cannot be built or tested
  here. The daemon side it will talk to is finished and tested.
- **No compounding.** Hunspell `COMPOUNDRULE` is not implemented, so an ad-hoc
  German compound such as `Berufserfahrung` is offered only when the dictionary
  lists it as a stem. `Erfahrung` and `Bewerbung` *are* found, via affix
  expansion.
- **Single-level affix expansion.** One prefix or one suffix per form, no cross
  products and no continuation flags. This is what keeps the German index at
  1.03M entries instead of tens of millions.
- **No next-word prediction.** `ContextPredictor` exists and is wired in, but
  no n-gram model ships, so an empty prefix produces nothing. There is
  deliberately no hardcoded German preposition table standing in for a model.
- **Personal learning has no size cap or CLI eviction yet.** The SQLite
  dictionary grows without bound and there is no `wordpilot dictionary forget`
  command, only a library-level `remove_word`. Fine for an MVP, worth fixing
  before this is used for months.
- **`ß` does not case-fold to `ss`.** `char::to_lowercase` leaves it alone, so
  typing `strasse` will not find `Straße`. Typing `straß` will.
- **Grammar and AI are not implemented.** Phase 1C.

## Privacy

WordPilot sees everything you type, so the defaults are conservative:

- the socket lives in `$XDG_RUNTIME_DIR/wordpilot/` with mode `0600`, inside a
  `0700` directory — never `/tmp`
- remote AI is **off** by default; an API key is read from an environment
  variable, never stored in the config file and never logged
- password and sensitive fields disable WordPilot completely, and nothing from
  them reaches the personal dictionary
- password managers are blacklisted by default
- logs record `request_len` and `language`, not your text, unless you
  explicitly set `logging.log_user_text`
- learning can be switched off entirely with `privacy.learning_enabled`

## Development

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release
```

190 tests, covering prefix extraction, Unicode and umlaut handling, affix
expansion, ranking, language detection, config parsing, protocol
serialisation, socket framing, the SQLite personal dictionary and the full
client-to-daemon vertical.
