//! Workday mapping, delivery and CSV staging.
//!
//! Delivery is chosen with `WORKDAY_MODE`:
//! - `simulated` (default): an in-process fake receiver. Nothing leaves the app.
//! - `soap`: Workday Web Services over HTTPS with WS-Security UsernameToken (an integration
//!   system user). The SOAP *body* comes from a template file supplied by your Workday team,
//!   generated from your tenant's WSDL, because review templates and operations differ per tenant.
//!   This module handles the envelope, authentication, escaping, timeouts, fault handling and
//!   reading the receipt ID from the response.

use crate::domain::{Analysis, ApiError, Review};
use quick_xml::{events::Event, Reader};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Stable vendor-neutral mapping contract. The SOAP template maps these fields to the
/// tenant's actual review-template sections.
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

/// Receipt from a delivery. `system` is "simulated" or "workday"; only "workday" receipts
/// come from a real tenant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryReceipt {
    pub review_id: String,
    #[serde(alias = "mock_receipt_id")]
    pub receipt_id: String,
    pub status: String,
    pub at: String,
    #[serde(default = "simulated_system")]
    pub system: String,
}
fn simulated_system() -> String { "simulated".into() }

/// Why a delivery failed. The difference matters: resending after an *unknown* outcome
/// could create a duplicate review in Workday, so those are never retried automatically.
#[derive(Debug)]
pub enum DeliveryError {
    /// Nothing reached Workday (connection refused, DNS, 503). Safe to retry.
    Unavailable(String),
    /// Workday answered with a SOAP fault or error. Nothing was created. Retry may help.
    Rejected(String),
    /// The request may have been processed (timeout, dropped connection, 504).
    /// A person must check Workday before retrying.
    Unknown(String),
}
impl DeliveryError {
    pub fn message(&self) -> String {
        match self {
            Self::Unavailable(m) => format!("Workday unavailable: {m}"),
            Self::Rejected(m) => format!("Workday rejected the review: {m}"),
            Self::Unknown(m) => format!("Outcome unknown, check Workday before retrying: {m}"),
        }
    }
    pub fn retryable(&self) -> bool { !matches!(self, Self::Unknown(_)) }
}

#[derive(Clone)]
pub struct SoapConfig {
    client: reqwest::Client,
    endpoint: String,
    username: String,
    password: String,
    template: String,
    receipt_element: String,
}

#[derive(Clone)]
pub enum WorkdayClient {
    Simulated,
    Soap(Box<SoapConfig>),
}

/// Marker left in the example template. Startup refuses to run with it, so nobody goes
/// live with an unvalidated request body.
pub const TEMPLATE_MARKER: &str = "REPLACE_WITH_TENANT_REQUEST";
const PLACEHOLDERS: [&str; 6] = ["source_review_id", "employee_id", "review_period", "template_id",
    "overall_comments", "item"];
const LISTS: [&str; 3] = ["accomplishments", "goals", "development_areas"];

fn allowed_endpoint(url: &Url) -> bool {
    let host = url.host_str().unwrap_or_default();
    let local = ["127.0.0.1", "localhost"].contains(&host);
    url.username().is_empty() && url.password().is_none() && (
        (url.scheme() == "https" && (host.ends_with(".workday.com") || host.ends_with(".myworkday.com")))
        // Plain http only to a local mock, for testing the adapter.
        || (local && ["http", "https"].contains(&url.scheme())))
}

impl WorkdayClient {
    pub fn from_env() -> anyhow::Result<Self> {
        match std::env::var("WORKDAY_MODE").unwrap_or_else(|_| "simulated".into()).trim().to_ascii_lowercase().as_str() {
            "simulated" | "" => Ok(Self::Simulated),
            "soap" => {
                let need = |k: &str| std::env::var(k).ok().map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
                    .ok_or_else(|| anyhow::anyhow!("{k} is required when WORKDAY_MODE=soap"));
                let endpoint = need("WORKDAY_ENDPOINT")?;
                if !allowed_endpoint(&Url::parse(&endpoint)?) {
                    anyhow::bail!("WORKDAY_ENDPOINT must be an https URL on *.workday.com or *.myworkday.com");
                }
                let template_path = std::env::var("WORKDAY_REQUEST_TEMPLATE")
                    .unwrap_or_else(|_| "./workday/request-template.xml".into());
                let template = std::fs::read_to_string(&template_path)
                    .map_err(|e| anyhow::anyhow!("Cannot read WORKDAY_REQUEST_TEMPLATE {template_path}: {e}"))?;
                check_template(&template)?;
                let timeout = std::env::var("WORKDAY_TIMEOUT_SECONDS").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(60);
                Ok(Self::Soap(Box::new(SoapConfig {
                    // A failed connection means nothing was sent (safe to retry); a timeout after
                    // sending does not, so the two are kept apart.
                    client: reqwest::Client::builder().connect_timeout(Duration::from_secs(10))
                        .timeout(Duration::from_secs(timeout)).build()?,
                    endpoint,
                    username: need("WORKDAY_USERNAME")?,
                    password: need("WORKDAY_PASSWORD")?,
                    template,
                    receipt_element: std::env::var("WORKDAY_RECEIPT_ELEMENT").ok()
                        .filter(|v| !v.trim().is_empty()).unwrap_or_else(|| "ID".into()),
                })))
            }
            other => anyhow::bail!("Unsupported WORKDAY_MODE: {other}; use simulated or soap"),
        }
    }

