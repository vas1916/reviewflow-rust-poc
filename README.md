# ReviewFlow

A small proof of concept for automating employee self-reviews: an employee submits a review, the app pulls out the key points, a reviewer checks and approves them, and the approved result is staged for Workday.

It's a single Rust binary (Axum, SQLite, and a plain HTML/JS page served by the app itself). Everything runs on your machine.

The flow looks like this:

```
submit → extract key points → check each point against the original text → reviewer approves → stage → export CSV → simulated Workday receipt
```

A few things to know before you try it:

- There's no real Microsoft Teams or Workday connection. The "Workday receipt" comes from a fake receiver inside the app, so you can see the whole round trip without a Workday tenant.
- The CSV export is a generic format I made up for staging. It isn't an approved Workday EIB template.
- Please use made-up reviews only. The security here is good enough for a demo, not for real HR data. See [docs/SECURITY.md](docs/SECURITY.md) for what would need to change.

## Running it

You'll need either Rust (recommended) or Docker Desktop. If you want the local AI mode, you'll also need [Ollama](https://ollama.com/download).

Pick the launcher for your system and mode:

| Mode | Windows | macOS | Linux |
|---|---|---|---|
| Rules only, no AI model | `Run-ReviewFlow.cmd` | `Run-ReviewFlow.command` | `bash Run-ReviewFlow.sh` |
| Local AI with Ollama | `Run-ReviewFlow-LocalAI.cmd` | `Run-ReviewFlow-LocalAI.command` | `bash Run-ReviewFlow-LocalAI.sh` |

There are `.bat` versions of the Windows launchers too, if you prefer those.

Each launcher:

1. Creates or fixes your `.env` file. If the API tokens are missing, still set to the example values, too short, or the same as each other, it generates new ones. Your other settings are left alone, and tokens are never printed to the terminal.
2. Starts Ollama, if you picked the AI mode.
3. Builds and starts the server.
4. Opens http://127.0.0.1:8080 in your browser.

Once the page is open, copy `REVIEWER_API_TOKEN` from your `.env` file into the token box. Keep `.env` to yourself; it's already in `.gitignore`.

If you switch modes while an old server is still running in another window, close it first, otherwise the new mode won't take effect.

### Local AI mode

The AI launcher downloads `qwen3:4b-instruct` the first time (about 2.5 GB). After that, everything runs offline. It can be slow on a laptop without a GPU. To use a different model, run `ollama pull <model>` and change `OLLAMA_MODEL` in `.env`.

A couple of things I did to keep the AI honest:

- Every point it extracts has to quote the employee's own words, and the app checks that the quote really appears in the review. Anything it can't back up gets rejected.
- A person still has to approve the result. The AI never rates or grades anyone.
- The app only talks to Ollama on your own machine (localhost or `host.docker.internal`), so review text can't be sent somewhere else by accident.

The easiest setup is Rust plus Ollama installed directly. Docker plus Ollama works too, but getting the container to reach Ollama on the host can take some extra fiddling depending on your OS.

### Windows: "linker `link.exe` not found"

This means Rust is installed but the Microsoft C++ build tools aren't. The code itself is fine. You have two ways around it:

- **Install the build tools (best for the AI mode).** Get [Visual Studio Build Tools](https://visualstudio.microsoft.com/downloads/), and in the installer tick "Desktop development with C++" (make sure the MSVC build tools and a Windows SDK are included). Then run the launcher again. It finds the build tools on its own.
- **Use Docker instead.** Start [Docker Desktop](https://www.docker.com/products/docker-desktop/) in Linux containers mode and run `Run-ReviewFlow-Docker.bat`. Rust gets built inside Linux, so you don't need `link.exe` at all. This is meant for rules mode; the AI mode needs extra network setup under Docker.

Please don't download a stray `link.exe` from the internet, and don't switch to the GNU Rust target unless you set up a full MinGW toolchain with it.

## Running it by hand

```bash
cd reviewflow-rust-poc
cp .env.example .env
# Put two different random tokens in .env, at least 16 characters each
set -a; source .env; set +a
cargo run
```

Open http://127.0.0.1:8080, paste your reviewer token, and walk through it: submit the sample review, analyze it, look at the evidence, approve, stage, export, and finally click "Simulate Workday receipt".

The default `AI_MODE=rules` doesn't use any AI. It just picks up bullet points under section headings, which is enough to try out the whole flow. You can also point it at Azure OpenAI with `AI_MODE=azure`, but only do that if your company has approved sending HR data there.

### With Docker

```bash
docker build -t reviewflow-poc .
docker run --rm \
  -p 127.0.0.1:8080:8080 \
  -v reviewflow-data:/data \
  -e EMPLOYEE_API_TOKEN="<your employee token>" \
  -e REVIEWER_API_TOKEN="<your reviewer token>" \
  reviewflow-poc
```

Use tokens you've saved somewhere, since you'll need the reviewer one to log in to the page.

## Trying the API

```bash
export BASE=http://127.0.0.1:8080
export TOKEN="$REVIEWER_API_TOKEN"

curl -s -X POST "$BASE/api/v1/reviews" \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  --data-binary @fixtures/sample-review.json

# Use the id from the response above as $ID
curl -s -X POST "$BASE/api/v1/reviews/$ID/analyze" -H "Authorization: Bearer $TOKEN"
curl -s -X PUT "$BASE/api/v1/reviews/$ID/approve" -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' -d '{"revised":null}'
curl -s -X POST "$BASE/api/v1/reviews/$ID/stage" -H "Authorization: Bearer $TOKEN"
curl -s -X POST "$BASE/api/v1/reviews/$ID/mock-deliver" -H "Authorization: Bearer $TOKEN"
curl -s "$BASE/api/v1/reviews/$ID/audit" -H "Authorization: Bearer $TOKEN"
curl -s "$BASE/api/v1/workday/staging.csv" -H "Authorization: Bearer $TOKEN" -o staging.csv
```

Submitting the same review twice (same `source_system` and `source_id`) gives you back the original instead of a duplicate. Staging twice is also safe. The fake receiver answers with `MOCK_ACCEPTED_NOT_WORKDAY` so nobody mistakes it for a real Workday confirmation.

## What's where

```text
src/main.rs        Routes, token checks, serves the web page
src/domain.rs      Data types and validation
src/ai.rs          Rules, Ollama and Azure OpenAI extraction
src/db.rs          SQLite storage, audit log, outbox
src/workday.rs     Workday mapping and the CSV export
static/            The web page (no build step, no Node)
fixtures/          A made-up sample review
scripts/           .env setup helpers and a smoke test
docs/              Architecture, tradeoffs and security notes
```

## What it should do

1. Accept a review with a unique source ID.
2. Return the original if the same review is sent again.
3. Extract key points with rules, Ollama or Azure OpenAI.
4. Reject any extracted quote that isn't in the original text.
5. Refuse to approve before extraction, or stage before approval.
6. Stage an approved review once, with a fixed JSON copy and its SHA-256 hash.
7. Produce a CSV that's safe to open in Excel (formulas get neutralized).
8. Mark a review `MOCK_RECEIVED` after the simulated Workday receipt.
9. Keep the review text out of the audit log.
10. Never show a review as imported into Workday without a real receipt from Workday.

## What's missing on purpose

- No Teams, SharePoint, Forms or Workday connections. The web page and the REST API are the only ways in.
- No real sign-in. There are two shared tokens (employee and reviewer), which is fine for a demo and nothing more.
- The page can't edit or reject a review yet. You can send edits through the approve endpoint as `revised` JSON.
- No background retries, no real Workday calls, no database encryption, no high availability.
- SQLite works for one instance on one machine. Running several copies against the same file won't work well.

Before deploying anywhere, run `cargo fmt --check && cargo test && cargo clippy -- -D warnings`.

## More detail

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): how it's put together, with diagrams
- [docs/TRADEOFFS.md](docs/TRADEOFFS.md): why each piece was chosen and when I'd swap it out
- [docs/SECURITY.md](docs/SECURITY.md): what's protected now and what has to happen before real data
