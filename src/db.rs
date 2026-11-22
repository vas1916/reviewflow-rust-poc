use crate::domain::{Analysis, ApiError, AuditEvent, NewReview, Review, validate_analysis};
use crate::workday::{MockReceipt, SimulatedWorkday, WorkdayDeliveryAdapter, WorkdayDraft};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde_json;
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
    })
}
const FIELDS: &str = "id, employee_id, review_period, template_id, source_system, source_id, raw_text, extracted_json, approved_json, status, created_at, updated_at";
fn get(conn: &Connection, id: &str) -> Result<Review, ApiError> {
    conn.query_row(&format!("SELECT {FIELDS} FROM reviews WHERE id=?1"), [id], map_review)
        .optional().map_err(|_| ApiError::Internal)?.ok_or(ApiError::NotFound)
}
fn event(conn: &Connection, id: &str, name: &str, actor: &str) -> Result<(), ApiError> {
    conn.execute("INSERT INTO audit_events (review_id,event,actor,created_at) VALUES (?1,?2,?3,?4)",
        params![id, name, actor, Utc::now().to_rfc3339()]).map_err(|_| ApiError::Internal)?;
    Ok(())
}

impl Store {
    pub fn init(path: String) -> anyhow::Result<Self> {
        let conn = Connection::open(&path)?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;
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
          );")?;
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
                (id,employee_id,review_period,template_id,source_system,source_id,raw_text,status,created_at,updated_at)
                VALUES (?1,?2,?3,?4,?5,?6,?7,'SUBMITTED',?8,?8)",
                params![id, input.employee_id, input.review_period, input.template_id,
                    input.source_system, input.source_id, input.raw_text, now])
                .map_err(|_| ApiError::Internal)?;
            if inserted == 1 {
                event(conn, &id, "SUBMITTED", "submitter")?;
                get(conn, &id)
            } else {
                let existing = conn.query_row(&format!("SELECT {FIELDS} FROM reviews WHERE source_system=?1 AND source_id=?2"),
                    params![input.source_system, input.source_id], map_review)
                    .map_err(|_| ApiError::Internal)?;
                if existing.raw_text != input.raw_text || existing.employee_id != input.employee_id ||
                   existing.review_period != input.review_period || existing.template_id != input.template_id {
                    return Err(ApiError::Conflict("source_system/source_id already used for different review content".into()));
                }
                Ok(existing) // Idempotent repeat submission.
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

    pub async fn save_analysis(&self, id:String, analysis:Analysis) -> Result<Review, ApiError> {
        self.run(move |conn| {
            let tx = conn.transaction().map_err(|_| ApiError::Internal)?;
            let review = get(&tx, &id)?;
            if review.status != "SUBMITTED" {
                return Err(ApiError::Conflict("Only submitted reviews may be analyzed".into()));
            }
            let status = if analysis.risk_flags.is_empty() {"EXTRACTED"} else {"NEEDS_REVIEW"};
            tx.execute("UPDATE reviews SET extracted_json=?1,status=?2,updated_at=?3 WHERE id=?4",
                params![serde_json::to_string(&analysis).map_err(|_|ApiError::Internal)?,
                    status, Utc::now().to_rfc3339(),id]).map_err(|_|ApiError::Internal)?;
            event(&tx,&id,status,"analyzer")?;
            tx.commit().map_err(|_|ApiError::Internal)?;
            get(conn,&id)
        }).await
    }

    pub async fn approve(&self, id:String, revised:Option<Analysis>) -> Result<Review, ApiError> {
        self.run(move |conn| {
            let tx = conn.transaction().map_err(|_|ApiError::Internal)?;
            let review = get(&tx,&id)?;
            if review.status != "EXTRACTED" && review.status != "NEEDS_REVIEW" {
                return Err(ApiError::Conflict("Only analyzed reviews may be approved".into()));
            }
            let approved = revised.or(review.extracted).ok_or(ApiError::Internal)?;
            validate_analysis(&approved, &review.raw_text)?;
            tx.execute("UPDATE reviews SET approved_json=?1,status='APPROVED',updated_at=?2 WHERE id=?3",
                params![serde_json::to_string(&approved).map_err(|_|ApiError::Internal)?,Utc::now().to_rfc3339(),id])
                .map_err(|_|ApiError::Internal)?;
            event(&tx,&id,"APPROVED","reviewer-token (POC only)")?;
            tx.commit().map_err(|_|ApiError::Internal)?;
            get(conn,&id)
        }).await
    }

    pub async fn stage(&self, id:String) -> Result<Review, ApiError> {
        self.run(move |conn| {
            let tx = conn.transaction().map_err(|_|ApiError::Internal)?;
            let review = get(&tx,&id)?;
            if review.status == "STAGED" { return Ok(review); }
            if review.status != "APPROVED" {
                return Err(ApiError::Conflict("Review must be approved before staging".into()));
            }
            let approved = review.approved.as_ref().ok_or(ApiError::Internal)?;
            let draft = WorkdayDraft::from_review(&review, approved);
            let payload = serde_json::to_string(&draft).map_err(|_|ApiError::Internal)?;
            let digest = format!("{:x}", Sha256::digest(payload.as_bytes()));
            tx.execute("INSERT INTO workday_outbox (review_id,payload_json,payload_sha256,state,created_at)
                VALUES (?1,?2,?3,'STAGED',?4)",
                params![id,payload,digest,Utc::now().to_rfc3339()]).map_err(|_|ApiError::Internal)?;
            tx.execute("UPDATE reviews SET status='STAGED',updated_at=?1 WHERE id=?2",
                params![Utc::now().to_rfc3339(), id]).map_err(|_|ApiError::Internal)?;
            event(&tx,&id,"STAGED","reviewer-token (POC only)")?;
            tx.commit().map_err(|_|ApiError::Internal)?;
            get(conn,&id)
        }).await
    }

    /// Simulated Workday endpoint in the SAME process (no external tenant).
    /// Makes the complete delivery/reconciliation contract testable locally.
    pub async fn mock_deliver(&self, id:String) -> Result<MockReceipt,ApiError> {
        self.run(move |conn| {
            let tx = conn.transaction().map_err(|_|ApiError::Internal)?;
            let review = get(&tx,&id)?;
            if review.status == "MOCK_RECEIVED" {
                return tx.query_row("SELECT receipt_json FROM mock_workday_receipts WHERE review_id=?1",[&id],
                    |row|row.get::<_,String>(0))
                    .map_err(|_|ApiError::Internal)
                    .and_then(|raw|serde_json::from_str(&raw).map_err(|_|ApiError::Internal));
            }
            if review.status != "STAGED" {
                return Err(ApiError::Conflict("Stage the approved review before simulating delivery".into()));
            }
            let payload:String = tx.query_row("SELECT payload_json FROM workday_outbox WHERE review_id=?1",[&id],
                |row|row.get(0)).map_err(|_|ApiError::Internal)?;
            let draft:WorkdayDraft = serde_json::from_str(&payload).map_err(|_|ApiError::Internal)?;
            let receipt = SimulatedWorkday.deliver(&draft)?;
            tx.execute("INSERT INTO mock_workday_receipts (review_id,receipt_json) VALUES (?1,?2)",
                params![id,serde_json::to_string(&receipt).map_err(|_|ApiError::Internal)?])
                .map_err(|_|ApiError::Internal)?;
            tx.execute("UPDATE workday_outbox SET state='MOCK_RECEIVED' WHERE review_id=?1",[&id])
                .map_err(|_|ApiError::Internal)?;
            tx.execute("UPDATE reviews SET status='MOCK_RECEIVED',updated_at=?1 WHERE id=?2",
                params![Utc::now().to_rfc3339(),id]).map_err(|_|ApiError::Internal)?;
            event(&tx,&id,"MOCK_RECEIVED_NOT_WORKDAY","simulator")?;
            tx.commit().map_err(|_|ApiError::Internal)?;
            Ok(receipt)
        }).await
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
