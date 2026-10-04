// Blocking Unix-domain-socket client for the WordPilot daemon's
// newline-delimited JSON protocol (wordpilot-protocol/src/codec.rs).
//
// Deliberately synchronous: Fcitx's key event handling is itself synchronous,
// the daemon answers in well under a millisecond once dictionaries are warm
// (measured: ~0.1 ms, see the Rust workspace's latency test), and a short
// socket timeout bounds the worst case so a stuck or dead daemon cannot hang
// a keystroke indefinitely. If this ever needs to be non-blocking, it belongs
// behind this same interface -- nothing in engine.cpp should need to change.
#pragma once

#include <string>

namespace wordpilot {

class SocketClient {
public:
    SocketClient();
    ~SocketClient();

    SocketClient(const SocketClient &) = delete;
    SocketClient &operator=(const SocketClient &) = delete;

    // Sends one line and reads one line back. Connects lazily on first use
    // and reconnects automatically after a failure, so a daemon restart
    // (spec §44: "daemon должен уметь перезапускаться без перезапуска Fcitx")
    // is transparent to the caller beyond one failed request.
    //
    // Returns false on any I/O failure or timeout; `response` is untouched in
    // that case. This is the single point where "is the daemon there at all"
    // is decided -- callers never need their own timeout logic.
    bool request(const std::string &requestLine, std::string &response);

private:
    bool ensureConnected();
    void disconnect();

    int fd_ = -1;
    std::string socketPath_;
};

} // namespace wordpilot
