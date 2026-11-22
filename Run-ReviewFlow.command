#!/usr/bin/env bash
# macOS double-click launcher (Terminal opens .command files).
cd "$(dirname "$0")" || exit 1
exec /bin/bash ./Run-ReviewFlow.sh
