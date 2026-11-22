//! The review agent: a background worker that moves reviews through the workflow on its own.
//!
//! It is deliberately a fixed sequence, not an LLM planner. Only the summarizing step uses AI.
//!   SUBMITTED -> summarize and verify quotes -> notify approvers in Teams
//!   APPROVED  -> stage the fixed Workday payload
//!   STAGED    -> deliver through the Workday adapter (simulated or live, see WORKDAY_MODE)
//! A person must approve in between; the agent never approves, edits or rates a review.

use crate::ai::Processor;
use crate::db::Store;
use crate::delivery::{self, Outcome};
use crate::workday::WorkdayClient;
use crate::domain::{status, ApiError, Review};
use crate::teams::{Notice, Notifier};
use std::{sync::Arc, time::Duration};
use tokio::sync::Notify;

const ACTOR: &str = "agent";

#[derive(Clone, Debug)]
pub struct AgentConfig {
    pub enabled: bool,
    pub poll: Duration,
    pub max_attempts: i64,
    pub auto_deliver: bool,
}

impl AgentConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let flag = |k: &str, default: bool| -> anyhow::Result<bool> {
            match std::env::var(k).map(|v| v.trim().to_ascii_lowercase()) {
                Err(_) => Ok(default),
                Ok(v) if v.is_empty() => Ok(default),
                Ok(v) if ["1", "true", "yes", "on"].contains(&v.as_str()) => Ok(true),
                Ok(v) if ["0", "false", "no", "off"].contains(&v.as_str()) => Ok(false),
                Ok(v) => anyhow::bail!("{k} must be true or false, got {v}"),
            }
        };
        let num = |k: &str, default: u64, min: u64, max: u64| -> anyhow::Result<u64> {
            match std::env::var(k).ok().filter(|v| !v.trim().is_empty()) {
                None => Ok(default),
                Some(v) => {
                    let n: u64 = v.trim().parse().map_err(|_| anyhow::anyhow!("{k} must be a number"))?;
                    if !(min..=max).contains(&n) { anyhow::bail!("{k} must be between {min} and {max}"); }
                    Ok(n)
                }
            }
        };
        Ok(Self {
            enabled: flag("AGENT_ENABLED", true)?,
            poll: Duration::from_secs(num("AGENT_POLL_SECONDS", 10, 1, 3600)?),
            max_attempts: num("AGENT_MAX_ATTEMPTS", 3, 1, 10)? as i64,
            auto_deliver: flag("AGENT_AUTO_DELIVER", true)?,
        })
    }
}

#[derive(Clone)]
pub struct Agent {
    pub db: Store,
    pub processor: Processor,
    pub notifier: Notifier,
    pub workday: WorkdayClient,
    pub config: AgentConfig,
    /// Lets API handlers wake the agent right away instead of waiting for the next poll.
    pub wake: Arc<Notify>,
}

impl Agent {
    pub fn spawn(self) {
        tokio::spawn(async move {
            tracing::info!(poll = ?self.config.poll, auto_deliver = self.config.auto_deliver, "Review agent started");
            loop {
                if let Err(e) = self.tick().await {
                    tracing::warn!(error = %e.message(), "Review agent cycle failed");
                }
                tokio::select! {
                    _ = tokio::time::sleep(self.config.poll) => {}
                    _ = self.wake.notified() => {}
                }
            }
        });
    }

    /// One pass over every step. Each review is handled independently so one failure
    /// never blocks the others.
    pub async fn tick(&self) -> Result<(), ApiError> {
        for review in self.db.due(status::SUBMITTED, 5).await? {
            self.summarize(review).await;
        }
        for review in self.db.due(status::APPROVED, 25).await? {
            if let Err(e) = self.db.stage(review.id.clone(), ACTOR.into()).await {
                tracing::warn!(review = %review.id, error = %e.message(), "Staging failed");
            }
        }
        if self.config.auto_deliver {
            for review in self.db.due(status::STAGED, 25).await? {
                self.deliver(review).await;
            }
        }
        Ok(())
    }

