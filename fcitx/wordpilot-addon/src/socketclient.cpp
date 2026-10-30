#include "socketclient.h"

#include <cerrno>
#include <cstdlib>
#include <cstring>

#include <sys/socket.h>
#include <sys/types.h>
#include <sys/un.h>
#include <unistd.h>

namespace wordpilot {

namespace {

// Bounds every request so a hung daemon costs one slow keystroke, not an
// unresponsive input method (the hot path has a hard latency
// budget; this is the backstop for when something upstream has gone wrong).
constexpr int kTimeoutMs = 200;

// Resolves $XDG_RUNTIME_DIR/wordpilot/daemon.sock, matching
// wordpilot_core::paths::socket_path(). No $HOME fallback, on purpose: a
// socket outside $XDG_RUNTIME_DIR would not be guaranteed user-private
//. $WORDPILOT_SOCKET overrides it, for a daemon started with
// `--socket` or `general.socket_path` -- set it in Fcitx's environment.
std::string defaultSocketPath() {
    const char *explicitPath = std::getenv("WORDPILOT_SOCKET");
    if (explicitPath && explicitPath[0] != '\0') {
        return explicitPath;
    }
    const char *runtimeDir = std::getenv("XDG_RUNTIME_DIR");
    if (!runtimeDir || runtimeDir[0] == '\0') {
        return {};
    }
    return std::string(runtimeDir) + "/wordpilot/daemon.sock";
}

} // namespace

SocketClient::SocketClient() : socketPath_(defaultSocketPath()) {}

SocketClient::~SocketClient() { disconnect(); }

void SocketClient::disconnect() {
    if (fd_ >= 0) {
        ::close(fd_);
        fd_ = -1;
    }
}

bool SocketClient::ensureConnected() {
    if (fd_ >= 0) {
        return true;
    }
    if (socketPath_.empty()) {
        return false;
    }

    fd_ = ::socket(AF_UNIX, SOCK_STREAM, 0);
    if (fd_ < 0) {
        return false;
    }

    struct timeval timeout {};
    timeout.tv_sec = kTimeoutMs / 1000;
    timeout.tv_usec = (kTimeoutMs % 1000) * 1000;
    ::setsockopt(fd_, SOL_SOCKET, SO_RCVTIMEO, &timeout, sizeof(timeout));
    ::setsockopt(fd_, SOL_SOCKET, SO_SNDTIMEO, &timeout, sizeof(timeout));

    struct sockaddr_un addr {};
    addr.sun_family = AF_UNIX;
    if (socketPath_.size() >= sizeof(addr.sun_path)) {
        disconnect();
        return false;
    }
    std::strncpy(addr.sun_path, socketPath_.c_str(), sizeof(addr.sun_path) - 1);

    if (::connect(fd_, reinterpret_cast<struct sockaddr *>(&addr), sizeof(addr)) != 0) {
        disconnect();
        return false;
    }
    return true;
}

bool SocketClient::request(const std::string &requestLine, std::string &response) {
    if (!ensureConnected()) {
        return false;
    }

    std::string line = requestLine;
    if (line.empty() || line.back() != '\n') {
        line.push_back('\n');
    }

    size_t sent = 0;
    while (sent < line.size()) {
        ssize_t n = ::send(fd_, line.data() + sent, line.size() - sent, MSG_NOSIGNAL);
        if (n <= 0) {
            // A write failure most likely means the daemon restarted and the
            // old socket is dead; drop it so the *next* call reconnects
            // instead of repeating the same failure forever.
            disconnect();
            return false;
        }
        sent += static_cast<size_t>(n);
    }

    response.clear();
    char buffer[4096];
    while (true) {
        ssize_t n = ::recv(fd_, buffer, sizeof(buffer), 0);
        if (n < 0) {
            disconnect();
            return false;
        }
        if (n == 0) {
            // Clean EOF before a newline arrived: the daemon closed the
            // connection without answering.
            disconnect();
            return false;
        }
        response.append(buffer, static_cast<size_t>(n));
        if (response.find('\n') != std::string::npos) {
            break;
        }
    }
    return true;
}

} // namespace wordpilot
