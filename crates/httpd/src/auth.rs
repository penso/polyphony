use std::sync::Arc;

use axum::{
    Form,
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse, Redirect},
};
use axum_login::{AuthManagerLayerBuilder, AuthUser, AuthnBackend, UserId};
use minijinja::Environment;
use serde::Deserialize;
use subtle::ConstantTimeEq;
use tokio::sync::RwLock;
use tower_sessions::SessionManagerLayer;
use tower_sessions_sqlx_store::SqliteStore;

/// Authenticated dashboard user.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct User {
    pub username: String,
    token: String,
}

impl User {
    pub fn new(username: String, token: String) -> Self {
        Self { username, token }
    }
}

impl AuthUser for User {
    type Id = String;

    fn id(&self) -> Self::Id {
        self.username.clone()
    }

    fn session_auth_hash(&self) -> &[u8] {
        self.token.as_bytes()
    }
}

/// Authentication backend backed by a static list of users from config.
#[derive(Debug, Clone)]
pub struct Backend {
    users: Arc<RwLock<Vec<User>>>,
}

impl Backend {
    pub fn new(users: Vec<User>) -> Self {
        Self {
            users: Arc::new(RwLock::new(users)),
        }
    }

    pub(crate) async fn replace_users(&self, users: Vec<User>) {
        *self.users.write().await = users;
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Credentials {
    pub username: String,
    pub token: String,
}

impl AuthnBackend for Backend {
    type Credentials = Credentials;
    type Error = std::convert::Infallible;
    type User = User;

    async fn authenticate(
        &self,
        creds: Self::Credentials,
    ) -> Result<Option<Self::User>, Self::Error> {
        let users = self.users.read().await;
        let found = users.iter().find(|u| {
            u.username == creds.username
                && bool::from(u.token.as_bytes().ct_eq(creds.token.as_bytes()))
        });
        Ok(found.cloned())
    }

    async fn get_user(&self, user_id: &UserId<Self>) -> Result<Option<Self::User>, Self::Error> {
        let users = self.users.read().await;
        Ok(users.iter().find(|u| u.id() == *user_id).cloned())
    }
}

pub(crate) type AuthSession = axum_login::AuthSession<Backend>;

/// Build the auth + session layers. Returns `None` if no users are configured.
pub async fn build_auth_layer(
    users: Vec<User>,
    session_db_url: &str,
) -> Result<
    (
        Backend,
        axum_login::AuthManagerLayer<Backend, SqliteStore>,
        SessionManagerLayer<SqliteStore>,
    ),
    String,
> {
    let pool = sqlx::SqlitePool::connect(session_db_url)
        .await
        .map_err(|e| format!("session db connect: {e}"))?;
    let session_store = SqliteStore::new(pool);
    session_store
        .migrate()
        .await
        .map_err(|e| format!("session db migrate: {e}"))?;

    let session_layer = SessionManagerLayer::new(session_store.clone())
        .with_secure(false)
        .with_expiry(tower_sessions::Expiry::OnInactivity(time::Duration::days(
            7,
        )));

    let backend = Backend::new(users);
    let auth_layer = AuthManagerLayerBuilder::new(backend.clone(), session_layer.clone()).build();
    Ok((backend, auth_layer, session_layer))
}

/// Render the login page.
pub(crate) async fn login_page(
    State(env): State<Arc<Environment<'static>>>,
    auth_session: AuthSession,
) -> impl IntoResponse {
    if auth_session.user.is_some() {
        return Redirect::to("/").into_response();
    }
    render_login(&env, None).into_response()
}

/// Handle login form submission.
pub(crate) async fn login_submit(
    State(env): State<Arc<Environment<'static>>>,
    mut auth_session: AuthSession,
    Form(creds): Form<Credentials>,
) -> impl IntoResponse {
    match auth_session.authenticate(creds).await {
        Ok(Some(user)) => {
            if let Err(e) = auth_session.login(&user).await {
                tracing::error!("session login failed: {e}");
                return (StatusCode::INTERNAL_SERVER_ERROR, "session error").into_response();
            }
            Redirect::to("/").into_response()
        },
        Ok(None) => render_login(&env, Some("Invalid username or token")).into_response(),
        Err(_) => render_login(&env, Some("Authentication error")).into_response(),
    }
}

/// Handle logout.
pub(crate) async fn logout(mut auth_session: AuthSession) -> impl IntoResponse {
    let _ = auth_session.logout().await;
    Redirect::to("/login")
}

fn render_login(env: &Environment<'_>, error: Option<&str>) -> impl IntoResponse {
    let ctx = minijinja::context! { error => error };
    match env.get_template("login.html") {
        Ok(tmpl) => match tmpl.render(ctx) {
            Ok(html) => Html(html).into_response(),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("render error: {e}"),
            )
                .into_response(),
        },
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("template error: {e}"),
        )
            .into_response(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn backend_reloads_users_in_place() {
        let backend = Backend::new(vec![User::new("alice".into(), "token-a".into())]);

        let first = backend
            .authenticate(Credentials {
                username: "alice".into(),
                token: "token-a".into(),
            })
            .await
            .expect("auth result");
        assert!(first.is_some());

        backend
            .replace_users(vec![User::new("bob".into(), "token-b".into())])
            .await;

        let alice = backend
            .authenticate(Credentials {
                username: "alice".into(),
                token: "token-a".into(),
            })
            .await
            .expect("auth result");
        assert!(alice.is_none());

        let bob = backend
            .authenticate(Credentials {
                username: "bob".into(),
                token: "token-b".into(),
            })
            .await
            .expect("auth result");
        assert!(bob.is_some());
    }
}
