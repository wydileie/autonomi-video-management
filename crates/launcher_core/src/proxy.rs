use std::{
    env,
    path::{Path, PathBuf},
};

use anyhow::anyhow;
use axum::{
    body::Body,
    extract::{Path as AxumPath, State},
    http::{header, HeaderMap, Request, Response, StatusCode},
    response::IntoResponse,
};
use reqwest::Client;

#[derive(Clone)]
pub(crate) struct ProxyState {
    pub(crate) origin: String,
    pub(crate) admin_base: String,
    pub(crate) client: Client,
    pub(crate) runtime_config_js: String,
    pub(crate) stream_base: String,
}

pub(crate) async fn validate_native_request(
    State(state): State<ProxyState>,
    request: Request<Body>,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let navigation = request.method() == axum::http::Method::GET
        && !request.uri().path().starts_with("/api")
        && !request.uri().path().starts_with("/stream")
        && request
            .headers()
            .get("sec-fetch-dest")
            .is_some_and(|v| v == "document");
    if !native_headers_allowed(request.headers(), &state.origin, navigation) {
        return (StatusCode::FORBIDDEN, "invalid native Host or Origin").into_response();
    }
    let mut response = next.run(request).await;
    response.headers_mut().insert(header::CONTENT_SECURITY_POLICY,
        axum::http::HeaderValue::from_static("default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; media-src 'self' blob:; connect-src 'self'; worker-src 'self' blob:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'"));
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        axum::http::HeaderValue::from_static("nosniff"),
    );
    Ok::<_, std::convert::Infallible>(response).into_response()
}

fn native_headers_allowed(headers: &HeaderMap, origin: &str, navigation: bool) -> bool {
    let expected_host = origin.strip_prefix("http://").unwrap_or("");
    headers.get(header::HOST).and_then(|h| h.to_str().ok()) == Some(expected_host)
        && headers.get(header::ORIGIN).is_none_or(|h| {
            h.to_str().ok().is_some_and(|value| {
                value == origin
                    || (navigation
                        && matches!(value, "tauri://localhost" | "http://tauri.localhost"))
            })
        })
        && headers
            .get("sec-fetch-site")
            .is_none_or(|h| h != "cross-site" || navigation)
}

fn sanitized_headers(headers: &HeaderMap) -> HeaderMap {
    let mut result = headers.clone();
    for value in headers.get_all(header::CONNECTION) {
        if let Ok(value) = value.to_str() {
            for name in value.split(',').map(str::trim) {
                result.remove(name);
            }
        }
    }
    for name in [
        "connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
        "host",
        "forwarded",
        "x-forwarded-for",
        "x-forwarded-host",
        "x-forwarded-proto",
    ] {
        result.remove(name);
    }
    result
}

pub(crate) fn resolve_frontend_dir(explicit: Option<&Path>) -> anyhow::Result<PathBuf> {
    if let Some(value) = explicit {
        return Ok(value.to_path_buf());
    }
    if let Ok(value) = env::var("AUTVID_FRONTEND_DIR") {
        return Ok(PathBuf::from(value));
    }
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_build = manifest_dir.join("../../apps/web/build");
    if repo_build.join("index.html").is_file() {
        return Ok(repo_build);
    }
    let exe_dir = env::current_exe()?
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow!("could not resolve launcher executable directory"))?;
    for candidate in [
        exe_dir.join("frontend"),
        exe_dir.join("../Resources/frontend"),
    ] {
        if candidate.join("index.html").is_file() {
            return Ok(candidate);
        }
    }
    Err(anyhow!(
        "frontend build not found; set AUTVID_FRONTEND_DIR to a directory containing index.html"
    ))
}

pub(crate) async fn runtime_config(State(state): State<ProxyState>) -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        state.runtime_config_js,
    )
}

pub(crate) async fn proxy_admin(
    State(state): State<ProxyState>,
    AxumPath(path): AxumPath<String>,
    headers: HeaderMap,
    request: Request<Body>,
) -> Response<Body> {
    proxy_to(state, headers, request, format!("/{}", path), true).await
}

