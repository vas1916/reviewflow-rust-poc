# Security and privacy

**Classification:** Employee performance reviews are confidential HR data, often containing personal data, manager commentary, and sensitive workplace information. **Do not run this POC on real reviews.**

## Implemented developer-only controls

- Shared access codes (employee, reviewer, optional intake), each 16+ characters and all different; role checks on every route. Used in the default `AUTH_MODE=token`.
- Default server binds to loopback `127.0.0.1`.
- `DefaultBodyLimit` caps request body at 128 KiB; review text capped at 20,000 characters.
- Basic allowed-character employee ID validation; idempotent source identifiers.
- SQL parameters in all queries; no arbitrary LLM SQL, APIs or tool calling.
- Deterministic state transitions and human approval before outbox staging.
- Literal source evidence checks for each AI-extracted item; response must parse as JSON.
- No performance rating generation. Nothing reaches Workday without a person's approval; the agent cannot approve.
- CSV formula injection neutralization in user-opened staging CSV.
- No raw review content in application tracing/audit events.
- Opt-in Azure OpenAI integration requires HTTPS to an `*.openai.azure.com` resource; Ollama is limited to localhost or `host.docker.internal`.
- Teams webhook limited to `*.logic.azure.com` / `*.powerplatform.com`; cards never contain review text.
- Workday endpoint limited to `*.workday.com` / `*.myworkday.com` over HTTPS (plain HTTP only to a localhost mock).
- No cross-origin access configured.

## Live-mode controls (v0.4)

- `AUTH_MODE=entra`: Microsoft Entra ID sign-in through Azure built-in authentication. The approver recorded in the history is the signed-in account. The shared reviewer token is not accepted in this mode.
- Managers (`ReviewFlow.Reviewer`) can only list, open and act on reviews where they are the manager named on the form. Other reviews return "not found". HR (`ReviewFlow.HR`) sees all, and only HR can export the CSV.
- **Only use `AUTH_MODE=entra` behind Azure Container Apps or App Service authentication.** The app trusts the `X-MS-CLIENT-PRINCIPAL` header, which only the platform can set. Never expose the container port directly.
- Workday delivery uses an integration system user over HTTPS to `*.workday.com` / `*.myworkday.com` only. Values are XML-escaped. A delivery whose outcome is unknown (timeout, crash mid-send) is never resent automatically.

## Mandatory before real employee data

1. **Identity and authorization:** Entra SSO/OIDC, user-specific `employee_id` derived server-side from trusted Workday-to-Entra mapping. Employees may only create/view their own review. Managers/HR can approve only assigned reviews; step-up approval and segregation of duties.
2. **Transport and storage:** TLS, restricted VNet/private endpoints, at-rest encryption (managed DB/TDE or SQLCipher), encrypted backups, DLP, secret rotation, managed identity, no human-readable sensitive telemetry.
3. **Access policy:** Review participants, HR/manager approval visibility, period closing policy, lawful basis and company HR notice, external vendor data processing approval, data residency and retention/deletion schedule.
4. **AI safety:** Data minimization; approved Azure region; no training on submitted reviews without authorization; documented prompts; prompt injection evals; hallucination testing; evidence ground truth; bias/fairness assessment; approval for any generated prose.
5. **Workday safeguards:** Scoped integration-system user and domains, tenant sandbox tests, stable SOAP WSDL/valid inbound EIB, no import of prohibited fields, review lifecycle validation, two-phase submission and reconciliation.
6. **Audit:** Real identity in approval event, immutable audit evidence or SIEM, versioning of before/after edits, anomaly detection, access logs, request correlation IDs and tamper-resistant storage.
7. **Lifecycle and resiliency:** Outbox leased jobs, exponential backoff, dead-letter queue, rate limits, backup and restore exercise, encryption/key loss plan, exception handling and alerts.
8. **AppSec:** Dependency scanning/SBOM, SAST, penetration test, CSRF defenses for cookie auth, Content Security Policy, WAF and request throttling, safe HTML rendering, accessibility review.
9. **Product governance:** Explicit review of impact on employees; no automated ratings or consequential employment recommendations; incident response and correction/appeal mechanism.

## Limitations and threats

- In `AUTH_MODE=token`, the shared reviewer token acts as HR: it sees all reviews and can approve any of them. Use `AUTH_MODE=entra` for anything beyond a demo.
- The demo API accepts an employee ID in request body; it is **not verified against the caller**.
- In `AUTH_MODE=token`, the approver name typed in the UI is recorded as "unverified". In `AUTH_MODE=entra` the signed-in account is recorded instead.
- Reviewer edits keep the employee's original quote as evidence, but the edited wording itself is not re-checked for meaning.
- The intake endpoint trusts the `Employee ID` and `Manager email` answers in the form. A wrong manager email routes the review to the wrong approver. In production, derive both from Entra or Workday instead.
- Teams cards contain no review text, but they do show employee IDs to everyone in the channel. Use a private channel limited to approvers.
- `TEAMS_WEBHOOK_URL` is a bearer secret (anyone with it can post to the channel). Keep it out of source control like the tokens.
- The intake token is shared with Power Automate. Store it in Key Vault or as a secure input, and rotate it with `INTAKE_API_TOKEN`.
- SQLite raw review text is **not encrypted by application code**; only synthetic data should be used.
- Model-generated summary is not fully entailed by simple quote matching; reviewer must verify summary.
- `STAGED` means queued in the local outbox. `MOCK_RECEIVED` / `MOCK_ACCEPTED_NOT_WORKDAY` mean the in-process simulator accepted it. **Only `DELIVERED` reflects a Workday response.**
- The Workday SOAP adapter has been tested against a local mock only. The request body comes from a tenant template that must be validated in a Workday sandbox.
- `DELIVERED` means Workday accepted the request and returned the configured receipt element. ReviewFlow does not yet re-query Workday to reconcile.

## Workday go-live checklist

- [ ] Tenant integration owner has provided WWS SOAP API version or inbound EIB template
- [ ] Correct employee self-review business process, current review template and section IDs validated
- [ ] Workday worker IDs/WIDs and periods mapped to employee identities
- [ ] Integration-system user permissions, tenant security policies and audit approved
- [ ] Confirmed submission semantics, idempotency and authoritative receipt/status
- [ ] Sandbox tests for partial failure, duplicate/retry, closed period and expired review
- [ ] HR data processing, security and AI governance signoff complete
