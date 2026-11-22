# Security and privacy

**Classification:** Employee performance reviews are confidential HR data, often containing personal data, manager commentary, and sensitive workplace information. **Do not run this POC on real reviews.**

## Implemented developer-only controls

- Two distinct API tokens with >=16-character requirement; role checks for mutation/read routes.
- Default server binds to loopback `127.0.0.1`.
- `DefaultBodyLimit` caps request body at 128 KiB; review text capped at 20,000 characters.
- Basic allowed-character employee ID validation; idempotent source identifiers.
- SQL parameters in all queries; no arbitrary LLM SQL, APIs or tool calling.
- Deterministic state transitions and human approval before outbox staging.
- Literal source evidence checks for each AI-extracted item; response must parse as JSON.
- No performance rating generation or autonomous Workday write.
- CSV formula injection neutralization in user-opened staging CSV.
- No raw review content in application tracing/audit events.
- Opt-in Azure OpenAI integration requires HTTPS to an `*.openai.azure.com` resource.
- No cross-origin access configured.

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

## POC limitations and threats

- The shared reviewer token can view all reviews and approve its own submission. This is intentionally **not enterprise RBAC**.
- The demo API accepts an employee ID in request body; it is **not verified against the caller**.
- The UI's approval button does not offer an inline diff/edit interface; send `revised` JSON to the API for controlled correction.
- SQLite raw review text is **not encrypted by application code**; only synthetic data should be used.
- Model-generated summary is not fully entailed by simple quote matching; reviewer must verify summary.
- `STAGED` means durable local export only. `MOCK_RECEIVED` and `MOCK_ACCEPTED_NOT_WORKDAY` mean in-process simulator acceptance. **Neither is a real Workday import acknowledgement.**
- An external Workday SOAP/WWS integration is **not implemented**; integration credentials and current tenant schemas are required.

## Workday go-live checklist

- [ ] Tenant integration owner has provided WWS SOAP API version or inbound EIB template
- [ ] Correct employee self-review business process, current review template and section IDs validated
- [ ] Workday worker IDs/WIDs and periods mapped to employee identities
- [ ] Integration-system user permissions, tenant security policies and audit approved
- [ ] Confirmed submission semantics, idempotency and authoritative receipt/status
- [ ] Sandbox tests for partial failure, duplicate/retry, closed period and expired review
- [ ] HR data processing, security and AI governance signoff complete
