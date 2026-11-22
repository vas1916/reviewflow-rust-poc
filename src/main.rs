mod ai;
mod db;
mod domain;
mod workday;

use ai::Processor;
use axum::{
    extract::{DefaultBodyLimit, Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{Html, IntoResponse},
    routing::{get, post, put},
    Json, Router,
};
use db::Store;
use domain::{validate_new, ApiError, Approval, NewReview, Review};
use serde_json::{json, Value};
use std::sync::Arc;
use subtle::ConstantTimeEq;
use tracing_subscriber::EnvFilter;

#[derive(Clone)]
struct AppState {
    db: Store,
    processor: Processor,
    employee_token: Arc<String>,
    reviewer_token: Arc<String>,
}

enum Role { Submitter, Reviewer }

fn token_equal(presented: &str, expected: &str) -> bool {
    bool::from(presented.as_bytes().ct_eq(expected.as_bytes()))
}

fn authorize(headers: &HeaderMap, state: &AppState, role: Role) -> Result<(), ApiError> {
    let provided = headers.get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .ok_or(ApiError::Unauthorized)?;
    if token_equal(provided, &state.reviewer_token) ||
      (matches!(role, Role::Submitter) && token_equal(provided, &state.employee_token)) {
        Ok(())
    } else { Err(ApiError::Unauthorized) }
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
    Json(json!({"status":"ok","workday":"staging-only","ai_mode":state.processor.mode()}))
}

async fn submit(State(state):State<AppState>, headers:HeaderMap, Json(input):Json<NewReview>)
    -> Result<impl IntoResponse, ApiError> {
    authorize(&headers, &state, Role::Submitter)?;
    validate_new(&input)?;
    let review = state.db.create(input).await?;
    Ok((StatusCode::CREATED, Json(review)))
}
async fn all_reviews(State(state):State<AppState>, headers:HeaderMap)
    -> Result<Json<Vec<Review>>,ApiError> {
    authorize(&headers,&state,Role::Reviewer)?;
    Ok(Json(state.db.list().await?))
}
async fn get_review(State(state):State<AppState>, headers:HeaderMap, Path(id):Path<String>)
    -> Result<Json<Review>,ApiError> {
    authorize(&headers,&state,Role::Reviewer)?;
    Ok(Json(state.db.get(id).await?))
}
async fn analyze(State(state):State<AppState>, headers:HeaderMap, Path(id):Path<String>)
    -> Result<Json<Review>,ApiError> {
    authorize(&headers,&state,Role::Reviewer)?;
    let review = state.db.get(id.clone()).await?;
    if review.status != "SUBMITTED" {
        return Err(ApiError::Conflict("Only SUBMITTED reviews can be analyzed".into()));
    }
    let output = state.processor.analyze(&review.raw_text).await?;
    Ok(Json(state.db.save_analysis(id, output).await?))
}
async fn approve(State(state):State<AppState>, headers:HeaderMap, Path(id):Path<String>,
                 Json(body):Json<Approval>) -> Result<Json<Review>,ApiError> {
    authorize(&headers,&state,Role::Reviewer)?;
    Ok(Json(state.db.approve(id,body.revised).await?))
}
async fn stage(State(state):State<AppState>, headers:HeaderMap, Path(id):Path<String>)
    -> Result<Json<Review>,ApiError> {
    authorize(&headers,&state,Role::Reviewer)?;
    Ok(Json(state.db.stage(id).await?))
}
async fn mock_deliver(State(state):State<AppState>, headers:HeaderMap, Path(id):Path<String>)
    -> Result<impl IntoResponse,ApiError> {
    authorize(&headers,&state,Role::Reviewer)?;
    Ok(Json(state.db.mock_deliver(id).await?))
}
async fn audit(State(state):State<AppState>, headers:HeaderMap, Path(id):Path<String>)
    -> Result<impl IntoResponse,ApiError> {
    authorize(&headers,&state,Role::Reviewer)?;
    Ok(Json(state.db.audit(id).await?))
}
async fn export_staging(State(state):State<AppState>, headers:HeaderMap)
    -> Result<impl IntoResponse,ApiError> {
    authorize(&headers,&state,Role::Reviewer)?;
    let drafts = state.db.staged().await?;
    let data = workday::staging_csv(&drafts)?;
    Ok(([(header::CONTENT_TYPE,"text/csv; charset=utf-8"),
         (header::CONTENT_DISPOSITION,"attachment; filename=reviewflow-staging.csv")], data))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter(
        EnvFilter::try_from_default_env().unwrap_or_else(|_| "reviewflow=info,tower_http=warn".into())
    ).init();
    let employee = std::env::var("EMPLOYEE_API_TOKEN")?;
    let reviewer = std::env::var("REVIEWER_API_TOKEN")?;
    if employee.len() < 16 || reviewer.len() < 16 || employee == reviewer {
        anyhow::bail!("Set distinct EMPLOYEE_API_TOKEN and REVIEWER_API_TOKEN (>=16 chars each)");
    }
    let path = std::env::var("SQLITE_PATH").unwrap_or_else(|_| "./reviewflow.db".into());
    let bind = std::env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".into());
    let state = AppState { db:Store::init(path)?, processor:Processor::from_env()?,
        employee_token:Arc::new(employee), reviewer_token:Arc::new(reviewer) };
    let app = Router::new()
        .route("/",get(index))
        .route("/assets/app.js",get(javascript))
        .route("/assets/style.css",get(stylesheet))
        .route("/healthz",get(health))
        .route("/api/v1/reviews",post(submit).get(all_reviews))
        .route("/api/v1/reviews/{id}",get(get_review))
        .route("/api/v1/reviews/{id}/analyze",post(analyze))
        .route("/api/v1/reviews/{id}/approve",put(approve))
        .route("/api/v1/reviews/{id}/stage",post(stage))
        .route("/api/v1/reviews/{id}/audit",get(audit))
        .route("/api/v1/reviews/{id}/mock-deliver",post(mock_deliver))
        .route("/api/v1/workday/staging.csv",get(export_staging))
        .layer(DefaultBodyLimit::max(128 * 1024))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!(%bind,"ReviewFlow POC running; Workday staging only, no direct tenant write");
    axum::serve(listener, app).await?;
    Ok(())
}
