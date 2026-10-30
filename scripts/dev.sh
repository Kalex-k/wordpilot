#!/usr/bin/env bash
# Development loop for WordPilot: build, link, restart -- without reinstalling.
#
#   scripts/dev.sh link          symlink the Rust binaries into ~/.local/bin
#   scripts/dev.sh link-addon    symlink the Fcitx addon into ~/.local (no root)
#   scripts/dev.sh build         incremental release build of daemon + addon
#   scripts/dev.sh test          Rust tests + addon integration test
#   scripts/dev.sh restart       restart the daemon (and Fcitx with --fcitx)
#   scripts/dev.sh up            build + restart: the usual edit-compile-try loop
#   scripts/dev.sh reload        make the daemon re-read config.toml now
#
# After `link` and `link-addon` once, every later build is picked up by a
# plain restart: the installed paths point at the build outputs. Config edits
# need not even that -- the daemon re-reads config.toml within a second and
# the addon fetches the new settings from the daemon within two.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ADDON_SRC="$ROOT/fcitx/wordpilot-addon"
ADDON_BUILD="$ADDON_SRC/build"
BIN_DIR="$HOME/.local/bin"
USER_ADDON_LIB="$HOME/.local/lib/fcitx5"
USER_ADDON_CONF="${XDG_DATA_HOME:-$HOME/.local/share}/fcitx5/addon"
BINARIES=(wordpilot-daemon wordpilot wordpilot-test-client)

log() { printf '==> %s\n' "$*"; }

build_rust() {
    log "cargo build --release"
    cargo build --release --manifest-path "$ROOT/Cargo.toml"
}

build_addon() {
    if [ ! -f "$ADDON_BUILD/CMakeCache.txt" ]; then
        log "configuring the Fcitx addon"
        cmake -S "$ADDON_SRC" -B "$ADDON_BUILD"
    fi
    log "building the Fcitx addon"
    cmake --build "$ADDON_BUILD" -j"$(nproc)"
}

link_rust() {
    mkdir -p "$BIN_DIR"
    for binary in "${BINARIES[@]}"; do
        ln -sfn "$ROOT/target/release/$binary" "$BIN_DIR/$binary"
        log "$BIN_DIR/$binary -> target/release/$binary"
    done
}

link_addon() {
    mkdir -p "$USER_ADDON_LIB" "$USER_ADDON_CONF"
    ln -sfn "$ADDON_BUILD/libwordpilot.so" "$USER_ADDON_LIB/libwordpilot.so"
    ln -sfn "$ADDON_SRC/wordpilot-addon.conf" "$USER_ADDON_CONF/wordpilot-addon.conf"
    log "$USER_ADDON_LIB/libwordpilot.so -> build output"
    log "$USER_ADDON_CONF/wordpilot-addon.conf -> source tree"
    cat <<EOF

Fcitx only loads libraries from \$FCITX_ADDON_DIRS (default: /usr/lib/fcitx5),
and setting it *replaces* the default. Start Fcitx with:

    FCITX_ADDON_DIRS=$USER_ADDON_LIB:/usr/lib/fcitx5 fcitx5 -d --replace

and export the same variable wherever your session starts fcitx5. If a copy
is also installed in /usr/lib/fcitx5, the one listed first wins.
EOF
}

restart() {
    if systemctl --user is-enabled --quiet wordpilot.service 2>/dev/null ||
        systemctl --user is-active --quiet wordpilot.service 2>/dev/null; then
        log "restarting wordpilot.service"
        systemctl --user restart wordpilot.service
    elif pgrep -x wordpilot-daemon >/dev/null; then
        log "restarting the manually started daemon"
        pkill -x wordpilot-daemon || true
        sleep 0.3
        nohup "$BIN_DIR/wordpilot-daemon" >/dev/null 2>&1 &
    else
        log "daemon is not running; start it with: systemctl --user start wordpilot"
    fi
    if [ "${1:-}" = "--fcitx" ]; then
        log "restarting Fcitx with this shell's environment (incl. FCITX_ADDON_DIRS)"
        fcitx5 -r -d >/dev/null 2>&1 || log "fcitx5 -r failed; restart it from your session"
    fi
}

run_tests() {
    log "cargo test"
    cargo test --workspace --manifest-path "$ROOT/Cargo.toml"
    build_addon
    log "addon integration test"
    ctest --test-dir "$ADDON_BUILD" --output-on-failure
}

case "${1:-}" in
    link) link_rust ;;
    link-addon) link_addon ;;
    build) build_rust; build_addon ;;
    test) run_tests ;;
    restart) restart "${2:-}" ;;
    up) build_rust; build_addon; restart "${2:-}" ;;
    reload) "$BIN_DIR/wordpilot" reload ;;
    *)
        sed -n '2,15p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
        exit 2
        ;;
esac
