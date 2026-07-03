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
    pub(crate) admin_base: String,
    pub(crate) client: Client,
    pub(crate) runtime_config_js: String,
    pub(crate) stream_base: String,
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
    for (name, value) in headers {
        if let Some(name) = name {
            if name != header::HOST {
                builder = builder.header(name, value);
            }
        }
    }

    match builder.send().await {
        Ok(response) => {
            let status = response.status();
            let mut response_builder = Response::builder().status(status);
            for (name, value) in response.headers() {
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
