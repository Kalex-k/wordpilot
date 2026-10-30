// End-to-end test of the addon inside a real, in-process fcitx::Instance.
//
// Fcitx's TestFrontend addon plays the application: it creates input
// contexts, feeds them key events and asserts on every string the addon
// commits. A small fake daemon on a private Unix socket plays
// wordpilot-daemon, answering with canned candidates and recording what the
// addon sent -- so this tests the adapter alone, independent of which
// dictionaries the machine has installed.

#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>

#include <atomic>
#include <cstdlib>
#include <cstring>
#include <filesystem>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

#include <fcitx-utils/eventdispatcher.h>
#include <fcitx-utils/log.h>
#include <fcitx-utils/testing.h>
#include <fcitx/addonmanager.h>
#include <fcitx/inputcontext.h>
#include <fcitx/inputcontextmanager.h>
#include <fcitx/inputmethodgroup.h>
#include <fcitx/inputmethodmanager.h>
#include <fcitx/instance.h>

#include "json.h"
#include "testfrontend_public.h"

using namespace fcitx;
using wordpilot::JsonValue;

namespace {

// What the fake daemon saw, for the assertions below.
struct Recorded {
    std::mutex mutex;
    std::vector<std::string> beforeCursor;   // one per complete request
    std::vector<std::string> applications;   // one per complete request
    std::vector<std::string> accepted;       // "word/language/app"
    std::vector<std::string> languages;      // set-language requests

    template <typename F>
    auto with(F f) {
        std::lock_guard<std::mutex> lock(mutex);
        return f(*this);
    }
};

class FakeDaemon {
public:
    explicit FakeDaemon(std::string path) : path_(std::move(path)) {
        ::unlink(path_.c_str());
        listenFd_ = ::socket(AF_UNIX, SOCK_STREAM, 0);
        sockaddr_un addr{};
        addr.sun_family = AF_UNIX;
        std::strncpy(addr.sun_path, path_.c_str(), sizeof(addr.sun_path) - 1);
        FCITX_ASSERT(::bind(listenFd_, reinterpret_cast<sockaddr *>(&addr), sizeof(addr)) == 0);
        FCITX_ASSERT(::listen(listenFd_, 4) == 0);
        thread_ = std::thread([this] { run(); });
    }

    ~FakeDaemon() {
        stop_ = true;
        ::shutdown(listenFd_, SHUT_RDWR);
        ::close(listenFd_);
        thread_.join();
        ::unlink(path_.c_str());
    }

    Recorded recorded;

private:
    void run() {
        while (!stop_) {
            int fd = ::accept(listenFd_, nullptr, nullptr);
            if (fd < 0) {
                return;
            }
            serve(fd);
            ::close(fd);
        }
    }

    void serve(int fd) {
        std::string pending;
        char buffer[4096];
        while (true) {
            ssize_t n = ::recv(fd, buffer, sizeof(buffer), 0);
            if (n <= 0) {
                return;
            }
            pending.append(buffer, static_cast<size_t>(n));
            size_t newline;
            while ((newline = pending.find('\n')) != std::string::npos) {
                std::string reply = answer(pending.substr(0, newline)) + "\n";
                pending.erase(0, newline + 1);
                ::send(fd, reply.data(), reply.size(), MSG_NOSIGNAL);
            }
        }
    }

    std::string answer(const std::string &line) {
        JsonValue request = JsonValue::parse(line);
        const std::string type = request.get("type").asString();
        if (type == "frontend-config") {
            return R"({"type":"frontend-config","generation":7,"autocomplete_enabled":true,)"
                   R"("min_prefix_length":2,"max_candidates":5,"password_field_detection":true,)"
                   R"("sensitive_field_detection":true,"disabled_applications":["KeePassXC"],)"
                   R"("hotkeys":{"language_german":"Control+Alt+g","language_english":"",)"
                   R"("language_auto":"","ai":""}})";
        }
        if (type == "complete") {
            const std::string before = request.get("context").get("before_cursor").asString();
            const std::string app = request.get("application").get("name").asString("<none>");
            recorded.with([&](Recorded &r) {
                r.beforeCursor.push_back(before);
                r.applications.push_back(app);
            });
            size_t start = before.size();
            while (start > 0 && std::isalpha(static_cast<unsigned char>(before[start - 1]))) {
                --start;
            }
            const std::string word = before.substr(start);
            std::string candidates;
            if (word.rfind("bew", 0) == 0) {
                candidates = R"({"text":"bewerben","source":"personal","score":0.9},)"
                             R"({"text":"bewegen","source":"frequency","score":0.5})";
            }
            return R"({"type":"completion","language":"de_DE","prefix":)" +
                   JsonValue::escapeString(word) + R"(,"prefix_len_bytes":)" +
                   std::to_string(word.size()) + R"(,"candidates":[)" + candidates + "]}";
        }
        if (type == "accept-candidate") {
            recorded.with([&](Recorded &r) {
                r.accepted.push_back(request.get("word").asString() + "/" +
                                     request.get("language").asString() + "/" +
                                     request.get("application").get("name").asString());
            });
            return R"({"type":"accepted"})";
        }
        if (type == "set-language") {
            const std::string language = request.get("language").asString();
            recorded.with([&](Recorded &r) { r.languages.push_back(language); });
            return R"({"type":"language-set","language":)" + JsonValue::escapeString(language) + "}";
        }
        return R"({"type":"error","code":"unsupported","message":"fake"})";
    }

