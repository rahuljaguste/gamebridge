// SPDX-License-Identifier: AGPL-3.0-only
//! Embedded static assets (no build step).

use axum::http::header;
use axum::response::{IntoResponse, Response};

const INDEX: &str = include_str!("../static/index.html");
const APP_JS: &str = include_str!("../static/app.js");
const MOTION_MATH_JS: &str = include_str!("../static/motion-math.js");
const STYLE_CSS: &str = include_str!("../static/style.css");

// The assets are embedded in the binary, so a restarted server always serves
// the new build. Prevent browsers from caching an older app.js across
// restarts.
const NO_STORE: &str = "no-store";

pub async fn index() -> Response {
    asset(INDEX, "text/html; charset=utf-8")
}

pub async fn app_js() -> Response {
    asset(APP_JS, "application/javascript; charset=utf-8")
}

pub async fn motion_math_js() -> Response {
    asset(MOTION_MATH_JS, "application/javascript; charset=utf-8")
}

pub async fn style_css() -> Response {
    asset(STYLE_CSS, "text/css; charset=utf-8")
}

fn asset(body: &'static str, content_type: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, NO_STORE),
        ],
        body,
    )
        .into_response()
}
