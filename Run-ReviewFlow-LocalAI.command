#!/usr/bin/env bash
cd "$(dirname "$0")" || exit 1
export RF_LOCAL_AI=1
exec /bin/bash ./Run-ReviewFlow.sh
