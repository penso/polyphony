use std::{path::PathBuf, sync::Arc};

use async_graphql_axum::{GraphQLRequest, GraphQLResponse, GraphQLSubscription};
use axum::{
    Router,
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::get,
};
use minijinja::Environment;
use polyphony_core::RuntimeSnapshot;
use polyphony_orchestrator::RuntimeCommand;
use tokio::sync::{mpsc, watch};

use crate::{graphql, templates};

#[derive(Clone)]
struct AppState {
    schema: graphql::PolyphonySchema,
    snapshot_rx: watch::Receiver<RuntimeSnapshot>,
    template_env: Arc<Environment<'static>>,
}

pub fn build_router(
    snapshot_rx: watch::Receiver<RuntimeSnapshot>,
    command_tx: mpsc::UnboundedSender<RuntimeCommand>,
    template_dir: PathBuf,
) -> Router {
    let schema = graphql::build_schema(snapshot_rx.clone(), command_tx);
    let template_env = Arc::new(templates::build_env(&template_dir));

    let state = AppState {
        schema: schema.clone(),
        snapshot_rx,
        template_env,
    };

    Router::new()
        // SSR pages
        .route("/", get(page_index))
        .route("/triggers", get(page_triggers))
        .route("/movements", get(page_movements))
        .route("/agents", get(page_agents))
        .route("/tasks", get(page_tasks))
        .route("/logs", get(page_logs))
        // GraphQL
        .route("/graphql", get(graphql_playground).post(graphql_handler))
        .route_service("/graphql/ws", GraphQLSubscription::new(schema))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// SSR page handlers
// ---------------------------------------------------------------------------

async fn page_index(State(state): State<AppState>) -> impl IntoResponse {
    render_page(&state, "index.html")
}

async fn page_triggers(State(state): State<AppState>) -> impl IntoResponse {
    render_page(&state, "triggers.html")
}

async fn page_movements(State(state): State<AppState>) -> impl IntoResponse {
    render_page(&state, "movements.html")
}

async fn page_agents(State(state): State<AppState>) -> impl IntoResponse {
    render_page(&state, "agents.html")
}

async fn page_tasks(State(state): State<AppState>) -> impl IntoResponse {
    render_page(&state, "tasks.html")
}

async fn page_logs(State(state): State<AppState>) -> impl IntoResponse {
    render_page(&state, "logs.html")
}

fn render_page(
    state: &AppState,
    template_name: &str,
) -> Result<Html<String>, (StatusCode, String)> {
    let snapshot = state.snapshot_rx.borrow().clone();
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