    std::string path_;
    int listenFd_ = -1;
    std::atomic<bool> stop_{false};
    std::thread thread_;
};

FakeDaemon *daemon_ = nullptr;

void type(AddonInstance *frontend, ICUUID uuid, const std::string &keys) {
    for (char c : keys) {
        frontend->call<ITestFrontend::keyEvent>(uuid, Key(std::string(1, c).c_str()), false);
    }
}

void scheduleEvents(EventDispatcher *dispatcher, Instance *instance) {
    dispatcher->schedule([dispatcher, instance]() {
        auto *frontend = instance->addonManager().addon("testfrontend");
        FCITX_ASSERT(frontend);
        FCITX_ASSERT(instance->addonManager().addon("wordpilot-addon", true));

        auto group = instance->inputMethodManager().currentGroup();
        group.inputMethodList().clear();
        group.inputMethodList().push_back(InputMethodGroupItem("keyboard-us"));
        group.inputMethodList().push_back(InputMethodGroupItem("wordpilot"));
        group.setDefaultInputMethod("");
        instance->inputMethodManager().setGroup(group);

        auto uuid = frontend->call<ITestFrontend::createInputContext>("testapp");
        auto *ic = instance->inputContextManager().findByUUID(uuid);
        FCITX_ASSERT(ic);
        ic->focusIn();
        instance->setCurrentInputMethod(ic, "wordpilot", true);
        FCITX_ASSERT(instance->currentInputMethod() == "wordpilot");

        // Tab takes the first candidate, and the acceptance is reported with
        // the language the daemon completed in and the application name.
        frontend->call<ITestFrontend::pushCommitExpectation>("bewerben");
        type(frontend, uuid, "bew");
        FCITX_ASSERT(frontend->call<ITestFrontend::sendKeyEvent>(uuid, Key("Tab"), false));
        daemon_->recorded.with([](Recorded &r) {
            FCITX_ASSERT(r.accepted.size() == 1) << r.accepted.size();
            FCITX_ASSERT(r.accepted[0] == "bewerben/de_DE/testapp") << r.accepted[0];
            FCITX_ASSERT(r.applications.back() == "testapp");
        });

        // Down + Enter takes the selected candidate.
        frontend->call<ITestFrontend::pushCommitExpectation>("bewegen");
        type(frontend, uuid, "bew");
        FCITX_ASSERT(frontend->call<ITestFrontend::sendKeyEvent>(uuid, Key("Down"), false));
        FCITX_ASSERT(frontend->call<ITestFrontend::sendKeyEvent>(uuid, Key("Return"), false));

        // A separator commits the typed text as is and still reaches the app.
        frontend->call<ITestFrontend::pushCommitExpectation>("bew");
        type(frontend, uuid, "bew");
        FCITX_ASSERT(!frontend->call<ITestFrontend::sendKeyEvent>(uuid, Key("space"), false));

        // Escape closes the popup but keeps the text.
        frontend->call<ITestFrontend::pushCommitExpectation>("bew");
        type(frontend, uuid, "bew");
        FCITX_ASSERT(frontend->call<ITestFrontend::sendKeyEvent>(uuid, Key("Escape"), false));

        // Without a popup, Tab is an ordinary Tab after the typed word.
        frontend->call<ITestFrontend::pushCommitExpectation>("xy");
        type(frontend, uuid, "xy");
        FCITX_ASSERT(!frontend->call<ITestFrontend::sendKeyEvent>(uuid, Key("Tab"), false));

        // Backspace edits the composition by characters.
        frontend->call<ITestFrontend::pushCommitExpectation>("be");
        type(frontend, uuid, "bex");
        FCITX_ASSERT(frontend->call<ITestFrontend::sendKeyEvent>(uuid, Key("BackSpace"), false));
        FCITX_ASSERT(!frontend->call<ITestFrontend::sendKeyEvent>(uuid, Key("period"), false));

        // The language hotkey comes from the daemon's config, not a constant.
        FCITX_ASSERT(
            frontend->call<ITestFrontend::sendKeyEvent>(uuid, Key("Control+Alt+g"), false));
        daemon_->recorded.with([](Recorded &r) {
            FCITX_ASSERT(r.languages.size() == 1 && r.languages[0] == "de");
        });

        // Surrounding text is sent as context ...
        ic->setCapabilityFlags(ic->capabilityFlags() | CapabilityFlag::SurroundingText);
        ic->surroundingText().setText("Ich habe ", 9, 9);
        frontend->call<ITestFrontend::pushCommitExpectation>("bewerben");
        type(frontend, uuid, "bew");
        FCITX_ASSERT(frontend->call<ITestFrontend::sendKeyEvent>(uuid, Key("Tab"), false));
        daemon_->recorded.with([](Recorded &r) {
            FCITX_ASSERT(r.beforeCursor.back() == "Ich habe bew") << r.beforeCursor.back();
        });

        // ... and a caret right after a letter is an existing word being
        // edited, which the addon leaves alone.
        ic->surroundingText().setText("Bew", 3, 3);
        FCITX_ASSERT(!frontend->call<ITestFrontend::sendKeyEvent>(uuid, Key("e"), false));
        ic->surroundingText().invalidate();

        // Password fields are never composed and never sent to the daemon.
        const size_t requestsBefore =
            daemon_->recorded.with([](Recorded &r) { return r.beforeCursor.size(); });
        ic->setCapabilityFlags(ic->capabilityFlags() | CapabilityFlag::Password);
        FCITX_ASSERT(!frontend->call<ITestFrontend::sendKeyEvent>(uuid, Key("b"), false));
        FCITX_ASSERT(!frontend->call<ITestFrontend::sendKeyEvent>(uuid, Key("e"), false));
        ic->setCapabilityFlags(CapabilityFlags());
        FCITX_ASSERT(daemon_->recorded.with([](Recorded &r) { return r.beforeCursor.size(); }) ==
                     requestsBefore);

        // Neither are blacklisted applications (case-insensitive match).
        auto keepass = frontend->call<ITestFrontend::createInputContext>("keepassxc");
        auto *keepassIc = instance->inputContextManager().findByUUID(keepass);
        keepassIc->focusIn();
        instance->setCurrentInputMethod(keepassIc, "wordpilot", true);
        FCITX_ASSERT(!frontend->call<ITestFrontend::sendKeyEvent>(keepass, Key("b"), false));
        FCITX_ASSERT(daemon_->recorded.with([](Recorded &r) { return r.beforeCursor.size(); }) ==
                     requestsBefore);

        dispatcher->schedule([dispatcher, instance]() {
            dispatcher->detach();
            instance->exit();
        });
    });
}

} // namespace

int main() {
    const auto socketPath =
        std::filesystem::temp_directory_path() / ("wordpilot-test-" + std::to_string(::getpid()) + ".sock");
    FakeDaemon daemon(socketPath.string());
    daemon_ = &daemon;
    ::setenv("WORDPILOT_SOCKET", socketPath.c_str(), 1);

    setupTestingEnvironment(WORDPILOT_TEST_BINARY_DIR, {"."}, {"test"});
    char arg0[] = "testwordpilot";
    char arg1[] = "--disable=all";
    char arg2[] = "--enable=testim,testfrontend,testui,wordpilot-addon";
    char *argv[] = {arg0, arg1, arg2};
    Instance instance(FCITX_ARRAY_SIZE(argv), argv);
    instance.addonManager().registerDefaultLoader(nullptr);
    EventDispatcher dispatcher;
    dispatcher.attach(&instance.eventLoop());
    scheduleEvents(&dispatcher, &instance);
    instance.exec();
    daemon_ = nullptr;
    return 0;
}
