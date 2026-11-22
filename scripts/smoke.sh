#!/usr/bin/env bash
set -euo pipefail
: "${REVIEWER_API_TOKEN:?set REVIEWER_API_TOKEN first}"
BASE="${BASE:-http://127.0.0.1:8080}"
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SOURCE="smoke-$(date +%s)-$RANDOM"
DATA="$(python3 - "$DIR/fixtures/sample-review.json" "$SOURCE" <<'PY'
import json,sys
obj=json.load(open(sys.argv[1]))
obj['source_id']=sys.argv[2]
print(json.dumps(obj))
PY
)"
res="$(curl -fsS "$BASE/api/v1/reviews" -X POST -H "Authorization: Bearer $REVIEWER_API_TOKEN" -H 'Content-Type: application/json' -d "$DATA")"
id="$(printf '%s' "$res" | python3 -c 'import sys,json;print(json.load(sys.stdin)["id"])')"
echo "Created review: $id"
curl -fsS -X POST "$BASE/api/v1/reviews/$id/analyze" -H "Authorization: Bearer $REVIEWER_API_TOKEN" >/dev/null
curl -fsS -X PUT "$BASE/api/v1/reviews/$id/approve" -H "Authorization: Bearer $REVIEWER_API_TOKEN" -H 'Content-Type: application/json' -d '{"revised":null}' >/dev/null
curl -fsS -X POST "$BASE/api/v1/reviews/$id/stage" -H "Authorization: Bearer $REVIEWER_API_TOKEN" >/dev/null
# Export BEFORE mock delivery, while item is in the STAGED queue.
curl -fsS "$BASE/api/v1/workday/staging.csv" -H "Authorization: Bearer $REVIEWER_API_TOKEN" -o "$DIR/smoke-staging.csv"
curl -fsS -X POST "$BASE/api/v1/reviews/$id/mock-deliver" -H "Authorization: Bearer $REVIEWER_API_TOKEN"
curl -fsS "$BASE/api/v1/reviews/$id/audit" -H "Authorization: Bearer $REVIEWER_API_TOKEN"
echo -e "\nSuccess: staged and simulated receipt locally; saved $DIR/smoke-staging.csv (NOT sent to Workday)"
