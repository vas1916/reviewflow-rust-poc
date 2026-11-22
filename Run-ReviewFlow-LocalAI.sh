#!/usr/bin/env bash
cd "$(dirname "${BASH_SOURCE[0]}")" || exit 1
export RF_LOCAL_AI=1
exec bash ./Run-ReviewFlow.sh
