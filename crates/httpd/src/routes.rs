use std::{path::PathBuf, sync::Arc};

use async_graphql_axum::{GraphQLRequest, GraphQLResponse, GraphQLSubscription};
use axum::{
    Router,
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::get,
};
use axum_login::AuthManagerLayer;
use minijinja::Environment;
use polyphony_core::RuntimeSnapshot;
use polyphony_orchestrator::RuntimeCommand;
use polyphony_workflow::WebhooksConfig;
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
}

/// Build the full httpd router.
///
/// When `auth_layer` is `Some`, dashboard pages require login.
/// Webhook routes are always outside the auth layer (they have their own auth).
pub fn build_router(
    snapshot_rx: watch::Receiver<RuntimeSnapshot>,
    command_tx: mpsc::UnboundedSender<RuntimeCommand>,
    template_dir: PathBuf,
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

async fn page_logs(State(state): State<AppState>) -> impl IntoResponse {
    render_page(&state, "logs.html")
}

fn render_page(
    state: &AppState,
    template_name: &str,
) -> Result<Html<String>, (StatusCode, String)> {
    let mut snapshot = state.snapshot_rx.borrow().clone();
    // Enrich snapshot with repo registry data (read from disk)
    if snapshot.repo_registrations.is_empty() {
        let registry_path = dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".polyphony")
            .join("repos.json");
        if let Ok(registry) = polyphony_core::load_repo_registry(&registry_path) {
            snapshot.repo_registrations = registry.repos;
        }
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
