//! Sends one staged review to Workday: claim -> send (no DB transaction held) -> record.
//! Shared by the agent and the manual "Send to Workday" action.

use crate::db::{Claim, Store};
use crate::domain::{ApiError, Review};
use crate::workday::{DeliveryReceipt, WorkdayClient};

pub enum Outcome {
    Delivered(Review, DeliveryReceipt),
    AlreadyDelivered(DeliveryReceipt),
    /// Delivery failed; the review shows whether a retry is scheduled or HR must act.
    Failed(Review, String),
}

pub async fn deliver(db: &Store, workday: &WorkdayClient, id: String, actor: String, max_attempts: i64)
    -> Result<Outcome, ApiError> {
    let draft = match db.claim_delivery(id.clone()).await? {
        Claim::AlreadyDelivered(receipt) => return Ok(Outcome::AlreadyDelivered(receipt)),
        Claim::Send(draft) => draft,
    };
    match workday.deliver(&draft).await {
        Ok(receipt) => {
            let review = db.complete_delivery(id, receipt.clone(), actor).await?;
            Ok(Outcome::Delivered(review, receipt))
        }
        Err(e) => {
            let message = e.message();
            let review = db.release_delivery(id, message.clone(), e.retryable(), max_attempts).await?;
            Ok(Outcome::Failed(review, message))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ai::rules_analysis, domain::{status, NewReview}};

    #[tokio::test]
    async fn only_one_of_two_concurrent_deliveries_sends() {
        let path = std::env::temp_dir().join(format!("reviewflow-deliver-{}.db", uuid::Uuid::new_v4()));
        let db = Store::init(path.to_string_lossy().into_owned()).unwrap();
        let text = "Achievements\n- Shipped v2.";
        let r = db.create(NewReview { employee_id: "E".into(), review_period: "2026".into(), template_id: "T".into(),
            source_system: "t".into(), source_id: "1".into(), raw_text: text.into(), submitted_by: None, manager_email: None }).await.unwrap();
        db.save_analysis(r.id.clone(), rules_analysis(text), "t").await.unwrap();
        db.approve(r.id.clone(), None, None, "t".into()).await.unwrap();
        db.stage(r.id.clone(), "t".into()).await.unwrap();

        let (a, b) = tokio::join!(
            deliver(&db, &WorkdayClient::Simulated, r.id.clone(), "a".into(), 3),
            deliver(&db, &WorkdayClient::Simulated, r.id.clone(), "b".into(), 3));
        let sent = [&a, &b].iter().filter(|o| matches!(o, Ok(Outcome::Delivered(..)))).count();
        let other_ok = [&a, &b].iter().all(|o| matches!(o, Ok(Outcome::Delivered(..)) | Ok(Outcome::AlreadyDelivered(_)) | Err(ApiError::Conflict(_))));
        assert_eq!(sent, 1);
        assert!(other_ok);
        let review = db.get(r.id.clone()).await.unwrap();
        assert_eq!(review.status, status::MOCK_RECEIVED);
        assert!(review.workday_receipt_id.unwrap().starts_with("MOCK-WD-"));
        assert!(matches!(deliver(&db, &WorkdayClient::Simulated, r.id, "c".into(), 3).await, Ok(Outcome::AlreadyDelivered(_))));
        let _ = std::fs::remove_file(path);
    }
}
