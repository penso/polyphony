use std::{path::PathBuf, sync::Arc};

use async_graphql_axum::{GraphQLRequest, GraphQLResponse, GraphQLSubscription};
use axum::{
    Form, Router,
    extract::{Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Redirect},
    routing::{get, post},
};
use axum_login::AuthManagerLayer;
use minijinja::Environment;
use polyphony_core::RuntimeSnapshot;
use polyphony_orchestrator::RuntimeCommand;
use polyphony_workflow::WebhooksConfig;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};
use tower_http::services::ServeDir;
use tower_sessions::SessionManagerLayer;
use tower_sessions_sqlx_store::SqliteStore;

use crate::{auth, graphql, templates, webhooks};

#[derive(Clone)]
struct AppState {
    schema: graphql::PolyphonySchema,
    snapshot_rx: watch::Receiver<RuntimeSnapshot>,
    template_env: Arc<Environment<'static>>,
    workflow_path: PathBuf,
    auth_backend: Option<auth::Backend>,
    webhooks_config: Option<WebhooksConfig>,
}

/// Build the full httpd router.
///
/// When `auth_layer` is `Some`, dashboard pages require login.
/// Webhook routes are always outside the auth layer (they have their own auth).
pub fn build_router(
    snapshot_rx: watch::Receiver<RuntimeSnapshot>,
    command_tx: mpsc::UnboundedSender<RuntimeCommand>,
    template_dir: PathBuf,
    workflow_path: PathBuf,
    auth_backend: Option<auth::Backend>,
    auth_layer: Option<AuthManagerLayer<auth::Backend, SqliteStore>>,
    session_layer: Option<SessionManagerLayer<SqliteStore>>,
    webhooks_config: Option<WebhooksConfig>,
) -> Router {
    let schema = graphql::build_schema(snapshot_rx.clone(), command_tx.clone());
    let template_env = Arc::new(templates::build_env(&template_dir));

    let state = AppState {
        schema: schema.clone(),
        snapshot_rx,
        template_env: template_env.clone(),
        workflow_path,
        auth_backend,
        webhooks_config: webhooks_config.clone(),
    };

    // Static file serving (CSS, JS, etc.)
    let static_dir = template_dir
        .parent()
        .map(|p| p.join("static"))
        .unwrap_or_else(|| template_dir.join("../static"));

    // SSR + GraphQL pages
    let dashboard_routes = Router::new()
        .nest_service("/static", ServeDir::new(static_dir))
        .route("/", get(page_index))
        .route("/inbox", get(page_inbox))
        .route("/runs", get(page_runs))
        .route("/agents", get(page_agents))
        .route("/outcomes", get(page_outcomes))
        .route("/tasks", get(page_tasks))
        .route("/repos", get(page_repos))
        .route("/users", get(page_users))
        .route("/users/create", post(create_user))
        .route("/users/update", post(update_user))
        .route("/users/delete", post(delete_user))
        .route("/docs", get(page_docs))
        .route("/logs", get(page_logs))
        .route("/graphql", get(graphql_playground).post(graphql_handler))
        .route_service("/graphql/ws", GraphQLSubscription::new(schema))
        .with_state(state);

    // Login/logout routes (always public)
    let login_routes = Router::new()
        .route("/login", get(auth::login_page).post(auth::login_submit))
        .route("/logout", get(auth::logout))
        .with_state(template_env);

    // Webhook routes (own auth, outside dashboard auth)
    let webhook_routes = if let Some(ref config) = webhooks_config {
        if config.enabled {
            webhooks::webhook_router(command_tx, config)
        } else {
            Router::new()
        }
    } else {
        Router::new()
    };

    let mut app = Router::new();

    if let (Some(auth_layer), Some(session_layer)) = (auth_layer, session_layer) {
        // With auth: protect dashboard, keep login + webhooks public
        let protected = dashboard_routes.route_layer(axum_login::login_required!(
            auth::Backend,
            login_url = "/login"
        ));
        app = app
            .merge(protected)
            .merge(login_routes)
            .merge(webhook_routes)
            .layer(auth_layer)
            .layer(session_layer);
    } else {
        // No auth: all routes open, login page not needed
        app = app.merge(dashboard_routes).merge(webhook_routes);
    }

    app
}