pub(crate) async fn proxy_stream(
    State(state): State<ProxyState>,
    AxumPath(path): AxumPath<String>,
    headers: HeaderMap,
    request: Request<Body>,
) -> Response<Body> {
    proxy_to(state, headers, request, stream_proxy_path(&path), false).await
}

pub(crate) fn stream_proxy_path(path: &str) -> String {
    // rust_stream mounts playback routes under /stream; preserve the prefix Axum strips here.
    format!("/stream/{path}")
}

pub(crate) async fn proxy_to(
    state: ProxyState,
    headers: HeaderMap,
    request: Request<Body>,
    path: String,
    admin: bool,
) -> Response<Body> {
    let (parts, body) = request.into_parts();
    let query = parts
        .uri
        .query()
        .map(|query| format!("?{query}"))
        .unwrap_or_default();
    let base = if admin {
        &state.admin_base
    } else {
        &state.stream_base
    };
    let url = format!("{base}{path}{query}");
    let mut builder = state
        .client
        .request(parts.method, url)
        .body(reqwest::Body::wrap_stream(body.into_data_stream()));
    builder = builder.headers(sanitized_headers(&headers));

    match builder.send().await {
        Ok(response) => {
            let status = response.status();
            let mut response_builder = Response::builder().status(status);
            for (name, value) in &sanitized_headers(response.headers()) {
                if name != header::CONTENT_LENGTH {
                    response_builder = response_builder.header(name, value);
                }
            }
            response_builder
                .body(Body::from_stream(response.bytes_stream()))
                .unwrap_or_else(|err| {
                    text_response(
                        StatusCode::BAD_GATEWAY,
                        format!("proxy response error: {err}"),
                    )
                })
        }
        Err(err) => text_response(StatusCode::BAD_GATEWAY, format!("proxy error: {err}")),
    }
}

pub(crate) fn text_response(status: StatusCode, body: String) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Body::from(body))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

#[cfg(test)]
mod security_tests {
    use super::*;
    #[test]
    fn desktop_document_navigation_does_not_authorize_cross_origin_api_requests() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "127.0.0.1:8080".parse().expect("host"));
        headers.insert(header::ORIGIN, "tauri://localhost".parse().expect("origin"));
        headers.insert("sec-fetch-site", "cross-site".parse().expect("fetch site"));
        assert!(native_headers_allowed(
            &headers,
            "http://127.0.0.1:8080",
            true
        ));
        assert!(!native_headers_allowed(
            &headers,
            "http://127.0.0.1:8080",
            false
        ));
        headers.insert(
            header::ORIGIN,
            "https://attacker.example".parse().expect("origin"),
        );
        assert!(!native_headers_allowed(
            &headers,
            "http://127.0.0.1:8080",
            true
        ));
    }

    #[test]
    fn native_host_and_origin_must_match_exact_port() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "127.0.0.1:8080".parse().expect("header"));
        assert!(native_headers_allowed(
            &headers,
            "http://127.0.0.1:8080",
            false
        ));
        headers.insert(
            header::ORIGIN,
            "https://attacker.example".parse().expect("header"),
        );
        assert!(!native_headers_allowed(
            &headers,
            "http://127.0.0.1:8080",
            false
        ));
        headers.remove(header::ORIGIN);
        headers.insert(header::HOST, "rebind.example:8080".parse().expect("header"));
        assert!(!native_headers_allowed(
            &headers,
            "http://127.0.0.1:8080",
            false
        ));
    }
    #[test]
    fn proxy_removes_connection_nominated_and_forwarding_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONNECTION,
            "x-private, keep-alive".parse().expect("header"),
        );
        headers.insert("x-private", "secret".parse().expect("header"));
        headers.insert("x-forwarded-for", "1.2.3.4".parse().expect("header"));
        let result = sanitized_headers(&headers);
        assert!(result.is_empty());
    }
}
