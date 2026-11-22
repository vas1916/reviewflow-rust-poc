#!/usr/bin/env bash
# ReviewFlow local POC launcher (macOS/Linux); detects Cargo, then Docker.
set -Eeuo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
ROOT="$(pwd)"

say() { printf '\n[ReviewFlow] %s\n' "$*"; }
failure() { printf '\n[ReviewFlow] ERROR: %s\n' "$*" >&2; exit 1; }

bash scripts/prepare-env.sh || failure 'Could not repair .env'
# Read only recognized keys, never source .env as executable shell script.
while IFS='=' read -r key value || [[ -n "${key:-}" ]]; do
  case "$key" in
    EMPLOYEE_API_TOKEN|REVIEWER_API_TOKEN|SQLITE_PATH|BIND_ADDR|AI_MODE|OLLAMA_BASE_URL|OLLAMA_MODEL|AZURE_OPENAI_ENDPOINT|AZURE_OPENAI_DEPLOYMENT|AZURE_OPENAI_API_KEY)
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

healthy() { command -v curl >/dev/null 2>&1 && curl --silent --fail --max-time 2 "$URL/healthz" 2>/dev/null | grep -q '"workday":"staging-only"'; }
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
if command -v cargo >/dev/null 2>&1; then
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
  say "Open $URL and paste REVIEWER_API_TOKEN from .env into the UI."
  say 'POC ONLY: Workday synchronization is simulated; do not use real employee information.'
  wait "$pid"
elif command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
  say 'Cargo unavailable; building in Docker instead.'
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
    "${extra_args[@]}" reviewflow-poc:local >/dev/null || failure 'Docker start failed'
  for _ in {1..90}; do if healthy; then break; fi; sleep 0.5; done
  healthy || failure 'Docker container not responding; inspect docker logs reviewflow-poc-local'
  open_browser
  say "Open $URL and paste REVIEWER_API_TOKEN from .env into the UI."
  say 'Stop Docker with: docker stop reviewflow-poc-local'
else
  failure 'Install either Rust (https://rustup.rs) or Docker Desktop, and rerun.'
fi
