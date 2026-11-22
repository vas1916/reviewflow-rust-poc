# Going live

ReviewFlow is built so that going live is mostly configuration: every live component is switched on by settings in [.env.production.example](../.env.production.example), not by code changes. This guide lists what each team needs to provide, the questions to ask them, and the order to switch things on.

Run `reviewflow --check-config` at any point. It validates the settings and shows which components are still in demo mode:

```
Configuration is valid. Component status:
  [live] Sign-in              Entra ID via Easy Auth, roles ReviewFlow.Reviewer / ReviewFlow.HR
  [live] Summaries            Azure OpenAI
  [demo] Workday              simulated receiver
  ...
1 component(s) still in demo mode.
```

## What changes between demo and live

| Component | Demo (today) | Live setting | Provided by |
|---|---|---|---|
| Hosting | Your laptop, `127.0.0.1` | Azure Container Apps (or App Service), HTTPS, one replica, `/data` on Azure Files | Azure / IT team |
| Sign-in | Shared access code | `AUTH_MODE=entra`: Microsoft sign-in, app roles for managers and HR | Entra ID admin |
| Who sees what | Everyone sees everything | Managers see only reviews where they are the approver; HR sees all | Automatic with Entra roles + manager email on the form |
| AI summaries | Local Ollama or rules | `AI_MODE=azure`: Azure OpenAI in your tenant | IT security |
| Teams intake | Test button | Power Automate flow posting form responses ([TEAMS-INTAKE.md](TEAMS-INTAKE.md)) | M365 admin / flow owner |
| Approver alerts | Off | `TEAMS_WEBHOOK_URL`: cards in a private approvers' channel | M365 admin |
| Workday | Simulated receiver | `WORKDAY_MODE=soap` + request template + integration user | Workday integration team |

## Questions to ask

### Workday integration team (the critical path)

1. **Which operation loads a completed self-evaluation in our tenant?** For example Put Employee Reviews, Import Employee Reviews, or an inbound EIB. *ReviewFlow supports SOAP directly. EIB means using the CSV export as a starting point instead.*
2. **Which business-process step should we fill: the employee self-evaluation, or a manager evaluation?** Does the review event need to exist and be open before we send?
3. **Can you generate a sample request for that operation from our tenant's WSDL** (API Explorer or SoapUI), with our review template's section IDs? *This becomes `workday/request-template.xml`.*
4. **What is the service URL and WWS version for our sandbox (Implementation) and production tenants?** → `WORKDAY_ENDPOINT`
5. **Can you create an integration system user (ISU) with only the security domains this operation needs?** → `WORKDAY_USERNAME`, `WORKDAY_PASSWORD`
6. **How do our employee IDs map to Workday worker IDs?** Is the "Employee ID" on the form the Workday Employee_ID, or do we need a lookup?
7. **Which element in the response identifies the created or updated review?** → `WORKDAY_RECEIPT_ELEMENT`
8. **What happens if the same review is sent twice?** Is it rejected as a duplicate, does it create a second one, or does it update? Can we pass our review ID as an external reference?
9. **Are there limits** on field lengths, formatting (plain text or rich text), request rate, or maintenance windows?
10. **Who signs off sandbox testing**, and how do we move the template from sandbox to production?

### Entra ID / Microsoft 365 admin

1. Can you create an **app registration** for ReviewFlow with two app roles, `ReviewFlow.Reviewer` (managers) and `ReviewFlow.HR`, and assign them to the right groups?
2. Can you enable **built-in authentication** (Microsoft provider) on the Container App, allowing unauthenticated requests through? *The app enforces sign-in itself; Power Automate needs to reach the intake endpoint with a token.*
3. Is the **Power Automate Premium** license available for the flow owner? *The HTTP action is premium.*
4. Can you create a **private Teams channel** for approvers, plus a Workflows webhook in it? → `TEAMS_WEBHOOK_URL`
5. Should approval cards go to a **shared channel**, or to **each manager in a 1:1 chat**? *For 1:1, build a Workflows flow that reads the card's `recipient` field. ReviewFlow already sends it.*

### Azure / cloud team

1. Which **subscription, region and resource group** should host it? Any naming or tagging rules?
2. Can we use **Azure Container Apps** with one replica and an **Azure Files** share for the database? Or would you rather we use a VM or App Service?
3. Will **Key Vault** hold the secrets (Workday password, Azure OpenAI key, intake token)?
4. Is there an approved **Azure OpenAI** resource and model deployment we can use, in an approved region? → `AZURE_OPENAI_*`
5. Does the app need to be **private** (internal network or VPN only)? Which custom domain and certificate should it use? → `PUBLIC_BASE_URL`
6. What are the **backup and log retention** requirements for the database share?

### HR

1. Which **form questions** do we use? The form must include **Employee ID**, **Review period** (or we set a default) and **Manager email** for routing ([TEAMS-INTAKE.md](TEAMS-INTAKE.md)).
2. **Who approves:** the direct manager, HR, or both? (Today: the manager named on the form, or anyone with the HR role.)
3. What should happen to a review that is **sent back**? Who tells the employee?
4. **How long** do we keep review text, summaries and history after delivery?
5. Who is the **pilot group**, and in which review cycle?
6. Who handles **"Needs attention"** items, such as failed deliveries?

### Legal, privacy and security

1. Is a **data protection impact assessment** needed? Who owns it?
2. Do employees need a **notice** that AI summarizes their self-review? Is **works council** consultation needed?
3. Do the **EU AI Act** obligations apply (AI used in employment decisions is high-risk)? *ReviewFlow doesn't rate employees, and a person approves every summary.*
4. Is **Azure OpenAI** approved for HR data, with no training on our data and an approved data region?
5. Is a **penetration test** or security review required before go-live?

