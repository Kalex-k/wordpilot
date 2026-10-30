# WordPilot

Word completion for the whole Linux desktop. Type `Bewerbu` in any text field
and you get `Bewerbung`, `Bewerbungen` in a small popup; `Tab` takes the first
one. It runs locally, learns the words you actually use, and supports German
and English (US/GB).

WordPilot is an [Fcitx 5](https://fcitx-im.org) input method. A small C++
addon handles the keyboard, and a Rust daemon does the lookup and ranking.

## What it does

- Completes words from Hunspell dictionaries. Affix rules are expanded, so
  `Erfahrung` is found even though the German dictionary only lists
  `Erfahren` with a suffix flag.
- Ranks candidates by word frequency, by how often and how recently you used
  them, and by the application you're typing in. The weights are configurable.
- Learns from the candidates you accept (stored in a local SQLite file).
- Suggests spelling corrections when nothing matches (`Erfharung` →
  `Erfahrung`). It never applies them on its own.
- Switches language with a hotkey, or detects it from the sentence.
- Stays out of password fields and out of password managers.
- Picks up config changes while running.

Grammar checking (LanguageTool) and AI rewriting are planned but not there
yet. The protocol already has the requests; the daemon answers them with an
`unsupported` error.

## How it works

```
application ──keys──▶ Fcitx 5 ──▶ wordpilot addon (C++)
     ▲                                   │ JSON lines over a Unix socket
     └────── preedit / commit ───────────┤ $XDG_RUNTIME_DIR/wordpilot/daemon.sock
                                         ▼
                              wordpilot-daemon (Rust)
                     dictionaries · frequency lists · personal
                     dictionary · language detection · ranking
```

The addon works like Fcitx's Pinyin or Hangul engines. The letters you type
stay underlined in the field (the "preedit") and aren't sent to the
application yet. After each letter the addon asks the daemon for candidates,
and it also sends the text around the cursor when the application provides
it. Accepting a candidate commits the whole word. Any other key commits what
you typed and then goes through to the application as usual. Because nothing
has been sent to the application before the commit, there's no need to delete
the prefix and retype the word, which is unreliable in some toolkits.

The daemon loads the dictionaries once, which takes about a second, and then
answers from memory in roughly 0.1 ms per keystroke. The addon doesn't read
the config file. It asks the daemon for its settings (hotkeys, minimum prefix
length, blacklist) and refreshes them every couple of seconds.

The daemon knows nothing about X11 or Wayland. A request carries text and an
application name, not window handles or coordinates. Fcitx deals with the
display server.

### Layout

```
crates/
  wordpilot-core          types, config, XDG paths
  wordpilot-dictionary    Hunspell reader, affix expansion, frequency lists, spelling
  wordpilot-ranking       candidate ranking
  wordpilot-autocomplete  the completion engine
  wordpilot-storage       SQLite personal dictionary
  wordpilot-protocol      wire format
  wordpilot-daemon        daemon, `wordpilot` CLI, `wordpilot-test-client`
fcitx/wordpilot-addon     Fcitx 5 addon + its integration test
config/                   example config with every option documented
scripts/                  dictionary download, systemd unit, dev helper
```

Built with Rust (tokio, serde, rusqlite with bundled SQLite) and C++20 against
the Fcitx 5 libraries. The dictionaries are LibreOffice's Hunspell files, and
the frequency lists come from
[FrequencyWords](https://github.com/hermitdave/FrequencyWords).

## X11 and Wayland

WordPilot works wherever Fcitx 5 works. What changes is how applications
reach Fcitx.

**X11:** applications use the GTK/Qt IM modules or XIM. Set these
variables before the applications start, e.g. in `~/.xprofile` or your window
manager's autostart:

```sh
export GTK_IM_MODULE=fcitx
export QT_IM_MODULE=fcitx
export XMODIFIERS=@im=fcitx
```

Everything works on X11, including the popup at the cursor and the
surrounding text in GTK and Qt applications. Old XIM-only programs still get
completion, but they don't report the surrounding text or (often) their
program name. That means the application blacklist can't match them.

**Wayland:** Fcitx connects to applications through the compositor's
text-input protocol (v3 for GTK 3/4, recent Qt 6, and Chromium/Electron with
`--enable-wayland-ime`) and to the compositor through input-method-v1/v2.

- **KDE Plasma:** select Fcitx 5 under *System Settings → Keyboard → Virtual
  Keyboard*, and don't set `GTK_IM_MODULE`/`QT_IM_MODULE`.
- **GNOME:** uses IBus by default. Set the variables above, or install the
  *Input Method Panel* extension.
- **Sway, Hyprland and other wlroots compositors:** start `fcitx5 -d` from
  the compositor config. Inline preedit works. On some compositors the popup
  doesn't follow the cursor. `QT_IM_MODULE=fcitx` helps Qt 5 apps.

Keep `XMODIFIERS=@im=fcitx` everywhere for XWayland applications.
`wordpilot doctor` checks the session type and only complains about the
variables that matter for it.

## Building

You need Rust (stable, 1.82+), CMake, a C++20 compiler and the Fcitx 5
development files. On Arch:

```sh
sudo pacman -S --needed base-devel cmake fcitx5 fcitx5-gtk fcitx5-qt fcitx5-configtool
```

The Debian/Ubuntu packages are `fcitx5-modules-dev` and `libfcitx5core-dev`;
on Fedora it's `fcitx5-devel`.

From the repository root:

```sh
cargo build --release

cd fcitx/wordpilot-addon
cmake -B build
cmake --build build
```

Tests:

```sh
cargo test --workspace
ctest --test-dir fcitx/wordpilot-addon/build
```

The addon test runs the addon inside a real Fcitx instance, using Fcitx's test
frontend and a fake daemon. CMake skips it if your Fcitx package doesn't ship
the test modules.

## Installing

Dictionaries and frequency lists go into `~/.local/share/wordpilot/`, so
no root is needed:

```sh
./scripts/fetch-dictionaries.sh
```

Dictionaries installed by your distribution (`/usr/share/hunspell`) are used
too, but you need the script for the frequency lists either way.

The daemon, as a systemd user service:

```sh
install -Dm755 -t ~/.local/bin target/release/{wordpilot-daemon,wordpilot,wordpilot-test-client}
install -Dm644 scripts/wordpilot.service -t ~/.config/systemd/user
systemctl --user daemon-reload
systemctl --user enable --now wordpilot
```

The addon (it installs into Fcitx's own directories):

```sh
sudo cmake --install fcitx/wordpilot-addon/build
fcitx5 -r -d
```

Then open `fcitx5-configtool`, untick *Only Show Current Language*, and add
**WordPilot**. `wordpilot doctor` will tell you if anything is still missing.

If you'd rather not install the addon as root, see `scripts/dev.sh link-addon`.
It links the addon into `~/.local`, and Fcitx then has to be started with
`FCITX_ADDON_DIRS=$HOME/.local/lib/fcitx5:/usr/lib/fcitx5`.

## Configuration

Settings go in `~/.config/wordpilot/config.toml`. You don't need the file:
without it, the defaults apply. To start one, copy the example, which lists
every option with its default value and a comment:

```sh
install -Dm644 config/wordpilot.example.toml ~/.config/wordpilot/config.toml
```

There's no need to restart anything after editing. The daemon re-reads the
file within a second of a save, and the addon picks up its part within two
seconds. `wordpilot reload` does it immediately and tells you if the file was
rejected. A broken file is ignored as a whole, and the previous settings stay
in effect. Two settings, `general.socket_path` and `logging.level`, need a
daemon restart. Changing `[dictionaries]` reloads the word lists in the
background.

Some things that might not be obvious:

- **Unknown keys are an error.** That way a typo doesn't silently do nothing.
- **Hotkeys look like `Ctrl+Alt+G`.** Modifiers are `Ctrl`, `Alt`, `Shift`
  and `Super`. Keys are letters, `F1`–`F12`, `Space`, `Tab`, or any X keysym
  name. An empty string turns a hotkey off. Hotkeys work whichever Fcitx input
  method is active.
- **Ranking weights are relative.** Raising one doesn't rescale the others.
- **`applications.disabled` matches the program name Fcitx reports**, case
  doesn't matter. `fcitx5-diagnose` shows the name.
- **A custom socket path also has to go to Fcitx.** Set `WORDPILOT_SOCKET` in
  Fcitx's environment, because the addon doesn't read the config.
- **`[grammar]` and `[ai]` are accepted but unused** until those features
  exist.

## Usage

Switch to WordPilot (`Ctrl+Space` by default in Fcitx, or the tray icon) and
type. The popup shows up after two letters.

- `Tab` inserts the first candidate.
- `↑`/`↓` move the selection, and `Enter` inserts the selected candidate.
- `Esc` closes the popup and keeps what you typed.
- Space, punctuation and other keys keep what you typed and then do their
  usual thing.
- When no popup is open, `Tab` and `Enter` behave normally.

The language hotkeys are `Super+Alt+D` (German), `Super+Alt+E` (English) and
`Super+Alt+A` (automatic). In automatic mode the language only changes after
a couple of clear words, so a single `die` or `in` won't flip it.

The `wordpilot` command:

```sh
wordpilot status
wordpilot language en          # de, en, en-GB, auto
wordpilot dictionary stats
wordpilot reload
wordpilot doctor
```

`wordpilot-test-client` talks to the daemon directly, which is handy without
Fcitx:

```sh
wordpilot-test-client complete --lang de Bewerbu
wordpilot-test-client complete --lang auto "Ich habe nicht viel Erfahr"
wordpilot-test-client --json complete --lang de bew
```

## Development

`scripts/dev.sh` keeps the edit-build-try loop short:

```sh
scripts/dev.sh link        # ~/.local/bin/wordpilot* -> target/release (once)
scripts/dev.sh link-addon  # addon -> ~/.local, no root (once)
scripts/dev.sh up          # build everything, restart the daemon
scripts/dev.sh up --fcitx  # same, and restart Fcitx to load a new addon
scripts/dev.sh test
```

Before sending a change, run `cargo fmt`, `cargo clippy --workspace
--all-targets -- -D warnings` and both test suites.

To measure latency against the dictionaries you have installed:

```sh
cargo test --release -p wordpilot-daemon --test latency -- --ignored --nocapture
```

On an i7-8700K with German (1.03 M word forms) and English (121 k) loaded,
startup takes about a second, and a completion takes 0.11 ms on average and
0.29 ms at worst. Peak memory use is about 105 MB.

The protocol is one JSON object per line. Requests are `complete`,
`accept-candidate`, `set-language`, `frontend-config`, `reload`, `status` and
`ping`. Errors come back as normal responses, so the connection stays open.
Try it with `wordpilot-test-client --json`, or
`socat - UNIX-CONNECT:$XDG_RUNTIME_DIR/wordpilot/daemon.sock`.

## Troubleshooting

- **No popup:** check that `wordpilot status` reports a running daemon, that
  WordPilot is the active input method, and what `wordpilot doctor` says
  about the environment variables.
- **WordPilot not listed in `fcitx5-configtool`:** the addon isn't installed
  where Fcitx looks. Check `wordpilot doctor` and `fcitx5-diagnose`.
- **The addon is listed but doesn't load:** run `fcitx5 -r -d --verbose '*=4'`.
  An `undefined symbol` error means it was built against a different Fcitx
  version and needs a rebuild.
- **A config change does nothing:** `wordpilot reload` shows the error, and
  `journalctl --user -u wordpilot` has the details.
- **Nothing happens in one particular app:** the app may be in
  `applications.disabled`, the field may be a password field, or on Wayland
  the app may not support text-input.

## Privacy

- The daemon makes no network connections at all.
- The socket is `0600`, in `$XDG_RUNTIME_DIR`. It is never in `/tmp`.
- In password fields, sensitive fields and blacklisted applications, the
  addon doesn't intercept keys and sends nothing to the daemon.
- Only words you accept are learned. Learning can be turned off.
- Requests aren't logged.
- At most 256 bytes on each side of the cursor are sent as context.

## Limitations

- The addon only composes Latin letters (ASCII and Latin-1, which covers
  `ä ö ü ß é`). Other scripts pass through unchanged.
- There's no next-word prediction yet, so nothing is suggested after a space.
- Hunspell compounding isn't supported, so an ad-hoc compound like
  `Berufserfahrung` only shows up if it's in the dictionary or you've used it
  before.
- `ß` isn't matched by `ss`: `strasse` won't find `Straße`.
- An unfinished word is dropped when the field loses focus. This is the usual
  Fcitx behaviour for composing input methods.
- The personal dictionary has no size limit and no `forget` command yet. To
  start over, stop the daemon and delete
  `~/.local/share/wordpilot/personal.sqlite`.
