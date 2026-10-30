// The WordPilot Fcitx 5 input method engine.
//
// Behaves like Fcitx's own composing engines (Pinyin, Table, Spell): typed
// Latin text is held in a per-InputContext preedit buffer rather than being
// forwarded to the application immediately, so a completion can replace it
// outright on commit. This avoids deleting the typed prefix and
// retyping the whole word, which is fragile across toolkits -- there is
// nothing to delete, because nothing was sent yet.
//
// Surrounding text: when the application reports it (most GTK/Qt widgets do,
// on X11 through the IM modules and on Wayland through text-input-v3), the
// text before and after the caret is captured when a word starts and sent
// along as context. That is what lets the daemon's language detection and
// its "caret inside a word" check work. Applications that do not report it
// still get completion of the word being typed, just without context.
//
// Settings: nothing here reads config.toml. The hotkeys, the minimum prefix
// length, the application blacklist and the privacy switches come from the
// daemon (`frontend-config` request) and are re-fetched every few seconds,
// so editing the config takes effect without restarting Fcitx and without
// rebuilding this addon.
#pragma once

#include <chrono>
#include <memory>
#include <string>
#include <vector>

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
    std::string contextBefore;  // application text left of the word, if known
    std::string contextAfter;   // application text right of the caret, if known
    std::vector<std::string> candidateWords;
    std::string candidateLanguage;  // language the daemon completed in
    int selected = -1;          // index into candidateWords, -1 = none
};

// The daemon-provided part of the configuration (see wordpilot-protocol's
// FrontendConfig). The defaults mirror config.toml's defaults and are what
// the addon uses until the daemon has answered once.
struct FrontendSettings {
    bool enabled = true;
    size_t minPrefixLength = 2;
    bool passwordFieldDetection = true;
    bool sensitiveFieldDetection = true;
    std::vector<std::string> disabledApplications;
    fcitx::Key hotkeyGerman;
    fcitx::Key hotkeyEnglish;
    fcitx::Key hotkeyAuto;
    long long generation = -1;  // -1: never fetched from the daemon
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

    // Re-reads the frontend settings from the daemon, at most once per
    // refresh interval unless `force` is set. A daemon that does not answer
    // leaves the previous settings in place.
    void refreshSettings(bool force = false);

    // Whether WordPilot must stay completely out of this field: a password
    // or sensitive field (per the privacy switches), or a blacklisted
    // application. Nothing is composed and nothing is sent to the daemon.
    bool isSuppressed(fcitx::InputContext *ic) const;

    // The `"application":{...}` member of a request for this field.
    std::string applicationJson(fcitx::InputContext *ic) const;

    // Captures the application's text around the caret into `s` when a new
    // word starts. Returns false when the caret is directly next to a letter
    // or digit -- i.e. the user is editing an existing word, which a
    // composing engine must not split.
    bool captureContext(fcitx::InputContext *ic, WordPilotState *s);

    // Queries the daemon for the current buffer and updates the preedit +
    // candidate list. A daemon error or timeout clears the candidate list but
    // never the buffer -- what the user typed is never lost because of a
    // dead daemon.
    void updateCandidates(fcitx::InputContext *ic, WordPilotState *s);

    // Replaces the buffer with `word` in both the preedit and (via
    // commitString) the application, then clears composing state.
    void commitWord(fcitx::InputContext *ic, WordPilotState *s,
                     const std::string &word);

    // Sends a SetLanguage request for the daemon's *active* language.
    // Commits whatever was being composed first, as plain typed text,
    // so a language switch mid-word never discards it.
    void switchLanguage(fcitx::InputContext *ic, WordPilotState *s,
                         const std::string &languageMode);

    // Commits the raw buffer unchanged -- used when the user types a
    // separator (space, punctuation) without picking a candidate.
    void commitBufferAsTyped(fcitx::InputContext *ic, WordPilotState *s);

    void clearState(fcitx::InputContext *ic, WordPilotState *s);
    void renderPreedit(fcitx::InputContext *ic, WordPilotState *s);

    fcitx::Instance *instance_;
    SocketClient socket_;
    fcitx::FactoryFor<WordPilotState> factory_;
    FrontendSettings settings_;
    std::chrono::steady_clock::time_point lastSettingsFetch_{};
    // Registered in the constructor so the three language hotkeys work
    // regardless of which input method is currently active -- Super+Alt+D
    // must switch WordPilot's language even while "Keyboard - US" is the
    // active Fcitx engine, not only while WordPilot itself is (the
    // hotkeys are global, not scoped to one input method). The handles must
    // outlive the watches; destroying one unregisters its callback.
    std::unique_ptr<fcitx::HandlerTableEntry<fcitx::EventHandler>> hotkeyHandle_;
    std::unique_ptr<fcitx::HandlerTableEntry<fcitx::EventHandler>> focusHandle_;
};

class WordPilotEngineFactory : public fcitx::AddonFactory {
public:
    fcitx::AddonInstance *create(fcitx::AddonManager *manager) override;
};

} // namespace wordpilot
