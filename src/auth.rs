//! Who is calling, and what they may see.
//!
//! `AUTH_MODE=token` (default, demo): shared bearer tokens. The reviewer token acts as HR.
//! `AUTH_MODE=entra`: Microsoft Entra ID sign-in handled by Azure App Service / Container Apps
//! built-in authentication ("Easy Auth"). The platform signs users in and passes the verified
//! identity in the `X-MS-CLIENT-PRINCIPAL` header, which it strips from incoming requests.
//! Only use this mode when the app is reachable exclusively through that platform ingress.
//!
//! Roles come from Entra app roles: `ENTRA_HR_ROLE` sees everything; `ENTRA_REVIEWER_ROLE`
//! (managers) sees only reviews whose `manager_email` matches their sign-in.

use crate::domain::{ApiError, Review};
use axum::http::{header, HeaderMap};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;

#[derive(Clone, Debug)]
pub enum AuthMode {
    Token,
    Entra { reviewer_role: String, hr_role: String },
}

impl AuthMode {
    pub fn from_env() -> anyhow::Result<Self> {
        let var = |k: &str, d: &str| std::env::var(k).ok().map(|v| v.trim().to_owned()).filter(|v| !v.is_empty()).unwrap_or_else(|| d.into());
        match var("AUTH_MODE", "token").to_ascii_lowercase().as_str() {
            "token" => Ok(Self::Token),
            "entra" => Ok(Self::Entra {
                reviewer_role: var("ENTRA_REVIEWER_ROLE", "ReviewFlow.Reviewer"),
                hr_role: var("ENTRA_HR_ROLE", "ReviewFlow.HR"),
            }),
            other => anyhow::bail!("Unsupported AUTH_MODE: {other}; use token or entra"),
        }
    }
    pub fn name(&self) -> &'static str { match self { Self::Token => "token", Self::Entra { .. } => "entra" } }
}

#[derive(Clone, Debug, Serialize)]
pub struct Principal {
    pub name: String,
    pub email: Option<String>,
    /// HR sees and acts on every review; other reviewers only on their own team's.
    pub hr: bool,
    /// True when the identity comes from Entra ID rather than a shared token.
    pub verified: bool,
}

impl Principal {
    pub fn can_access(&self, review: &Review) -> bool {
        self.hr || match (&self.email, &review.manager_email) {
            (Some(me), Some(manager)) => me.eq_ignore_ascii_case(manager),
            _ => false,
        }
    }
    pub fn check(&self, review: &Review) -> Result<(), ApiError> {
        // Same answer as "not found" so reviewers can't probe other teams' review IDs.
        if self.can_access(review) { Ok(()) } else { Err(ApiError::NotFound) }
    }
    pub fn require_hr(&self) -> Result<(), ApiError> {
        if self.hr { Ok(()) } else { Err(ApiError::Forbidden("Only HR can do this".into())) }
    }
    /// Label for the audit log.
    pub fn actor(&self, typed_name: Option<&str>) -> String {
        if self.verified {
            format!("{} (Entra ID)", self.email.as_deref().unwrap_or(&self.name))
        } else {
            match typed_name.map(str::trim).filter(|n| !n.is_empty()) {
                Some(n) => format!("{n} (reviewer, unverified POC identity)"),
                None => "reviewer token (POC only)".into(),
            }
        }
    }
}

#[derive(Deserialize)]
struct ClientPrincipal {
    #[serde(default)]
    claims: Vec<Claim>,
    #[serde(default)]
    role_typ: Option<String>,
}
#[derive(Deserialize)]
struct Claim { typ: String, val: String }