// ---------------------------------------------------------------------------
// SSR page handlers
// ---------------------------------------------------------------------------

async fn page_index(State(state): State<AppState>) -> impl IntoResponse {
    render_page(&state, "index.html")
}

async fn page_inbox(State(state): State<AppState>) -> impl IntoResponse {
    render_page(&state, "inbox.html")
}

async fn page_runs(State(state): State<AppState>) -> impl IntoResponse {
    render_page(&state, "runs.html")
}

async fn page_agents(State(state): State<AppState>) -> impl IntoResponse {
    render_page(&state, "agents.html")
}

async fn page_outcomes(State(state): State<AppState>) -> impl IntoResponse {
    render_page(&state, "outcomes.html")
}

async fn page_tasks(State(state): State<AppState>) -> impl IntoResponse {
    render_page(&state, "tasks.html")
}

async fn page_repos(State(state): State<AppState>) -> impl IntoResponse {
    render_page(&state, "repos.html")
}

async fn page_docs(State(state): State<AppState>) -> Result<Html<String>, (StatusCode, String)> {
    let snapshot = state.snapshot_rx.borrow().clone();

    let providers: Vec<serde_json::Value> = state
        .webhooks_config
        .as_ref()
        .filter(|c| c.enabled)
        .map(|c| {
            c.providers
                .iter()
                .map(|(name, p)| {
                    serde_json::json!({
                        "name": name,
                        "auth": p.auth,
                        "header": p.header,
                        "has_secret": !p.secret.is_empty(),
                        "endpoint": format!("/webhooks/{name}"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let webhooks_enabled = state.webhooks_config.as_ref().is_some_and(|c| c.enabled);

    let ctx = serde_json::json!({
        "webhooks_enabled": webhooks_enabled,
        "webhook_providers": providers,
        "dispatch_mode": snapshot.dispatch_mode,
        "generated_at": snapshot.generated_at,
    });

    let tmpl = state.template_env.get_template("docs.html").map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("template error: {e}"),
        )
    })?;
    let rendered = tmpl
        .render(minijinja::Value::from_serialize(&ctx))
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("render error: {e}"),
            )
        })?;
    Ok(Html(rendered))
}

async fn page_logs(State(state): State<AppState>) -> impl IntoResponse {
    render_page(&state, "logs.html")
}

async fn page_users(
    State(state): State<AppState>,
    Query(query): Query<UsersPageQuery>,
) -> impl IntoResponse {
    render_users_page(&state, query, None, StatusCode::OK)
}

async fn create_user(
    State(state): State<AppState>,
    Form(form): Form<CreateUserForm>,
) -> impl IntoResponse {
    let mut users = match load_dashboard_users(&state) {
        Ok(users) => users,
        Err(error) => {
            return render_users_page(
                &state,
                UsersPageQuery::default(),
                Some(error),
                StatusCode::BAD_REQUEST,
            );
        },
    };
    match prepare_user_for_create(&form) {
        Ok(user) => users.push(user),
        Err(error) => {
            return render_users_page(
                &state,
                UsersPageQuery::default(),
                Some(error),
                StatusCode::BAD_REQUEST,
            );
        },
    }
    if let Err(error) = persist_dashboard_users(&state, users).await {
        return render_users_page(
            &state,
            UsersPageQuery::default(),
            Some(error),
            StatusCode::BAD_REQUEST,
        );
    }
    Redirect::to("/users?result=created").into_response()
}

async fn update_user(
    State(state): State<AppState>,
    Form(form): Form<UpdateUserForm>,
) -> impl IntoResponse {
    let mut users = match load_dashboard_users(&state) {
        Ok(users) => users,
        Err(error) => {
            return render_users_page(
                &state,
                UsersPageQuery::default(),
                Some(error),
                StatusCode::BAD_REQUEST,
            );
        },
    };
    let original = form.original_username.trim();
    let Some(index) = users
        .iter()
        .position(|user| usernames_match(&user.username, original))
    else {
        return render_users_page(
            &state,
            UsersPageQuery::default(),
            Some(format!("dashboard user '{original}' was not found")),
            StatusCode::BAD_REQUEST,
        );
    };
    match prepare_user_for_update(&form) {
        Ok(user) => users[index] = user,
        Err(error) => {
            return render_users_page(
                &state,
                UsersPageQuery::default(),
                Some(error),
                StatusCode::BAD_REQUEST,
            );
        },
    }
    if let Err(error) = persist_dashboard_users(&state, users).await {
        return render_users_page(
            &state,
            UsersPageQuery::default(),
            Some(error),
            StatusCode::BAD_REQUEST,
        );
    }
    Redirect::to("/users?result=updated").into_response()
}

async fn delete_user(
    State(state): State<AppState>,
    Form(form): Form<DeleteUserForm>,
) -> impl IntoResponse {
    let mut users = match load_dashboard_users(&state) {
        Ok(users) => users,
        Err(error) => {
            return render_users_page(
                &state,
                UsersPageQuery::default(),
                Some(error),
                StatusCode::BAD_REQUEST,
            );
        },
    };
    let before = users.len();
    users.retain(|user| !usernames_match(&user.username, form.username.trim()));
    if users.len() == before {
        return render_users_page(
            &state,
            UsersPageQuery::default(),
            Some(format!(
                "dashboard user '{}' was not found",
                form.username.trim()
            )),
            StatusCode::BAD_REQUEST,
        );
    }
    if let Err(error) = persist_dashboard_users(&state, users).await {
        return render_users_page(
            &state,
            UsersPageQuery::default(),
            Some(error),
            StatusCode::BAD_REQUEST,
        );
    }
    Redirect::to("/users?result=deleted").into_response()
}

fn render_page(
    state: &AppState,
    template_name: &str,
) -> Result<Html<String>, (StatusCode, String)> {
    let mut snapshot = state.snapshot_rx.borrow().clone();
    // Enrich snapshot with repo registry data (read from disk)
    let registry_path = dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".polyphony")
        .join("repos.json");
    if let Ok(registry) = polyphony_core::load_repo_registry(&registry_path) {
        snapshot.repo_registrations = registry.repos;
    }
    let ctx = templates::snapshot_context(&snapshot);
    let tmpl = state
        .template_env
        .get_template(template_name)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("template error: {e}"),
            )
        })?;
    let rendered = tmpl.render(ctx).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("render error: {e}"),
        )
    })?;
    Ok(Html(rendered))
}

