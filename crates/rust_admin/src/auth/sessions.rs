use chrono::Utc;
use sqlx::Row;
use uuid::Uuid;

use crate::{
    db::{begin_immediate, db_error},
    errors::ApiError,
    AppState,
};

use super::*;

pub(crate) async fn create_refresh_session(
    state: &AppState,
    username: &str,
) -> Result<IssuedRefreshToken, ApiError> {
    let refresh = new_refresh_token(state);
    sqlx::query(
        r#"
        INSERT INTO admin_refresh_sessions (id, username, token_hash, expires_at)
        VALUES ($1, $2, $3, $4)
        "#,
    )
    .bind(refresh.session_id)
    .bind(username)
    .bind(&refresh.token_hash)
    .bind(refresh.expires_at)
    .execute(&state.pool)
    .await
    .map_err(db_error)?;
    Ok(refresh)
}

pub(crate) async fn rotate_refresh_session(
    state: &AppState,
    token: &str,
) -> Result<IssuedRefreshToken, ApiError> {
    let token_hash = refresh_token_hash(token);
    let refresh = new_refresh_token(state);
    let now = Utc::now();
    let mut tx = begin_immediate(&state.pool).await?;
    let row = sqlx::query(
        r#"
        SELECT id, username
        FROM admin_refresh_sessions
        WHERE token_hash=$1
            AND revoked_at IS NULL
            AND expires_at > $2
        "#,
    )
    .bind(&token_hash)
    .bind(now)
    .fetch_optional(&mut *tx)
    .await
    .map_err(db_error)?;

    let Some(row) = row else {
        return Err(ApiError::unauthorized("Invalid or expired refresh token"));
    };
    let previous_session_id = row.try_get::<Uuid, _>("id").map_err(db_error)?;
    let username = row.try_get::<String, _>("username").map_err(db_error)?;
    if username != state.config.admin_username {
        return Err(ApiError::unauthorized("Invalid or expired refresh token"));
    }

    sqlx::query(
        r#"
        INSERT INTO admin_refresh_sessions (id, username, token_hash, expires_at)
        VALUES ($1, $2, $3, $4)
        "#,
    )
    .bind(refresh.session_id)
    .bind(&username)
    .bind(&refresh.token_hash)
    .bind(refresh.expires_at)
    .execute(&mut *tx)
    .await
    .map_err(db_error)?;
    sqlx::query(
        r#"
        UPDATE admin_refresh_sessions
        SET revoked_at=$2,
            last_used_at=$2,
            replaced_by_session_id=$3
        WHERE id=$1
        "#,
    )
    .bind(previous_session_id)
    .bind(now)
    .bind(refresh.session_id)
    .execute(&mut *tx)
    .await
    .map_err(db_error)?;
    tx.commit().await.map_err(db_error)?;
    Ok(refresh)
}

pub(crate) async fn revoke_refresh_session(state: &AppState, token: &str) -> Result<(), ApiError> {
    let token_hash = refresh_token_hash(token);
    let now = Utc::now();
    sqlx::query(
        r#"
        UPDATE admin_refresh_sessions
        SET revoked_at=COALESCE(revoked_at, $2),
            last_used_at=$2
        WHERE token_hash=$1
            AND revoked_at IS NULL
        "#,
    )
    .bind(&token_hash)
    .bind(now)
    .execute(&state.pool)
    .await
    .map_err(db_error)?;
    Ok(())
}
