#!/usr/bin/env bash
# ReviewFlow local launcher (macOS/Linux); detects Cargo, then Docker.
# Usage: bash Run-ReviewFlow.sh [env|local-ai|docker]   (no argument in a terminal shows a menu)
set -Eeuo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
ROOT="$(pwd)"

say() { printf '\n[ReviewFlow] %s\n' "$*"; }
failure() { printf '\n[ReviewFlow] ERROR: %s\n' "$*" >&2; exit 1; }

mode="${1:-}"
if [[ -z "$mode" && "${RF_LOCAL_AI:-0}" != 1 && "${RF_FORCE_DOCKER:-0}" != 1 && -t 0 ]]; then
  printf '\nHow do you want to run ReviewFlow?\n'
  printf '  1. Use the settings in .env (default)\n'
  printf '  2. Local AI with Ollama (switches .env to AI_MODE=ollama)\n'
  printf '  3. Docker (no Rust toolchain needed)\n'
  read -r -t 20 -p 'Press 1, 2 or 3 [1]: ' choice || true
  case "${choice:-1}" in 2) mode=local-ai ;; 3) mode=docker ;; *) mode=env ;; esac
fi
case "${mode:-env}" in
  env) ;;
  local-ai) export RF_LOCAL_AI=1 ;;
  docker) export RF_FORCE_DOCKER=1 ;;
  *) failure "Unknown option '$mode'. Use: Run-ReviewFlow.sh [env|local-ai|docker]" ;;
esac

bash scripts/prepare-env.sh || failure 'Could not repair .env'
# Read only recognized keys, never source .env as executable shell script.
while IFS='=' read -r key value || [[ -n "${key:-}" ]]; do
  case "$key" in
    EMPLOYEE_API_TOKEN|REVIEWER_API_TOKEN|SQLITE_PATH|BIND_ADDR|AI_MODE|OLLAMA_BASE_URL|OLLAMA_MODEL|AZURE_OPENAI_ENDPOINT|AZURE_OPENAI_DEPLOYMENT|AZURE_OPENAI_API_KEY|INTAKE_API_TOKEN|AGENT_ENABLED|AGENT_POLL_SECONDS|AGENT_MAX_ATTEMPTS|AGENT_AUTO_DELIVER|TEAMS_WEBHOOK_URL|PUBLIC_BASE_URL|FORMS_DEFAULT_TEMPLATE|FORMS_DEFAULT_PERIOD|AUTH_MODE|ENTRA_REVIEWER_ROLE|ENTRA_HR_ROLE|WORKDAY_MODE|WORKDAY_ENDPOINT|WORKDAY_USERNAME|WORKDAY_PASSWORD|WORKDAY_REQUEST_TEMPLATE|WORKDAY_RECEIPT_ELEMENT|WORKDAY_TIMEOUT_SECONDS|AZURE_OPENAI_API_VERSION|SQLITE_JOURNAL_MODE)
      value="${value%$'\r'}"
      export "$key=$value" ;;
  esac
done < .env
[[ -n "${EMPLOYEE_API_TOKEN:-}" && -n "${REVIEWER_API_TOKEN:-}" ]] || failure 'API tokens missing after initialization'
[[ "$EMPLOYEE_API_TOKEN" != "$REVIEWER_API_TOKEN" ]] || failure 'API tokens must differ'
BIND_ADDR="${BIND_ADDR:-127.0.0.1:8080}"
PORT="${BIND_ADDR##*:}"
[[ "$PORT" =~ ^[0-9]{1,5}$ ]] || failure 'Invalid BIND_ADDR; expected 127.0.0.1:8080'
URL="http://127.0.0.1:$PORT"

healthy() { command -v curl >/dev/null 2>&1 && curl --silent --fail --max-time 2 "$URL/healthz" 2>/dev/null | grep -q '"status":"ok"'; }
ollama_healthy() { command -v curl >/dev/null 2>&1 && curl --silent --fail --max-time 2 'http://127.0.0.1:11434/api/tags' >/dev/null 2>&1; }
open_browser() {
  if command -v open >/dev/null 2>&1; then open "$URL" >/dev/null 2>&1 || true
  elif command -v xdg-open >/dev/null 2>&1; then xdg-open "$URL" >/dev/null 2>&1 || true; fi
}

