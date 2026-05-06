use axum::{
    extract::State,
    http::{header, StatusCode},
    response::IntoResponse,
    Json,
};

use crate::{
    models::{AutonomiHealth, HealthResponse, PostgresHealth},
    state::AppState,
    MIN_ANTD_SELF_ENCRYPTION_BYTES,
};

pub(super) async fn metrics(State(state): State<AppState>) -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        state.metrics.render_prometheus(),
    )
}

pub(super) async fn health(State(state): State<AppState>) -> impl IntoResponse {
    let autonomi = match state.antd.health().await {
        Ok(status) => AutonomiHealth {
            ok: status.status.eq_ignore_ascii_case("ok"),
            network: status.network,
            error: None,
        },
        Err(err) => AutonomiHealth {
            ok: false,
            network: None,
            error: Some(err.to_string()),
        },
    };
    let postgres = match sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&state.pool)
        .await
    {
        Ok(_) => PostgresHealth {
            ok: true,
            error: None,
        },
        Err(err) => PostgresHealth {
            ok: false,
            error: Some(err.to_string()),
        },
    };
    let write_ready = if state.config.antd_require_cost_ready {
        state
            .antd
            .data_cost_for_size(MIN_ANTD_SELF_ENCRYPTION_BYTES)
            .await
            .is_ok()
    } else {
        autonomi.ok
    };
    let ok = autonomi.ok && postgres.ok && write_ready;
    let status = if ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    (
        status,
        Json(HealthResponse {
            ok,
            autonomi,
            postgres,
            write_ready,
            payment_mode: state.config.antd_payment_mode.clone(),
            final_quote_approval_ttl_seconds: state.config.final_quote_approval_ttl_seconds,
            implementation: "rust_admin",
            role: "primary_admin",
        }),
    )
        .into_response()
}
