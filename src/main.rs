mod agent;
mod ai;
mod auth;
mod db;
mod delivery;
mod domain;
mod intake;
mod teams;
mod workday;

use agent::{Agent, AgentConfig};
use ai::Processor;
use auth::{bearer, entra_principal, token_equal, AuthMode, Principal};
use axum::{
    extract::{DefaultBodyLimit, Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{Html, IntoResponse},
    routing::{get, post, put},
    Json, Router,
};
use db::Store;
use delivery::Outcome;
use domain::{status, validate_label, validate_new, ApiError, Approval, NewReview, Review, SendBack};
use intake::{FormsSubmission, IntakeDefaults};
use serde_json::{json, Value};
use std::sync::Arc;
use teams::Notifier;
use tokio::sync::Notify;
use tracing_subscriber::EnvFilter;
use workday::WorkdayClient;

#[derive(Clone)]
struct AppState {
    db: Store,
    processor: Processor,
    notifier: Notifier,
    workday: WorkdayClient,
    auth: AuthMode,
    agent: AgentConfig,
    wake: Arc<Notify>,
    intake: IntakeDefaults,
    employee_token: Arc<String>,
    reviewer_token: Arc<String>,
    /// Optional dedicated token for the Power Automate flow, so it can be rotated separately.
    intake_token: Option<Arc<String>>,
}

/// A reviewer: the shared reviewer token (token mode) or an Entra ID user with a ReviewFlow role.
fn reviewer(headers: &HeaderMap, state: &AppState) -> Result<Principal, ApiError> {
    match &state.auth {
        AuthMode::Token => match bearer(headers) {
            Some(t) if token_equal(t, &state.reviewer_token) =>
                Ok(Principal { name: "Reviewer token".into(), email: None, hr: true, verified: false }),
            _ => Err(ApiError::Unauthorized),
        },
        AuthMode::Entra { reviewer_role, hr_role } => entra_principal(headers, reviewer_role, hr_role),
    }
}

/// Anyone allowed to create reviews: the employee or intake token (Power Automate), or a reviewer.
fn submitter(headers: &HeaderMap, state: &AppState) -> Result<(), ApiError> {
    if let Some(t) = bearer(headers) {
        if token_equal(t, &state.employee_token) || state.intake_token.as_ref().is_some_and(|i| token_equal(t, i)) {
            return Ok(());
        }
    }
    reviewer(headers, state).map(|_| ())
}

/// Loads a review and checks the caller may see it (HR: all; managers: their team).
async fn accessible(state: &AppState, who: &Principal, id: String) -> Result<Review, ApiError> {
    let review = state.db.get(id).await?;
    who.check(&review)?;
    Ok(review)
}

async fn index() -> Html<&'static str> { Html(include_str!("../static/index.html")) }
async fn javascript() -> impl IntoResponse {
    ([(header::CONTENT_TYPE,"application/javascript; charset=utf-8")],
        include_str!("../static/app.js"))
}
async fn stylesheet() -> impl IntoResponse {
    ([(header::CONTENT_TYPE,"text/css; charset=utf-8")],
        include_str!("../static/style.css"))
}
async fn health(State(state): State<AppState>) -> Json<Value> {
    Json(json!({
        "status": "ok",
        "workday": if state.workday.is_live() { "live" } else { "staging-only" },
        "workday_mode": state.workday.mode(),
        "auth_mode": state.auth.name(),
        "ai_mode": state.processor.mode(),
        "agent": {
            "enabled": state.agent.enabled,
            "auto_deliver": state.agent.auto_deliver,
            "poll_seconds": state.agent.poll.as_secs(),
        },
        "teams_notifications": state.notifier.enabled(),
    }))
}

/// Tells the web page how to sign in and who is signed in.
async fn me(State(state): State<AppState>, headers: HeaderMap) -> Json<Value> {
    let login_url = "/.auth/login/aad?post_login_redirect_uri=/";
    match reviewer(&headers, &state) {
        Ok(p) => Json(json!({"auth_mode": state.auth.name(), "signed_in": true, "user": p})),
        Err(ApiError::Forbidden(message)) => Json(json!({"auth_mode": state.auth.name(), "signed_in": true,
            "user": null, "error": message, "logout_url": "/.auth/logout"})),
        Err(_) => Json(json!({"auth_mode": state.auth.name(), "signed_in": false,
            "login_url": matches!(state.auth, AuthMode::Entra{..}).then_some(login_url)})),
    }
}

async fn create_review(state: &AppState, input: NewReview) -> Result<(StatusCode, Json<Review>), ApiError> {
    validate_new(&input)?;
    let review = state.db.create(input).await?;
    state.wake.notify_one();
    Ok((StatusCode::CREATED, Json(review)))
}

async fn submit(State(state):State<AppState>, headers:HeaderMap, Json(input):Json<NewReview>)
    -> Result<impl IntoResponse, ApiError> {
    submitter(&headers, &state)?;
    create_review(&state, input).await
}
/// Called by the Power Automate flow for each submitted Teams (Microsoft Forms) review.
async fn intake_forms(State(state):State<AppState>, headers:HeaderMap, Json(input):Json<FormsSubmission>)
    -> Result<impl IntoResponse, ApiError> {
    submitter(&headers, &state)?;
    let review = intake::to_new_review(input, &state.intake)?;
    let (code, Json(review)) = create_review(&state, review).await?;
    // Small response for the flow: enough to post a confirmation to the employee.
    Ok((code, Json(json!({
        "id": review.id,
        "status": review.status,
        "employee_id": review.employee_id,
        "review_period": review.review_period,
        "manager_email": review.manager_email,
        "link": state.notifier.review_link(&review.id),
    }))))
}
async fn all_reviews(State(state):State<AppState>, headers:HeaderMap)
    -> Result<Json<Vec<Review>>,ApiError> {
    let who = reviewer(&headers,&state)?;
    Ok(Json(state.db.list().await?.into_iter().filter(|r| who.can_access(r)).collect()))
}
async fn get_review(State(state):State<AppState>, headers:HeaderMap, Path(id):Path<String>)
    -> Result<Json<Review>,ApiError> {
    let who = reviewer(&headers,&state)?;
    Ok(Json(accessible(&state, &who, id).await?))
}
/// Manual summarize. The agent normally does this on its own.
async fn analyze(State(state):State<AppState>, headers:HeaderMap, Path(id):Path<String>)
    -> Result<Json<Review>,ApiError> {
    let who = reviewer(&headers,&state)?;
    let review = accessible(&state, &who, id.clone()).await?;
    if review.status != status::SUBMITTED {
        return Err(ApiError::Conflict("Only SUBMITTED reviews can be analyzed".into()));
    }
    let output = state.processor.analyze(&review.raw_text).await?;
    Ok(Json(state.db.save_analysis(id, output, "reviewer (manual)").await?))
}
async fn approve(State(state):State<AppState>, headers:HeaderMap, Path(id):Path<String>,
                 Json(body):Json<Approval>) -> Result<Json<Review>,ApiError> {
    let who = reviewer(&headers,&state)?;
    accessible(&state, &who, id.clone()).await?;
    // With Entra ID the signed-in identity is recorded; a typed name is only used in demo mode.
    let name = if who.verified { Some(who.email.clone().unwrap_or_else(|| who.name.clone())) } else { body.approved_by };
    if let Some(n) = &name { validate_label("approved_by", n, 254)?; }
    let actor = who.actor(name.as_deref());
    let review = state.db.approve(id, body.revised, name, actor).await?;
    state.wake.notify_one();
    Ok(Json(review))
}
async fn send_back(State(state):State<AppState>, headers:HeaderMap, Path(id):Path<String>,
                   Json(body):Json<SendBack>) -> Result<Json<Review>,ApiError> {
    let who = reviewer(&headers,&state)?;
    accessible(&state, &who, id.clone()).await?;
    if body.reason.trim().is_empty() || body.reason.chars().count() > 1000 {
        return Err(ApiError::Invalid("Give a reason between 1 and 1,000 characters".into()));
    }
    if let Some(name) = &body.returned_by { validate_label("returned_by", name, 100)?; }
    let actor = who.actor(body.returned_by.as_deref());
    Ok(Json(state.db.send_back(id, body.reason.trim().to_owned(), actor).await?))
}
async fn retry(State(state):State<AppState>, headers:HeaderMap, Path(id):Path<String>)
    -> Result<Json<Review>,ApiError> {
    let who = reviewer(&headers,&state)?;
    accessible(&state, &who, id.clone()).await?;
    let review = state.db.retry(id, who.actor(None)).await?;
    state.wake.notify_one();
    Ok(Json(review))
}
async fn stage(State(state):State<AppState>, headers:HeaderMap, Path(id):Path<String>)
    -> Result<Json<Review>,ApiError> {
    let who = reviewer(&headers,&state)?;
    accessible(&state, &who, id.clone()).await?;
    Ok(Json(state.db.stage(id, who.actor(None)).await?))
}
/// Manual "Send to Workday". Goes through the configured adapter (simulated or live).
async fn deliver(State(state):State<AppState>, headers:HeaderMap, Path(id):Path<String>)
    -> Result<impl IntoResponse,ApiError> {
    let who = reviewer(&headers,&state)?;
    accessible(&state, &who, id.clone()).await?;
    match delivery::deliver(&state.db, &state.workday, id, who.actor(None), state.agent.max_attempts).await? {
        Outcome::Delivered(review, receipt) => {
            state.notifier.notify(teams::Notice::Delivered, &review).await;
            Ok(Json(receipt))
        }
        Outcome::AlreadyDelivered(receipt) => Ok(Json(receipt)),
        Outcome::Failed(_, message) => Err(ApiError::Upstream(message)),
    }
}
async fn audit(State(state):State<AppState>, headers:HeaderMap, Path(id):Path<String>)
    -> Result<impl IntoResponse,ApiError> {
    let who = reviewer(&headers,&state)?;
    accessible(&state, &who, id.clone()).await?;
    Ok(Json(state.db.audit(id).await?))
}
async fn export_staging(State(state):State<AppState>, headers:HeaderMap)
    -> Result<impl IntoResponse,ApiError> {
    reviewer(&headers,&state)?.require_hr()?;
    let drafts = state.db.staged().await?;
    let data = workday::staging_csv(&drafts)?;
    Ok(([(header::CONTENT_TYPE,"text/csv; charset=utf-8"),
         (header::CONTENT_DISPOSITION,"attachment; filename=reviewflow-staging.csv")], data))
}

fn read_token(name: &str) -> anyhow::Result<Option<String>> {
    match std::env::var(name).ok().map(|v| v.trim().to_owned()).filter(|v| !v.is_empty()) {
        None => Ok(None),
        Some(v) if v.len() < 16 => anyhow::bail!("{name} must be at least 16 characters"),
        Some(v) => Ok(Some(v)),
    }
}

/// One line per component: what it's set to, and whether that's demo-only or ready for live use.
fn readiness(state: &AppState, bind: &str) -> Vec<(&'static str, String, bool)> {
    let loopback = bind.starts_with("127.") || bind.starts_with("localhost") || bind.starts_with("[::1]");
    vec![
        ("Sign-in", match &state.auth {
            AuthMode::Token => "shared tokens (demo only)".into(),
            AuthMode::Entra { reviewer_role, hr_role } => format!("Entra ID via Easy Auth, roles {reviewer_role} / {hr_role}"),
        }, matches!(state.auth, AuthMode::Entra { .. })),
        ("Summaries", match state.processor.mode() {
            "azure" => "Azure OpenAI".into(), "ollama" => "local Ollama (laptop only)".into(), _ => "rules, no AI".into(),
        }, state.processor.mode() == "azure"),
        ("Workday", if state.workday.is_live() { "SOAP to tenant".into() } else { "simulated receiver".into() }, state.workday.is_live()),
        ("Teams intake", if state.intake_token.is_some() { "dedicated INTAKE_API_TOKEN".into() } else { "shares EMPLOYEE_API_TOKEN".into() },
            state.intake_token.is_some()),
        ("Teams notifications", if state.notifier.enabled() { "Workflows webhook".into() } else { "off".into() }, state.notifier.enabled()),
        ("Review agent", if state.agent.enabled { format!("on, auto-deliver {}", state.agent.auto_deliver) } else { "off (manual)".into() }, state.agent.enabled),
        ("Network", format!("listening on {bind}"), !loopback),
    ]
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter(
        EnvFilter::try_from_default_env().unwrap_or_else(|_| "reviewflow=info,tower_http=warn".into())
    ).init();
    let check_only = std::env::args().any(|a| a == "--check-config");
    let employee = read_token("EMPLOYEE_API_TOKEN")?
        .ok_or_else(|| anyhow::anyhow!("EMPLOYEE_API_TOKEN is required"))?;
    let reviewer_token = read_token("REVIEWER_API_TOKEN")?
        .ok_or_else(|| anyhow::anyhow!("REVIEWER_API_TOKEN is required"))?;
    let intake_token = read_token("INTAKE_API_TOKEN")?;
    let mut tokens = vec![&employee, &reviewer_token];
    tokens.extend(intake_token.as_ref());
    if tokens.iter().enumerate().any(|(i, a)| tokens[i + 1..].contains(a)) {
        anyhow::bail!("EMPLOYEE_API_TOKEN, REVIEWER_API_TOKEN and INTAKE_API_TOKEN must all be different");
    }
    let path = std::env::var("SQLITE_PATH").unwrap_or_else(|_| "./reviewflow.db".into());
    let bind = std::env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".into());
    let state = AppState {
        db: Store::init(path)?, processor: Processor::from_env()?, notifier: Notifier::from_env()?,
        workday: WorkdayClient::from_env()?, auth: AuthMode::from_env()?,
        agent: AgentConfig::from_env()?, wake: Arc::new(Notify::new()), intake: IntakeDefaults::from_env(),
        employee_token: Arc::new(employee), reviewer_token: Arc::new(reviewer_token),
        intake_token: intake_token.map(Arc::new),
    };

    let report = readiness(&state, &bind);
    if check_only {
        println!("Configuration is valid. Component status:");
        for (name, value, live) in &report {
            println!("  [{}] {name:<20} {value}", if *live { "live" } else { "demo" });
        }
        let demo = report.iter().filter(|(_, _, live)| !live).count();
        println!("{}", if demo == 0 { "Ready for live use.".to_owned() } else { format!("{demo} component(s) still in demo mode.") });
        return Ok(());
    }
    for (name, value, live) in &report {
        tracing::info!(component = name, live, "{value}");
    }
    if matches!(state.auth, AuthMode::Token) && !bind.starts_with("127.") && !bind.starts_with("localhost") {
        tracing::warn!(%bind, "Shared-token sign-in is listening beyond this computer. Use AUTH_MODE=entra for anything but a local demo");
    }

    if state.agent.enabled {
        Agent { db: state.db.clone(), processor: state.processor.clone(), notifier: state.notifier.clone(),
                workday: state.workday.clone(), config: state.agent.clone(), wake: state.wake.clone() }.spawn();
    } else {
        tracing::info!("Review agent disabled (AGENT_ENABLED=false); use the manual buttons or API");
    }
    let app = Router::new()
        .route("/",get(index))
        .route("/assets/app.js",get(javascript))
        .route("/assets/style.css",get(stylesheet))
        .route("/healthz",get(health))
        .route("/api/v1/me",get(me))
        .route("/api/v1/intake/microsoft-forms",post(intake_forms))
        .route("/api/v1/reviews",post(submit).get(all_reviews))
        .route("/api/v1/reviews/{id}",get(get_review))
        .route("/api/v1/reviews/{id}/analyze",post(analyze))
        .route("/api/v1/reviews/{id}/approve",put(approve))
        .route("/api/v1/reviews/{id}/send-back",post(send_back))
        .route("/api/v1/reviews/{id}/retry",post(retry))
        .route("/api/v1/reviews/{id}/stage",post(stage))
        .route("/api/v1/reviews/{id}/audit",get(audit))
        .route("/api/v1/reviews/{id}/deliver",post(deliver))
        .route("/api/v1/reviews/{id}/mock-deliver",post(deliver)) // v0.2 name, kept for scripts
        .route("/api/v1/workday/staging.csv",get(export_staging))
        .layer(DefaultBodyLimit::max(128 * 1024))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!(%bind,"ReviewFlow running");
    axum::serve(listener, app).await?;
    Ok(())
}
