use crate::domain::{status, validate_analysis, Analysis, ApiError, AuditEvent, NewReview, Review};
use crate::workday::{DeliveryReceipt, WorkdayDraft};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, Row, TransactionBehavior};
use sha2::{Digest, Sha256};
use std::{sync::Arc, time::Duration};
use uuid::Uuid;

#[derive(Clone)]
pub struct Store { path: Arc<String> }

fn map_review(row: &Row<'_>) -> rusqlite::Result<Review> {
    let extracted: Option<String> = row.get(7)?;
    let approved: Option<String> = row.get(8)?;
    fn parse(v: Option<String>, index: usize) -> rusqlite::Result<Option<Analysis>> {
        v.map(|json| serde_json::from_str(&json).map_err(|e|
            rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(e))))
            .transpose()
    }
    Ok(Review {
        id: row.get(0)?, employee_id: row.get(1)?, review_period: row.get(2)?,
        template_id: row.get(3)?, source_system: row.get(4)?, source_id: row.get(5)?,
        raw_text: row.get(6)?, extracted: parse(extracted, 7)?, approved: parse(approved, 8)?,
        status: row.get(9)?, created_at: row.get(10)?, updated_at: row.get(11)?,
        submitted_by: row.get(12)?, approved_by: row.get(13)?, return_reason: row.get(14)?,
        attempts: row.get(15)?, last_error: row.get(16)?,
        manager_email: row.get(17)?, workday_receipt_id: row.get(18)?,
    })
}
const FIELDS: &str = "id, employee_id, review_period, template_id, source_system, source_id, raw_text, extracted_json, approved_json, status, created_at, updated_at, submitted_by, approved_by, return_reason, attempts, last_error, manager_email, workday_receipt_id";

/// Columns added after v0.2. Existing local databases are upgraded in place.
const ADDED_COLUMNS: [(&str, &str); 8] = [
    ("manager_email", "TEXT"),
    ("workday_receipt_id", "TEXT"),
    ("submitted_by", "TEXT"),
    ("approved_by", "TEXT"),
    ("return_reason", "TEXT"),
    ("attempts", "INTEGER NOT NULL DEFAULT 0"),
    ("next_attempt_at", "TEXT"),
    ("last_error", "TEXT"),
];

fn get(conn: &Connection, id: &str) -> Result<Review, ApiError> {
    conn.query_row(&format!("SELECT {FIELDS} FROM reviews WHERE id=?1"), [id], map_review)
        .optional().map_err(|_| ApiError::Internal)?.ok_or(ApiError::NotFound)
}
fn event(conn: &Connection, id: &str, name: &str, actor: &str) -> Result<(), ApiError> {
    conn.execute("INSERT INTO audit_events (review_id,event,actor,created_at) VALUES (?1,?2,?3,?4)",
        params![id, name, actor, Utc::now().to_rfc3339()]).map_err(|_| ApiError::Internal)?;
    Ok(())
}
/// Moves a review to a new state and clears any retry bookkeeping from the previous step.
fn set_status(conn: &Connection, id: &str, new_status: &str) -> Result<(), ApiError> {
    conn.execute("UPDATE reviews SET status=?1, attempts=0, next_attempt_at=NULL, last_error=NULL, updated_at=?2 WHERE id=?3",
        params![new_status, Utc::now().to_rfc3339(), id]).map_err(|_| ApiError::Internal)?;
    Ok(())
}

/// Seconds to wait before retry number `attempt` (1-based): 30s, 60s, 120s, ... capped at 30 min.
pub fn backoff_secs(attempt: i64) -> i64 {
    let shift = (attempt - 1).clamp(0, 6) as u32;
    (30_i64 << shift).min(1800)
}

pub enum Claim { Send(WorkdayDraft), AlreadyDelivered(DeliveryReceipt) }

