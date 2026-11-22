//! Intake for performance review forms submitted in Microsoft Teams (Microsoft Forms).
//!
//! Microsoft Graph has no supported API for reading Forms responses, so a Power Automate
//! flow ("When a new response is submitted" -> "Get response details" -> HTTP POST) pushes
//! each response here. See docs/TEAMS-INTAKE.md for the flow and the JSON body template.

use crate::domain::{validate_email, validate_label, ApiError, NewReview};
use serde::Deserialize;
use serde_json::{Map, Value};

#[derive(Debug, Deserialize)]
pub struct FormsSubmission {
    /// Microsoft Forms form ID. Becomes part of the idempotency key.
    pub form_id: String,
    /// Forms response ID. Power Automate retries reuse it, so duplicates are ignored.
    pub response_id: String,
    /// Responder email from "Get response details". Informational only.
    #[serde(default)]
    pub responder: Option<String>,
    /// Question title -> answer, built in the flow from dynamic content.
    pub answers: Map<String, Value>,
}

/// Defaults applied when the form doesn't ask for a value.
#[derive(Clone, Debug)]
pub struct IntakeDefaults {
    pub template_id: String,
    pub review_period: Option<String>,
}

impl IntakeDefaults {
    pub fn from_env() -> Self {
        let read = |k: &str| std::env::var(k).ok().map(|v| v.trim().to_owned()).filter(|v| !v.is_empty());
        Self {
            template_id: read("FORMS_DEFAULT_TEMPLATE").unwrap_or_else(|| "ANNUAL_REVIEW".into()),
            review_period: read("FORMS_DEFAULT_PERIOD"),
        }
    }
}

enum Field { EmployeeId, Manager, Period, Template, Achievements, Goals, Improvements, Other }

/// Classifies a form question by its title. Titles vary between forms, so match on keywords.
fn classify(question: &str) -> Field {
    let q = question.to_lowercase();
    let has = |w: &str| q.contains(w);
    if has("employee id") || has("employee number") || has("worker id") || has("employee_id") { Field::EmployeeId }
    // "Manager email" routes approval; "anything for your manager?" is a normal question.
    else if has("manager") && (has("email") || has("e-mail") || has("upn")) { Field::Manager }
    else if has("review period") || has("review year") || q.trim() == "period" { Field::Period }
    else if has("template") { Field::Template }
    else if has("achievement") || has("accomplishment") { Field::Achievements }
    else if has("goal") || has("objective") { Field::Goals }
    else if has("improve") || has("develop") || has("growth area") { Field::Improvements }
    else { Field::Other }
}

/// Forms answers arrive as strings; multi-select answers may be arrays or JSON-encoded arrays.
fn answer_text(v: &Value) -> String {
    match v {
        Value::String(s) => {
            if s.trim_start().starts_with('[') {
                if let Ok(Value::Array(items)) = serde_json::from_str::<Value>(s) {
                    return answer_text(&Value::Array(items));
                }
            }
            s.trim().to_owned()
        }
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(items) => items.iter().map(answer_text).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(", "),
        _ => String::new(),
    }
}

