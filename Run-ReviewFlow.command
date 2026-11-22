#!/usr/bin/env bash
# macOS double-click launcher (Finder opens .command files in Terminal). Shows the mode menu.
cd "$(dirname "$0")" || exit 1
exec /bin/bash ./Run-ReviewFlow.sh "$@"
