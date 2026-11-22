# Tradeoffs

What mattered most, in order: keeping HR data confidential, getting things right and being able to trace them, having few things to deploy, a small runtime footprint, and leaving room to scale later.

| Component | POC choice / why | Alternative | Tradeoff and when to switch |
|---|---|---|---|
| Review input | **Microsoft Forms in Teams, forwarded by Power Automate** (Graph has no supported Forms API); embedded UI + REST for tests | Teams Adaptive Card sent by a bot, or SharePoint form | Power Automate needs a Premium license and a reachable HTTPS endpoint, but keeps the familiar form. A bot card gives more control and in-Teams approval at the cost of a bot registration. Avoid blanket chat ingestion for HR reviews. |
| Application language | **Rust**; predictable memory, static native binary, lower GC overhead, safe concurrent code | **Go**; faster onboarding and builds; .NET 10/ASP.NET; deep MS ecosystem fit | Rust adds engineering/recruiting/compile cost; Go gives better team velocity if Rust skills scarce; .NET reduces Graph/Workday Enterprise integration friction if Microsoft-centric. For a light single deployment, both Rust and Go are credible. |
| HTTP server | Axum (Tokio); mature Rust ecosystem, async, middleware | Actix Web / Poem | Axum has straightforward ecosystem and modularity; Actix can be faster in specific benchmarks, not decisive at this scale. |
| Frontend | Embedded static HTML/JS; no Node/service | React/Vite SPA or Teams tab | Minimal POC delivery and zero frontend build versus richer approvals, accessibility, organizational auth and audit UX. **Move to Teams tab/SPA when HR workflows need it.** |
| Database | SQLite WAL; no second deployed process | PostgreSQL / Azure SQL | SQLite good for single instance and transactional outbox; constrained write concurrency and multi-instance deployment. **Use PostgreSQL for HA, multiple replicas, heavy audit loads.** |
| DB access | rusqlite with Tokio blocking pool; very small and predictable | sqlx async pool, Diesel | Each operation opens a SQLite connection and runs blocking work off the async executor. More overhead per request; acceptable for a POC. Use pool when profiling indicates need. |
| Orchestration | **In-process review agent** over a deterministic state machine; retries with backoff; wakes on API events | Temporal / Durable Functions / Camunda | Zero additional infra, but tied to one process: run a single replica. **Adopt a workflow engine only for long multi-step approval chains, escalations, or multi-replica deployment.** |
| Background messaging | SQLite transactional outbox; no broker | Azure Service Bus / RabbitMQ / Kafka | Cheapest deploy, transactional state+message persist; requires safe leasing for multi-worker dispatch. Add **Service Bus** for multi-service scale and resilient async delivery; Kafka only if true replay/stream scale needs it. |
| Extraction logic | Offline parser by default, Azure OpenAI opt-in | Local quantized model / Azure AI / rules only | Rules are cheap/private but miss complex prose. LLM handles unstructured reviews but adds latency, variable cost, hallucination and HR-data exposure. **Enable only with HR approval, evidence checks and model evaluation.** |
| Agent count | One bounded extraction agent + deterministic functions | Multi-agent CrewAI/AutoGen/LangGraph | Multi-agent increases latency, inference cost and nondeterministic behavior; for structured HR transactions it is usually unjustified. Consider multiple models only if measured precision improves. |
| Model hosting | Enterprise Azure OpenAI external API | Local llama.cpp/ONNX or self-hosted | Managed service reduces model serving deployment; local model improves data residency and offline behavior but adds GPU/CPU/ops and accuracy/latency tradeoffs. |
| Workday outbound | **SOAP adapter** (`WORKDAY_MODE=soap`) with a tenant-supplied request template; simulated receiver by default; generic CSV | Workday inbound EIB, Workday Orchestrate | Templates keep tenant specifics out of code; the envelope, auth, escaping and error handling are shared. EIB suits batch loads when a template exists (adapt the CSV export); Orchestrate reduces glue if the Workday platform investment already exists. |
| Mapping | Explicit WorkdayDraft + request template with placeholders | Dynamic self-describing ETL mappings | Static schema easy to test and govern; tenant mapping lives in the template file, approved and version-controlled, never LLM-created. |
| API authorization | `AUTH_MODE=token` (shared codes, demo) or **`AUTH_MODE=entra`** (Azure built-in authentication + app roles; managers see only their team) | In-app OIDC (MSAL + JWT validation) / mTLS | Easy Auth means no token handling in the app, but ties live sign-in to Azure App Service / Container Apps. Switch to in-app OIDC if hosting elsewhere. |
| Audit | SQLite append-only event rows | Immutable audit sink/WORM storage + SIEM | Simple and inspectable, but local administrator can tamper. Use immutable evidence archive and retention policies for HR compliance. |
| Secrets | Environment variables (POC) | Azure Key Vault / managed identity | No separate dependency locally. In production use platform identity, key rotation and secret access controls. |
| File storage | Local DB only | Encrypted object store for attachments | No attachment workflow in POC; future review evidence/attachments need virus scan, DLP and retention. |
| Monitoring | Basic tracing without payloads | OpenTelemetry + Azure Monitor | Small footprint vs centralized incident response and performance SLOs. Add correlated request IDs and privacy-safe metrics before production. |
| Deployment | Single binary or one Docker container | Azure Container Apps, VM, Kubernetes | One deployable is simpler. Containers help reproducibility; VM has smallest platform requirements; ACA offers managed identity/revisions; Kubernetes unjustified without existing platform need. |
| Scaling | Vertical single-instance | Horizontally scaled Axum + PostgreSQL + worker | Smallest operations footprint vs HA and peak campaign throughput. Split when SLO/availability or concurrency data justifies it. |