    pub fn mode(&self) -> &'static str {
        match self { Self::Simulated => "simulated", Self::Soap(_) => "soap" }
    }
    pub fn is_live(&self) -> bool { matches!(self, Self::Soap(_)) }

    pub async fn deliver(&self, draft: &WorkdayDraft) -> Result<DeliveryReceipt, DeliveryError> {
        match self {
            Self::Simulated => SimulatedWorkday.deliver(draft).map_err(|e| DeliveryError::Rejected(e.message())),
            Self::Soap(cfg) => cfg.deliver(draft).await,
        }
    }
}

impl SoapConfig {
    async fn deliver(&self, draft: &WorkdayDraft) -> Result<DeliveryReceipt, DeliveryError> {
        let envelope = envelope(&self.username, &self.password, &render(&self.template, draft));
        let response = self.client.post(&self.endpoint)
            .header("Content-Type", "text/xml; charset=utf-8")
            .body(envelope)
            .send().await
            .map_err(|e| if e.is_connect() || e.is_builder() {
                DeliveryError::Unavailable("could not connect".into())
            } else if e.is_timeout() {
                DeliveryError::Unknown("no response before the timeout".into())
            } else {
                DeliveryError::Unknown("connection dropped".into())
            })?;
        let status = response.status();
        let body = response.text().await.map_err(|_| DeliveryError::Unknown("response could not be read".into()))?;
        if let Some(fault) = find_text(&body, "Fault") {
            // SOAP 1.1 uses <faultstring>, SOAP 1.2 uses <Reason><Text>.
            let message = find_text(&body, "faultstring").or_else(|| find_text(&body, "Text")).unwrap_or(fault);
            return Err(DeliveryError::Rejected(truncate(&message, 300)));
        }
        if !status.is_success() {
            return Err(match status.as_u16() {
                401 | 403 => DeliveryError::Rejected("integration user was not accepted (check WORKDAY_USERNAME/PASSWORD and security domains)".into()),
                429 | 502 | 503 => DeliveryError::Unavailable(format!("HTTP {status}")),
                504 => DeliveryError::Unknown(format!("HTTP {status}")),
                _ => DeliveryError::Rejected(format!("HTTP {status}")),
            });
        }
        let receipt_id = find_text(&body, &self.receipt_element)
            .ok_or_else(|| DeliveryError::Unknown(format!("Workday accepted the request but no <{}> receipt was found", self.receipt_element)))?;
        Ok(DeliveryReceipt {
            review_id: draft.source_review_id.clone(),
            receipt_id: truncate(&receipt_id, 200),
            status: "ACCEPTED_BY_WORKDAY".into(),
            at: chrono::Utc::now().to_rfc3339(),
            system: "workday".into(),
        })
    }
}

fn truncate(s: &str, max: usize) -> String { s.trim().chars().take(max).collect() }

pub fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"), '<' => out.push_str("&lt;"), '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"), '\'' => out.push_str("&apos;"),
            c if (c as u32) < 0x20 && !matches!(c, '\n' | '\r' | '\t') => {} // invalid in XML 1.0
            c => out.push(c),
        }
    }
    out
}

/// Rejects templates that still hold the example marker or use unknown placeholders.
pub fn check_template(t: &str) -> anyhow::Result<()> {
    if t.contains(TEMPLATE_MARKER) {
        anyhow::bail!("The Workday request template is still the example. Replace it with the request body for your tenant (see docs/GO-LIVE.md)");
    }
    let mut rest = t;
    while let Some(start) = rest.find("{{") {
        let end = rest[start..].find("}}").ok_or_else(|| anyhow::anyhow!("Unclosed {{{{ in Workday template"))?;
        let name = rest[start + 2..start + end].trim().trim_start_matches(['#', '/']);
        if !PLACEHOLDERS.contains(&name) && !LISTS.contains(&name) && !LISTS.iter().any(|l| name == format!("{l}_joined")) {
            anyhow::bail!("Unknown placeholder {{{{{name}}}}} in Workday template");
        }
        rest = &rest[start + end + 2..];
    }
    Ok(())
}

