use super::final_quote::{assert_catalog_revision, catalog_digest, payment_api, UploadPlan};
use crate::{
    catalog::db_document::build_all_catalog_from_db,
    db::{begin_fenced, db_error, execute_fenced, parse_video_uuid},
    errors::ApiError,
    media::assert_under,
    state::AppState,
};
use autvid_common::payments::ContentQuote;
use futures_util::{stream, StreamExt, TryStreamExt};
use serde_json::Value;
use sqlx::Row;

pub(crate) async fn upload_approved_video_inner(
    state: &AppState,
    video_id: &str,
) -> Result<(), ApiError> {
    let uuid = parse_video_uuid(video_id)?;
    let row = sqlx::query("SELECT final_quote, approved_quote_id FROM videos WHERE id=$1")
        .bind(uuid)
        .fetch_one(&state.pool)
        .await
        .map_err(db_error)?;
    let quote: Value = row.try_get("final_quote").map_err(db_error)?;
    let mut plan: UploadPlan = serde_json::from_value(quote["plan"].clone())
        .map_err(|_| payment_api("approval_required: legacy quote must be regenerated"))?;
    if row
        .try_get::<Option<String>, _>("approved_quote_id")
        .map_err(db_error)?
        .as_deref()
        != Some(&plan.approval.quote_id)
    {
        return Err(payment_api(
            "approval_required: this quote has not been approved",
        ));
    }
    plan.approval
        .validate(chrono::Utc::now().timestamp(), &plan.approval.network)
        .map_err(payment_api)?;
    check_catalog(state, &plan, video_id).await?;
    // Check every file before the first payment; the gateway checks its streamed bytes
    // again against the same approval before each controlled signing operation.
    for file in &plan.files {
        let path = assert_under(&file.path, &state.config.upload_temp_dir)?;
        let (size, sha) = autvid_common::antd::sha256_file_async(&path)
            .await
            .map_err(payment_api)?;
        if size != file.quote.file_size || sha != file.quote.content_sha256 {
            return Err(payment_api("approval_required: media changed after quote"));
        }
    }
    let mut state = state.clone();
    state.antd = state.antd.with_approval(&plan.approval.quote_id);
    let state = &state;
    let files = std::mem::take(&mut plan.files);
    stream::iter(files).map(|file| async move {
        let _permit = state.upload_semaphore.acquire().await.map_err(payment_api)?;
        let result = state.antd.file_put_public(&file.path, &file.quote.payment_mode, state.config.antd_upload_verify, state.config.antd_upload_retries).await.map_err(payment_api)?;
        if result.address != file.quote.address || result.byte_size != file.quote.file_size || result.chunks_failed != 0 || result.chunks_stored != result.total_chunks || (state.config.antd_upload_verify && !result.verified) {
            return Err(payment_api("payment_recovery_required: incomplete or unexpected upload result"));
        }
        if let Some(variant_id) = file.variant_id {
            execute_fenced(state, sqlx::query("UPDATE video_segments SET autonomi_address=$1, autonomi_cost_atto=$2, autonomi_payment_mode=$3, byte_size=$4 WHERE variant_id=$5 AND segment_index=$6")
                .bind(result.address).bind(result.storage_cost_atto).bind(result.payment_mode_used).bind(i64::try_from(result.byte_size).map_err(payment_api)?).bind(variant_id).bind(file.segment_index)).await?;
        } else {
            execute_fenced(state, sqlx::query("UPDATE videos SET original_file_address=$1,original_file_autonomi_cost_atto=$2,original_file_autonomi_payment_mode=$3,original_file_byte_size=$4 WHERE id=$5")
                .bind(result.address).bind(result.storage_cost_atto).bind(result.payment_mode_used).bind(i64::try_from(result.byte_size).map_err(payment_api)?).bind(uuid)).await?;
        }
        Ok::<_,ApiError>(())
    }).buffer_unordered(state.config.antd_upload_concurrency.max(1)).try_collect::<Vec<_>>().await?;
    store_planned(state, &plan.manifest, &plan.manifest_quote).await?;
    check_catalog(state, &plan, video_id).await?;
    store_planned(state, &plan.catalog, &plan.catalog_quote).await?;
    store_planned(state, &plan.all_catalog, &plan.all_catalog_quote).await?;

    let mut tx = begin_fenced(state).await?;
    assert_catalog_revision(&mut tx, plan.base_revision).await?;
    sqlx::query("UPDATE videos SET status='ready',manifest_address=$1,is_public=$2,approval_expires_at=NULL,error_message=NULL,updated_at=$3 WHERE id=$4")
        .bind(&plan.manifest_quote.address).bind(plan.publish).bind(&plan.manifest.updated_at).bind(uuid).execute(&mut *tx).await.map_err(db_error)?;
    sqlx::query("UPDATE videos SET catalog_address=$1,all_catalog_address=$2 WHERE status='ready'")
        .bind(&plan.catalog_quote.address)
        .bind(&plan.all_catalog_quote.address)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
    // Commit the snapshot with ready state before exposing it through the file.
    sqlx::query("INSERT INTO application_state(key,value) VALUES('catalog_snapshot',$1) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
        .bind(serde_json::json!({"published_address":plan.catalog_quote.address,"all_address":plan.all_catalog_quote.address,"published":plan.catalog,"all":plan.all_catalog}).to_string())
        .execute(&mut *tx).await.map_err(db_error)?;
    crate::catalog::payments::commit_snapshot(state, tx).await?;
    tracing::info!(video_id, manifest=%plan.manifest_quote.address, "Approved media and catalog storage completed");
    Ok(())
}

async fn check_catalog(
    state: &AppState,
    plan: &UploadPlan,
    video_id: &str,
) -> Result<(), ApiError> {
    let current = build_all_catalog_from_db(state).await?;
    if catalog_digest(&current, Some(video_id))? != plan.base_catalog_digest {
        return Err(payment_api(
            "approval_required: catalog changed; regenerate the quote before publishing",
        ));
    }
    Ok(())
}

async fn store_planned<T: serde::Serialize>(
    state: &AppState,
    value: &T,
    quote: &ContentQuote,
) -> Result<(), ApiError> {
    let data = serde_json::to_vec(value).map_err(payment_api)?;
    let result = state
        .antd
        .data_put_public(&data, &quote.payment_mode)
        .await
        .map_err(payment_api)?;
    if result.address != quote.address {
        return Err(payment_api(
            "payment_recovery_required: stored metadata address differs from approval",
        ));
    }
    Ok(())
}
