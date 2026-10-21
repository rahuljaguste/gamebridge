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
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, NO_STORE),
        ],
        INDEX,
    )
        .into_response()
}

pub async fn app_js() -> Response {
    (
        [
            (
                header::CONTENT_TYPE,
                "application/javascript; charset=utf-8",
            ),
            (header::CACHE_CONTROL, NO_STORE),
        ],
        APP_JS,
    )
        .into_response()
}

pub async fn motion_math_js() -> Response {
    (
        [
            (
                header::CONTENT_TYPE,
                "application/javascript; charset=utf-8",
            ),
            (header::CACHE_CONTROL, NO_STORE),
        ],
        MOTION_MATH_JS,
    )
        .into_response()
}

pub async fn style_css() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CACHE_CONTROL, NO_STORE),
        ],
        STYLE_CSS,
    )
        .into_response()
}