fn render_users_page(
    state: &AppState,
    query: UsersPageQuery,
    error: Option<String>,
    status: StatusCode,
) -> axum::response::Response {
    let mut snapshot = state.snapshot_rx.borrow().clone();
    let registry_path = polyphony_core::default_repo_registry_path();
    if let Ok(registry) = polyphony_core::load_repo_registry(&registry_path) {
        snapshot.repo_registrations = registry.repos;
    }

    let users = match load_dashboard_users(state) {
        Ok(users) => users,
        Err(load_error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to load dashboard users: {load_error}"),
            )
                .into_response();
        },
    };

    let mut ctx = templates::snapshot_context_object(&snapshot);
    ctx.insert(
        "users".into(),
        serde_json::to_value(&users).unwrap_or_else(|_| serde_json::Value::Array(Vec::new())),
    );
    ctx.insert(
        "auth_live".into(),
        serde_json::Value::Bool(state.auth_backend.is_some()),
    );
    ctx.insert(
        "legacy_mode".into(),
        serde_json::Value::Bool(users.iter().any(|user| user.source == "legacy")),
    );
    ctx.insert(
        "success_message".into(),
        serde_json::Value::String(success_message(&query).unwrap_or_default().to_string()),
    );
    ctx.insert(
        "error_message".into(),
        serde_json::Value::String(error.unwrap_or_default()),
    );

    let tmpl = match state.template_env.get_template("users.html") {
        Ok(tmpl) => tmpl,
        Err(template_error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("template error: {template_error}"),
            )
                .into_response();
        },
    };
    match tmpl.render(minijinja::Value::from_serialize(ctx)) {
        Ok(rendered) => (status, Html(rendered)).into_response(),
        Err(render_error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("render error: {render_error}"),
        )
            .into_response(),
    }
}

fn load_dashboard_users(state: &AppState) -> Result<Vec<DashboardUserRow>, String> {
    let daemon = polyphony_workflow::load_daemon_config_from_workflow(&state.workflow_path)
        .map_err(|error| format!("loading daemon config: {error}"))?;
    Ok(effective_dashboard_users(&daemon))
}

