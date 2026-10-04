# wordpilot-addon (Fcitx 5)

The thin C++ layer (spec §20-§21). No ranking, no dictionaries, no AI: it
translates Fcitx key events into requests on the WordPilot daemon's Unix
socket (`wordpilot-protocol`) and renders the daemon's JSON responses as a
Fcitx candidate list.

## Build

Requires Fcitx 5 development headers (`fcitx5` + its CMake config files,
installed by the `fcitx5` package on Arch) and a C++20 compiler.

```sh
mkdir build && cd build
cmake -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=/usr ..
make
sudo make install
```

This installs `libwordpilot.so` to `/usr/lib/fcitx5/` and
`wordpilot-addon.conf` to `/usr/share/fcitx5/addon/`. Root is required because
that is where Fcitx loads shared-library addons from; see the repository's
`docs/RUNNING.ru.md` (not tracked in git) for the no-root development
workflow using `FCITX_ADDON_DIRS`.

After installing, add "WordPilot" as an input method in `fcitx5-configtool`
and restart Fcitx (`fcitx5 -r`).

## Design notes

- **Composing, not forwarding.** Typed letters are held in a per-field
  preedit buffer, the same pattern Fcitx's own Pinyin/Table/Spell engines
  use, rather than being sent to the application immediately. This is what
  lets a selected candidate replace the typed text outright on commit,
  without the delete-and-retype hack the spec rules out for AI actions
  (§26) -- here there is simply nothing to delete yet.
- **`language: "auto"` on every request.** The daemon tracks the user's
  active language (hotkeys, `wordpilot language <lang>`) as its own state;
  the adapter never needs to know or guess it.
- **200ms socket timeout.** A stuck or dead daemon costs one slow keystroke,
  never a hung input method. `SocketClient` reconnects automatically on the
  next call after any failure, so a daemon restart is transparent.
- **Own minimal JSON (`json.h`/`json.cpp`).** No `rapidjson` package is
  available in this environment; the protocol surface this adapter reads is
  small and fixed, so a ~250-line parser scoped to it is less than a
  dependency would be.
- **Letters only; no direct Unicode input method composition.** Covers ASCII
  plus Latin-1 Supplement (ä ö ü ß and other common European diacritics) via
  `Key::keySymToUnicode`/`keySymToUTF8`, which is what a working dead-key or
  compose-key sequence produces. This is not general Unicode input (no CJK,
  no explicit Unicode code-point entry) -- out of scope for the MVP and not
  silently pretended to work (spec §60).

## Known gap

No automated integration test yet. Fcitx 5 ships a `TestFrontend` addon
(`libtestfrontend.so`) built for exactly this -- driving key events into an
in-process `fcitx::Instance` and asserting on `commitString` calls -- but
wiring a CMake test target against it is a separate task from getting the
addon itself working. Verified so far: clean build against the real Fcitx 5
5.1.23 headers, correct `fcitx_addon_factory_instance` export, and a real
`fcitx5` process loading the addon and registering its input method without
error (`addonmanager.cpp: Loaded addon wordpilot-addon`,
`inputmethodmanager.cpp: Found 1 input method(s)`). Actually typing into a
real text field has not been exercised, since that would mean reconfiguring
live session input-method environment variables, which belongs to the
person using the machine, not to an unattended change.
