#include "engine.h"

#include <algorithm>

#include <fcitx/addonmanager.h>
#include <fcitx/candidatelist.h>
#include <fcitx/inputcontext.h>
#include <fcitx/inputcontextmanager.h>
#include <fcitx/inputpanel.h>
#include <fcitx/text.h>
#include <fcitx-utils/log.h>

#include "json.h"

namespace wordpilot {

namespace {

constexpr int kMaxCandidates = 5;    // mirrors autocomplete.max_candidates' default
constexpr int kMinPrefixLength = 2;  // mirrors autocomplete.min_prefix_length's default

// Language-switch hotkeys (spec §2, config.toml's [hotkeys] section).
//
// Hardcoded to the config defaults rather than read from config.toml: this
// adapter deliberately carries no TOML parser (spec §20 keeps the C++ layer
// thin), and three fixed chords are a small, documented simplification
// compared to pulling in a config-file dependency for them. If these need to
// become user-configurable from here, the config belongs in a small file this
// addon reads directly -- not a TOML library linked just for this.
// `.normalize()` matters here: Fcitx normalizes a letter keysym to uppercase
// once a non-Shift modifier (Alt/Super/Ctrl) is held -- confirmed by tracing
// a real key press, which arrived as `Key(Alt+Super+E ...)`, not `...+e...`.
// A hand-built `Key(FcitxKey_e, ...)` compares unequal to that without this
// call, so the hotkey would be received (instance.cpp logs it) but never
// matched by `check()`.
const fcitx::Key kHotkeyGerman =
    fcitx::Key(FcitxKey_d, fcitx::KeyStates(fcitx::KeyState::Super) | fcitx::KeyState::Alt)
        .normalize();
const fcitx::Key kHotkeyEnglish =
    fcitx::Key(FcitxKey_e, fcitx::KeyStates(fcitx::KeyState::Super) | fcitx::KeyState::Alt)
        .normalize();
const fcitx::Key kHotkeyAuto =
    fcitx::Key(FcitxKey_a, fcitx::KeyStates(fcitx::KeyState::Super) | fcitx::KeyState::Alt)
        .normalize();

// One candidate word that commits itself through the owning engine when
// selected. Fcitx calls `select()` with no other context, so the word and a
// back-pointer to the engine are captured at construction.
class WordPilotCandidateWord : public fcitx::CandidateWord {
public:
    WordPilotCandidateWord(WordPilotEngine *engine, WordPilotState *state,
                            std::string word)
        : fcitx::CandidateWord(fcitx::Text(word)), engine_(engine),
          state_(state), word_(std::move(word)) {}

