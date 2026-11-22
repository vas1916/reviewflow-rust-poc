# ReviewFlow

A proof of concept for an HR review assistant. Employees submit self-review forms in Microsoft Teams. A review agent summarizes each one and asks the employee's manager to approve it. Once approved, the agent sends the result to Workday.

It's a single Rust binary (Axum, SQLite, and a plain HTML/JS page served by the app itself). It runs on a laptop for demos, and on Azure Container Apps for live use.

The flow looks like this:

```
Teams form ──Power Automate──► intake ──► agent summarizes and checks every point against the employee's words
   ──► Teams card to the approver ──► manager edits / approves / sends back ──► agent stages and delivers to Workday
```

The agent runs every step except the decision. A person must approve, edit or send back each summary, and the AI never rates anyone. Failed steps are retried automatically with backoff, then flagged for HR on the dashboard.

A few things to know before you try it:

- **Teams intake works through a Power Automate flow.** Microsoft Graph has no supported API for reading Forms responses. See [docs/TEAMS-INTAKE.md](docs/TEAMS-INTAKE.md) for the flow, the webhook for Teams notification cards, and how to test without Power Automate.
- **Workday delivery is simulated by default.** `WORKDAY_MODE=soap` switches to Workday Web Services once your Workday team provides an integration user and a request template for your tenant (see [Going live](#going-live)).
- The CSV export is a generic staging format. It isn't an approved Workday EIB template.
- Use made-up reviews until the items in [docs/SECURITY.md](docs/SECURITY.md) and [docs/GO-LIVE.md](docs/GO-LIVE.md) are done.

## Running it

You'll need either Rust (recommended) or Docker Desktop. For the local AI mode you'll also need [Ollama](https://ollama.com/download).

There is one launcher per system:

| System | Launcher |
|---|---|
| Windows | Double-click `Run-ReviewFlow.cmd` |
| macOS | Double-click `Run-ReviewFlow.command` |
| Linux | `bash Run-ReviewFlow.sh` |

It asks how you want to run ReviewFlow:

1. **Use the settings in `.env`** (default, chosen automatically after 20 seconds)
2. **Local AI with Ollama.** Switches `.env` to `AI_MODE=ollama`, starts Ollama and downloads the model the first time.
3. **Docker.** Builds inside a Linux container, so no Rust or C++ build tools are needed.

To skip the menu, pass the mode: `Run-ReviewFlow.cmd local-ai`, `bash Run-ReviewFlow.sh docker`, or `env`.

The launcher then:

1. Creates or fixes your `.env` file. If the API tokens are missing, still set to the example values, too short, or the same as each other, it generates new ones. Your other settings are left alone, and tokens are never printed to the terminal.
2. Starts Ollama, if you picked local AI.
3. Builds and starts the server.
4. Opens http://127.0.0.1:8080 in your browser.

Sign in with `REVIEWER_API_TOKEN` from your `.env` file. Keep `.env` to yourself; it's in `.gitignore`.

If you switch modes while an old server is still running in another window, close it first, otherwise the new mode won't take effect.

### Local AI mode

The first run downloads `qwen3:4b-instruct` (about 2.5 GB). After that, everything runs offline. It takes about 30 seconds per review on a laptop without a GPU. To use a different model, run `ollama pull <model>` and change `OLLAMA_MODEL` in `.env`.

How the AI is kept honest:

- Every point it extracts has to quote the employee's own words, and the app checks that the quote really appears in the review. Anything it can't back up is rejected.
- The summary paragraph is *not* checked this way, so the approver must read it against the original (one click away on the review page).
- A person still has to approve the result. The AI never rates or grades anyone.
- The app only talks to Ollama on your own machine (localhost or `host.docker.internal`), so review text can't be sent somewhere else by accident.

The easiest setup is Rust plus Ollama installed directly. Docker plus Ollama works too, but getting the container to reach Ollama on the host can take extra setup depending on your OS.

### Windows: build tool errors

**"linker `link.exe` not found"** means Rust is installed but the Microsoft C++ build tools aren't. Either:

- **Install the build tools.** Get [Visual Studio Build Tools](https://visualstudio.microsoft.com/downloads/) and tick "Desktop development with C++" (MSVC build tools and a Windows SDK). The launcher finds them on its own.
- **Use Docker.** Start [Docker Desktop](https://www.docker.com/products/docker-desktop/) in Linux containers mode and choose **3** in the launcher. This suits rules mode; local AI needs extra network setup under Docker.

**ARM64 Windows (Snapdragon, Surface Pro X):** "unresolved external symbol memcpy" or "failed to find tool clang" means two more Build Tools components are needed: **MSVC ARM64 build tools** and **C++ Clang tools for Windows**. The launcher then sets up the ARM64 environment and puts Visual Studio's clang on PATH automatically.

Don't download a stray `link.exe` from the internet, and don't switch to the GNU Rust target unless you set up a full MinGW toolchain with it.

## Running it by hand

```bash
cd reviewflow-rust-poc
cp .env.example .env
# Put two different random tokens in .env, at least 16 characters each
set -a; source .env; set +a
cargo run                       # or: cargo run -- --check-config
```

Open http://127.0.0.1:8080 and sign in with your reviewer token. Open **Add a test review** and click **Submit sample Teams form**. Within a few seconds the agent summarizes it. Check the points against the employee's quotes, reword or remove any you disagree with, enter your name, and click **Approve and send to Workday**. The agent stages it and delivers it to the simulated receiver. Every step appears in the review's history.

To run every step by hand instead, set `AGENT_ENABLED=false`. The review page then shows manual buttons for each step.

The default `AI_MODE=rules` doesn't use any AI. It picks up the lines under section headings (Achievements, Goals, Improvements), which is enough to try the whole flow.

### With Docker

```bash
docker build -t reviewflow-poc .
docker run --rm -p 127.0.0.1:8080:8080 -v reviewflow-data:/data --env-file .env \
  -e SQLITE_PATH=/data/reviewflow.db -e BIND_ADDR=0.0.0.0:8080 reviewflow-poc
```

## Settings

All settings are environment variables, usually kept in `.env`. [.env.example](.env.example) covers local use and [.env.production.example](.env.production.example) covers live use. `reviewflow --check-config` validates them and shows which components are still in demo mode.

| Setting | Default | What it does |
|---|---|---|
| `EMPLOYEE_API_TOKEN`, `REVIEWER_API_TOKEN` | *(required)* | Shared access codes, 16+ characters, all different. The reviewer code signs in to the web page in token mode. |
| `INTAKE_API_TOKEN` | *(optional)* | Separate code for the Power Automate flow, so it can be rotated on its own. |
| `AUTH_MODE` | `token` | `entra` = Microsoft sign-in through Azure built-in authentication. Managers see only their team; HR sees all. |
| `ENTRA_REVIEWER_ROLE`, `ENTRA_HR_ROLE` | `ReviewFlow.Reviewer`, `ReviewFlow.HR` | Entra app roles for managers and HR. |
| `AI_MODE` | `rules` | `ollama` (local) or `azure` (Azure OpenAI). |
| `OLLAMA_BASE_URL`, `OLLAMA_MODEL` | `http://127.0.0.1:11434`, `qwen3:4b-instruct` | Local AI. The URL must be localhost or `host.docker.internal`. |
| `AZURE_OPENAI_ENDPOINT`, `_DEPLOYMENT`, `_API_KEY`, `_API_VERSION` | version `2024-10-21` | Azure OpenAI. The endpoint must be `https://*.openai.azure.com`. |
| `WORKDAY_MODE` | `simulated` | `soap` = Workday Web Services. |
| `WORKDAY_ENDPOINT`, `WORKDAY_USERNAME`, `WORKDAY_PASSWORD` | | Service URL (`*.workday.com` or `*.myworkday.com`) and integration system user. |
| `WORKDAY_REQUEST_TEMPLATE` | `./workday/request-template.xml` | SOAP request body for your tenant. See [workday/request-template.example.xml](workday/request-template.example.xml). |
| `WORKDAY_RECEIPT_ELEMENT`, `WORKDAY_TIMEOUT_SECONDS` | `ID`, `60` | Response element holding Workday's reference; request timeout. |
| `TEAMS_WEBHOOK_URL` | *(off)* | Teams Workflows webhook for approval cards. |
| `PUBLIC_BASE_URL` | `http://127.0.0.1:8080` | Address used in Teams card links. |
| `FORMS_DEFAULT_TEMPLATE`, `FORMS_DEFAULT_PERIOD` | `ANNUAL_REVIEW`, *(none)* | Used when the form doesn't ask for them. |
| `AGENT_ENABLED`, `AGENT_AUTO_DELIVER` | `true`, `true` | Run the workflow automatically; send to Workday right after approval. |
| `AGENT_POLL_SECONDS`, `AGENT_MAX_ATTEMPTS` | `10`, `3` | How often the agent checks; automatic attempts before HR is asked to retry. |
| `SQLITE_PATH`, `SQLITE_JOURNAL_MODE` | `./reviewflow.db`, `WAL` | Use `DELETE` on network shares such as Azure Files. |
| `BIND_ADDR` | `127.0.0.1:8080` | Listen address. |

## Trying the API

With the agent on (the default), you only need intake and approval. The agent does the rest:

```bash
export BASE=http://127.0.0.1:8080
export TOKEN="$REVIEWER_API_TOKEN"

# What the Power Automate flow sends for each Teams form response
curl -s -X POST "$BASE/api/v1/intake/microsoft-forms" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  --data-binary @fixtures/sample-teams-form.json

# After the agent has summarized it (status EXTRACTED or NEEDS_REVIEW):
curl -s -X PUT "$BASE/api/v1/reviews/$ID/approve" -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' -d '{"revised":null,"approved_by":"Priya Shah"}'
# ...or send it back to the employee:
curl -s -X POST "$BASE/api/v1/reviews/$ID/send-back" -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' -d '{"reason":"Please add measurable goals"}'
# Put a failed review back in the queue:
curl -s -X POST "$BASE/api/v1/reviews/$ID/retry" -H "Authorization: Bearer $TOKEN"
```

With `AGENT_ENABLED=false`, every step is a manual call:

```bash
curl -s -X POST "$BASE/api/v1/reviews" -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' --data-binary @fixtures/sample-review.json

# Use the id from the response above as $ID
curl -s -X POST "$BASE/api/v1/reviews/$ID/analyze" -H "Authorization: Bearer $TOKEN"
curl -s -X PUT "$BASE/api/v1/reviews/$ID/approve" -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' -d '{"revised":null}'
curl -s -X POST "$BASE/api/v1/reviews/$ID/stage" -H "Authorization: Bearer $TOKEN"
curl -s "$BASE/api/v1/workday/staging.csv" -H "Authorization: Bearer $TOKEN" -o staging.csv
curl -s -X POST "$BASE/api/v1/reviews/$ID/deliver" -H "Authorization: Bearer $TOKEN"
curl -s "$BASE/api/v1/reviews/$ID/audit" -H "Authorization: Bearer $TOKEN"
```

Submitting the same review twice (same `source_system` and `source_id`) returns the original instead of creating a duplicate. Staging and delivering twice are also safe. The simulated receiver answers with `MOCK_ACCEPTED_NOT_WORKDAY`, so nobody mistakes it for a real Workday confirmation. `bash scripts/smoke.sh` runs the whole path end to end.

## What's where

```text
src/main.rs        Routes, sign-in checks, --check-config, serves the web page, starts the agent
src/auth.rs        Shared-token or Entra ID (Easy Auth) sign-in, manager/HR access rules
src/agent.rs       Review agent: summarize, notify, stage, deliver, retry with backoff
src/intake.rs      Microsoft Forms (Teams) response -> review mapping
src/ai.rs          Rules, Ollama and Azure OpenAI extraction
src/domain.rs      Data types, statuses and validation
src/db.rs          SQLite storage, audit log, outbox, retry bookkeeping, schema upgrades
src/delivery.rs    Claim -> send -> record for one Workday delivery
src/workday.rs     Workday mapping, simulated + SOAP delivery, CSV export
src/teams.rs       Teams notification cards via a Workflows webhook
static/            The web page (no build step, no Node)
workday/           Workday SOAP request template (example; your Workday team fills it in)
fixtures/          Made-up sample review and Teams form response
scripts/           .env setup helpers and the smoke test
docs/              Architecture, go-live, Teams intake, tradeoffs and security notes
```

## What it does

1. Accepts Teams form responses from Power Automate, and ignores Power Automate retries.
2. Accepts a review with a unique source ID, and returns the original if the same review is sent again.
3. Extracts key points with rules, Ollama or Azure OpenAI, and rejects any quote that isn't in the original text.
4. Summarizes, stages and delivers without anyone clicking, but never approves without a person.
5. Lets the approver reword or remove points before approving, keeping each employee quote attached.
6. Sends a review back to the employee with a reason.
7. Refuses to approve before extraction, or to stage before approval.
8. Stages an approved review once, with a fixed JSON copy and its SHA-256 hash.
9. Delivers each review at most once, even if the agent and a person try at the same time.
10. Retries failed AI and delivery steps with backoff, then marks the review for HR's attention.
11. Never resends a delivery whose outcome is unknown (timeout, crash mid-send); HR checks Workday first.
12. Marks a review `DELIVERED` with Workday's reference, or `MOCK_RECEIVED` with the simulated receiver.
13. With `AUTH_MODE=entra`, shows managers only their team's reviews and records the signed-in approver.
14. Produces a CSV that's safe to open in Excel (formulas are neutralized).
15. Keeps review text out of the audit log and out of Teams cards.

## Limitations

- The Workday SOAP adapter is tested against a local mock only. It needs your tenant's request template and a sandbox test before real use.
- The manager comes from a "Manager email" question on the form, not from a Workday or Entra lookup.
- In the default `AUTH_MODE=token`, access codes are shared and the approver name is typed in. Use `AUTH_MODE=entra` for anything beyond a demo.
- When a review is sent back, the employee isn't notified automatically. Add a Teams message step to the Power Automate flow.
- SQLite and the in-process agent assume one running instance. High availability needs PostgreSQL.
- No database encryption beyond what the hosting platform provides.

## Going live

Going live is configuration, not code. Copy [.env.production.example](.env.production.example), fill in each section with the team that owns it, and check it with `reviewflow --check-config`. [docs/GO-LIVE.md](docs/GO-LIVE.md) has the questions to ask each team, the order to switch things on, Azure deployment commands and a test checklist.

| Setting | Demo | Live |
|---|---|---|
| `AUTH_MODE` | `token` (shared access codes) | `entra` (Microsoft sign-in; managers see only their team) |
| `WORKDAY_MODE` | `simulated` | `soap` (Workday Web Services, integration user, tenant request template) |
| `AI_MODE` | `rules` / `ollama` | `azure` |
| `TEAMS_WEBHOOK_URL` | empty | approvers' channel webhook |

Before deploying, run `cargo fmt --check && cargo test && cargo clippy -- -D warnings`.

## More detail

- [docs/GO-LIVE.md](docs/GO-LIVE.md): what each team provides, questions to ask, deployment and test checklist
- [docs/TEAMS-INTAKE.md](docs/TEAMS-INTAKE.md): Power Automate flow and Teams notification setup
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): how it's put together, with diagrams
- [docs/TRADEOFFS.md](docs/TRADEOFFS.md): why each piece was chosen and when to swap it out
- [docs/SECURITY.md](docs/SECURITY.md): what's protected now and what has to happen before real data
