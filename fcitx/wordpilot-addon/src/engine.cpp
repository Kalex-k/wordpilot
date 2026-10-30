#include "engine.h"

#include <algorithm>
#include <cctype>

#include <fcitx/addonmanager.h>
#include <fcitx/candidatelist.h>
#include <fcitx/event.h>
#include <fcitx/inputcontext.h>
#include <fcitx/inputcontextmanager.h>
#include <fcitx/inputpanel.h>
#include <fcitx/text.h>
#include <fcitx-utils/capabilityflags.h>
#include <fcitx-utils/log.h>
#include <fcitx-utils/utf8.h>

#include "json.h"

namespace wordpilot {

namespace {

// How stale the daemon-provided settings may get. Config edits reach the
// addon within this interval (the daemon itself notices them within ~1 s).
constexpr std::chrono::seconds kSettingsRefresh{2};

// How much application text around the caret is sent as context. The daemon
// only looks at the last few words, so a sentence or two is plenty -- and
// sending less of the user's document is better for privacy.
constexpr size_t kContextBytes = 256;

// Parses a hotkey the daemon already translated to Fcitx spelling
// ("Control+Alt+space"). `.normalize()` matters: Fcitx normalizes a letter
// keysym to uppercase once a non-Shift modifier (Alt/Super/Ctrl) is held --
// confirmed by tracing a real key press, which arrived as
// `Key(Alt+Super+E ...)`, not `...+e...`. A Key built from "Super+Alt+e"
// compares unequal to that without this call, so the hotkey would be
// received but never matched by `check()`.
fcitx::Key parseHotkey(const std::string &spec) {
    if (spec.empty()) {
        return fcitx::Key();
    }
    fcitx::Key key(spec.c_str());
    if (!key.isValid()) {
        FCITX_WARN() << "WordPilot: ignoring unparseable hotkey " << spec;
        return fcitx::Key();
    }
    return key.normalize();
}

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
// (a capability either works or the limitation is written down).
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

// Characters that glue onto a word. A new composition never starts right
// after or right before one of these: the user is editing an existing word
// (`Bew|` -> `Bewerbung`), and a completion of only the newly typed letters
// would be wrong.
bool isWordGlue(uint32_t codepoint) {
    return isComposableLetter(codepoint) || (codepoint >= '0' && codepoint <= '9') ||
           codepoint == '-' || codepoint == '\'' || codepoint == '_';
}

// Removes the last UTF-8 *character* from `text`, not the last byte --
// `ä` is two bytes, and popping one would leave a dangling lead byte. Same
// care the Rust side takes throughout (wordpilot-core::text::TextContext).
void utf8PopBack(std::string &text) {
    if (text.empty()) {
        return;
    }
    size_t pos = text.size() - 1;
    while (pos > 0 && fcitx::utf8::isContinuationByte(text[pos])) {
        --pos;
    }
    text.erase(pos);
}

// The last `maxBytes` bytes of `text`, moved forward to a character boundary.
std::string utf8Tail(const std::string &text, size_t maxBytes) {
    if (text.size() <= maxBytes) {
        return text;
    }
    size_t start = text.size() - maxBytes;
    while (start < text.size() && fcitx::utf8::isContinuationByte(text[start])) {
        ++start;
    }
    return text.substr(start);
}

// The first `maxBytes` bytes of `text`, moved back to a character boundary.
std::string utf8Head(const std::string &text, size_t maxBytes) {
    if (text.size() <= maxBytes) {
        return text;
    }
    size_t end = maxBytes;
    while (end > 0 && fcitx::utf8::isContinuationByte(text[end])) {
        --end;
    }
    return text.substr(0, end);
}

bool equalsIgnoreAsciiCase(const std::string &a, const std::string &b) {
    return a.size() == b.size() &&
           std::equal(a.begin(), a.end(), b.begin(), [](char x, char y) {
               return std::tolower(static_cast<unsigned char>(x)) ==
                      std::tolower(static_cast<unsigned char>(y));
           });
}

} // namespace

WordPilotEngine::WordPilotEngine(fcitx::Instance *instance)
    : instance_(instance),
      factory_([](fcitx::InputContext &) { return new WordPilotState; }) {
    instance_->inputContextManager().registerProperty("wordpilotState", &factory_);

    // Until the daemon answers, the hotkeys are config.toml's defaults.
    settings_.hotkeyGerman = parseHotkey("Super+Alt+D");
    settings_.hotkeyEnglish = parseHotkey("Super+Alt+E");
    settings_.hotkeyAuto = parseHotkey("Super+Alt+A");
    refreshSettings(true);

    // PostInputMethod: runs after whatever input method is currently active
    // has had its turn, so this never steals a key from, say, a Pinyin
    // engine's own composition -- but it still fires even when the active
    // engine is "Keyboard - US", which is the whole point: the hotkeys
    // switch WordPilot's language globally, not only while WordPilot is the
    // active input method.
    hotkeyHandle_ = instance_->watchEvent<fcitx::EventType::InputContextKeyEvent>(
        fcitx::EventWatcherPhase::PostInputMethod, [this](fcitx::KeyEvent &event) {
            if (event.isRelease() || event.filtered()) {
                return;
            }
            const auto &key = event.key();
            auto *ic = event.inputContext();
            const std::pair<const fcitx::Key *, const char *> bindings[] = {
                {&settings_.hotkeyGerman, "de"},
                {&settings_.hotkeyEnglish, "en"},
                {&settings_.hotkeyAuto, "auto"},
            };
            for (const auto &[hotkey, language] : bindings) {
                if (hotkey->isValid() && key.check(*hotkey)) {
                    switchLanguage(ic, state(ic), language);
                    event.filterAndAccept();
                    return;
                }
            }
        });

    // Focusing a field is a cheap moment to pick up config edits.
    focusHandle_ = instance_->watchEvent(
        fcitx::EventType::InputContextFocusIn, fcitx::EventWatcherPhase::Default,
        [this](fcitx::Event &) { refreshSettings(); });
}

std::vector<fcitx::InputMethodEntry> WordPilotEngine::listInputMethods() {
    std::vector<fcitx::InputMethodEntry> entries;
    // A single input method entry. The active language is handled
    // daemon-side via `SetLanguage`/hotkeys, not as separate Fcitx IMs --
    // that would make switching language a full IM switch, which is heavier
    // than a hotkey.
    entries.emplace_back("wordpilot", "WordPilot", "", "wordpilot-addon");
    entries.back().setNativeName("WordPilot").setIcon("input-keyboard").setLabel("WP");
    return entries;
}

WordPilotState *WordPilotEngine::state(fcitx::InputContext *ic) {
    return ic->propertyFor(&factory_);
}

void WordPilotEngine::refreshSettings(bool force) {
    const auto now = std::chrono::steady_clock::now();
    if (!force && settings_.generation >= 0 && now - lastSettingsFetch_ < kSettingsRefresh) {
        return;
    }
    lastSettingsFetch_ = now;

    std::string response;
    if (!socket_.request("{\"type\":\"frontend-config\"}", response)) {
        return;
    }
    JsonValue parsed = JsonValue::parse(response);
    if (parsed.get("type").asString() != "frontend-config") {
        // An older daemon without this request: keep the defaults.
        return;
    }
    const auto generation = static_cast<long long>(parsed.get("generation").asNumber(0));
    if (generation == settings_.generation) {
        return;
    }

    FrontendSettings next;
    next.generation = generation;
    next.enabled = parsed.get("autocomplete_enabled").asBool(true);
    next.minPrefixLength =
        static_cast<size_t>(std::max(1.0, parsed.get("min_prefix_length").asNumber(2)));
    next.passwordFieldDetection = parsed.get("password_field_detection").asBool(true);
    next.sensitiveFieldDetection = parsed.get("sensitive_field_detection").asBool(true);
    for (const auto &app : parsed.get("disabled_applications").items()) {
        next.disabledApplications.push_back(app.asString());
    }
    const auto &hotkeys = parsed.get("hotkeys");
    next.hotkeyGerman = parseHotkey(hotkeys.get("language_german").asString());
    next.hotkeyEnglish = parseHotkey(hotkeys.get("language_english").asString());
    next.hotkeyAuto = parseHotkey(hotkeys.get("language_auto").asString());
    settings_ = std::move(next);
    FCITX_INFO() << "WordPilot: settings updated from daemon (generation "
                 << settings_.generation << ")";
}

bool WordPilotEngine::isSuppressed(fcitx::InputContext *ic) const {
    const auto flags = ic->capabilityFlags();
    if (settings_.passwordFieldDetection && flags.test(fcitx::CapabilityFlag::Password)) {
        return true;
    }
    if (settings_.sensitiveFieldDetection && flags.test(fcitx::CapabilityFlag::Sensitive)) {
        return true;
    }
    const auto &program = ic->program();
    return !program.empty() &&
           std::any_of(settings_.disabledApplications.begin(),
                       settings_.disabledApplications.end(),
                       [&](const std::string &app) { return equalsIgnoreAsciiCase(app, program); });
}

std::string WordPilotEngine::applicationJson(fcitx::InputContext *ic) const {
    const auto flags = ic->capabilityFlags();
    const auto &program = ic->program();
    return std::string("{\"name\":") +
           (program.empty() ? std::string("null") : JsonValue::escapeString(program)) +
           ",\"sensitive\":" + (flags.test(fcitx::CapabilityFlag::Sensitive) ? "true" : "false") +
           ",\"password\":" + (flags.test(fcitx::CapabilityFlag::Password) ? "true" : "false") +
           "}";
}

bool WordPilotEngine::captureContext(fcitx::InputContext *ic, WordPilotState *s) {
    s->contextBefore.clear();
    s->contextAfter.clear();
    if (!ic->capabilityFlags().test(fcitx::CapabilityFlag::SurroundingText)) {
        return true;
    }
    const auto &surrounding = ic->surroundingText();
    // A selection is about to be replaced by what the user types, so the
    // text around it is not the word's context.
    if (!surrounding.isValid() || surrounding.anchor() != surrounding.cursor()) {
        return true;
    }
    const std::string &text = surrounding.text();
    const size_t length = fcitx::utf8::lengthValidated(text);
    if (length == fcitx::utf8::INVALID_LENGTH || surrounding.cursor() > length) {
        return true;
    }
    // The cursor is a character index; the strings are UTF-8 bytes.
    const auto split = fcitx::utf8::nextNChar(text.begin(), surrounding.cursor());
    const std::string before(text.begin(), split);
    const std::string after(split, text.end());
    if (!before.empty() && isWordGlue(fcitx::utf8::getLastChar(before))) {
        return false;
    }
    if (!after.empty() && isWordGlue(fcitx::utf8::getChar(after))) {
        return false;
    }
    s->contextBefore = utf8Tail(before, kContextBytes);
    s->contextAfter = utf8Head(after, kContextBytes);
    return true;
}

void WordPilotEngine::activate(const fcitx::InputMethodEntry &, fcitx::InputContextEvent &) {
    refreshSettings();
}

void WordPilotEngine::deactivate(const fcitx::InputMethodEntry &entry,
                                   fcitx::InputContextEvent &event) {
    // Switching to another input method mid-word keeps what was typed, the
    // same as Fcitx's own Pinyin engine does. Other deactivations fall back
    // to a plain reset.
    if (event.type() == fcitx::EventType::InputContextSwitchInputMethod) {
        auto *ic = event.inputContext();
        commitBufferAsTyped(ic, state(ic));
        return;
    }
    reset(entry, event);
}

void WordPilotEngine::reset(const fcitx::InputMethodEntry &, fcitx::InputContextEvent &event) {
    // Fcitx convention for composing engines: a reset (focus moved, the
    // application reset the field) drops the uncommitted preedit.
    auto *ic = event.inputContext();
    clearState(ic, state(ic));
}

void WordPilotEngine::clearState(fcitx::InputContext *ic, WordPilotState *s) {
    s->buffer.clear();
    s->contextBefore.clear();
    s->contextAfter.clear();
    s->candidateWords.clear();
    s->candidateLanguage.clear();
    s->selected = -1;
    ic->inputPanel().reset();
    ic->updatePreedit();
    ic->updateUserInterface(fcitx::UserInterfaceComponent::InputPanel);
}

void WordPilotEngine::commitBufferAsTyped(fcitx::InputContext *ic, WordPilotState *s) {
    // Clear the preedit before committing, so the client never shows the word
    // twice (once as preedit, once as committed text).
    std::string typed = std::move(s->buffer);
    clearState(ic, s);
    if (!typed.empty()) {
        ic->commitString(typed);
    }
}

void WordPilotEngine::commitWord(fcitx::InputContext *ic, WordPilotState *s,
                                   const std::string &word) {
    const std::string language =
        s->candidateLanguage.empty() ? std::string("auto") : s->candidateLanguage;
    clearState(ic, s);
    ic->commitString(word);

    // Tell the daemon this candidate was accepted, so it can be learned
    // (wordpilot-storage's PersonalDictionary). The
    // language is the one the daemon completed in, so the word is filed
    // under the right dictionary even in auto mode. Fire and forget: a
    // dropped accept is a missed learning signal, never a reason to
    // interrupt typing.
    std::string request = std::string("{\"type\":\"accept-candidate\",\"word\":") +
                           JsonValue::escapeString(word) +
                           ",\"language\":" + JsonValue::escapeString(language) +
                           ",\"application\":" + applicationJson(ic) + "}";
    std::string response;
    socket_.request(request, response);
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
    if (!socket_.request(request, response)) {
        FCITX_WARN() << "WordPilot: daemon unreachable, language not switched";
        return;
    }
    // The daemon declines a language without a loaded dictionary and answers
    // with the mode it actually kept.
    JsonValue parsed = JsonValue::parse(response);
    FCITX_INFO() << "WordPilot: active language " << parsed.get("language").asString("?");
}

void WordPilotEngine::updateCandidates(fcitx::InputContext *ic, WordPilotState *s) {
    s->candidateWords.clear();
    s->candidateLanguage.clear();
    s->selected = -1;

    if (fcitx::utf8::length(s->buffer) < settings_.minPrefixLength) {
        renderPreedit(ic, s);
        return;
    }

    // "auto" language: the daemon tracks the user's active language
    // (hotkeys) and resolves it server-side, so the adapter never
    // needs to know or guess it. No max_candidates: the daemon applies
    // autocomplete.max_candidates from its config.
    std::string request =
        std::string("{\"type\":\"complete\",\"context\":{\"before_cursor\":") +
        JsonValue::escapeString(s->contextBefore + s->buffer) +
        ",\"after_cursor\":" + JsonValue::escapeString(s->contextAfter) +
        "},\"language\":\"auto\",\"application\":" + applicationJson(ic) + "}";
    std::string response;
    if (!socket_.request(request, response)) {
        // Daemon unreachable: the buffer the user typed is untouched, only
        // the candidate list disappears.
        renderPreedit(ic, s);
        return;
    }

    JsonValue parsed = JsonValue::parse(response);
    // The daemon completes the word it finds before the caret; committing a
    // candidate replaces exactly the buffer, so the two must agree.
    if (parsed.get("type").asString() != "completion" ||
        parsed.get("prefix").asString() != s->buffer) {
        renderPreedit(ic, s);
        return;
    }
    for (const auto &candidate : parsed.get("candidates").items()) {
        s->candidateWords.push_back(candidate.get("text").asString());
    }
    s->candidateLanguage = parsed.get("language").asString();
    if (!s->candidateWords.empty()) {
        s->selected = 0;
    }
    renderPreedit(ic, s);
}

void WordPilotEngine::renderPreedit(fcitx::InputContext *ic, WordPilotState *s) {
    fcitx::Text preedit;
    preedit.append(s->buffer, fcitx::TextFormatFlag::Underline);
    preedit.setCursor(static_cast<int>(s->buffer.size()));
    // Inline in the application when it can show a preedit, otherwise in
    // Fcitx's own panel -- never both, which would show the word twice.
    if (ic->capabilityFlags().test(fcitx::CapabilityFlag::Preedit)) {
        ic->inputPanel().setClientPreedit(preedit);
        ic->inputPanel().setPreedit(fcitx::Text());
    } else {
        ic->inputPanel().setClientPreedit(fcitx::Text());
        ic->inputPanel().setPreedit(preedit);
    }

    if (s->candidateWords.empty()) {
        ic->inputPanel().setCandidateList(nullptr);
    } else {
        auto list = std::make_unique<fcitx::CommonCandidateList>();
        for (const auto &word : s->candidateWords) {
            list->append<WordPilotCandidateWord>(this, s, word);
        }
        // One page holds every candidate, so Up/Down below never has to page.
        list->setPageSize(static_cast<int>(s->candidateWords.size()));
        list->setCursorIndex(std::max(s->selected, 0));
        ic->inputPanel().setCandidateList(std::move(list));
    }
    ic->updatePreedit();
    ic->updateUserInterface(fcitx::UserInterfaceComponent::InputPanel);
}

void WordPilotEngine::keyEvent(const fcitx::InputMethodEntry &, fcitx::KeyEvent &event) {
    if (event.isRelease()) {
        return;
    }
    auto *ic = event.inputContext();
    auto *s = state(ic);
    const auto &key = event.key();

    // Language hotkeys are handled by a global event watcher (see the
    // constructor) that fires regardless of which input method is active, so
    // they are NOT checked here.

    // No active composition: only start one on a letter. Everything else
    // (digits, punctuation, navigation, shortcuts) passes straight through to
    // the application, exactly as if WordPilot were not running.
    if (s->buffer.empty()) {
        const uint32_t codepoint = fcitx::Key::keySymToUnicode(key.sym());
        if (!isComposableKey(key) || !isComposableLetter(codepoint)) {
            return;
        }
        refreshSettings();
        if (!settings_.enabled || isSuppressed(ic) || !captureContext(ic, s)) {
            return;
        }
        s->buffer += fcitx::Key::keySymToUTF8(key.sym());
        updateCandidates(ic, s);
        event.filterAndAccept();
        return;
    }

    // A composition is in progress.
    const bool popupShown = !s->candidateWords.empty();

    if (key.check(FcitxKey_Escape)) {
        // Escape closes the popup and keeps the typed text. When no popup was
        // showing, the Escape itself also reaches the application.
        commitBufferAsTyped(ic, s);
        if (popupShown) {
            event.filterAndAccept();
        }
        return;
    }
    if (key.check(FcitxKey_BackSpace)) {
        utf8PopBack(s->buffer);
        if (s->buffer.empty()) {
            clearState(ic, s);
        } else {
            updateCandidates(ic, s);
        }
        event.filterAndAccept();
        return;
    }
    if (popupShown && (key.check(FcitxKey_Up) || key.check(FcitxKey_Down))) {
        const int count = static_cast<int>(s->candidateWords.size());
        const int step = key.check(FcitxKey_Up) ? -1 : 1;
        s->selected = (s->selected + step + count) % count;
        renderPreedit(ic, s);
        event.filterAndAccept();
        return;
    }
    if (key.check(FcitxKey_Tab)) {
        // Tab always accepts the first candidate, regardless of
        // where Up/Down moved the cursor. Without a popup it is an ordinary
        // Tab (focus change, indentation) after the typed word.
        if (popupShown) {
            commitWord(ic, s, s->candidateWords.front());
            event.filterAndAccept();
        } else {
            commitBufferAsTyped(ic, s);
        }
        return;
    }
    if (key.check(FcitxKey_Return) || key.check(FcitxKey_KP_Enter)) {
        // Enter accepts the *selected* candidate only when the popup is
        // active; otherwise it must behave like a normal newline, so the
        // event is allowed through after committing the plain buffer.
        if (popupShown && s->selected >= 0 &&
            s->selected < static_cast<int>(s->candidateWords.size())) {
            commitWord(ic, s, s->candidateWords[s->selected]);
            event.filterAndAccept();
        } else {
            commitBufferAsTyped(ic, s);
        }
        return;
    }
    {
        const uint32_t codepoint = fcitx::Key::keySymToUnicode(key.sym());
        if (isComposableKey(key) && isComposableLetter(codepoint)) {
            s->buffer += fcitx::Key::keySymToUTF8(key.sym());
            updateCandidates(ic, s);
            event.filterAndAccept();
            return;
        }
    }

    // Any other key (space, punctuation, digits, arrows without a popup,
    // shortcuts) ends the composition: commit what was typed verbatim, then
    // let the key itself fall through to the application untouched.
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