/// Turns a Forms response into a review. The answers are kept verbatim under section
/// headings so every AI-extracted quote can still be checked against the employee's words.
pub fn to_new_review(sub: FormsSubmission, defaults: &IntakeDefaults) -> Result<NewReview, ApiError> {
    validate_label("form_id", &sub.form_id, 120)?;
    validate_label("response_id", &sub.response_id, 120)?;
    if let Some(r) = &sub.responder { validate_label("responder", r, 254)?; }

    let (mut employee_id, mut manager, mut period, mut template) = (None, None, None, None);
    let (mut achievements, mut goals, mut improvements, mut other) = (vec![], vec![], vec![], vec![]);
    for (question, value) in &sub.answers {
        let answer = answer_text(value);
        if answer.is_empty() { continue; }
        let question = question.trim();
        match classify(question) {
            Field::EmployeeId => employee_id = Some(answer),
            Field::Manager => manager = Some(answer),
            Field::Period => period = Some(answer),
            Field::Template => template = Some(answer),
            Field::Achievements => achievements.push(answer),
            Field::Goals => goals.push(answer),
            Field::Improvements => improvements.push(answer),
            Field::Other => other.push(format!("{}:\n{answer}", question.trim_end_matches([':', '?']).trim())),
        }
    }

    let employee_id = employee_id.ok_or_else(|| ApiError::Invalid(
        "The form response has no Employee ID answer; add an 'Employee ID' question to the form".into()))?;
    let review_period = period.or_else(|| defaults.review_period.clone()).ok_or_else(|| ApiError::Invalid(
        "The form response has no review period; add a 'Review period' question or set FORMS_DEFAULT_PERIOD".into()))?;

    if let Some(m) = &manager { validate_email("Manager email answer", m)?; }

    let mut blocks = vec![];
    for (heading, answers) in [("Achievements", achievements), ("Goals", goals), ("Improvements", improvements)] {
        if !answers.is_empty() { blocks.push(format!("{heading}:\n{}", answers.join("\n"))); }
    }
    blocks.extend(other);
    if blocks.is_empty() {
        return Err(ApiError::Invalid("The form response has no review answers".into()));
    }

    Ok(NewReview {
        employee_id,
        review_period,
        template_id: template.unwrap_or_else(|| defaults.template_id.clone()),
        source_system: format!("microsoft-forms:{}", sub.form_id.trim()),
        source_id: sub.response_id.trim().to_owned(),
        raw_text: blocks.join("\n\n"),
        submitted_by: sub.responder.map(|r| r.trim().to_owned()),
        manager_email: manager.map(|m| m.trim().to_ascii_lowercase()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ai::rules_analysis, domain::{validate_analysis, validate_new}};
    use serde_json::json;

    fn defaults() -> IntakeDefaults { IntakeDefaults { template_id: "ANNUAL_REVIEW".into(), review_period: None } }

    #[test]
    fn maps_power_automate_payload_to_review() {
        let sub: FormsSubmission = serde_json::from_value(json!({
            "form_id": "f1", "response_id": "42", "responder": "alex@contoso.com",
            "answers": {
                "Employee ID": "EMP-001",
                "Manager email": "Dana.Lee@Contoso.com",
                "Anything you want your manager to know?": "Happy to mentor again.",
                "Review period": "2026",
                "What were your key achievements this year?": "Cut cloud costs by 18%.\nMentored two interns.",
                "What are your goals for next year?": "Lead the data platform migration.",
                "Which areas do you want to develop?": "Public speaking.",
                "Skills used": "[\"Rust\",\"SQL\"]",
                "Anything else?": ""
            }
        })).unwrap();
        let r = to_new_review(sub, &defaults()).unwrap();
        validate_new(&r).unwrap();
        assert_eq!(r.source_system, "microsoft-forms:f1");
        assert_eq!(r.source_id, "42");
        assert_eq!(r.template_id, "ANNUAL_REVIEW");
        assert_eq!(r.manager_email.as_deref(), Some("dana.lee@contoso.com"));
        assert!(r.raw_text.contains("Anything you want your manager to know:
Happy to mentor again."));
        assert!(r.raw_text.contains("Skills used:\nRust, SQL"));
        assert!(!r.raw_text.contains("Anything else"));
        let a = rules_analysis(&r.raw_text);
        assert_eq!((a.achievements.len(), a.goals.len(), a.improvements.len()), (2, 1, 1));
        assert!(validate_analysis(&a, &r.raw_text).is_ok());
    }

    #[test]
    fn missing_employee_id_or_period_is_rejected() {
        let sub = |answers: Value| serde_json::from_value::<FormsSubmission>(
            json!({"form_id":"f","response_id":"1","answers":answers})).unwrap();
        assert!(to_new_review(sub(json!({"Review period":"2026","Goals":"x"})), &defaults()).is_err());
        assert!(to_new_review(sub(json!({"Employee ID":"E1","Goals":"x"})), &defaults()).is_err());
        let with_default = IntakeDefaults { review_period: Some("2026".into()), ..defaults() };
        assert!(to_new_review(sub(json!({"Employee ID":"E1","Goals":"x"})), &with_default).is_ok());
    }
}