/// Fills the template. Scalars: {{employee_id}} etc. Lists repeat a block per item:
/// {{#goals}}<x>{{item}}</x>{{/goals}}, or join with newlines: {{goals_joined}}.
/// Every value is XML-escaped.
pub fn render(template: &str, d: &WorkdayDraft) -> String {
    let lists: [(&str, &Vec<String>); 3] = [("accomplishments", &d.accomplishments), ("goals", &d.goals),
        ("development_areas", &d.development_areas)];
    let mut out = template.to_owned();
    for (name, items) in lists {
        let (open, close) = (format!("{{{{#{name}}}}}"), format!("{{{{/{name}}}}}"));
        while let (Some(s), Some(e)) = (out.find(&open), out.find(&close)) {
            if e < s { break; }
            let block = out[s + open.len()..e].to_owned();
            let repeated: String = items.iter().map(|i| block.replace("{{item}}", &xml_escape(i))).collect();
            out.replace_range(s..e + close.len(), &repeated);
        }
        out = out.replace(&format!("{{{{{name}_joined}}}}"), &xml_escape(&items.join("\n")));
    }
    for (name, value) in [("source_review_id", &d.source_review_id), ("employee_id", &d.employee_id),
        ("review_period", &d.review_period), ("template_id", &d.template_id), ("overall_comments", &d.overall_comments)] {
        out = out.replace(&format!("{{{{{name}}}}}"), &xml_escape(value));
    }
    out
}

pub fn envelope(user: &str, password: &str, body: &str) -> String {
    format!(r#"<?xml version="1.0" encoding="UTF-8"?>
<soapenv:Envelope xmlns:soapenv="http://schemas.xmlsoap.org/soap/envelope/">
  <soapenv:Header>
    <wsse:Security soapenv:mustUnderstand="1" xmlns:wsse="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-secext-1.0.xsd">
      <wsse:UsernameToken>
        <wsse:Username>{}</wsse:Username>
        <wsse:Password Type="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-username-token-profile-1.0#PasswordText">{}</wsse:Password>
      </wsse:UsernameToken>
    </wsse:Security>
  </soapenv:Header>
  <soapenv:Body>
{}
  </soapenv:Body>
</soapenv:Envelope>"#, xml_escape(user), xml_escape(password), body)
}

/// Text of the first element with this local name (namespace prefix ignored).
pub fn find_text(xml: &str, local_name: &str) -> Option<String> {
    let mut reader = Reader::from_str(xml);
    let mut inside = false;
    let mut text = String::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) if e.local_name().as_ref() == local_name.as_bytes() => { inside = true; text.clear(); }
            Ok(Event::Text(t)) if inside => text.push_str(&t.unescape().ok()?),
            Ok(Event::End(e)) if inside && e.local_name().as_ref() == local_name.as_bytes() => {
                return Some(text.trim().to_owned()).filter(|t| !t.is_empty());
            }
            Ok(Event::Eof) | Err(_) => return None,
            _ => {}
        }
    }
}

/// Real adapters implement this contract; kept for the simulated receiver.
pub trait WorkdayDeliveryAdapter {
    fn deliver(&self, draft: &WorkdayDraft) -> Result<DeliveryReceipt, ApiError>;
}