    void select(fcitx::InputContext *ic) const override {
        engine_->commitWordPublic(ic, state_, word_);
    }

private:
    WordPilotEngine *engine_;
    WordPilotState *state_;
    std::string word_;
};

// A Key a composing engine should treat as ordinary text, not a command.
// Fcitx's `Key::isSimple()` is what the keyboard engine itself uses for "no
// modifiers beyond shift", which is exactly "the user is typing".
bool isComposableKey(const fcitx::Key &key) {
    const fcitx::KeyStates blocked =
        fcitx::KeyStates(fcitx::KeyState::Ctrl) | fcitx::KeyState::Alt | fcitx::KeyState::Super;
    return key.isSimple() && !key.states().testAny(blocked);
}

// Whether `codepoint` is a letter this engine will compose, covering ASCII
// plus the Latin-1 Supplement letters German needs (ä ö ü ß Ä Ö Ü and other
// European diacritics), excluding the two symbols that live inside that
// block (0xD7 multiplication sign, 0xF7 division sign).
//
// This is not full Unicode letter classification -- it is the documented
// scope for "typed as a direct key or a working dead-key/compose sequence"
// (spec §60: a capability either works or the limitation is written down).
// A fuller classification belongs behind the same `isComposableKey` call if
// a language needs it later.
bool isComposableLetter(uint32_t codepoint) {
    if ((codepoint >= 'a' && codepoint <= 'z') ||
        (codepoint >= 'A' && codepoint <= 'Z')) {
        return true;
    }
    if (codepoint >= 0xC0 && codepoint <= 0xFF && codepoint != 0xD7 &&
        codepoint != 0xF7) {
        return true;
    }
    return false;
}

// Removes the last UTF-8 *character* from `text`, not the last byte --
// `ä` is two bytes, and popping one would leave a dangling lead byte. Same
// care the Rust side takes throughout (wordpilot-core::text::TextContext).
void utf8PopBack(std::string &text) {
    if (text.empty()) {
        return;
    }
    size_t pos = text.size() - 1;
    // Continuation bytes look like 10xxxxxx; walk back over them to the
    // character's lead byte.
    while (pos > 0 && (static_cast<unsigned char>(text[pos]) & 0xC0) == 0x80) {
        --pos;
    }
    text.erase(pos);
}

} // namespace

WordPilotEngine::WordPilotEngine(fcitx::Instance *instance)
    : instance_(instance),
      factory_([](fcitx::InputContext &) { return new WordPilotState; }) {
    instance_->inputContextManager().registerProperty("wordpilotState", &factory_);

    // PostInputMethod: runs after whatever input method is currently active
    // has had its turn, so this never steals a key from, say, a Pinyin
    // engine's own composition -- but it still fires even when the active
    // engine is "Keyboard - US", which is the whole point (spec §2's hotkeys
    // switch WordPilot's language globally, not only while WordPilot is the
    // active input method).
    hotkeyHandle_ = instance_->watchEvent<fcitx::EventType::InputContextKeyEvent>(
        fcitx::EventWatcherPhase::PostInputMethod, [this](fcitx::KeyEvent &event) {
            if (event.isRelease() || event.filtered()) {
                return;
            }
            const auto &key = event.key();
            auto *ic = event.inputContext();
            auto *s = state(ic);

            if (key.check(kHotkeyGerman)) {
                switchLanguage(ic, s, "de");
                event.filterAndAccept();
            } else if (key.check(kHotkeyEnglish)) {
                switchLanguage(ic, s, "en");
                event.filterAndAccept();
            } else if (key.check(kHotkeyAuto)) {
                switchLanguage(ic, s, "auto");
                event.filterAndAccept();
            }
        });
}

std::vector<fcitx::InputMethodEntry> WordPilotEngine::listInputMethods() {
    std::vector<fcitx::InputMethodEntry> entries;
    // A single input method entry for the MVP (spec §2's ActiveLanguage is
    // handled daemon-side via `SetLanguage`/hotkeys, not as separate Fcitx
    // IMs -- that would make switching language a full IM switch, which is
    // heavier than the hotkeys the spec actually asks for).
    entries.emplace_back("wordpilot", "WordPilot", "", "wordpilot-addon");
    entries.back().setNativeName("WordPilot").setIcon("input-keyboard");
    return entries;
}

WordPilotState *WordPilotEngine::state(fcitx::InputContext *ic) {
    return ic->propertyFor(&factory_);
}

void WordPilotEngine::activate(const fcitx::InputMethodEntry &, fcitx::InputContextEvent &) {}

void WordPilotEngine::deactivate(const fcitx::InputMethodEntry &entry,
                                   fcitx::InputContextEvent &event) {
    reset(entry, event);
}

void WordPilotEngine::reset(const fcitx::InputMethodEntry &, fcitx::InputContextEvent &event) {
    auto *ic = event.inputContext();
    auto *s = state(ic);
    s->buffer.clear();
    s->candidateWords.clear();
    s->selected = -1;
    ic->inputPanel().reset();
    ic->updatePreedit();
    ic->updateUserInterface(fcitx::UserInterfaceComponent::InputPanel);
}

void WordPilotEngine::commitBufferAsTyped(fcitx::InputContext *ic, WordPilotState *s) {
    if (!s->buffer.empty()) {
        ic->commitString(s->buffer);
    }
    s->buffer.clear();
    s->candidateWords.clear();
    s->selected = -1;
    ic->inputPanel().reset();
    ic->updatePreedit();
}

void WordPilotEngine::commitWord(fcitx::InputContext *ic, WordPilotState *s,
                                   const std::string &word) {
    ic->commitString(word);
    // Tell the daemon this candidate was accepted, so it can be learned
    // (spec §39, wired to wordpilot-storage's PersonalDictionary). Fire and
    // forget over the same connection: a dropped accept is a missed learning
    // signal, never a reason to interrupt typing. "auto" language matches
    // `complete`'s request -- the daemon resolves it against its own active
    // language, the adapter never needs to track it.
    std::string request = std::string("{\"type\":\"accept-candidate\",\"word\":") +
                           JsonValue::escapeString(word) +
                           ",\"language\":\"auto\"}";
    std::string response;
    socket_.request(request, response);

    s->buffer.clear();
    s->candidateWords.clear();
    s->selected = -1;
    ic->inputPanel().reset();
    ic->updatePreedit();
}

void WordPilotEngine::switchLanguage(fcitx::InputContext *ic, WordPilotState *s,
                                       const std::string &languageMode) {
    // Whatever was mid-composition is committed as plain typed text first --
    // a language switch is not the moment to silently drop or guess-correct
    // it.
    if (!s->buffer.empty()) {
        commitBufferAsTyped(ic, s);
    }
    std::string request = std::string("{\"type\":\"set-language\",\"language\":") +
                           JsonValue::escapeString(languageMode) + "}";
    std::string response;
    // Fire and forget: a dropped language switch just means the next
    // keystroke still queries under the old language, which the user can
    // retry. Nothing here blocks typing on the daemon answering.
    socket_.request(request, response);
}

void WordPilotEngine::updateCandidates(fcitx::InputContext *ic, WordPilotState *s) {
    s->candidateWords.clear();
    s->selected = -1;

    if (static_cast<int>(s->buffer.size()) < kMinPrefixLength) {
        renderPreedit(ic, s);
        return;
    }

    // "auto" language: the daemon tracks the user's active language
    // (hotkeys, spec §2) and resolves it server-side, so the adapter never
    // needs to know or guess it.
    std::string request = std::string("{\"type\":\"complete\",\"context\":{\"before_cursor\":") +
                           JsonValue::escapeString(s->buffer) +
                           "},\"language\":\"auto\",\"max_candidates\":" +
                           std::to_string(kMaxCandidates) + "}";
    std::string response;
    if (!socket_.request(request, response)) {
        // Daemon unreachable: the buffer the user typed is untouched, only
        // the candidate list disappears (spec §44).
        renderPreedit(ic, s);
        return;
    }

    JsonValue parsed = JsonValue::parse(response);
    if (parsed.get("type").asString() != "completion") {
        renderPreedit(ic, s);
        return;
    }
    for (const auto &candidate : parsed.get("candidates").items()) {
        s->candidateWords.push_back(candidate.get("text").asString());
    }
    if (!s->candidateWords.empty()) {
        s->selected = 0;
    }
    renderPreedit(ic, s);
}

void WordPilotEngine::renderPreedit(fcitx::InputContext *ic, WordPilotState *s) {
    fcitx::Text preedit;
    preedit.append(s->buffer);
    preedit.setCursor(static_cast<int>(s->buffer.size()));
    ic->inputPanel().setClientPreedit(preedit);
    ic->inputPanel().setPreedit(preedit);

    if (s->candidateWords.empty()) {
        ic->inputPanel().setCandidateList(nullptr);
    } else {
        auto list = std::make_unique<fcitx::CommonCandidateList>();
        for (const auto &word : s->candidateWords) {
            list->append<WordPilotCandidateWord>(this, s, word);
        }
        list->setCursorIndex(std::max(s->selected, 0));
        ic->inputPanel().setCandidateList(std::move(list));
    }
    ic->updatePreedit();
    ic->updateUserInterface(fcitx::UserInterfaceComponent::InputPanel);
}

void WordPilotEngine::keyEvent(const fcitx::InputMethodEntry &entry, fcitx::KeyEvent &event) {
    if (event.isRelease()) {
        return;
    }
    auto *ic = event.inputContext();
    auto *s = state(ic);
    const auto &key = event.key();

    // Language hotkeys are handled by a global event watcher (see the
    // constructor) that fires regardless of which input method is active, so
    // they are NOT checked here -- a per-engine check would only work while
    // WordPilot itself is the active Fcitx input method.

    // No active composition: only start one on a letter. Everything else
    // (digits used as shortcuts elsewhere, punctuation, navigation) passes
    // straight through to the application, exactly as if WordPilot were not
    // running.
    if (s->buffer.empty()) {
        uint32_t codepoint = fcitx::Key::keySymToUnicode(key.sym());
        if (isComposableKey(key) && isComposableLetter(codepoint)) {
            s->buffer += fcitx::Key::keySymToUTF8(key.sym());
            updateCandidates(ic, s);
            event.filterAndAccept();
        }
        return;
    }

    // A composition is in progress.
    if (key.check(FcitxKey_Escape)) {
        reset(entry, event);
        event.filterAndAccept();
        return;
    }
    if (key.check(FcitxKey_BackSpace)) {
        utf8PopBack(s->buffer);
        if (s->buffer.empty()) {
            reset(entry, event);
        } else {
            updateCandidates(ic, s);
        }
        event.filterAndAccept();
        return;
    }
    if (key.check(FcitxKey_Up)) {
        if (!s->candidateWords.empty()) {
            s->selected = (s->selected - 1 + static_cast<int>(s->candidateWords.size())) %
                          static_cast<int>(s->candidateWords.size());
            renderPreedit(ic, s);
        }
        event.filterAndAccept();
        return;
    }
    if (key.check(FcitxKey_Down)) {
        if (!s->candidateWords.empty()) {
            s->selected = (s->selected + 1) % static_cast<int>(s->candidateWords.size());
            renderPreedit(ic, s);
        }
        event.filterAndAccept();
        return;
    }
    if (key.check(FcitxKey_Tab)) {
        // Tab always accepts the first candidate (spec §32), regardless of
        // where Up/Down moved the cursor.
        if (!s->candidateWords.empty()) {
            commitWord(ic, s, s->candidateWords.front());
        } else {
            commitBufferAsTyped(ic, s);
        }
        event.filterAndAccept();
        return;
    }
    if (key.check(FcitxKey_Return) || key.check(FcitxKey_KP_Enter)) {
        // Enter accepts the *selected* candidate only when the popup is
        // active; otherwise it must behave like a normal newline, so the
        // event is allowed through after committing the plain buffer
        // (spec §32: "Enter — принять выбранный вариант, если candidate
        // popup активен").
        if (s->selected >= 0 &&
            s->selected < static_cast<int>(s->candidateWords.size())) {
            commitWord(ic, s, s->candidateWords[s->selected]);
            event.filterAndAccept();
        } else {
            commitBufferAsTyped(ic, s);
            // Not accepted: the Return itself still reaches the application.
        }
        return;
    }
    {
        uint32_t codepoint = fcitx::Key::keySymToUnicode(key.sym());
        if (isComposableKey(key) && isComposableLetter(codepoint)) {
            s->buffer += fcitx::Key::keySymToUTF8(key.sym());
            updateCandidates(ic, s);
            event.filterAndAccept();
            return;
        }
    }

    // Any other key (space, punctuation, arrow keys while composing, etc.)
    // ends the composition: commit what was typed verbatim, then let the key
    // itself fall through to the application untouched.
    commitBufferAsTyped(ic, s);
}

void WordPilotEngine::commitWordPublic(fcitx::InputContext *ic, WordPilotState *s,
                                          const std::string &word) {
    commitWord(ic, s, word);
}

fcitx::AddonInstance *WordPilotEngineFactory::create(fcitx::AddonManager *manager) {
    return new WordPilotEngine(manager->instance());
}

} // namespace wordpilot
