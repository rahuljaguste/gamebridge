// SPDX-License-Identifier: AGPL-3.0-only
//! HTTP and WebSocket routes.

pub mod api;
pub(crate) mod auth;
mod guard;
pub mod static_files;
pub mod ws;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::http::{HeaderName, HeaderValue, header};
use axum::response::Response;
use axum::routing::{delete, get, post};
use axum::{Extension, Router, middleware};

use crate::state::AppState;

pub fn router(
    state: Arc<AppState>,
    bind: SocketAddr,
    allow_remote: bool,
    auth: auth::Auth,
) -> Router {
    // Everything that can read console state or drive input lives behind the
    // token when one is configured. `/api/auth/*` stays open so the login
    // screen can discover whether a token is required.
    let protected = Router::new()
        .route("/hosts", get(api::list_hosts))
        .route("/register", post(api::register))
        .route("/psn/login", get(api::psn_login))
        .route("/psn/account-id", post(api::psn_account_id))
        .route("/psn/public-lookup", post(api::psn_public_lookup))
        .route("/hosts/{id}", delete(api::delete_host))
        .route("/discover", post(api::discover))
        .route("/detect", post(api::detect))
        .route("/wakeup/{id}", post(api::wakeup))
        .route("/session/start", post(api::start_session))
        .route("/session/answer", post(api::answer))
        .route("/session/restart", post(api::restart_session))
        .route("/session/stop", post(api::stop_session))
        .route("/status", get(api::status))
        .route(
            "/system/game-controller",
            get(api::game_controller_preference).post(api::set_game_controller_preference),
        )
        .route("/session/ws", get(ws::ws_handler))
        .route("/demo/start", post(crate::demo::start))
        .route_layer(middleware::from_fn_with_state(auth.clone(), auth::require));

    let public = Router::new()
        .route("/auth/status", get(auth::status))
        .route("/auth/login", post(auth::login))
        .route("/auth/logout", post(auth::logout));

    Router::new()
        .route("/", get(static_files::index))
        .route("/app.js", get(static_files::app_js))
        .route("/motion-math.js", get(static_files::motion_math_js))
        .route("/style.css", get(static_files::style_css))
        .nest("/api", protected.merge(public))
        .layer(Extension(auth))
        .layer(middleware::from_fn_with_state(
            guard::LocalOnly::new(bind, allow_remote),
            guard::local_only,
        ))
        .layer(middleware::from_fn(security_headers))
        .with_state(state)
}

/// The UI is entirely embedded, uses no inline script or style, and opens no
/// third-party origins. A strict policy therefore costs nothing and removes
/// the XSS class from a network-facing page.
async fn security_headers(request: axum::extract::Request, next: middleware::Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        HeaderName::from_static("content-security-policy"),
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self'; style-src 'self'; \
             img-src 'self' data:; media-src 'self' blob:; connect-src 'self'; \
             object-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'",
        ),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        HeaderName::from_static("x-frame-options"),
        HeaderValue::from_static("DENY"),
    );
    headers.insert(
        HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static(
            "camera=(), microphone=(), geolocation=(), \
             accelerometer=(self), gyroscope=(self), magnetometer=(self)",
        ),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::extract::connect_info::MockConnectInfo;
    use axum::http::{Request, StatusCode, header};
    use tower::ServiceExt;

    use crate::config::ConfigStore;

    fn app(auth: auth::Auth) -> Router {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::load(Some(dir.path().to_path_buf())).expect("config store");
        let state = AppState::new(store, None);
        router(
            state,
            "127.0.0.1:8080".parse().expect("address"),
            false,
            auth,
        )
        .layer(MockConnectInfo::<SocketAddr>(
            "127.0.0.1:54321".parse().expect("client address"),
        ))
    }

    fn request(method: &str, uri: &str, headers: &[(&str, &str)], body: &str) -> Request<Body> {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::HOST, "127.0.0.1:8080");
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        builder.body(Body::from(body.to_string())).expect("request")
    }

    #[tokio::test]
    async fn api_requires_auth() {
        let app = app(auth::Auth::from_token("s3cret-token"));

        let response = app
            .clone()
            .oneshot(request("GET", "/api/hosts", &[], ""))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        // The login screen must be reachable without a token.
        let response = app
            .clone()
            .oneshot(request("GET", "/api/auth/status", &[], ""))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(request(
                "GET",
                "/api/hosts",
                &[("authorization", "Bearer s3cret-token")],
                "",
            ))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(request(
                "POST",
                "/api/detect",
                &[("content-type", "application/json")],
                r#"{"host":"127.0.0.1"}"#,
            ))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .oneshot(request("GET", "/api/session/ws", &[], ""))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn login_cookie_authorizes_api() {
        let app = app(auth::Auth::from_token("s3cret-token"));

        let response = app
            .clone()
            .oneshot(request(
                "POST",
                "/api/auth/login",
                &[("content-type", "application/json")],
                r#"{"token":"s3cret-token"}"#,
            ))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let set_cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .expect("set-cookie")
            .to_str()
            .expect("cookie text");
        let cookie = set_cookie
            .split(';')
            .next()
            .expect("cookie pair")
            .to_string();

        let response = app
            .clone()
            .oneshot(request("GET", "/api/hosts", &[("cookie", &cookie)], ""))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(request(
                "POST",
                "/api/auth/login",
                &[("content-type", "application/json")],
                r#"{"token":"wrong"}"#,
            ))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        // Logging out revokes only the server-side session, not the token.
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                "/api/auth/logout",
                &[("cookie", &cookie)],
                "{}",
            ))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let response = app
            .oneshot(request("GET", "/api/hosts", &[("cookie", &cookie)], ""))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn disabled_auth_leaves_api_open() {
        let app = app(auth::Auth::disabled());
        let response = app
            .oneshot(request("GET", "/api/hosts", &[], ""))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn static_ui_sets_a_strict_content_security_policy() {
        let app = app(auth::Auth::disabled());
        let response = app
            .oneshot(request("GET", "/", &[], ""))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let csp = response
            .headers()
            .get("content-security-policy")
            .expect("csp header")
            .to_str()
            .expect("csp text");
        assert!(csp.contains("default-src 'self'"));
        assert!(csp.contains("script-src 'self'"));
        assert!(csp.contains("frame-ancestors 'none'"));
        let policy = response
            .headers()
            .get("permissions-policy")
            .expect("permissions-policy header")
            .to_str()
            .expect("permissions-policy text");
        assert!(policy.contains("accelerometer=(self)"));
        assert!(policy.contains("gyroscope=(self)"));
        assert!(policy.contains("magnetometer=(self)"));
    }
}