    async fn summarize(&self, review: Review) {
        match self.processor.analyze(&review.raw_text).await {
            Ok(analysis) => match self.db.save_analysis(review.id.clone(), analysis, ACTOR).await {
                Ok(updated) => {
                    tracing::info!(review = %updated.id, status = %updated.status, "Review summarized");
                    self.notifier.notify(Notice::ReadyForApproval, &updated).await;
                }
                Err(ApiError::Conflict(_)) => {} // Analyzed manually in the meantime.
                Err(e) => tracing::warn!(review = %review.id, error = %e.message(), "Saving analysis failed"),
            },
            Err(e) => {
                let message = e.message();
                tracing::warn!(review = %review.id, error = %message, "Summarizing failed");
                match self.db.record_failure(review.id.clone(), status::SUBMITTED, status::ANALYSIS_FAILED,
                                             message, self.config.max_attempts).await {
                    Ok(r) if r.status == status::ANALYSIS_FAILED => self.notifier.notify(Notice::AnalysisFailed, &r).await,
                    Ok(_) => {}
                    Err(e) => tracing::warn!(review = %review.id, error = %e.message(), "Recording failure failed"),
                }
            }
        }
    }

    async fn deliver(&self, review: Review) {
        match delivery::deliver(&self.db, &self.workday, review.id.clone(), ACTOR.into(), self.config.max_attempts).await {
            Ok(Outcome::Delivered(updated, receipt)) => {
                tracing::info!(review = %updated.id, system = %receipt.system, "Delivered to Workday");
                self.notifier.notify(Notice::Delivered, &updated).await;
            }
            Ok(Outcome::AlreadyDelivered(_)) | Err(ApiError::Conflict(_)) => {} // Done or in progress elsewhere.
            Ok(Outcome::Failed(updated, message)) => {
                tracing::warn!(review = %updated.id, error = %message, "Workday delivery failed");
                if updated.status == status::DELIVERY_FAILED {
                    self.notifier.notify(Notice::DeliveryFailed, &updated).await;
                }
            }
            Err(e) => tracing::warn!(review = %review.id, error = %e.message(), "Delivery bookkeeping failed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::NewReview;

    #[tokio::test]
    async fn agent_runs_everything_except_the_human_approval() {
        let path = std::env::temp_dir().join(format!("reviewflow-agent-{}.db", uuid::Uuid::new_v4()));
        let db = Store::init(path.to_string_lossy().into_owned()).unwrap();
        let agent = Agent {
            db: db.clone(), processor: Processor::Rules,
            notifier: Notifier::from_env().unwrap(), workday: WorkdayClient::Simulated,
            config: AgentConfig { enabled: true, poll: Duration::from_secs(1), max_attempts: 3, auto_deliver: true },
            wake: Arc::new(Notify::new()),
        };
        let r = db.create(NewReview { employee_id: "E1".into(), review_period: "2026".into(), template_id: "T".into(),
            source_system: "test".into(), source_id: "1".into(),
            raw_text: "Achievements\n- Shipped v2.\nGoals\n- Learn Rust.".into(), submitted_by: None, manager_email: None }).await.unwrap();

        agent.tick().await.unwrap();
        let r = db.get(r.id).await.unwrap();
        assert_eq!(r.status, status::EXTRACTED);

        agent.tick().await.unwrap(); // Nothing happens without a person approving.
        assert_eq!(db.get(r.id.clone()).await.unwrap().status, status::EXTRACTED);

        db.approve(r.id.clone(), None, Some("Sam".into()), "Sam".into()).await.unwrap();
        agent.tick().await.unwrap(); // Stages and delivers in one pass.
        assert_eq!(db.get(r.id.clone()).await.unwrap().status, status::MOCK_RECEIVED);
        let events: Vec<String> = db.audit(r.id).await.unwrap().into_iter().map(|e| e.event).collect();
        assert_eq!(events, ["SUBMITTED", "EXTRACTED", "APPROVED", "STAGED", "MOCK_RECEIVED_NOT_WORKDAY"]);
        let _ = std::fs::remove_file(path);
    }
}
