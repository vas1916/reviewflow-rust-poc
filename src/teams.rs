//! Teams notifications through a Teams "Workflows" webhook (Power Automate).
//!
//! Cards carry only the employee ID, period, status and a link back to ReviewFlow.
//! Review text, summaries and quotes never leave the app through this channel.

use crate::domain::Review;
use reqwest::Url;
use serde_json::{json, Value};
use std::time::Duration;

#[derive(Clone, Copy, Debug)]
pub enum Notice { ReadyForApproval, AnalysisFailed, Delivered, DeliveryFailed }

#[derive(Clone)]
pub struct Notifier {
    webhook: Option<(reqwest::Client, String)>,
    public_base_url: String,
}

/// Only Power Automate / Logic Apps hosts may receive notifications.
fn allowed_webhook(url: &Url) -> bool {
    let host = url.host_str().unwrap_or_default();
    url.scheme() == "https" && url.username().is_empty() && url.password().is_none() &&
        (host.ends_with(".logic.azure.com") || host.ends_with(".powerplatform.com"))
}

impl Notifier {
    pub fn from_env() -> anyhow::Result<Self> {
        let public_base_url = std::env::var("PUBLIC_BASE_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:8080".into()).trim_end_matches('/').to_owned();
        let base = Url::parse(&public_base_url)?;
        if !["http", "https"].contains(&base.scheme()) {
            anyhow::bail!("PUBLIC_BASE_URL must be an http(s) URL");
        }
        let webhook = match std::env::var("TEAMS_WEBHOOK_URL").ok().filter(|v| !v.trim().is_empty()) {
            None => None,
            Some(raw) => {
                let url = Url::parse(raw.trim())?;
                if !allowed_webhook(&url) {
                    anyhow::bail!("TEAMS_WEBHOOK_URL must be an https Teams Workflows URL (*.logic.azure.com or *.powerplatform.com)");
                }
                Some((reqwest::Client::builder().timeout(Duration::from_secs(10)).build()?, url.to_string()))
            }
        };
        Ok(Self { webhook, public_base_url })
    }

    pub fn enabled(&self) -> bool { self.webhook.is_some() }

    pub fn review_link(&self, id: &str) -> String { format!("{}/#review-{id}", self.public_base_url) }

    /// Best effort: a Teams outage must never block or roll back the review workflow.
    pub async fn notify(&self, notice: Notice, review: &Review) {
        let Some((client, url)) = &self.webhook else { return };
        let card = card(notice, review, &self.review_link(&review.id));
        match client.post(url).json(&card).send().await.and_then(|r| r.error_for_status()) {
            Ok(_) => tracing::info!(review = %review.id, ?notice, "Teams notification sent"),
            Err(e) => tracing::warn!(review = %review.id, ?notice, status = ?e.status(), "Teams notification failed"),
        }
    }
}

pub fn card(notice: Notice, r: &Review, link: &str) -> Value {
    let (title, detail, color) = match notice {
        Notice::ReadyForApproval => {
            let a = r.extracted.as_ref();
            let points = a.map(|a| a.achievements.len() + a.goals.len() + a.improvements.len()).unwrap_or(0);
            let flags = a.map(|a| a.risk_flags.len()).unwrap_or(0);
            let flag_text = match flags { 0 => "no items to check".to_owned(), 1 => "1 item to check".to_owned(), n => format!("{n} items to check") };
            ("Self-review ready for approval", format!("{points} key points summarized, {flag_text}."), "Accent")
        }
        Notice::AnalysisFailed => ("Self-review could not be summarized",
            "The assistant tried several times. Open the review to retry or send it back.".to_owned(), "Attention"),
        Notice::Delivered if r.status == crate::domain::status::DELIVERED => ("Self-review delivered to Workday",
            format!("Workday accepted the approved review. Workday reference: {}.", r.workday_receipt_id.as_deref().unwrap_or("not returned")), "Good"),
        Notice::Delivered => ("Self-review sent to Workday (simulated)",
            "Approved and delivered to the simulated Workday receiver. No real Workday tenant was called.".to_owned(), "Good"),
        Notice::DeliveryFailed => ("Workday delivery failed",
            "The assistant could not deliver the approved review. Open it to retry.".to_owned(), "Attention"),
    };
    json!({
        // Ignored by the standard "Post to a channel" workflow. A custom Workflows flow can
        // use it to send the card to the approving manager in a 1:1 chat instead.
        "recipient": r.manager_email,
        "event": format!("{notice:?}"),
        "review_id": r.id,
        "type": "message",
        "attachments": [{
            "contentType": "application/vnd.microsoft.card.adaptive",
            "content": {
                "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
                "type": "AdaptiveCard",
                "version": "1.4",
                "body": [
                    {"type": "TextBlock", "text": title, "weight": "Bolder", "size": "Medium", "color": color, "wrap": true},
                    {"type": "FactSet", "facts": [
                        {"title": "Employee", "value": r.employee_id},
                        {"title": "Review period", "value": r.review_period},
                        {"title": "Template", "value": r.template_id}
                    ]},
                    {"type": "TextBlock", "text": detail, "wrap": true}
                ],
                "actions": [{"type": "Action.OpenUrl", "title": "Open in ReviewFlow", "url": link}]
            }
        }]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_power_automate_hosts_are_accepted() {
        let ok = |s: &str| allowed_webhook(&Url::parse(s).unwrap());
        assert!(ok("https://prod-01.westus.logic.azure.com:443/workflows/abc/triggers/manual/paths/invoke?sig=x"));
        assert!(ok("https://default123.ab.environment.api.powerplatform.com/powerautomate/automations/direct/workflows/x"));
        assert!(!ok("http://prod-01.westus.logic.azure.com/workflows/abc"));
        assert!(!ok("https://evil.example.com/logic.azure.com"));
    }

    #[test]
    fn card_never_contains_review_text() {
        let r = Review { id: "1".into(), employee_id: "EMP-1".into(), review_period: "2026".into(),
            template_id: "T".into(), source_system: "s".into(), source_id: "x".into(),
            raw_text: "SECRET REVIEW TEXT".into(), extracted: None, approved: None, status: "EXTRACTED".into(),
            created_at: String::new(), updated_at: String::new(), submitted_by: Some("a@b.c".into()),
            approved_by: None, return_reason: None, attempts: 0, last_error: None,
            manager_email: Some("dana@contoso.com".into()), workday_receipt_id: None };
        let text = card(Notice::ReadyForApproval, &r, "http://x/#review-1").to_string();
        assert!(!text.contains("SECRET"));
        assert!(!text.contains("a@b.c"));
        assert!(text.contains("#review-1"));
        assert!(text.contains("\"recipient\":\"dana@contoso.com\""));
    }
}