pub struct SimulatedWorkday;
impl WorkdayDeliveryAdapter for SimulatedWorkday {
    fn deliver(&self, draft: &WorkdayDraft) -> Result<DeliveryReceipt, ApiError> {
        if draft.employee_id.is_empty() || draft.template_id.is_empty() {
            return Err(ApiError::Invalid("Missing mock Workday worker/template reference".into()));
        }
        let key = draft.source_review_id.replace('-', "");
        let suffix: String = key.chars().take(12).collect();
        Ok(DeliveryReceipt {
            review_id: draft.source_review_id.clone(),
            receipt_id: format!("MOCK-WD-{suffix}"),
            status: "MOCK_ACCEPTED_NOT_WORKDAY".into(),
            at: chrono::Utc::now().to_rfc3339(),
            system: "simulated".into(),
        })
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
    use axum::{routing::post, Router};

    fn draft() -> WorkdayDraft {
        WorkdayDraft { source_review_id: "r-1".into(), employee_id: "EMP-1".into(), review_period: "2026".into(),
            template_id: "T".into(), accomplishments: vec!["Cut costs <20%> & more".into(), "Shipped v2".into()],
            goals: vec!["Lead migration".into()], development_areas: vec![], overall_comments: "Good \"year\"".into() }
    }

    #[test]
    fn spreadsheet_formula_is_not_executable() {
        assert_eq!(csv_cell("=1+2"), "'=1+2");
        assert_eq!(csv_cell("normal"), "normal");
    }

    #[test]
    fn template_renders_lists_and_escapes_values() {
        let t = "<W>{{employee_id}}</W>{{#accomplishments}}<A>{{item}}</A>{{/accomplishments}}<G>{{goals_joined}}</G>{{#development_areas}}<D>{{item}}</D>{{/development_areas}}<C>{{overall_comments}}</C>";
        check_template(t).unwrap();
        assert_eq!(render(t, &draft()),
            "<W>EMP-1</W><A>Cut costs &lt;20%&gt; &amp; more</A><A>Shipped v2</A><G>Lead migration</G><C>Good &quot;year&quot;</C>");
    }

    #[test]
    fn example_or_misspelled_templates_are_refused() {
        assert!(check_template(&format!("<{TEMPLATE_MARKER}/>")).is_err());
        assert!(check_template("<x>{{employe_id}}</x>").is_err());
    }

    #[test]
    fn only_workday_hosts_or_local_mocks() {
        let ok = |s: &str| allowed_endpoint(&Url::parse(s).unwrap());
        assert!(ok("https://wd2-impl-services1.workday.com/ccx/service/acme/Talent/v44.0"));
        assert!(ok("http://127.0.0.1:9000/mock"));
        assert!(!ok("http://wd2-impl-services1.workday.com/ccx"));
        assert!(!ok("https://workday.com.evil.example/ccx"));
    }

    async fn mock(reply: &'static str, status: u16, delay_ms: u64) -> String {
        let app = Router::new().route("/ws", post(move |body: String| async move {
            assert!(body.contains("<wsse:Username>ISU@acme</wsse:Username>"));
            assert!(body.contains("Cut costs &lt;20%&gt;"));
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            (axum::http::StatusCode::from_u16(status).unwrap(), reply)
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}/ws")
    }
    fn client(endpoint: String, timeout_ms: u64) -> SoapConfig {
        SoapConfig { client: reqwest::Client::builder().connect_timeout(Duration::from_millis(500))
                .timeout(Duration::from_millis(timeout_ms)).build().unwrap(),
            endpoint, username: "ISU@acme".into(), password: "secret".into(),
            template: "<bsvc:Request>{{#accomplishments}}<bsvc:A>{{item}}</bsvc:A>{{/accomplishments}}</bsvc:Request>".into(),
            receipt_element: "ID".into() }
    }

    #[tokio::test]
    async fn soap_success_fault_and_timeout_are_classified() {
        let ok = mock(r#"<env:Envelope xmlns:env="http://schemas.xmlsoap.org/soap/envelope/"><env:Body><wd:Response xmlns:wd="urn:com.workday/bsvc"><wd:Employee_Review_Reference><wd:ID wd:type="WID">abc123</wd:ID></wd:Employee_Review_Reference></wd:Response></env:Body></env:Envelope>"#, 200, 0).await;
        let r = client(ok, 2000).deliver(&draft()).await.unwrap();
        assert_eq!((r.receipt_id.as_str(), r.system.as_str()), ("abc123", "workday"));

        let fault = mock(r#"<env:Envelope xmlns:env="http://schemas.xmlsoap.org/soap/envelope/"><env:Body><env:Fault><faultcode>SOAP-ENV:Client.validationError</faultcode><faultstring>Invalid ID value. 'EMP-1' is not a valid ID value for type = 'Employee_ID'</faultstring></env:Fault></env:Body></env:Envelope>"#, 500, 0).await;
        let e = client(fault, 2000).deliver(&draft()).await.unwrap_err();
        assert!(matches!(e, DeliveryError::Rejected(ref m) if m.contains("not a valid ID")) && e.retryable());

        let slow = mock("<ok/>", 200, 1500).await;
        let e = client(slow, 300).deliver(&draft()).await.unwrap_err();
        assert!(matches!(e, DeliveryError::Unknown(_)) && !e.retryable());

        let e = client("http://127.0.0.1:9/ws".into(), 2000).deliver(&draft()).await.unwrap_err();
        assert!(matches!(e, DeliveryError::Unavailable(_)) && e.retryable());
    }
}
