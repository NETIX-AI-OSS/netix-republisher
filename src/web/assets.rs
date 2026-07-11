//! Embedded browser UI. The three files under `web/` are compiled into the
//! binary so the container image is fully self-contained (a strict CSP plus no
//! external requests keeps it usable on air-gapped OT networks).

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};

const INDEX_HTML: &str = include_str!("../../web/index.html");
const APP_JS: &str = include_str!("../../web/app.js");
const STYLE_CSS: &str = include_str!("../../web/style.css");
const FAVICON_SVG: &str = include_str!("../../web/favicon.svg");

fn asset(content_type: &'static str, body: &'static str) -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
            // Assets are versioned by the binary itself; revalidate cheaply.
            (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
        ],
        body,
    )
        .into_response()
}

pub async fn index() -> Response {
    asset("text/html; charset=utf-8", INDEX_HTML)
}

pub async fn app_js() -> Response {
    asset("application/javascript; charset=utf-8", APP_JS)
}

pub async fn style_css() -> Response {
    asset("text/css; charset=utf-8", STYLE_CSS)
}

pub async fn favicon() -> Response {
    asset("image/svg+xml", FAVICON_SVG)
}
