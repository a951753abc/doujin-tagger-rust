//! Embedded single-page frontend assets.

use axum::body::Body;
use axum::http::HeaderValue;
use axum::response::Response;

pub(crate) const FRONTEND_INDEX: &str = include_str!("../static/index.html");
pub(crate) const FRONTEND_CSS: &str = include_str!("../static/app.css");
pub(crate) const FRONTEND_JAVASCRIPT: &str = include_str!("../static/app.js");

pub(crate) async fn frontend_index() -> Response {
    frontend_response(FRONTEND_INDEX, "text/html; charset=utf-8", "no-store", true)
}

pub(crate) async fn frontend_css() -> Response {
    frontend_response(FRONTEND_CSS, "text/css; charset=utf-8", "no-cache", false)
}

pub(crate) async fn frontend_javascript() -> Response {
    frontend_response(
        FRONTEND_JAVASCRIPT,
        "text/javascript; charset=utf-8",
        "no-cache",
        false,
    )
}

pub(crate) fn frontend_response(
    body: &'static str,
    content_type: &'static str,
    cache_control: &'static str,
    is_document: bool,
) -> Response {
    let mut response = Response::new(Body::from(body));
    let headers = response.headers_mut();
    headers.insert("content-type", HeaderValue::from_static(content_type));
    headers.insert("cache-control", HeaderValue::from_static(cache_control));
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    if is_document {
        headers.insert(
            "content-security-policy",
            HeaderValue::from_static(
                "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data: https://ehgt.org https://*.ehgt.org; connect-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'",
            ),
        );
    }
    response
}
