//! Quotes are prepared from actual content. Frozen metadata makes the reviewed
//! digest reproducible without depending on upload completion times or prices.
use crate::{
    catalog::db_document::{
        build_all_catalog_from_db, build_catalog_entry_from_db, build_manifest_from_db,
    },
    db::{db_error, parse_video_uuid},
    errors::ApiError,
    media::assert_under,
    models::{ManifestOriginalFile, PublicCatalogDocument, VideoManifestDocument},
    state::AppState,
};
use autvid_common::payments::{
    amount, content_digest, ApprovedContent, ContentQuote, PaymentApproval,
};
use axum::http::StatusCode;
use chrono::Utc;
use futures_util::{stream, StreamExt, TryStreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::Row;
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct PlannedFile {
    pub path: PathBuf,
    pub variant_id: Option<Uuid>,
    pub segment_index: Option<i32>,
    pub quote: ContentQuote,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct UploadPlan {
    pub approval: PaymentApproval,
    pub files: Vec<PlannedFile>,
    pub manifest: VideoManifestDocument,
    pub manifest_quote: ContentQuote,
    pub catalog: PublicCatalogDocument,
    pub catalog_quote: ContentQuote,
    pub all_catalog: PublicCatalogDocument,
    pub all_catalog_quote: ContentQuote,
    pub base_catalog_digest: String,
    pub base_revision: i64,
    pub publish: bool,
}

pub(crate) fn payment_api(error: impl std::fmt::Display) -> ApiError {
    ApiError::new(StatusCode::CONFLICT, error.to_string())
}

pub(crate) fn catalog_digest(
    catalog: &PublicCatalogDocument,
    exclude: Option<&str>,
) -> Result<String, ApiError> {
    let mut videos = catalog
        .videos
        .iter()
        .filter(|v| Some(v.id.as_str()) != exclude)
        .cloned()
        .collect::<Vec<_>>();
    videos.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(autvid_common::antd::hex_lower(&Sha256::digest(
        serde_json::to_vec(&videos).map_err(payment_api)?,
    )))
}

pub(crate) async fn build_final_upload_quote(
    state: &AppState,
    video_id: &str,
) -> Result<Value, ApiError> {
    let uuid = parse_video_uuid(video_id)?;
    let video = sqlx::query(
        "SELECT upload_original, job_source_path, publish_when_ready FROM videos WHERE id=$1",
    )
    .bind(uuid)
    .fetch_one(&state.pool)
    .await
    .map_err(db_error)?;
    let rows = sqlx::query("SELECT s.variant_id,s.segment_index,s.local_path FROM video_segments s JOIN video_variants v ON v.id=s.variant_id WHERE v.video_id=$1 ORDER BY v.height DESC,v.id,s.segment_index")
        .bind(uuid).fetch_all(&state.pool).await.map_err(db_error)?;
    if rows.is_empty() || rows.len() > 65_536 {
        return Err(payment_api("invalid transcoded segment count"));
    }
    let mut inputs = Vec::with_capacity(rows.len() + 1);
    for row in rows {
        inputs.push((
            PathBuf::from(row.try_get::<String, _>("local_path").map_err(db_error)?),
            Some(row.try_get::<Uuid, _>("variant_id").map_err(db_error)?),
            Some(row.try_get::<i32, _>("segment_index").map_err(db_error)?),
        ));
    }
    if video
        .try_get::<bool, _>("upload_original")
        .map_err(db_error)?
    {
        inputs.push((
            PathBuf::from(
                video
                    .try_get::<String, _>("job_source_path")
                    .map_err(db_error)?,
            ),
            None,
            None,
        ));
    }
    let files: Vec<PlannedFile> = stream::iter(inputs)
        .map(|(path, variant_id, segment_index)| async move {
            let path = assert_under(&path, &state.config.upload_temp_dir)?;
            let _permit = state.quote_semaphore.acquire().await.map_err(payment_api)?;
            let quote = state
                .antd
                .file_cost(&path, &state.config.antd_payment_mode)
                .await
                .map_err(payment_api)?;
            let (size, sha) = autvid_common::antd::sha256_file_async(&path)
                .await
                .map_err(payment_api)?;
            if size != quote.file_size || sha != quote.content_sha256 {
                return Err(payment_api("content changed during final quote"));
            }
            Ok::<_, ApiError>(PlannedFile {
                path,
                variant_id,
                segment_index,
                quote,
            })
        })
        .buffered(state.config.antd_quote_concurrency.max(1))
        .try_collect()
        .await?;

    let mut manifest = build_manifest_from_db(state, video_id, true).await?;
    manifest.updated_at = Utc::now().to_rfc3339();
    for file in &files {
        if let Some(id) = file.variant_id {
            let segment = manifest
                .variants
                .iter_mut()
                .find(|v| v.id == id.to_string())
                .and_then(|v| {
                    v.segments
                        .iter_mut()
                        .find(|s| Some(s.segment_index) == file.segment_index)
                })
                .ok_or_else(|| payment_api("quoted segment is absent from manifest"))?;
            segment.autonomi_address = Some(file.quote.address.clone());
            segment.byte_size = Some(i64::try_from(file.quote.file_size).map_err(payment_api)?);
        } else {
            manifest.original_file = Some(ManifestOriginalFile {
                autonomi_address: file.quote.address.clone(),
                byte_size: Some(i64::try_from(file.quote.file_size).map_err(payment_api)?),
                autonomi_cost_atto: None,
                payment_mode: Some(file.quote.payment_mode.clone()),
            });
        }
    }
    let metadata_mode = &state.config.antd_metadata_payment_mode;
    let manifest_quote = state
        .antd
        .content_cost(
            &serde_json::to_vec(&manifest).map_err(payment_api)?,
            metadata_mode,
        )
        .await
        .map_err(payment_api)?;
    let base_revision = catalog_revision(state).await?;
    let mut all_catalog = build_all_catalog_from_db(state).await?;
    let base_catalog_digest = catalog_digest(&all_catalog, Some(video_id))?;
    all_catalog.videos.retain(|v| v.id != video_id);
    let publish: bool = video.try_get("publish_when_ready").map_err(db_error)?;
    let mut entry =
        build_catalog_entry_from_db(state, video_id, manifest_quote.address.clone()).await?;
    entry.is_public = publish;
    entry.updated_at = manifest.updated_at.clone();
    all_catalog.videos.insert(0, entry);
    let mut catalog = all_catalog.clone();
    catalog.catalog_kind = "published".into();
    catalog.videos.retain(|v| v.is_public);
    let catalog_quote = state
        .antd
        .content_cost(
            &serde_json::to_vec(&catalog).map_err(payment_api)?,
            metadata_mode,
        )
        .await
        .map_err(payment_api)?;
    let all_catalog_quote = state
        .antd
        .content_cost(
            &serde_json::to_vec(&all_catalog).map_err(payment_api)?,
            metadata_mode,
        )
        .await
        .map_err(payment_api)?;
    let quotes = files
        .iter()
        .map(|f| &f.quote)
        .chain([&manifest_quote, &catalog_quote, &all_catalog_quote])
        .collect::<Vec<_>>();
    let network = manifest_quote.network.clone();
    if quotes.iter().any(|q| q.network != network) {
        return Err(payment_api("network changed during quote"));
    }
    let contents = quotes
        .iter()
        .map(|q| ApprovedContent {
            sha256: q.content_sha256.clone(),
            byte_size: q.file_size,
            payment_mode: q.payment_mode.clone(),
        })
        .collect::<Vec<_>>();
    let total = |gas: bool| -> Result<u128, ApiError> {
        quotes.iter().try_fold(0u128, |sum, q| {
            sum.checked_add(
                amount(if gas {
                    &q.estimated_gas_cost_wei
                } else {
                    &q.cost
                })
                .map_err(payment_api)?,
            )
            .ok_or_else(|| payment_api("quote sum overflow"))
        })
    };
    let storage = total(false)?.to_string();
    let gas = total(true)?.to_string();
    let approval = PaymentApproval {
        quote_id: Uuid::new_v4().to_string(),
        content_digest: content_digest(&network, &contents).map_err(payment_api)?,
        network,
        expires_at: Utc::now().timestamp()
            + state.config.final_quote_approval_ttl_seconds.min(86_400),
        max_storage_atto: storage.clone(),
        max_gas_wei: gas.clone(),
        contents,
    };
    approval
        .validate(Utc::now().timestamp(), &approval.network)
        .map_err(payment_api)?;
    let variants = manifest.variants.iter().map(|v| {
        let quoted = files.iter().filter(|f| f.variant_id.is_some_and(|id| id.to_string()==v.id)).map(|f| &f.quote).collect::<Vec<_>>();
        let storage = quoted.iter().try_fold(0u128, |sum,q| sum.checked_add(amount(&q.cost).map_err(payment_api)?).ok_or_else(|| payment_api("variant cost overflow")))?;
        let gas = quoted.iter().try_fold(0u128, |sum,q| sum.checked_add(amount(&q.estimated_gas_cost_wei).map_err(payment_api)?).ok_or_else(|| payment_api("variant gas overflow")))?;
        Ok::<_,ApiError>(json!({"resolution":v.resolution,"width":v.width,"height":v.height,"segment_count":v.segment_count,"estimated_bytes":quoted.iter().map(|q|q.file_size).sum::<u64>(),"actual_bytes":quoted.iter().map(|q|q.file_size).sum::<u64>(),"chunk_count":quoted.iter().map(|q|q.chunk_count).sum::<usize>(),"storage_cost_atto":storage.to_string(),"estimated_gas_cost_wei":gas.to_string(),"payment_mode":state.config.antd_payment_mode}))
    }).collect::<Result<Vec<_>,_>>()?;
    let media_bytes = files.iter().map(|f| f.quote.file_size).sum::<u64>();
    let metadata_bytes =
        manifest_quote.file_size + catalog_quote.file_size + all_catalog_quote.file_size;
    let original = files.iter().find(|f|f.variant_id.is_none()).map(|f|json!({"byte_size":f.quote.file_size,"chunk_count":f.quote.chunk_count,"storage_cost_atto":f.quote.cost,"estimated_gas_cost_wei":f.quote.estimated_gas_cost_wei,"payment_mode":f.quote.payment_mode}));
    if catalog_revision(state).await? != base_revision {
        return Err(payment_api(
            "Catalog changed during quoting; retry quote preparation",
        ));
    }
    let plan = UploadPlan {
        approval: approval.clone(),
        files,
        manifest,
        manifest_quote,
        catalog,
        catalog_quote,
        all_catalog,
        all_catalog_quote,
        base_catalog_digest,
        base_revision,
        publish,
    };
    Ok(
        json!({"quote_type":"final", "quote_id":approval.quote_id,"approval":approval,"plan":plan,
        "duration_seconds":plan.manifest.variants.iter().filter_map(|v|v.total_duration).fold(0.0,f64::max),
        "segment_duration":state.config.hls_segment_duration,"payment_mode":state.config.antd_payment_mode,
        "estimated_bytes":media_bytes+metadata_bytes,"actual_media_bytes":media_bytes,"actual_transcoded_bytes":media_bytes-original.as_ref().and_then(|o|o["byte_size"].as_u64()).unwrap_or(0),"metadata_bytes":metadata_bytes,
        "segment_count":plan.files.iter().filter(|f|f.variant_id.is_some()).count(),"chunk_count":plan.files.iter().map(|f|f.quote.chunk_count).sum::<usize>()+plan.manifest_quote.chunk_count+plan.catalog_quote.chunk_count+plan.all_catalog_quote.chunk_count,
        "storage_cost_atto":storage,"estimated_gas_cost_wei":gas,"original_file":original,"sampled":false,
        "confidence":"actual_content_storage_upper_bound","gas_confidence":"aggregate_signing_cap","approval_ttl_seconds":state.config.final_quote_approval_ttl_seconds,"variants":variants}),
    )
}

pub(crate) async fn catalog_revision(state: &AppState) -> Result<i64, ApiError> {
    sqlx::query_scalar("SELECT value FROM catalog_generation WHERE singleton=1")
        .fetch_one(&state.pool)
        .await
        .map_err(db_error)
}
pub(crate) async fn assert_catalog_revision(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    expected: i64,
) -> Result<(), ApiError> {
    let actual: i64 = sqlx::query_scalar("SELECT value FROM catalog_generation WHERE singleton=1")
        .fetch_one(&mut **tx)
        .await
        .map_err(db_error)?;
    if actual != expected {
        return Err(payment_api(
            "approval_required: catalog changed; regenerate the quote",
        ));
    }
    Ok(())
}
