# Connecting Teams forms and notifications

This guide connects two things:

1. **Intake.** Employees fill in a Microsoft Forms self-review in Teams. A Power Automate flow sends each response to ReviewFlow.
2. **Notifications.** ReviewFlow posts a card to a Teams channel when a review is ready for approval, has failed, or was delivered.

## Why Power Automate

As of March 2026, Microsoft Graph has no supported API for reading Microsoft Forms responses. The supported route is Power Automate's Forms connector, which fires on every new response. ReviewFlow never reads Teams chats or polls Forms. It only receives the responses the flow sends.

## Before you start

- **ReviewFlow must be reachable from Power Automate.** Power Automate runs in Microsoft's cloud, so `http://127.0.0.1:8080` won't work. For a test, expose your laptop with a tunnel your IT team approves (for example Microsoft Dev Tunnels). For a pilot, host ReviewFlow in your company's Azure with HTTPS and set `BIND_ADDR=0.0.0.0:8080`.
- **The HTTP action is a premium connector.** The flow owner needs a Power Automate Premium license.
- **Use made-up data** until the items in [SECURITY.md](SECURITY.md) are done.

## 1. Prepare the form

ReviewFlow reads answers by question title, so titles only need to contain these keywords:

| ReviewFlow field | The question title should contain | Required |
|---|---|---|
| Employee ID | "Employee ID", "Employee number" or "Worker ID" | Yes |
| Approving manager | "Manager" plus "email" (e.g. "Manager email") | Yes for live use: it decides who can approve, and is sent as `recipient` on Teams cards |
| Review period | "Review period" or "Review year" | Yes, unless `FORMS_DEFAULT_PERIOD` is set |
| Workday template | "Template" | No, defaults to `FORMS_DEFAULT_TEMPLATE` |
| Achievements | "achievement" or "accomplishment" | No |
| Goals | "goal" or "objective" | No |
| Development areas | "improve", "develop" or "growth area" | No |

Any other question is kept as context under its own title, and the assistant can read it. The employee's answers are stored word for word, so every point the assistant extracts can be checked against what they wrote.

## 2. Create the flow

1. In Power Automate, create an **Automated cloud flow**.
2. **Trigger:** Microsoft Forms, *When a new response is submitted*. Pick your form.
3. **Action:** Microsoft Forms, *Get response details*. Use the same form and the trigger's *Response Id*.
4. **Action:** HTTP.
   - Method: `POST`
   - URI: `https://<your-reviewflow-host>/api/v1/intake/microsoft-forms`
   - Headers: `Authorization: Bearer <INTAKE_API_TOKEN>` and `Content-Type: application/json`
   - Body: the template below, with each `<...>` replaced by dynamic content from *Get response details*.
   - In the action's settings, turn on **Secure inputs** so the token doesn't appear in run history. Better still, read the token from Azure Key Vault.
5. *(Optional)* **Action:** Microsoft Teams, *Post message in a chat or channel*. Send the responder a confirmation that uses `link` from the HTTP response.

Body template (see [fixtures/sample-teams-form.json](../fixtures/sample-teams-form.json) for a filled-in example):

```json
{
  "form_id": "annual-self-review-2026",
  "response_id": "<Response Id>",
  "responder": "<Responders' Email>",
  "answers": {
    "Employee ID": "<Employee ID answer>",
    "Review period": "<Review period answer>",
    "What were your key achievements this year?": "<achievements answer>",
    "What are your goals for next year?": "<goals answer>",
    "Which areas do you want to develop?": "<development answer>"
  }
}
```

If Power Automate retries the call, ReviewFlow recognizes the same `response_id` and returns the original review instead of creating a duplicate. A successful call returns HTTP 201:

```json
{"id":"...","status":"SUBMITTED","employee_id":"EMP-002","review_period":"2026","link":"https://<host>/#review-..."}
```

A 400 response means the form is missing a required answer (the message says which one). A 409 response means the same response ID was sent with different answers.

## 3. Turn on Teams notifications

1. In the Teams channel where approvers work, open **Workflows**. Choose the template *Post to a channel when a webhook request is received*.
2. Copy the webhook URL it gives you into `.env` as `TEAMS_WEBHOOK_URL`.
3. Set `PUBLIC_BASE_URL` to the address reviewers use to open ReviewFlow. The card's **Open in ReviewFlow** button links there.

Cards only show the employee ID, review period, template and status. Review text, summaries and quotes are never posted to Teams.

To send each card to the approving manager in a 1:1 chat instead of a channel, build the webhook flow yourself: trigger *When a Teams webhook request is received*, then *Post card in a chat or channel* with **Post as** Flow bot, **Post in** Chat with Flow bot, **Recipient** `triggerBody()?['recipient']`, and the card from `triggerBody()?['attachments'][0]['content']`.

## 4. Test it without Power Automate

From the web app, open **Add a test review** and click **Submit sample Teams form**. Or call the endpoint directly:

```bash
curl -s -X POST http://127.0.0.1:8080/api/v1/intake/microsoft-forms \
  -H "Authorization: Bearer $REVIEWER_API_TOKEN" -H 'Content-Type: application/json' \
  --data-binary @fixtures/sample-teams-form.json
```

`bash scripts/smoke.sh` runs the whole path, from a Teams form to the simulated Workday receipt.