## Recommended stack by stage

- **Demo:** Rust/Axum, embedded static UI, SQLite WAL, rules or local Ollama, simulated Workday. **1 process, 1 DB file.**
- **Production v1 (minimal ops, supported today):** same binary on one Azure Container App replica, Entra built-in authentication, Azure Files for SQLite, Key Vault, Azure OpenAI, Workday SOAP adapter. **1 app deployment + 1 file share.** See [GO-LIVE.md](GO-LIVE.md).
- **Production v1.5 (if HA is required):** managed PostgreSQL instead of SQLite, agent as a single worker.
- **Production v2 (if volume justifies):** Same bounded modules, run API and worker as separate process roles, Service Bus, OpenTelemetry, Workday reconciliation, zero-trust boundaries.

## What I left out for now

- No vector database: individual reviews do not need semantic retrieval for simple extraction.
- No Kafka: there is no streaming analytics requirement yet.
- No Kubernetes: one Rust API plus database does not need an orchestrator.
- No ML model in the container: inference optional through an approved managed endpoint.
- No API gateway deployment in the POC: local auth suffices for synthetic data; production perimeter uses existing enterprise gateway if available.

Anything that talks to the outside world sits behind an adapter (`Processor`, `Store`, `WorkdayClient`, `Notifier`), so each of these choices can be swapped later without rewriting the rest. And AI output should never decide on its own what gets written to Workday.


## Local AI vs cloud AI (v0.2)

- **Local Ollama (`AI_MODE=ollama`)**: your review text is sent only to the Ollama HTTP service bound to your local machine, with approximately 2.5 GB model download and model runtime memory overhead. No hosted AI API cost, but slower inference on CPUs; another local service must run. Default model is `qwen3:4b-instruct`. Supports JSON schema-constrained output, backed by the same evidence validator and human approval as the Azure path.
- **Offline rules (`AI_MODE=rules`)**: lowest resource consumption, no second service, highly deterministic. It cannot infer robust summaries from unstructured reviews and requires explicit sections. This is the safest fallback for a lightweight demo without a GPU.
- **Azure OpenAI (`AI_MODE=azure`)**: external managed inference and potentially better quality, but HR data egress, enterprise approvals, tenancy controls, cost, and latency must be assessed. Not enabled by default.
- **Alternative (embedded llama.cpp / Candle)**: potentially true single-binary local inference but significantly increases Rust build/runtime complexity, RAM needs, platform packaging and hardware compatibility. Defer until the product requires fully bundled offline model weights.

### Local launcher tradeoff
There is one launcher per system (`Run-ReviewFlow.cmd`, `Run-ReviewFlow.sh`, and `Run-ReviewFlow.command` as the macOS double-click wrapper) with a mode menu, instead of one file per mode. On Windows the `.env` repair lives in its own PowerShell script, which is easier to get right and test than doing it in a `.cmd` file, at the cost of one extra file. The launcher repairs only developer API secrets and preserves all other settings. It does not provision Entra sign-in or Workday credentials; those come from [GO-LIVE.md](GO-LIVE.md).
