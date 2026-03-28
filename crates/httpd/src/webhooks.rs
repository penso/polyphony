use std::{collections::HashMap, sync::Arc};

use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
};
use hmac::{Hmac, Mac};
use polyphony_orchestrator::RuntimeCommand;
use polyphony_workflow::WebhooksConfig;
use serde::Serialize;
use sha2::Sha256;
use subtle::ConstantTimeEq;
use tokio::sync::mpsc;

type HmacSha256 = Hmac<Sha256>;

// ---------------------------------------------------------------------------
// Auth strategies
// ---------------------------------------------------------------------------

/// Strategy for verifying inbound webhook authenticity.
pub(crate) trait WebhookAuthStrategy: Send + Sync {
    fn verify(&self, headers: &HeaderMap, body: &[u8]) -> bool;
}

/// HMAC-SHA256 verification (GitHub-style).
/// Expects `X-Hub-Signature-256: sha256=<hex>`.
struct HmacSha256Strategy {
    secret: String,
}

impl WebhookAuthStrategy for HmacSha256Strategy {
    fn verify(&self, headers: &HeaderMap, body: &[u8]) -> bool {
        let Some(sig_header) = headers.get("x-hub-signature-256") else {
            return false;
        };
        let sig_str = sig_header.to_str().unwrap_or_default();
        let Some(hex_sig) = sig_str.strip_prefix("sha256=") else {
            return false;
        };
        let Ok(expected_bytes) = hex::decode(hex_sig) else {
            return false;
        };
        let Ok(mut mac) = HmacSha256::new_from_slice(self.secret.as_bytes()) else {
            return false;
        };
        mac.update(body);
        let computed = mac.finalize().into_bytes();
        bool::from(computed.as_slice().ct_eq(&expected_bytes))
    }
}

/// HMAC-SHA256 with a custom header and raw hex signature (Linear-style).
/// Reads the named header as raw hex (no `sha256=` prefix), computes HMAC over body, compares.
struct HmacSha256HeaderStrategy {
    secret: String,
    header_name: String,
}

impl WebhookAuthStrategy for HmacSha256HeaderStrategy {
    fn verify(&self, headers: &HeaderMap, body: &[u8]) -> bool {
        let Some(sig_header) = headers.get(self.header_name.as_str()) else {
            return false;
        };
        let hex_sig = sig_header.to_str().unwrap_or_default();
        let Ok(expected_bytes) = hex::decode(hex_sig) else {
            return false;
        };
        let Ok(mut mac) = HmacSha256::new_from_slice(self.secret.as_bytes()) else {
            return false;
        };
        mac.update(body);
        let computed = mac.finalize().into_bytes();
        bool::from(computed.as_slice().ct_eq(&expected_bytes))
    }
}

/// Token-in-header verification (GitLab-style).
/// Compares the value of a named header to the secret.
struct TokenHeaderStrategy {
    secret: String,
    header_name: String,
}

impl WebhookAuthStrategy for TokenHeaderStrategy {
    fn verify(&self, headers: &HeaderMap, _body: &[u8]) -> bool {
        let Some(value) = headers.get(self.header_name.as_str()) else {
            return false;
        };
        let token = value.to_str().unwrap_or_default();
        bool::from(token.as_bytes().ct_eq(self.secret.as_bytes()))
    }
}

/// Bearer token verification (generic).
/// Expects `Authorization: Bearer <token>`.
struct BearerStrategy {
    token: String,
}

impl WebhookAuthStrategy for BearerStrategy {
    fn verify(&self, headers: &HeaderMap, _body: &[u8]) -> bool {
        let Some(auth) = headers.get("authorization") else {
            return false;
        };
        let value = auth.to_str().unwrap_or_default();
        let token = value.strip_prefix("Bearer ").unwrap_or(value);
        bool::from(token.as_bytes().ct_eq(self.token.as_bytes()))
    }
}

// ---------------------------------------------------------------------------
// Strategy factory
// ---------------------------------------------------------------------------

