use axum::{http::StatusCode, response::{IntoResponse, Response}, Json};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug)]
pub enum ApiError {
    Invalid(String),
    Unauthorized,
    NotFound,
    Conflict(String),
    Upstream(String),
    Internal,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::Invalid(s) => (StatusCode::BAD_REQUEST, s),
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "Invalid or missing token".into()),
            Self::NotFound => (StatusCode::NOT_FOUND, "Review not found".into()),
            Self::Conflict(s) => (StatusCode::CONFLICT, s),
            Self::Upstream(s) => (StatusCode::BAD_GATEWAY, s),
            Self::Internal => (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error".into()),
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceItem {
    pub text: String,
    pub evidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Analysis {
    #[serde(default)]
    pub achievements: Vec<EvidenceItem>,
    #[serde(default)]
    pub goals: Vec<EvidenceItem>,
    #[serde(default)]
    pub improvements: Vec<EvidenceItem>,
    pub summary: String,
    #[serde(default)]
    pub risk_flags: Vec<String>,
    #[serde(default)]
    pub processor: String,
}

#[derive(Debug, Deserialize)]
pub struct NewReview {
    pub employee_id: String,
    pub review_period: String,
    pub template_id: String,
    pub source_system: String,
    pub source_id: String,
    pub raw_text: String,
}

#[derive(Debug, Deserialize)]
pub struct Approval {
    pub revised: Option<Analysis>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Review {
    pub id: String,
    pub employee_id: String,
    pub review_period: String,
    pub template_id: String,
    pub source_system: String,
    pub source_id: String,
    pub raw_text: String,
    pub extracted: Option<Analysis>,
    pub approved: Option<Analysis>,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize)]
pub struct AuditEvent {
    pub event: String,
    pub actor: String,
    pub at: String,
}

pub fn validate_new(r: &NewReview) -> Result<(), ApiError> {
    if r.employee_id.is_empty() || r.employee_id.len() > 64 ||
       !r.employee_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        return Err(ApiError::Invalid("employee_id must be 1-64 alphanumeric, '_' or '-' characters".into()));
    }
    for (name, value, max) in [
        ("review_period", &r.review_period, 40),
        ("template_id", &r.template_id, 80),
        ("source_system", &r.source_system, 40),
        ("source_id", &r.source_id, 256),
    ] {
        if value.trim().is_empty() || value.len() > max || value.chars().any(|c| c.is_control()) {
            return Err(ApiError::Invalid(format!("Invalid {name} (required, <= {max} chars)")));
        }
    }
    if r.raw_text.trim().is_empty() || r.raw_text.chars().count() > 20_000 {
        return Err(ApiError::Invalid("raw_text must be between 1 and 20,000 characters".into()));
    }
    Ok(())
}

pub fn validate_analysis(a: &Analysis, source: &str) -> Result<(), ApiError> {
    if a.summary.trim().is_empty() || a.summary.chars().count() > 2000 {
        return Err(ApiError::Invalid("Summary required (max 2,000 characters)".into()));
    }
    if a.risk_flags.len() > 20 || a.risk_flags.iter().any(|x| x.len() > 120) {
        return Err(ApiError::Invalid("Invalid risk flags".into()));
    }
    for items in [&a.achievements, &a.goals, &a.improvements] {
        if items.len() > 30 {
            return Err(ApiError::Invalid("Too many extracted evidence items".into()));
        }
        for item in items {
            if item.text.trim().is_empty() || item.text.chars().count() > 2000 ||
               item.evidence.trim().is_empty() || item.evidence.chars().count() > 2000 ||
               !source.contains(&item.evidence) {
                return Err(ApiError::Invalid("Every extracted item must cite an exact substring from the original review".into()));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fabricated_evidence_is_rejected() {
        let a = Analysis {
            achievements: vec![EvidenceItem { text:"Raised productivity 50%".into(), evidence:"50%".into() }],
            goals: vec![], improvements: vec![], summary:"Improved productivity".into(),
            risk_flags: vec![], processor:"test".into()
        };
        assert!(validate_analysis(&a, "I improved team communication").is_err());
    }
}