## Go-live steps, in order

1. **Workday sandbox.** Get answers 1–7 from the Workday team. Copy `workday/request-template.example.xml` to `workday/request-template.xml` and paste in their request with the placeholders.
2. **Test the Workday connection from a laptop.** Set `WORKDAY_MODE=soap` and the sandbox values in your local `.env`, run `reviewflow --check-config`, then approve one made-up review and confirm it appears in the sandbox tenant.
3. **Entra app registration** with the two app roles; assign the pilot managers and HR.
4. **Deploy to Azure** (see below) with `.env.production.example` values as Container App secrets and environment variables.
5. **Turn on built-in authentication** for the Container App. Check that a manager sees only their reviews and HR sees all.
6. **Power Automate flow** from the Teams form ([TEAMS-INTAKE.md](TEAMS-INTAKE.md)), using `INTAKE_API_TOKEN`.
7. **Teams webhook** in the approvers' channel → `TEAMS_WEBHOOK_URL`.
8. **User acceptance test** against the Workday sandbox (checklist below).
9. **Switch to production:** change `WORKDAY_ENDPOINT` and the ISU credentials to production values, then restart. Nothing else changes.

## Deploying to Azure Container Apps

These are the main `az` commands. Check them against the current Azure CLI docs, and replace `<...>` values.

```bash
RG=rg-reviewflow; LOC=<region>; ACR=<registryname>; SA=<storageaccount>; ENV=cae-reviewflow; APP=reviewflow

az group create -n $RG -l $LOC
az acr create -g $RG -n $ACR --sku Basic
az acr build -r $ACR -t reviewflow:1.0 .                      # builds the Dockerfile in this repo

az storage account create -g $RG -n $SA --sku Standard_LRS
az storage share-rm create -g $RG --storage-account $SA -n reviewflow-data
az containerapp env create -g $RG -n $ENV -l $LOC
az containerapp env storage set -g $RG -n $ENV --storage-name reviewflowdata \
  --azure-file-account-name $SA --azure-file-share-name reviewflow-data \
  --azure-file-account-key "$(az storage account keys list -g $RG -n $SA --query [0].value -o tsv)" \
  --access-mode ReadWrite

az containerapp create -g $RG -n $APP --environment $ENV \
  --image $ACR.azurecr.io/reviewflow:1.0 --registry-server $ACR.azurecr.io \
  --ingress external --target-port 8080 --min-replicas 1 --max-replicas 1 \
  --secrets workday-password=<...> openai-key=<...> intake-token=<...> employee-token=<...> reviewer-token=<...> teams-webhook=<...> \
  --env-vars AUTH_MODE=entra WORKDAY_MODE=soap SQLITE_JOURNAL_MODE=DELETE \
    WORKDAY_PASSWORD=secretref:workday-password AZURE_OPENAI_API_KEY=secretref:openai-key \
    INTAKE_API_TOKEN=secretref:intake-token EMPLOYEE_API_TOKEN=secretref:employee-token \
    REVIEWER_API_TOKEN=secretref:reviewer-token TEAMS_WEBHOOK_URL=secretref:teams-webhook \
    <...the remaining non-secret values from .env.production.example...>
```

Then:

- **Mount the share at `/data`.** Volume mounts are set in the app's YAML (`az containerapp show ... -o yaml`, add a `volumes` entry of type `AzureFile` with `storageName: reviewflowdata` and a `volumeMounts` entry at `/data`, then `az containerapp update --yaml`).
- **Turn on Microsoft sign-in:** `az containerapp auth microsoft update -g $RG -n $APP --client-id <app-id> --client-secret <secret> --issuer https://login.microsoftonline.com/<tenant-id>/v2.0`, then `az containerapp auth update -g $RG -n $APP --unauthenticated-client-action AllowAnonymous`.
- **Keep one replica.** SQLite and the in-process agent assume a single instance. Moving to several replicas means switching storage to PostgreSQL (see "When code changes are still needed").
- **Health probe:** `GET /healthz`.

## User acceptance test (against the Workday sandbox)

- [ ] A Teams form submission appears in ReviewFlow within seconds, with the right manager as approver.
- [ ] Submitting the same response twice (re-run the flow) creates only one review.
- [ ] The summary appears, and every point quotes the employee's words.
- [ ] A manager sees only their own team's reviews; another manager can't open them by link.
- [ ] HR sees all reviews and can download the CSV.
- [ ] Approve: the review appears in the Workday sandbox, and ReviewFlow shows the Workday reference.
- [ ] Send back with a reason: nothing goes to Workday.
- [ ] Wrong Workday password: the review shows "Needs attention" with a clear message; Retry works after fixing it.
- [ ] Stop the app during delivery and restart: the review shows "outcome unknown, check Workday" and is **not** resent.
- [ ] Teams cards contain no review text.
- [ ] The history shows each approver's real Microsoft account.

## When code changes are still needed

The configuration covers the usual setup. You'd need code changes only if:

- **The operation needs a lookup first.** For example, Workday needs the review event's ID, which you'd have to fetch with a Get operation before sending. Add that call in `SoapConfig::deliver` in `src/workday.rs`.
- **Managers should come from Workday or Entra instead of the form.** Replace the form's manager email with a lookup at intake (`src/intake.rs`).
- **You use EIB instead of SOAP.** Adapt `staging_csv` in `src/workday.rs` to the EIB template's columns, and upload through Workday's EIB process.
- **You need several replicas or high availability.** Replace SQLite in `src/db.rs` with PostgreSQL, and move the agent to a single worker.
- **Approval should be done from inside the Teams card.** This needs a Teams bot. Today the card links to ReviewFlow, where the manager approves.