fn build_strategy(
    config: &polyphony_workflow::WebhookProviderConfig,
) -> Result<Box<dyn WebhookAuthStrategy>, String> {
    match config.auth.as_str() {
        "hmac_sha256" => Ok(Box::new(HmacSha256Strategy {
            secret: config.secret.clone(),
        })),
        "hmac_sha256_header" => Ok(Box::new(HmacSha256HeaderStrategy {
            secret: config.secret.clone(),
            header_name: config
                .header
                .clone()
                .unwrap_or_else(|| "X-Signature".into()),
        })),
        "token_header" => Ok(Box::new(TokenHeaderStrategy {
            secret: config.secret.clone(),
            header_name: config
                .header
                .clone()
                .unwrap_or_else(|| "X-Webhook-Token".into()),
        })),
        "bearer" => Ok(Box::new(BearerStrategy {
            token: config.secret.clone(),
        })),
        other => Err(format!("unknown webhook auth strategy: {other}")),
    }
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub(crate) struct WebhookState {
    command_tx: mpsc::UnboundedSender<RuntimeCommand>,
    providers: HashMap<String, Arc<dyn WebhookAuthStrategy>>,
}

#[derive(Serialize)]
struct WebhookResponse {
    accepted: bool,
    provider: String,
}

#[derive(Serialize)]
struct WebhookErrorResponse {
    error: String,
}

async fn handle_webhook(
    State(state): State<WebhookState>,
    Path(provider_name): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let Some(strategy) = state.providers.get(&provider_name) else {
        return (
            StatusCode::NOT_FOUND,
            Json(WebhookErrorResponse {
                error: format!("unknown webhook provider: {provider_name}"),
            }),
        )
            .into_response();
    };

    if !strategy.verify(&headers, &body) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(WebhookErrorResponse {
                error: "webhook authentication failed".into(),
            }),
        )
            .into_response();
    }

    tracing::info!(provider = %provider_name, body_len = body.len(), "webhook received, triggering refresh");
    let _ = state.command_tx.send(RuntimeCommand::Refresh);

    (
        StatusCode::OK,
        Json(WebhookResponse {
            accepted: true,
            provider: provider_name,
        }),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Router builder
// ---------------------------------------------------------------------------

pub(crate) fn webhook_router(
    command_tx: mpsc::UnboundedSender<RuntimeCommand>,
    config: &WebhooksConfig,
) -> Router {
    let providers: HashMap<String, Arc<dyn WebhookAuthStrategy>> = config
        .providers
        .iter()
        .filter_map(|(name, cfg)| match build_strategy(cfg) {
            Ok(s) => Some((name.clone(), Arc::from(s))),
            Err(e) => {
                tracing::warn!(provider = %name, "skipping webhook provider: {e}");
                None
            },
        })
        .collect();

    let state = WebhookState {
        command_tx,
        providers,
    };

    Router::new()
        .route("/webhooks/{provider}", post(handle_webhook))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use axum::http::HeaderMap;
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    use super::*;

    type TestHmac = Hmac<Sha256>;

    fn compute_hmac(secret: &str, body: &[u8]) -> String {
        let mut mac = TestHmac::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        let result = mac.finalize().into_bytes();
        format!("sha256={}", hex::encode(result))
    }

    // -- HmacSha256Strategy --

    #[test]
    fn hmac_sha256_valid_signature() {
        let strategy = HmacSha256Strategy {
            secret: "mysecret".into(),
        };
        let body = b"hello world";
        let sig = compute_hmac("mysecret", body);
        let mut headers = HeaderMap::new();
        headers.insert("x-hub-signature-256", sig.parse().unwrap());
        assert!(strategy.verify(&headers, body));
    }

    #[test]
    fn hmac_sha256_invalid_signature() {
        let strategy = HmacSha256Strategy {
            secret: "mysecret".into(),
        };
        let body = b"hello world";
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-hub-signature-256",
            "sha256=0000000000000000000000000000000000000000000000000000000000000000"
                .parse()
                .unwrap(),
        );
        assert!(!strategy.verify(&headers, body));
    }

    #[test]
    fn hmac_sha256_missing_header() {
        let strategy = HmacSha256Strategy {
            secret: "mysecret".into(),
        };
        assert!(!strategy.verify(&HeaderMap::new(), b"body"));
    }

    #[test]
    fn hmac_sha256_bad_prefix() {
        let strategy = HmacSha256Strategy {
            secret: "mysecret".into(),
        };
        let mut headers = HeaderMap::new();
        headers.insert("x-hub-signature-256", "md5=abcdef".parse().unwrap());
        assert!(!strategy.verify(&headers, b"body"));
    }

    // -- HmacSha256HeaderStrategy (Linear-style) --

    fn compute_hmac_raw(secret: &str, body: &[u8]) -> String {
        let mut mac = TestHmac::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        let result = mac.finalize().into_bytes();
        hex::encode(result)
    }

    #[test]
    fn hmac_sha256_header_valid() {
        let strategy = HmacSha256HeaderStrategy {
            secret: "linear-secret".into(),
            header_name: "Linear-Signature".into(),
        };
        let body = b"{\"action\":\"create\"}";
        let sig = compute_hmac_raw("linear-secret", body);
        let mut headers = HeaderMap::new();
        headers.insert("Linear-Signature", sig.parse().unwrap());
        assert!(strategy.verify(&headers, body));
    }

    #[test]
    fn hmac_sha256_header_invalid() {
        let strategy = HmacSha256HeaderStrategy {
            secret: "linear-secret".into(),
            header_name: "Linear-Signature".into(),
        };
        let mut headers = HeaderMap::new();
        headers.insert("Linear-Signature", "badhex".parse().unwrap());
        assert!(!strategy.verify(&headers, b"body"));
    }

    #[test]
    fn hmac_sha256_header_missing() {
        let strategy = HmacSha256HeaderStrategy {
            secret: "linear-secret".into(),
            header_name: "Linear-Signature".into(),
        };
        assert!(!strategy.verify(&HeaderMap::new(), b"body"));
    }

    // -- TokenHeaderStrategy --

    #[test]
    fn token_header_valid() {
        let strategy = TokenHeaderStrategy {
            secret: "gitlab-secret".into(),
            header_name: "X-Gitlab-Token".into(),
        };
        let mut headers = HeaderMap::new();
        headers.insert("X-Gitlab-Token", "gitlab-secret".parse().unwrap());
        assert!(strategy.verify(&headers, b""));
    }

    #[test]
    fn token_header_invalid() {
        let strategy = TokenHeaderStrategy {
            secret: "gitlab-secret".into(),
            header_name: "X-Gitlab-Token".into(),
        };
        let mut headers = HeaderMap::new();
        headers.insert("X-Gitlab-Token", "wrong".parse().unwrap());
        assert!(!strategy.verify(&headers, b""));
    }

    #[test]
    fn token_header_missing() {
        let strategy = TokenHeaderStrategy {
            secret: "gitlab-secret".into(),
            header_name: "X-Gitlab-Token".into(),
        };
        assert!(!strategy.verify(&HeaderMap::new(), b""));
    }

    // -- BearerStrategy --

    #[test]
    fn bearer_valid() {
        let strategy = BearerStrategy {
            token: "my-token".into(),
        };
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer my-token".parse().unwrap());
        assert!(strategy.verify(&headers, b""));
    }

    #[test]
    fn bearer_invalid() {
        let strategy = BearerStrategy {
            token: "my-token".into(),
        };
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer wrong-token".parse().unwrap());
        assert!(!strategy.verify(&headers, b""));
    }

    #[test]
    fn bearer_missing() {
        let strategy = BearerStrategy {
            token: "my-token".into(),
        };
        assert!(!strategy.verify(&HeaderMap::new(), b""));
    }

    // -- build_strategy --

    #[test]
    fn build_strategy_hmac() {
        let config = polyphony_workflow::WebhookProviderConfig {
            auth: "hmac_sha256".into(),
            secret: "s".into(),
            header: None,
        };
        assert!(build_strategy(&config).is_ok());
    }

    #[test]
    fn build_strategy_hmac_header() {
        let config = polyphony_workflow::WebhookProviderConfig {
            auth: "hmac_sha256_header".into(),
            secret: "s".into(),
            header: Some("Linear-Signature".into()),
        };
        assert!(build_strategy(&config).is_ok());
    }

    #[test]
    fn build_strategy_token_header() {
        let config = polyphony_workflow::WebhookProviderConfig {
            auth: "token_header".into(),
            secret: "s".into(),
            header: Some("X-Custom".into()),
        };
        assert!(build_strategy(&config).is_ok());
    }

    #[test]
    fn build_strategy_bearer() {
        let config = polyphony_workflow::WebhookProviderConfig {
            auth: "bearer".into(),
            secret: "s".into(),
            header: None,
        };
        assert!(build_strategy(&config).is_ok());
    }

    #[test]
    fn build_strategy_unknown() {
        let config = polyphony_workflow::WebhookProviderConfig {
            auth: "magic".into(),
            secret: "s".into(),
            header: None,
        };
        assert!(build_strategy(&config).is_err());
    }

    // -- Integration tests with router --

    #[tokio::test]
    async fn webhook_router_accepts_valid_hmac() {
        use axum::{body::Body, http::Request};
        use tower::ServiceExt;

        let (tx, mut rx) = mpsc::unbounded_channel();
        let config = WebhooksConfig {
            enabled: true,
            providers: HashMap::from([(
                "github".into(),
                polyphony_workflow::WebhookProviderConfig {
                    auth: "hmac_sha256".into(),
                    secret: "test-secret".into(),
                    header: None,
                },
            )]),
        };
        let router = webhook_router(tx, &config);
        let body_bytes = b"{\"action\":\"opened\"}";
        let sig = compute_hmac("test-secret", body_bytes);

        let request = Request::builder()
            .method("POST")
            .uri("/webhooks/github")
            .header("x-hub-signature-256", sig)
            .body(Body::from(body_bytes.to_vec()))
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(matches!(rx.try_recv(), Ok(RuntimeCommand::Refresh)));
    }

    #[tokio::test]
    async fn webhook_router_rejects_invalid_hmac() {
        use axum::{body::Body, http::Request};
        use tower::ServiceExt;

        let (tx, _rx) = mpsc::unbounded_channel();
        let config = WebhooksConfig {
            enabled: true,
            providers: HashMap::from([(
                "github".into(),
                polyphony_workflow::WebhookProviderConfig {
                    auth: "hmac_sha256".into(),
                    secret: "test-secret".into(),
                    header: None,
                },
            )]),
        };
        let router = webhook_router(tx, &config);

        let request = Request::builder()
            .method("POST")
            .uri("/webhooks/github")
            .header("x-hub-signature-256", "sha256=bad")
            .body(Body::from(b"{}".to_vec()))
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn webhook_router_rejects_unknown_provider() {
        use axum::{body::Body, http::Request};
        use tower::ServiceExt;

        let (tx, _rx) = mpsc::unbounded_channel();
        let config = WebhooksConfig {
            enabled: true,
            providers: HashMap::new(),
        };
        let router = webhook_router(tx, &config);

        let request = Request::builder()
            .method("POST")
            .uri("/webhooks/unknown")
            .body(Body::from(b"{}".to_vec()))
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn webhook_router_token_header_valid() {
        use axum::{body::Body, http::Request};
        use tower::ServiceExt;

        let (tx, mut rx) = mpsc::unbounded_channel();
        let config = WebhooksConfig {
            enabled: true,
            providers: HashMap::from([(
                "gitlab".into(),
                polyphony_workflow::WebhookProviderConfig {
                    auth: "token_header".into(),
                    secret: "gl-secret".into(),
                    header: Some("X-Gitlab-Token".into()),
                },
            )]),
        };
        let router = webhook_router(tx, &config);

        let request = Request::builder()
            .method("POST")
            .uri("/webhooks/gitlab")
            .header("X-Gitlab-Token", "gl-secret")
            .body(Body::from(b"{}".to_vec()))
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(matches!(rx.try_recv(), Ok(RuntimeCommand::Refresh)));
    }
}