async fn persist_dashboard_users(
    state: &AppState,
    mut users: Vec<DashboardUserRow>,
) -> Result<(), String> {
    validate_dashboard_users(&users)?;
    users.sort_by(|left, right| {
        normalized_username(&left.username).cmp(&normalized_username(&right.username))
    });

    let daemon =
        polyphony_workflow::update_daemon_config_in_workflow(&state.workflow_path, |daemon| {
            daemon.auth_token = None;
            daemon.users = users
                .iter()
                .map(|user| polyphony_workflow::DaemonUserConfig {
                    username: user.username.clone(),
                    token: user.token.clone(),
                })
                .collect();
        })
        .map_err(|error| format!("saving daemon config: {error}"))?;

    if let Some(backend) = &state.auth_backend {
        let reloaded_users = effective_dashboard_users(&daemon)
            .into_iter()
            .map(|user| auth::User::new(user.username, user.token))
            .collect();
        backend.replace_users(reloaded_users).await;
    }

    Ok(())
}

fn effective_dashboard_users(config: &polyphony_workflow::DaemonConfig) -> Vec<DashboardUserRow> {
    let mut users = if !config.users.is_empty() {
        config
            .users
            .iter()
            .map(|user| DashboardUserRow::new(user.username.clone(), user.token.clone(), "user"))
            .collect::<Vec<_>>()
    } else {
        config
            .auth_token
            .clone()
            .filter(|token| !token.trim().is_empty())
            .map(|token| vec![DashboardUserRow::new("admin".into(), token, "legacy")])
            .unwrap_or_default()
    };
    users.sort_by(|left, right| {
        normalized_username(&left.username).cmp(&normalized_username(&right.username))
    });
    users
}

fn prepare_user_for_create(form: &CreateUserForm) -> Result<DashboardUserRow, String> {
    Ok(DashboardUserRow::new(
        sanitize_username(&form.username)?,
        sanitize_token(&form.token)?,
        "user",
    ))
}

fn prepare_user_for_update(form: &UpdateUserForm) -> Result<DashboardUserRow, String> {
    Ok(DashboardUserRow::new(
        sanitize_username(&form.username)?,
        sanitize_token(&form.token)?,
        "user",
    ))
}

fn validate_dashboard_users(users: &[DashboardUserRow]) -> Result<(), String> {
    if users.is_empty() {
        return Err(
            "refusing to remove the last dashboard user; edit WORKFLOW.md directly if you want to disable auth"
                .into(),
        );
    }
    for user in users {
        sanitize_username(&user.username)?;
        sanitize_token(&user.token)?;
    }
    for (index, user) in users.iter().enumerate() {
        let duplicate = users
            .iter()
            .skip(index + 1)
            .any(|other| usernames_match(&user.username, &other.username));
        if duplicate {
            return Err(format!(
                "dashboard username '{}' already exists",
                user.username.trim()
            ));
        }
    }
    Ok(())
}

fn sanitize_username(username: &str) -> Result<String, String> {
    let username = username.trim();
    if username.is_empty() {
        return Err("username cannot be empty".into());
    }
    Ok(username.to_string())
}

fn sanitize_token(token: &str) -> Result<String, String> {
    let token = token.trim();
    if token.is_empty() {
        return Err("token cannot be empty".into());
    }
    Ok(token.to_string())
}

fn usernames_match(left: &str, right: &str) -> bool {
    normalized_username(left) == normalized_username(right)
}

fn normalized_username(username: &str) -> String {
    username.trim().to_ascii_lowercase()
}