/// Decodes the Easy Auth principal header into a reviewer, if they hold a ReviewFlow role.
pub fn entra_principal(headers: &HeaderMap, reviewer_role: &str, hr_role: &str) -> Result<Principal, ApiError> {
    let raw = headers.get("x-ms-client-principal").and_then(|h| h.to_str().ok()).ok_or(ApiError::Unauthorized)?;
    let decoded = STANDARD.decode(raw.trim()).map_err(|_| ApiError::Unauthorized)?;
    let p: ClientPrincipal = serde_json::from_slice(&decoded).map_err(|_| ApiError::Unauthorized)?;
    let role_typ = p.role_typ.unwrap_or_else(|| "roles".into());
    let claim = |types: &[&str]| p.claims.iter().find(|c| types.contains(&c.typ.as_str())).map(|c| c.val.clone());
    let roles: Vec<&str> = p.claims.iter()
        .filter(|c| c.typ == role_typ || c.typ == "roles" || c.typ.ends_with("/claims/role"))
        .map(|c| c.val.as_str()).collect();
    let hr = roles.contains(&hr_role);
    if !hr && !roles.contains(&reviewer_role) {
        return Err(ApiError::Forbidden(format!("Your account needs the {reviewer_role} or {hr_role} role in Entra ID")));
    }
    let email = claim(&["preferred_username", "email", "upn",
        "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/upn",
        "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/emailaddress"])
        .or_else(|| headers.get("x-ms-client-principal-name").and_then(|h| h.to_str().ok()).map(str::to_owned));
    let name = claim(&["name"]).or_else(|| email.clone()).unwrap_or_else(|| "Signed-in user".into());
    Ok(Principal { name, email, hr, verified: true })
}

pub fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers.get(header::AUTHORIZATION).and_then(|h| h.to_str().ok()).and_then(|h| h.strip_prefix("Bearer "))
}

pub fn token_equal(presented: &str, expected: &str) -> bool {
    bool::from(presented.as_bytes().ct_eq(expected.as_bytes()))
}

#[cfg(test)]
pub fn encode_principal(claims: &[(&str, &str)]) -> String {
    let claims: Vec<_> = claims.iter().map(|(t, v)| serde_json::json!({"typ": t, "val": v})).collect();
    STANDARD.encode(serde_json::json!({"auth_typ":"aad","claims":claims,"name_typ":"name","role_typ":"roles"}).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(value: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("x-ms-client-principal", value.parse().unwrap());
        h
    }
    fn review(manager: Option<&str>) -> Review {
        Review { id: "1".into(), employee_id: "E".into(), review_period: "2026".into(), template_id: "T".into(),
            source_system: "s".into(), source_id: "x".into(), raw_text: String::new(), extracted: None, approved: None,
            status: "EXTRACTED".into(), created_at: String::new(), updated_at: String::new(), submitted_by: None,
            approved_by: None, return_reason: None, attempts: 0, last_error: None,
            manager_email: manager.map(str::to_owned), workday_receipt_id: None }
    }

    #[test]
    fn manager_sees_only_their_team_and_hr_sees_all() {
        let mgr = entra_principal(&headers(&encode_principal(&[("name","Dana"),("preferred_username","dana@contoso.com"),("roles","ReviewFlow.Reviewer")])),
            "ReviewFlow.Reviewer", "ReviewFlow.HR").unwrap();
        assert!(!mgr.hr && mgr.verified);
        assert!(mgr.can_access(&review(Some("DANA@contoso.com"))));
        assert!(!mgr.can_access(&review(Some("sam@contoso.com"))));
        assert!(!mgr.can_access(&review(None)));
        let hr = entra_principal(&headers(&encode_principal(&[("preferred_username","hr@contoso.com"),("roles","ReviewFlow.HR")])),
            "ReviewFlow.Reviewer", "ReviewFlow.HR").unwrap();
        assert!(hr.hr && hr.can_access(&review(None)));
        assert_eq!(hr.actor(Some("ignored")), "hr@contoso.com (Entra ID)");
    }

    #[test]
    fn users_without_a_role_or_with_a_bad_header_are_refused() {
        let none = encode_principal(&[("preferred_username","x@contoso.com")]);
        assert!(matches!(entra_principal(&headers(&none), "R", "H"), Err(ApiError::Forbidden(_))));
        assert!(matches!(entra_principal(&headers("not-base64!"), "R", "H"), Err(ApiError::Unauthorized)));
        assert!(matches!(entra_principal(&HeaderMap::new(), "R", "H"), Err(ApiError::Unauthorized)));
    }
}
