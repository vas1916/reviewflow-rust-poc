use crate::domain::{Analysis, ApiError, Review};
use serde::{Serialize, Deserialize};

/// Stable vendor-neutral mapping contract. A tenant-specific Workday adapter
/// must map these fields to the actual review-template sections/IDs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkdayDraft {
    pub source_review_id: String,
    pub employee_id: String,
    pub review_period: String,
    pub template_id: String,
    pub accomplishments: Vec<String>,
    pub goals: Vec<String>,
    pub development_areas: Vec<String>,
    pub overall_comments: String,
}

impl WorkdayDraft {
    pub fn from_review(r: &Review, a: &Analysis) -> Self {
        Self {
            source_review_id: r.id.clone(),
            employee_id: r.employee_id.clone(),
            review_period: r.review_period.clone(),
            template_id: r.template_id.clone(),
            accomplishments: a.achievements.iter().map(|x|x.text.clone()).collect(),
            goals: a.goals.iter().map(|x|x.text.clone()).collect(),
            development_areas: a.improvements.iter().map(|x|x.text.clone()).collect(),
            overall_comments: a.summary.clone(),
        }
    }
}

/// Explicitly simulated receipt; never use as a Workday tenant acknowledgement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MockReceipt {
    pub review_id: String,
    pub mock_receipt_id: String,
    pub status: String,
    pub at: String,
}

/// Real adapters should implement this using a tenant-validated Workday operation
/// and return an authoritative receipt from that tenant.
#[allow(dead_code)]
pub trait WorkdayDeliveryAdapter {
    fn deliver(&self, draft: &WorkdayDraft) -> Result<MockReceipt, ApiError>;
}

pub struct SimulatedWorkday;
impl WorkdayDeliveryAdapter for SimulatedWorkday {
    fn deliver(&self, draft: &WorkdayDraft) -> Result<MockReceipt, ApiError> {
        if draft.employee_id.is_empty() || draft.template_id.is_empty() {
            return Err(ApiError::Invalid("Missing mock Workday worker/template reference".into()));
        }
        let key = draft.source_review_id.replace('-', "");
        let suffix: String = key.chars().take(12).collect();
        Ok(MockReceipt {
            review_id: draft.source_review_id.clone(),
            mock_receipt_id: format!("MOCK-WD-{suffix}"),
            status: "MOCK_ACCEPTED_NOT_WORKDAY".into(),
            at: chrono::Utc::now().to_rfc3339(),
        })
    }
}

/// Port for a future Workday outbound connector. POC implements only staging;
/// an actual SOAP/EIB submission cannot be completed until tenant-specific
/// schemas, review lifecycle, and integration credentials have been supplied.
#[allow(dead_code)]
pub trait WorkdayAdapter {
    fn stage(&self, draft: &WorkdayDraft) -> Result<Vec<u8>, ApiError>;
}

pub struct CsvStagingAdapter;

impl WorkdayAdapter for CsvStagingAdapter {
    fn stage(&self, draft: &WorkdayDraft) -> Result<Vec<u8>, ApiError> {
        staging_csv(std::slice::from_ref(draft))
    }
}

fn csv_cell(value: &str) -> String {
    // Protect human-opened CSVs against spreadsheet formula injection.
    // This is a staging format, NOT a tenant-validated Workday EIB template.
    let dangerous = matches!(value.trim_start().chars().next(), Some('='|'+'|'-'|'@'));
    if dangerous { format!("'{value}") } else { value.to_owned() }
}

pub fn staging_csv(drafts: &[WorkdayDraft]) -> Result<Vec<u8>, ApiError> {
    let mut writer = csv::Writer::from_writer(vec![]);
    writer.write_record(["Source_Review_ID", "Employee_ID", "Review_Period", "Template_ID",
        "Accomplishments", "Goals", "Development_Areas", "Overall_Comments"])
        .map_err(|_| ApiError::Internal)?;
    for d in drafts {
        writer.write_record([
            csv_cell(&d.source_review_id), csv_cell(&d.employee_id),
            csv_cell(&d.review_period), csv_cell(&d.template_id),
            csv_cell(&d.accomplishments.join(" | ")),
            csv_cell(&d.goals.join(" | ")),
            csv_cell(&d.development_areas.join(" | ")),
            csv_cell(&d.overall_comments),
        ]).map_err(|_| ApiError::Internal)?;
    }
    writer.into_inner().map_err(|_| ApiError::Internal)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn spreadsheet_formula_is_not_executable() {
        assert_eq!(csv_cell("=1+2"), "'=1+2");
        assert_eq!(csv_cell("normal"), "normal");
    }
}
