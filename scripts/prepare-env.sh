#!/usr/bin/env bash
# Repair missing/example tokens without requiring Python or sourcing untrusted .env.
set -Eeuo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
umask 077
if [[ ! -f .env ]]; then : > .env; fi

keys=(EMPLOYEE_API_TOKEN REVIEWER_API_TOKEN SQLITE_PATH BIND_ADDR AI_MODE OLLAMA_BASE_URL OLLAMA_MODEL AZURE_OPENAI_ENDPOINT AZURE_OPENAI_DEPLOYMENT AZURE_OPENAI_API_KEY)
for key in "${keys[@]}"; do unset "$key" 2>/dev/null || true; done
preserved=()
while IFS= read -r line || [[ -n "$line" ]]; do
  [[ "$line" == *$'\r' ]] && line="${line%$'\r'}"
  if [[ "$line" == *=* ]]; then
    key="${line%%=*}"
    key="$(printf '%s' "$key" | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')"
    value="${line#*=}"
    value="$(printf '%s' "$value" | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')"
    case "$key" in
      EMPLOYEE_API_TOKEN|REVIEWER_API_TOKEN|SQLITE_PATH|BIND_ADDR|AI_MODE|OLLAMA_BASE_URL|OLLAMA_MODEL|AZURE_OPENAI_ENDPOINT|AZURE_OPENAI_DEPLOYMENT|AZURE_OPENAI_API_KEY)
        # Unquote commonly copied example values.
        if [[ "$value" == \"*\" || "$value" == \'*\' ]]; then value="${value:1:${#value}-2}"; fi
        printf -v "$key" '%s' "$value"; continue ;;
    esac
  fi
  preserved+=("$line")
done < .env

random_hex() { od -An -N24 -tx1 /dev/urandom | tr -d '[:space:]'; }
invalid_token() {
  [[ -z "$1" || ${#1} -lt 16 || "$1" == REPLACE_* || "$1" == YOUR_* ]]
}
repaired=false
if invalid_token "${EMPLOYEE_API_TOKEN:-}"; then EMPLOYEE_API_TOKEN="$(random_hex)"; repaired=true; fi
if invalid_token "${REVIEWER_API_TOKEN:-}"; then REVIEWER_API_TOKEN="$(random_hex)"; repaired=true; fi
if [[ "$EMPLOYEE_API_TOKEN" == "$REVIEWER_API_TOKEN" ]]; then REVIEWER_API_TOKEN="$(random_hex)"; repaired=true; fi
SQLITE_PATH="${SQLITE_PATH:-./reviewflow.db}"
BIND_ADDR="${BIND_ADDR:-127.0.0.1:8080}"
AI_MODE="${AI_MODE:-rules}"
OLLAMA_BASE_URL="${OLLAMA_BASE_URL:-http://127.0.0.1:11434}"
OLLAMA_MODEL="${OLLAMA_MODEL:-qwen3:4b-instruct}"
if [[ "${RF_LOCAL_AI:-0}" == "1" ]]; then AI_MODE=ollama; fi

{
  for line in "${preserved[@]}"; do printf '%s\n' "$line"; done
  for key in "${keys[@]}"; do
    if [[ -n "${!key:-}" ]]; then printf '%s=%s\n' "$key" "${!key}"; fi
  done
} > .env.new
mv -f .env.new .env
chmod 600 .env || true
[[ "$repaired" == true ]] && echo '[ReviewFlow] Repaired missing or placeholder API tokens in .env.'
echo '[ReviewFlow] Local configuration ready.'
