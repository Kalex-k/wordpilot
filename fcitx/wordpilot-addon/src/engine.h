// The WordPilot Fcitx 5 input method engine.
//
// Behaves like Fcitx's own composing engines (Pinyin, Table, Spell): typed
// Latin text is held in a per-InputContext preedit buffer rather than being
// forwarded to the application immediately, so a completion can replace it
// outright on commit. This avoids the delete-then-retype hack the spec
// explicitly rules out for AI text replacement (§26) -- for plain
// completion there is nothing to delete, because nothing was sent yet.
//
// Limitation, documented rather than hidden (spec §60): this engine reads
// its own preedit buffer, not the application's surrounding text. Fcitx 5
// exposes `InputContext::surroundingText()` when the application's text
// field protocol supports it (most GTK/Qt widgets do over Wayland's
// text-input-v3 or the X11 equivalents), but a composing engine that hasn't
// committed anything yet has nothing in the surrounding text to read -- the
// preedit buffer the user is actively typing *is* the authoritative state
// here. Surrounding text becomes relevant once the AI actions (Phase 1C)
// need the sentence around an existing, already-committed selection; this
// engine does not implement that yet.
#pragma once

#include <memory>
#include <string>
#include <unordered_map>

#include <fcitx/addonfactory.h>
#include <fcitx/inputmethodengine.h>
#include <fcitx/instance.h>

#include "socketclient.h"

namespace wordpilot {

// Per-InputContext state. Fcitx gives one engine instance to the whole
// process, shared across every focused text field, so anything that must not
// leak between fields (the current word, the candidate list) lives here,
// keyed by InputContext.
struct WordPilotState : public fcitx::InputContextProperty {
    std::string buffer;         // the word composed so far, UTF-8
    std::vector<std::string> candidateWords;
    int selected = -1;          // index into candidateWords, -1 = none
};

class WordPilotEngine : public fcitx::InputMethodEngine {
public:
    explicit WordPilotEngine(fcitx::Instance *instance);

    std::vector<fcitx::InputMethodEntry> listInputMethods() override;

    void keyEvent(const fcitx::InputMethodEntry &entry,
                  fcitx::KeyEvent &event) override;
    void activate(const fcitx::InputMethodEntry &entry,
                  fcitx::InputContextEvent &event) override;
    void deactivate(const fcitx::InputMethodEntry &entry,
                     fcitx::InputContextEvent &event) override;
    void reset(const fcitx::InputMethodEntry &entry,
               fcitx::InputContextEvent &event) override;

    // Called by WordPilotCandidateWord::select, which only has the engine
    // pointer available, not access to its private members.
    void commitWordPublic(fcitx::InputContext *ic, WordPilotState *s,
                           const std::string &word);

private:
    WordPilotState *state(fcitx::InputContext *ic);

    // Queries the daemon for the current buffer and updates the preedit +
    // candidate list. A daemon error or timeout clears the candidate list but
    // never the buffer -- what the user typed is never lost because of a
    // dead daemon (spec §44).
    void updateCandidates(fcitx::InputContext *ic, WordPilotState *s);

    // Replaces the buffer with `word` in both the preedit and (via
    // commitString) the application, then clears composing state.
    void commitWord(fcitx::InputContext *ic, WordPilotState *s,
                     const std::string &word);

    // Sends a SetLanguage request for the daemon's *active* language (spec
    // §2). Commits whatever was being composed first, as plain typed text,
    // so a language switch mid-word never discards it.
    void switchLanguage(fcitx::InputContext *ic, WordPilotState *s,
                         const std::string &languageMode);

    // Commits the raw buffer unchanged -- used when the user types a
    // separator (space, punctuation) without picking a candidate.
    void commitBufferAsTyped(fcitx::InputContext *ic, WordPilotState *s);

    void renderPreedit(fcitx::InputContext *ic, WordPilotState *s);

    fcitx::Instance *instance_;
    SocketClient socket_;
    fcitx::FactoryFor<WordPilotState> factory_;
    // Registered in the constructor so the three language hotkeys work
    // regardless of which input method is currently active -- Super+Alt+D
    // must switch WordPilot's language even while "Keyboard - US" is the
    // active Fcitx engine, not only while WordPilot itself is (spec §2: the
    // hotkeys are global, not scoped to one input method). The handle must
    // outlive the watch; destroying it unregisters the callback.
    std::unique_ptr<fcitx::HandlerTableEntry<fcitx::EventHandler>> hotkeyHandle_;
};

class WordPilotEngineFactory : public fcitx::AddonFactory {
public:
    fcitx::AddonInstance *create(fcitx::AddonManager *manager) override;
};

} // namespace wordpilot
