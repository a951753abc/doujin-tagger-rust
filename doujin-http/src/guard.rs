//! Loopback origin request guard middleware.

use std::net::IpAddr;
use std::str::FromStr;

use axum::body::Body;
use axum::extract::Request;
use axum::http::uri::Authority;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::error::ApiError;

pub(crate) async fn request_guard(request: Request<Body>, next: Next) -> Response {
    if !request
        .headers()
        .get(axum::http::header::HOST)
        .and_then(|value| value.to_str().ok())
        .is_some_and(is_allowed_loopback_authority)
    {
        return ApiError::new(
            StatusCode::MISDIRECTED_REQUEST,
            "invalid_host",
            "HTTP Host 必須是 localhost loopback",
        )
        .into_response();
    }
    if is_mutating(request.method())
        && let Some(value) = origin_or_referer(request.headers())
        && !value.to_str().ok().is_some_and(is_allowed_loopback_origin)
    {
        return ApiError::forbidden(
            "cross_site_write_rejected",
            "寫入要求的 Origin／Referer 不是 localhost loopback",
        )
        .into_response();
    }
    next.run(request).await
}

pub(crate) fn is_mutating(method: &Method) -> bool {
    matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    )
}

pub(crate) fn origin_or_referer(headers: &HeaderMap) -> Option<&axum::http::HeaderValue> {
    headers
        .get(axum::http::header::ORIGIN)
        .or_else(|| headers.get(axum::http::header::REFERER))
}

pub(crate) fn is_allowed_loopback_origin(value: &str) -> bool {
    let Ok(uri) = Uri::from_str(value) else {
        return false;
    };
    if uri.scheme().is_none() || uri.authority().is_none() {
        return false;
    }
    let Some(host) = uri.host() else {
        return false;
    };
    is_allowed_loopback_host(host)
}

pub(crate) fn is_allowed_loopback_authority(value: &str) -> bool {
    Authority::from_str(value)
        .ok()
        .is_some_and(|authority| is_allowed_loopback_host(authority.host()))
}

pub(crate) fn is_allowed_loopback_host(host: &str) -> bool {
    let host = host.trim_matches(['[', ']']);
    host.eq_ignore_ascii_case("localhost")
        || IpAddr::from_str(host).is_ok_and(|address| address.is_loopback())
}
