#!/usr/bin/env bash
# Installs Hunspell dictionaries and frequency lists into the XDG data
# directory, so WordPilot works without root.
#
# Prefer your distribution's packages when you can install them:
#   sudo pacman -S hunspell-de hunspell-en_us
# This script is the no-root fallback, and it also fetches the frequency lists,
# which no distribution packages.

set -euo pipefail

DATA_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/wordpilot"
DICT_DIR="$DATA_DIR/dictionaries"
FREQ_DIR="$DATA_DIR/frequency"

DICT_BASE="https://raw.githubusercontent.com/LibreOffice/dictionaries/master"
FREQ_BASE="https://raw.githubusercontent.com/hermitdave/FrequencyWords/master/content/2018"

mkdir -p "$DICT_DIR" "$FREQ_DIR"

fetch() {
    local url="$1" dest="$2"
    if [ -f "$dest" ]; then
        echo "  have     $(basename "$dest")"
        return
    fi
    if curl -sSfL -o "$dest.part" "$url"; then
        mv "$dest.part" "$dest"
        echo "  fetched  $(basename "$dest")"
    else
        rm -f "$dest.part"
        echo "  FAILED   $(basename "$dest") <- $url" >&2
        return 1
    fi
}

echo "dictionaries -> $DICT_DIR"
# German ships as de_DE_frami in the LibreOffice repository; WordPilot accepts
# either name, but the canonical one keeps `doctor` output tidy.
fetch "$DICT_BASE/de/de_DE_frami.dic" "$DICT_DIR/de_DE.dic"
fetch "$DICT_BASE/de/de_DE_frami.aff" "$DICT_DIR/de_DE.aff"
fetch "$DICT_BASE/en/en_US.dic" "$DICT_DIR/en_US.dic"
fetch "$DICT_BASE/en/en_US.aff" "$DICT_DIR/en_US.aff"
fetch "$DICT_BASE/en/en_GB.dic" "$DICT_DIR/en_GB.dic"
fetch "$DICT_BASE/en/en_GB.aff" "$DICT_DIR/en_GB.aff"

echo "frequency lists -> $FREQ_DIR"
fetch "$FREQ_BASE/de/de_50k.txt" "$FREQ_DIR/de_50k.txt"
fetch "$FREQ_BASE/en/en_50k.txt" "$FREQ_DIR/en_50k.txt"

echo
echo "done; verify with: wordpilot doctor"