if [[ "${AI_MODE:-rules}" == ollama ]]; then
  command -v ollama >/dev/null 2>&1 || failure 'Ollama not installed. Install https://ollama.com/download first.'
  say "Enabling local AI with ${OLLAMA_MODEL:-qwen3:4b-instruct}"
  if ! ollama_healthy; then
    say 'Starting Ollama background service...'
    ollama serve > "$ROOT/ollama-local.log" 2>&1 &
  fi
  for _ in {1..40}; do if ollama_healthy; then break; fi; sleep 1; done
  ollama_healthy || failure 'Ollama did not respond at http://127.0.0.1:11434'
  if ! ollama list | grep -Fq "${OLLAMA_MODEL:-qwen3:4b-instruct}"; then
    say 'Pulling local model (first run downloads roughly 2.5 GB)...'
    ollama pull "${OLLAMA_MODEL:-qwen3:4b-instruct}" || failure 'Ollama model download failed'
  fi
fi

if healthy; then
  current_mode="$(curl --silent --fail --max-time 2 "$URL/healthz" | sed -n 's/.*"ai_mode":"\([^"]*\)".*/\1/p')"
  [[ "$current_mode" == "${AI_MODE:-rules}" ]] || failure "Another ReviewFlow instance is running with AI_MODE=${current_mode:-unknown}. Stop it and relaunch to switch modes."
  say "A local ReviewFlow instance is already available at $URL."
  open_browser
  exit 0
fi

if ! command -v cargo >/dev/null 2>&1 && [[ -x "$HOME/.cargo/bin/cargo" ]]; then export PATH="$HOME/.cargo/bin:$PATH"; fi
sign_in_note() {
  if [[ "${AUTH_MODE:-token}" == entra ]]; then say 'Note: AUTH_MODE=entra needs Azure sign-in in front of the app. For local use set AUTH_MODE=token.'
  else say "Open $URL and sign in with REVIEWER_API_TOKEN from .env."; fi
  if [[ "${WORKDAY_MODE:-simulated}" == soap ]]; then say 'Workday: LIVE. Approved reviews are sent to WORKDAY_ENDPOINT.'
  else say 'Workday: simulated. Use made-up reviews only.'; fi
}

if [[ "${RF_FORCE_DOCKER:-0}" != 1 ]] && command -v cargo >/dev/null 2>&1; then
  say 'Building the Rust app (first run downloads dependencies)...'
  cargo build || failure 'Rust build failed; review the output above'
  say 'Starting Rust application; Ctrl+C stops the application.'
  "$ROOT/target/debug/reviewflow" &
  pid=$!
  trap 'kill "$pid" 2>/dev/null || true' EXIT INT TERM
  for _ in {1..90}; do
    if healthy; then break; fi
    if ! kill -0 "$pid" 2>/dev/null; then failure 'Server exited before it was ready'; fi
    sleep 0.5
  done
  healthy || failure 'Server did not respond at expected URL'
  open_browser
  sign_in_note
  wait "$pid"
elif command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
  say 'Building in Docker (Linux toolchain).'
  docker build -t reviewflow-poc:local . || failure 'Docker build failed'
  docker container inspect reviewflow-poc-local >/dev/null 2>&1 && docker stop reviewflow-poc-local >/dev/null || true
  extra_args=()
  if [[ "${AI_MODE:-rules}" == ollama ]]; then
    say 'Docker + Ollama may require host connectivity configuration; native Cargo is recommended.'
    extra_args=(-e OLLAMA_BASE_URL=http://host.docker.internal:11434 --add-host=host.docker.internal:host-gateway)
  fi
  docker run -d --rm --name reviewflow-poc-local \
    -p "127.0.0.1:$PORT:8080" -v reviewflow-poc-data:/data --env-file .env \
    -e SQLITE_PATH=/data/reviewflow.db -e BIND_ADDR=0.0.0.0:8080 \
    -e WORKDAY_REQUEST_TEMPLATE=/app/workday/request-template.xml \
    "${extra_args[@]}" reviewflow-poc:local >/dev/null || failure 'Docker start failed'
  for _ in {1..90}; do if healthy; then break; fi; sleep 0.5; done
  healthy || failure 'Docker container not responding; inspect docker logs reviewflow-poc-local'
  open_browser
  sign_in_note
  say 'Stop Docker with: docker stop reviewflow-poc-local'
elif [[ "${RF_FORCE_DOCKER:-0}" == 1 ]]; then
  failure 'Docker mode needs Docker Desktop installed and running.'
else
  failure 'Install either Rust (https://rustup.rs) or Docker Desktop, and rerun.'
fi