fn receipt(conn: &Connection, id: &str) -> Result<DeliveryReceipt, ApiError> {
    let raw: Option<String> = conn.query_row("SELECT receipt_json FROM workday_receipts WHERE review_id=?1", [id], |r| r.get(0))
        .optional().map_err(|_| ApiError::Internal)?;
    let raw = match raw {
        Some(r) => r,
        // Receipts written by v0.2 databases.
        None => conn.query_row("SELECT receipt_json FROM mock_workday_receipts WHERE review_id=?1", [id], |r| r.get(0))
            .map_err(|_| ApiError::Internal)?,
    };
    serde_json::from_str(&raw).map_err(|_| ApiError::Internal)
}

impl Store {
    pub fn init(path: String) -> anyhow::Result<Self> {
        let conn = Connection::open(&path)?;
        conn.busy_timeout(Duration::from_secs(5))?;
        // WAL is fastest on a local disk. Network file shares (e.g. Azure Files) need DELETE.
        let journal = std::env::var("SQLITE_JOURNAL_MODE").unwrap_or_else(|_| "WAL".into()).to_ascii_uppercase();
        if !["WAL", "DELETE"].contains(&journal.as_str()) { anyhow::bail!("SQLITE_JOURNAL_MODE must be WAL or DELETE"); }
        conn.query_row(&format!("PRAGMA journal_mode={journal}"), [], |_| Ok(()))?;
        conn.execute_batch("PRAGMA foreign_keys=ON;
          CREATE TABLE IF NOT EXISTS reviews (
             id TEXT PRIMARY KEY, employee_id TEXT NOT NULL, review_period TEXT NOT NULL,
             template_id TEXT NOT NULL, source_system TEXT NOT NULL, source_id TEXT NOT NULL,
             raw_text TEXT NOT NULL, extracted_json TEXT, approved_json TEXT,
             status TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
             UNIQUE(source_system, source_id)
          );
          CREATE INDEX IF NOT EXISTS idx_reviews_status ON reviews(status);
          CREATE TABLE IF NOT EXISTS audit_events (
             id INTEGER PRIMARY KEY AUTOINCREMENT, review_id TEXT NOT NULL REFERENCES reviews(id),
             event TEXT NOT NULL, actor TEXT NOT NULL, created_at TEXT NOT NULL
          );
          CREATE TABLE IF NOT EXISTS workday_outbox (
             review_id TEXT PRIMARY KEY REFERENCES reviews(id), payload_json TEXT NOT NULL,
             payload_sha256 TEXT NOT NULL, state TEXT NOT NULL, created_at TEXT NOT NULL
          );
          CREATE TABLE IF NOT EXISTS mock_workday_receipts (
             review_id TEXT PRIMARY KEY REFERENCES reviews(id), receipt_json TEXT NOT NULL
          );
          CREATE TABLE IF NOT EXISTS workday_receipts (
             review_id TEXT PRIMARY KEY REFERENCES reviews(id), system TEXT NOT NULL,
             receipt_json TEXT NOT NULL, created_at TEXT NOT NULL
          );")?;
        let existing: Vec<String> = conn.prepare("SELECT name FROM pragma_table_info('reviews')")?
            .query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        for (name, ddl) in ADDED_COLUMNS {
            if !existing.iter().any(|c| c == name) {
                conn.execute_batch(&format!("ALTER TABLE reviews ADD COLUMN {name} {ddl}"))?;
            }
        }
        // A delivery that was in flight when the server stopped may or may not have reached
        // Workday. Never resend it automatically; ask a person to check first.
        let mut stmt = conn.prepare("SELECT review_id FROM workday_outbox WHERE state='SENDING'")?;
        let stuck: Vec<String> = stmt.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        drop(stmt);
        for id in stuck {
            conn.execute("UPDATE workday_outbox SET state='UNKNOWN' WHERE review_id=?1", [&id])?;
            conn.execute("UPDATE reviews SET status=?1, last_error=?2, next_attempt_at=NULL, updated_at=?3 WHERE id=?4",
                params![status::DELIVERY_FAILED, "Outcome unknown, check Workday before retrying: the server stopped during delivery",
                    Utc::now().to_rfc3339(), id])?;
            event(&conn, &id, "DELIVERY_OUTCOME_UNKNOWN", "system restart").map_err(|e| anyhow::anyhow!(e.message()))?;
        }
        Ok(Self {path:Arc::new(path)})
    }

    async fn run<T,F>(&self, f:F) -> Result<T,ApiError>
    where T:Send+'static, F:FnOnce(&mut Connection)->Result<T,ApiError>+Send+'static {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = Connection::open(path.as_str()).map_err(|_| ApiError::Internal)?;
            conn.busy_timeout(Duration::from_secs(5)).map_err(|_| ApiError::Internal)?;
            conn.execute_batch("PRAGMA foreign_keys=ON").map_err(|_| ApiError::Internal)?;
            f(&mut conn)
        }).await.map_err(|_| ApiError::Internal)?
    }

    pub async fn create(&self, input: NewReview) -> Result<Review, ApiError> {
        self.run(move |conn| {
            let id = Uuid::new_v4().to_string();
            let now = Utc::now().to_rfc3339();
            let inserted = conn.execute("INSERT OR IGNORE INTO reviews
                (id,employee_id,review_period,template_id,source_system,source_id,raw_text,status,created_at,updated_at,submitted_by,manager_email)
                VALUES (?1,?2,?3,?4,?5,?6,?7,'SUBMITTED',?8,?8,?9,?10)",
                params![id, input.employee_id, input.review_period, input.template_id,
                    input.source_system, input.source_id, input.raw_text, now, input.submitted_by,
                    input.manager_email.map(|m| m.trim().to_ascii_lowercase())])
                .map_err(|_| ApiError::Internal)?;
            if inserted == 1 {
                event(conn, &id, status::SUBMITTED, &format!("submitter via {}", input.source_system))?;
                get(conn, &id)
            } else {
                let existing = conn.query_row(&format!("SELECT {FIELDS} FROM reviews WHERE source_system=?1 AND source_id=?2"),
                    params![input.source_system, input.source_id], map_review)
                    .map_err(|_| ApiError::Internal)?;
                if existing.raw_text != input.raw_text || existing.employee_id != input.employee_id ||
                   existing.review_period != input.review_period || existing.template_id != input.template_id {
                    return Err(ApiError::Conflict("source_system/source_id already used for different review content".into()));
                }
                Ok(existing) // Idempotent repeat submission (e.g. Power Automate retried the call).
            }
        }).await
    }

    pub async fn get(&self, id: String) -> Result<Review, ApiError> {
        self.run(move |conn| get(conn, &id)).await
    }

    pub async fn list(&self) -> Result<Vec<Review>, ApiError> {
        self.run(move |conn| {
            let mut stmt = conn.prepare(&format!("SELECT {FIELDS} FROM reviews ORDER BY created_at DESC LIMIT 100"))
                .map_err(|_| ApiError::Internal)?;
            let it = stmt.query_map([], map_review).map_err(|_| ApiError::Internal)?;
            it.collect::<rusqlite::Result<Vec<_>>>().map_err(|_| ApiError::Internal)
        }).await
    }

    /// Reviews in `state` whose retry delay (if any) has passed, oldest first. Used by the agent.
    pub async fn due(&self, state: &'static str, limit: i64) -> Result<Vec<Review>, ApiError> {
        self.run(move |conn| {
            let mut stmt = conn.prepare(&format!("SELECT {FIELDS} FROM reviews
                WHERE status=?1 AND (next_attempt_at IS NULL OR next_attempt_at <= ?2)
                ORDER BY created_at LIMIT ?3")).map_err(|_| ApiError::Internal)?;
            let it = stmt.query_map(params![state, Utc::now().to_rfc3339(), limit], map_review)
                .map_err(|_| ApiError::Internal)?;
            it.collect::<rusqlite::Result<Vec<_>>>().map_err(|_| ApiError::Internal)
        }).await
    }

    /// Records a failed automatic attempt. Schedules a retry with backoff, or moves the review
    /// to `failed_state` once `max_attempts` is reached. Returns the review afterwards.
    pub async fn record_failure(&self, id: String, expected: &'static str, failed_state: &'static str,
                                error: String, max_attempts: i64) -> Result<Review, ApiError> {
        self.run(move |conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| ApiError::Internal)?;
            let review = get(&tx, &id)?;
            if review.status != expected { return Ok(review); } // Someone else moved it on.
            let attempts = review.attempts + 1;
            let now = Utc::now();
            if attempts >= max_attempts {
                tx.execute("UPDATE reviews SET status=?1, attempts=?2, next_attempt_at=NULL, last_error=?3, updated_at=?4 WHERE id=?5",
                    params![failed_state, attempts, error, now.to_rfc3339(), id]).map_err(|_| ApiError::Internal)?;
                event(&tx, &id, failed_state, "agent")?;
            } else {
                let next = now + chrono::Duration::seconds(backoff_secs(attempts));
                tx.execute("UPDATE reviews SET attempts=?1, next_attempt_at=?2, last_error=?3, updated_at=?4 WHERE id=?5",
                    params![attempts, next.to_rfc3339(), error, now.to_rfc3339(), id]).map_err(|_| ApiError::Internal)?;
                event(&tx, &id, &format!("{expected}_RETRY_SCHEDULED"), "agent")?;
            }
            tx.commit().map_err(|_| ApiError::Internal)?;
            get(conn, &id)
        }).await
    }

    pub async fn save_analysis(&self, id:String, analysis:Analysis, actor: &'static str) -> Result<Review, ApiError> {
        self.run(move |conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| ApiError::Internal)?;
            let review = get(&tx, &id)?;
            if review.status != status::SUBMITTED {
                return Err(ApiError::Conflict("Only submitted reviews may be analyzed".into()));
            }
            let new_status = if analysis.risk_flags.is_empty() {status::EXTRACTED} else {status::NEEDS_REVIEW};
            tx.execute("UPDATE reviews SET extracted_json=?1 WHERE id=?2",
                params![serde_json::to_string(&analysis).map_err(|_|ApiError::Internal)?, id])
                .map_err(|_|ApiError::Internal)?;
            set_status(&tx, &id, new_status)?;
            event(&tx,&id,new_status,actor)?;
            tx.commit().map_err(|_|ApiError::Internal)?;
            get(conn,&id)
        }).await
    }

    pub async fn approve(&self, id:String, revised:Option<Analysis>, approved_by: Option<String>, actor: String) -> Result<Review, ApiError> {
        self.run(move |conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_|ApiError::Internal)?;
            let review = get(&tx,&id)?;
            if review.status != status::EXTRACTED && review.status != status::NEEDS_REVIEW {
                return Err(ApiError::Conflict("Only analyzed reviews may be approved".into()));
            }
            let original = review.extracted.clone().ok_or(ApiError::Internal)?;
            let edited = revised.is_some();
            let mut approved = revised.unwrap_or_else(|| original.clone());
            approved.processor = if edited {
                format!("{} + reviewer edits", original.processor)
            } else { original.processor };
            validate_analysis(&approved, &review.raw_text)?;
            tx.execute("UPDATE reviews SET approved_json=?1, approved_by=?2 WHERE id=?3",
                params![serde_json::to_string(&approved).map_err(|_|ApiError::Internal)?, approved_by, id])
                .map_err(|_|ApiError::Internal)?;
            set_status(&tx, &id, status::APPROVED)?;
            let name = if edited { "APPROVED_WITH_EDITS" } else { status::APPROVED };
            event(&tx,&id,name,&actor)?;
            tx.commit().map_err(|_|ApiError::Internal)?;
            get(conn,&id)
        }).await
    }

    /// Sends a review back to the employee instead of approving it. The employee resubmits
    /// through the form, which creates a new review with a new source ID.
    pub async fn send_back(&self, id:String, reason:String, actor: String) -> Result<Review, ApiError> {
        self.run(move |conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_|ApiError::Internal)?;
            let review = get(&tx,&id)?;
            if ![status::EXTRACTED, status::NEEDS_REVIEW, status::ANALYSIS_FAILED].contains(&review.status.as_str()) {
                return Err(ApiError::Conflict("Only reviews waiting for approval can be sent back".into()));
            }
            tx.execute("UPDATE reviews SET return_reason=?1 WHERE id=?2", params![reason, id])
                .map_err(|_|ApiError::Internal)?;
            set_status(&tx, &id, status::RETURNED)?;
            event(&tx,&id,status::RETURNED,&actor)?;
            tx.commit().map_err(|_|ApiError::Internal)?;
            get(conn,&id)
        }).await
    }

    /// Puts a failed review back in the queue for another automatic attempt.
    pub async fn retry(&self, id:String, actor: String) -> Result<Review, ApiError> {
        self.run(move |conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_|ApiError::Internal)?;
            let review = get(&tx,&id)?;
            let back_to = match review.status.as_str() {
                status::ANALYSIS_FAILED => status::SUBMITTED,
                status::DELIVERY_FAILED => status::STAGED,
                _ => return Err(ApiError::Conflict("Only failed reviews can be retried".into())),
            };
            if back_to == status::STAGED {
                tx.execute("UPDATE workday_outbox SET state='STAGED' WHERE review_id=?1", [&id]).map_err(|_|ApiError::Internal)?;
            }
            set_status(&tx, &id, back_to)?;
            event(&tx,&id,"RETRY_REQUESTED",&actor)?;
            tx.commit().map_err(|_|ApiError::Internal)?;
            get(conn,&id)
        }).await
    }

    pub async fn stage(&self, id:String, actor: String) -> Result<Review, ApiError> {
        self.run(move |conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_|ApiError::Internal)?;
            let review = get(&tx,&id)?;
            // Already staged or further along: staging again is a no-op.
            if [status::STAGED, status::DELIVERY_FAILED, status::MOCK_RECEIVED, status::DELIVERED].contains(&review.status.as_str()) {
                return Ok(review);
            }
            if review.status != status::APPROVED {
                return Err(ApiError::Conflict("Review must be approved before staging".into()));
            }
            let approved = review.approved.as_ref().ok_or(ApiError::Internal)?;
            let draft = WorkdayDraft::from_review(&review, approved);
            let payload = serde_json::to_string(&draft).map_err(|_|ApiError::Internal)?;
            let digest = format!("{:x}", Sha256::digest(payload.as_bytes()));
            tx.execute("INSERT INTO workday_outbox (review_id,payload_json,payload_sha256,state,created_at)
                VALUES (?1,?2,?3,'STAGED',?4)",
                params![id,payload,digest,Utc::now().to_rfc3339()]).map_err(|_|ApiError::Internal)?;
            set_status(&tx, &id, status::STAGED)?;
            event(&tx,&id,status::STAGED,&actor)?;
            tx.commit().map_err(|_|ApiError::Internal)?;
            get(conn,&id)
        }).await
    }

    /// Reserves a staged review for delivery. Only one caller can hold the claim, so the
    /// agent and a manual click can never send the same review twice.
    pub async fn claim_delivery(&self, id:String) -> Result<Claim, ApiError> {
        self.run(move |conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_|ApiError::Internal)?;
            let review = get(&tx,&id)?;
            if review.status == status::MOCK_RECEIVED || review.status == status::DELIVERED {
                return Ok(Claim::AlreadyDelivered(receipt(&tx, &id)?));
            }
            if review.status != status::STAGED {
                return Err(ApiError::Conflict("Stage the approved review before delivering it".into()));
            }
            let claimed = tx.execute("UPDATE workday_outbox SET state='SENDING' WHERE review_id=?1 AND state='STAGED'", [&id])
                .map_err(|_|ApiError::Internal)?;
            if claimed != 1 { return Err(ApiError::Conflict("Delivery is already in progress".into())); }
            let payload:String = tx.query_row("SELECT payload_json FROM workday_outbox WHERE review_id=?1",[&id],
                |row|row.get(0)).map_err(|_|ApiError::Internal)?;
            let draft = serde_json::from_str(&payload).map_err(|_|ApiError::Internal)?;
            tx.commit().map_err(|_|ApiError::Internal)?;
            Ok(Claim::Send(draft))
        }).await
    }

    /// Records a successful delivery and its receipt.
    pub async fn complete_delivery(&self, id:String, r:DeliveryReceipt, actor: String) -> Result<Review, ApiError> {
        self.run(move |conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_|ApiError::Internal)?;
            let live = r.system == "workday";
            let (final_status, event_name) = if live { (status::DELIVERED, "DELIVERED_TO_WORKDAY") }
                else { (status::MOCK_RECEIVED, "MOCK_RECEIVED_NOT_WORKDAY") };
            tx.execute("INSERT INTO workday_receipts (review_id,system,receipt_json,created_at) VALUES (?1,?2,?3,?4)",
                params![id, r.system, serde_json::to_string(&r).map_err(|_|ApiError::Internal)?, Utc::now().to_rfc3339()])
                .map_err(|_|ApiError::Internal)?;
            tx.execute("UPDATE workday_outbox SET state=?1 WHERE review_id=?2", params![final_status, id])
                .map_err(|_|ApiError::Internal)?;
            tx.execute("UPDATE reviews SET workday_receipt_id=?1 WHERE id=?2", params![r.receipt_id, id])
                .map_err(|_|ApiError::Internal)?;
            set_status(&tx, &id, final_status)?;
            event(&tx,&id,event_name,&actor)?;
            tx.commit().map_err(|_|ApiError::Internal)?;
            get(conn,&id)
        }).await
    }

    /// Releases a failed claim. Retryable failures go back to STAGED with backoff; an unknown
    /// outcome goes straight to DELIVERY_FAILED so a person checks Workday first.
    pub async fn release_delivery(&self, id:String, error:String, retryable:bool, max_attempts:i64) -> Result<Review, ApiError> {
        let outbox_state = if retryable { "STAGED" } else { "UNKNOWN" };
        let key = id.clone();
        self.run(move |conn| {
            conn.execute("UPDATE workday_outbox SET state=?1 WHERE review_id=?2 AND state='SENDING'", params![outbox_state, key])
                .map_err(|_|ApiError::Internal)?;
            Ok(())
        }).await?;
        self.record_failure(id, status::STAGED, status::DELIVERY_FAILED, error, if retryable { max_attempts } else { 0 }).await
    }

    pub async fn staged(&self) -> Result<Vec<WorkdayDraft>,ApiError> {
        self.run(move |conn| {
            let mut stmt = conn.prepare("SELECT payload_json FROM workday_outbox WHERE state='STAGED' ORDER BY created_at")
                .map_err(|_|ApiError::Internal)?;
            let rows = stmt.query_map([], |r|r.get::<_,String>(0)).map_err(|_|ApiError::Internal)?;
            let raw = rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|_|ApiError::Internal)?;
            raw.into_iter().map(|s|serde_json::from_str(&s).map_err(|_|ApiError::Internal)).collect()
        }).await
    }

    pub async fn audit(&self,id:String) -> Result<Vec<AuditEvent>,ApiError> {
        self.run(move |conn| {
            get(conn,&id)?;
            let mut stmt = conn.prepare("SELECT event,actor,created_at FROM audit_events WHERE review_id=?1 ORDER BY id")
                .map_err(|_|ApiError::Internal)?;
            let rows = stmt.query_map([id], |r| Ok(AuditEvent {event:r.get(0)?,actor:r.get(1)?,at:r.get(2)?}))
                .map_err(|_|ApiError::Internal)?;
            rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|_|ApiError::Internal)
        }).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::rules_analysis;

    const TEXT: &str = "Achievements\n- Shipped the billing rewrite.\nGoals\n- Lead the Q3 migration.";

    async fn store() -> (Store, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!("reviewflow-test-{}.db", Uuid::new_v4()));
        (Store::init(path.to_string_lossy().into_owned()).unwrap(), path)
    }
    fn new_review(source_id: &str) -> NewReview {
        NewReview { employee_id:"EMP-9".into(), review_period:"2026".into(), template_id:"T".into(),
            source_system:"test".into(), source_id:source_id.into(), raw_text:TEXT.into(), submitted_by:None, manager_email:None }
    }

    #[tokio::test]
    async fn full_lifecycle_with_send_back_and_retry() {
        let (db, path) = store().await;
        // Approve path.
        let r = db.create(new_review("a")).await.unwrap();
        let r = db.save_analysis(r.id, rules_analysis(TEXT), "test").await.unwrap();
        assert_eq!(r.status, status::EXTRACTED);
        let r = db.approve(r.id, None, Some("Dana".into()), "Dana".into()).await.unwrap();
        assert_eq!(r.approved_by.as_deref(), Some("Dana"));
        let r = db.stage(r.id, "test".into()).await.unwrap();
        assert_eq!(r.status, status::STAGED);
        let Claim::Send(draft) = db.claim_delivery(r.id.clone()).await.unwrap() else { panic!("expected a claim") };
        assert!(db.claim_delivery(r.id.clone()).await.is_err()); // second sender is refused
        let receipt = crate::workday::WorkdayClient::Simulated.deliver(&draft).await.unwrap();
        db.complete_delivery(r.id.clone(), receipt, "test".into()).await.unwrap();
        // Staging again after delivery is a harmless no-op.
        assert_eq!(db.stage(r.id, "test".into()).await.unwrap().status, status::MOCK_RECEIVED);

        // Send-back path.
        let r = db.create(new_review("b")).await.unwrap();
        assert!(db.send_back(r.id.clone(), "x".into(), "t".into()).await.is_err()); // not analyzed yet
        let r = db.save_analysis(r.id, rules_analysis(TEXT), "test").await.unwrap();
        let r = db.send_back(r.id, "Please add measurable goals".into(), "t".into()).await.unwrap();
        assert_eq!(r.status, status::RETURNED);
        assert!(db.approve(r.id, None, None, "t".into()).await.is_err());

        // Failure, backoff, give up, then manual retry.
        let r = db.create(new_review("c")).await.unwrap();
        let r = db.record_failure(r.id, status::SUBMITTED, status::ANALYSIS_FAILED, "AI down".into(), 2).await.unwrap();
        assert_eq!((r.status.as_str(), r.attempts), (status::SUBMITTED, 1));
        assert!(db.due(status::SUBMITTED, 10).await.unwrap().iter().all(|d| d.id != r.id)); // waiting for backoff
        let r = db.record_failure(r.id, status::SUBMITTED, status::ANALYSIS_FAILED, "AI down".into(), 2).await.unwrap();
        assert_eq!(r.status, status::ANALYSIS_FAILED);
        let r = db.retry(r.id, "t".into()).await.unwrap();
        assert_eq!((r.status.as_str(), r.attempts, r.last_error.is_none()), (status::SUBMITTED, 0, true));
        assert!(db.due(status::SUBMITTED, 10).await.unwrap().iter().any(|d| d.id == r.id));
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn unknown_outcome_is_never_retried_automatically() {
        let (db, path) = store().await;
        let r = db.create(new_review("u")).await.unwrap();
        db.save_analysis(r.id.clone(), rules_analysis(TEXT), "t").await.unwrap();
        db.approve(r.id.clone(), None, None, "t".into()).await.unwrap();
        db.stage(r.id.clone(), "t".into()).await.unwrap();
        assert!(matches!(db.claim_delivery(r.id.clone()).await.unwrap(), Claim::Send(_)));
        let r2 = db.release_delivery(r.id.clone(), "timeout".into(), false, 5).await.unwrap();
        assert_eq!(r2.status, status::DELIVERY_FAILED);
        // HR checks Workday, then retries: the review is claimable again.
        db.retry(r.id.clone(), "hr".into()).await.unwrap();
        assert!(matches!(db.claim_delivery(r.id.clone()).await.unwrap(), Claim::Send(_)));
        // The server "crashes" mid-send: restart flags it instead of resending.
        drop(db);
        let db = Store::init(path.to_string_lossy().into_owned()).unwrap();
        let r3 = db.get(r.id.clone()).await.unwrap();
        assert_eq!(r3.status, status::DELIVERY_FAILED);
        assert!(r3.last_error.unwrap().contains("check Workday"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn backoff_grows_and_caps() {
        assert_eq!(backoff_secs(1), 30);
        assert_eq!(backoff_secs(2), 60);
        assert_eq!(backoff_secs(20), 1800);
    }
}