fn success_message(query: &UsersPageQuery) -> Option<&'static str> {
    match query.result.as_deref() {
        Some("created") => Some("Dashboard user added."),
        Some("updated") => Some("Dashboard user updated."),
        Some("deleted") => Some("Dashboard user removed."),
        _ => None,
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
struct UsersPageQuery {
    result: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CreateUserForm {
    username: String,
    token: String,
}

#[derive(Debug, Deserialize)]
struct UpdateUserForm {
    original_username: String,
    username: String,
    token: String,
}

#[derive(Debug, Deserialize)]
struct DeleteUserForm {
    username: String,
}

#[derive(Debug, Clone, Serialize)]
struct DashboardUserRow {
    username: String,
    token: String,
    masked_token: String,
    source: &'static str,
}

impl DashboardUserRow {
    fn new(username: String, token: String, source: &'static str) -> Self {
        Self {
            username,
            masked_token: mask_token(&token),
            token,
            source,
        }
    }
}

fn mask_token(token: &str) -> String {
    let visible = token.chars().count().min(4);
    let suffix: String = token
        .chars()
        .rev()
        .take(visible)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    if token.is_empty() {
        return "empty".into();
    }
    if token.chars().count() <= 4 {
        return "****".into();
    }
    format!("****{}", suffix)
}

// ---------------------------------------------------------------------------
// GraphQL handlers
// ---------------------------------------------------------------------------

async fn graphql_handler(State(state): State<AppState>, req: GraphQLRequest) -> GraphQLResponse {
    state.schema.execute(req.into_inner()).await.into()
}

async fn graphql_playground() -> impl IntoResponse {
    Html(async_graphql::http::playground_source(
        async_graphql::http::GraphQLPlaygroundConfig::new("/graphql")
            .subscription_endpoint("/graphql/ws"),
    ))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use serde_json::json;
    use tower::ServiceExt;

    use super::*;

    fn test_snapshot() -> RuntimeSnapshot {
        serde_json::from_value(json!({
            "generated_at": "2026-01-01T00:00:00Z",
            "dispatch_mode": "manual",
            "counts": {
                "running": 0,
                "retrying": 0,
                "runs": 0,
                "tasks_pending": 0,
                "tasks_in_progress": 0,
                "tasks_completed": 0,
                "worktrees": 0
            },
            "running": [],
            "retrying": [],
            "codex_totals": { "input_tokens": 0, "output_tokens": 0, "total_tokens": 0, "seconds_running": 0.0 },
            "rate_limits": null,
            "throttles": [],
            "budgets": [],
            "agent_catalogs": [],
            "saved_contexts": [],
            "recent_events": [],
            "repo_registrations": []
        }))
        .expect("snapshot should deserialize")
    }

    fn template_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("templates")
    }

    #[tokio::test]
    async fn users_page_renders_legacy_auth_user() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workflow_path = dir.path().join("WORKFLOW.md");
        std::fs::write(
            &workflow_path,
            "---\ndaemon:\n  auth_token: legacy-secret\n---\nPrompt\n",
        )
        .expect("write workflow");

        let (snapshot_tx, snapshot_rx) = watch::channel(test_snapshot());
        let _ = snapshot_tx;
        let (command_tx, _command_rx) = mpsc::unbounded_channel();
        let router = build_router(
            snapshot_rx,
            command_tx,
            template_dir(),
            workflow_path,
            None,
            None,
            None,
            None,
        );

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/users")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let html = String::from_utf8(body.to_vec()).expect("utf8");
        assert_eq!(status, StatusCode::OK, "{html}");
        assert!(html.contains("admin"));
        assert!(html.contains("legacy"));
        assert!(html.contains("daemon.auth_token"));
    }

    #[tokio::test]
    async fn create_user_persists_named_dashboard_users() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workflow_path = dir.path().join("WORKFLOW.md");
        std::fs::write(&workflow_path, "---\ntracker:\n  kind: none\n---\nPrompt\n")
            .expect("write workflow");

        let (snapshot_tx, snapshot_rx) = watch::channel(test_snapshot());
        let _ = snapshot_tx;
        let (command_tx, _command_rx) = mpsc::unbounded_channel();
        let router = build_router(
            snapshot_rx,
            command_tx,
            template_dir(),
            workflow_path.clone(),
            None,
            None,
            None,
            None,
        );

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/users/create")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("username=alice&token=secret-1"))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        let daemon = polyphony_workflow::load_daemon_config_from_workflow(&workflow_path)
            .expect("load daemon");
        assert!(daemon.auth_token.is_none());
        assert_eq!(daemon.users, vec![polyphony_workflow::DaemonUserConfig {
            username: "alice".into(),
            token: "secret-1".into(),
        }]);
    }

    #[test]
    fn validate_dashboard_users_rejects_duplicates() {
        let users = vec![
            DashboardUserRow::new("alice".into(), "secret-1".into(), "user"),
            DashboardUserRow::new("Alice".into(), "secret-2".into(), "user"),
        ];

        let error = validate_dashboard_users(&users).expect_err("duplicate usernames");
        assert!(error.contains("already exists"));
    }
}
