#!/usr/bin/env bash
# End-to-end smoke test against a running server. Works with the review agent on or off.
set -euo pipefail
: "${REVIEWER_API_TOKEN:?set REVIEWER_API_TOKEN first}"
BASE="${BASE:-http://127.0.0.1:8080}"
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
AUTH="Authorization: Bearer $REVIEWER_API_TOKEN"
JSON='Content-Type: application/json'
field() { python3 -c "import sys,json;print(json.load(sys.stdin)[\"$1\"])"; }
status_of() { curl -fsS "$BASE/api/v1/reviews/$1" -H "$AUTH" | field status; }
AGENT="$(curl -fsS "$BASE/healthz" | python3 -c 'import sys,json;h=json.load(sys.stdin).get("agent",{});print("1" if h.get("enabled") and h.get("auto_deliver") else "0")')"
# Manual fallback for a step when the agent is off. Tolerates 409 (step already done).
step() { [[ "$AGENT" == 1 ]] && return 0; local code; code=$(curl -sS -o /dev/null -w '%{http_code}' "$@" -H "$AUTH"); [[ "$code" == 2* || "$code" == 409 ]] || { echo "Step failed: HTTP $code $*" >&2; exit 1; }; }
wait_for() { # id, space-separated acceptable statuses, seconds
  local s; for _ in $(seq 1 "$3"); do s=$(status_of "$1"); [[ " $2 " == *" $s "* ]] && { echo "$s"; return; }; sleep 1; done
  echo "Timed out waiting for $2 (last: $s)" >&2; exit 1
}

# 1. A Teams (Microsoft Forms) submission, exactly as the Power Automate flow sends it.
RESPONSE_ID="smoke-$(date +%s)-$RANDOM"
id="$(curl -fsS "$BASE/api/v1/intake/microsoft-forms" -X POST -H "$AUTH" -H "$JSON" -d @- <<EOF | field id
{"form_id":"smoke-form","response_id":"$RESPONSE_ID","responder":"smoke@example.com",
 "answers":{"Employee ID":"EMP-001","Review period":"2026",
  "Key achievements":"Reduced service response latency by 25%.\nMentored two engineers during onboarding.",
  "Goals for next year":"Improve observability and incident response.",
  "Development areas":"Develop stronger technical documentation habits."}}
EOF
)"
echo "Created review from Teams form: $id"

# 2. Summarize (agent does it on its own; the manual call is a fallback).
step -X POST "$BASE/api/v1/reviews/$id/analyze"
echo "Summarized: $(wait_for "$id" "EXTRACTED NEEDS_REVIEW" 300)"

# 3. Human approval.
curl -fsS -X PUT "$BASE/api/v1/reviews/$id/approve" -H "$AUTH" -H "$JSON" \
  -d '{"revised":null,"approved_by":"Smoke Test"}' >/dev/null
echo "Approved"

# 4. Stage and deliver (agent does both; manual calls are fallbacks).
step -X POST "$BASE/api/v1/reviews/$id/stage"
curl -fsS "$BASE/api/v1/workday/staging.csv" -H "$AUTH" -o "$DIR/smoke-staging.csv"
step -X POST "$BASE/api/v1/reviews/$id/deliver"
echo "Delivered: $(wait_for "$id" "MOCK_RECEIVED DELIVERED" 60)"

curl -fsS "$BASE/api/v1/reviews/$id/audit" -H "$AUTH"
echo -e "\nSuccess: Teams form -> summary -> approval -> Workday delivery (MOCK_RECEIVED = simulated receiver, DELIVERED = real tenant)"
