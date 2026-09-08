//! Catalog-only publication shares the media payment and lease contract.
use super::{db_document::build_all_catalog_from_db, state_file::write_catalog_state};
use crate::{
    db::{begin_fenced, begin_immediate, db_error, execute_fenced},
    errors::ApiError,
    models::PublicCatalogDocument,
    pipeline::{assert_catalog_revision, catalog_revision, payment_api},
    state::AppState,
};
use autvid_common::payments::{
    amount, content_digest, ApprovedContent, ContentQuote, PaymentApproval,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

#[derive(Serialize, Deserialize)]
struct CatalogPlan {
    approval: PaymentApproval,
    catalog: PublicCatalogDocument,
    all_catalog: PublicCatalogDocument,
    catalog_quote: ContentQuote,
    all_catalog_quote: ContentQuote,
    revision: i64,
}

pub(crate) async fn prepare(state: &AppState) -> Result<(), ApiError> {
    let mut check = begin_fenced(state).await?;
    require_reconciled_catalog(&mut check).await?;
    check.commit().await.map_err(db_error)?;
    let revision = catalog_revision(state).await?;
    let all_catalog = build_all_catalog_from_db(state).await?;
    let mut catalog = all_catalog.clone();
    catalog.catalog_kind = "published".into();
    catalog.videos.retain(|v| v.is_public);
    let mode = &state.config.antd_metadata_payment_mode;
    let catalog_quote = state
        .antd
        .content_cost(&serde_json::to_vec(&catalog).map_err(payment_api)?, mode)
        .await
        .map_err(payment_api)?;
    let all_catalog_quote = state
        .antd
        .content_cost(
            &serde_json::to_vec(&all_catalog).map_err(payment_api)?,
            mode,
        )
        .await
        .map_err(payment_api)?;
    let network = catalog_quote.network.clone();
    if all_catalog_quote.network != network {
        return Err(payment_api("Network changed during catalog quote"));
    }
    let quotes = [&catalog_quote, &all_catalog_quote];
    let contents = quotes
        .iter()
        .map(|q| ApprovedContent {
            sha256: q.content_sha256.clone(),
            byte_size: q.file_size,
            payment_mode: q.payment_mode.clone(),
        })
        .collect::<Vec<_>>();
    let sum = |gas: bool| -> Result<String, ApiError> {
        Ok(quotes
            .iter()
            .try_fold(0u128, |sum, q| {
                sum.checked_add(
                    amount(if gas {
                        &q.estimated_gas_cost_wei
                    } else {
                        &q.cost
                    })
                    .map_err(payment_api)?,
                )
                .ok_or_else(|| payment_api("Catalog quote overflow"))
            })?
            .to_string())
    };
    let approval = PaymentApproval {
        quote_id: Uuid::new_v4().to_string(),
        network: network.clone(),
        content_digest: content_digest(&network, &contents).map_err(payment_api)?,
        expires_at: Utc::now().timestamp()
            + state.config.final_quote_approval_ttl_seconds.min(86_400),
        max_storage_atto: sum(false)?,
        max_gas_wei: sum(true)?,
        contents,
    };
    let plan = CatalogPlan {
        approval,
        catalog,
        all_catalog,
        catalog_quote,
        all_catalog_quote,
        revision,
    };
    let mut tx = begin_fenced(state).await?;
    assert_catalog_revision(&mut tx, revision).await?;
    require_reconciled_catalog(&mut tx).await?;
    sqlx::query("UPDATE catalog_approvals SET state='superseded' WHERE state='draft'")
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
    sqlx::query("INSERT INTO catalog_approvals(id,plan,state,created_at) VALUES($1,$2,'draft',$3)")
        .bind(&plan.approval.quote_id)
        .bind(serde_json::to_string(&plan).map_err(payment_api)?)
        .bind(Utc::now())
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
    tx.commit().await.map_err(db_error)?;
    Ok(())
}

pub(crate) async fn summary(state: &AppState) -> Result<Option<Value>, ApiError> {
    let row=sqlx::query("SELECT plan,state,error_message FROM catalog_approvals ORDER BY created_at DESC,id DESC LIMIT 1").fetch_optional(&state.pool).await.map_err(db_error)?;
    row.map(|row| {
        let plan:CatalogPlan=serde_json::from_str(row.try_get("plan").map_err(db_error)?).map_err(payment_api)?;
        let mut approval=serde_json::to_value(&plan.approval).map_err(payment_api)?;
        if let Some(object)=approval.as_object_mut(){object.remove("contents");}
        Ok(json!({"state":row.try_get::<String,_>("state").map_err(db_error)?,"approval":approval,"error":row.try_get::<Option<String>,_>("error_message").map_err(db_error)?}))
    }).transpose()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ApprovalRequest {
    pub quote_id: String,
    pub max_storage_atto: String,
    pub max_gas_wei: String,
}

pub(crate) async fn approve(state: &AppState, request: ApprovalRequest) -> Result<(), ApiError> {
    let mut tx = begin_immediate(&state.pool).await?;
    let row = sqlx::query("SELECT plan,state FROM catalog_approvals WHERE id=$1")
        .bind(&request.quote_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(db_error)?;
    if row.try_get::<String, _>("state").map_err(db_error)? != "draft" {
        return Err(payment_api("Catalog quote is no longer awaiting approval"));
    }
    let plan: CatalogPlan =
        serde_json::from_str(row.try_get("plan").map_err(db_error)?).map_err(payment_api)?;
    if request.max_storage_atto != plan.approval.max_storage_atto
        || request.max_gas_wei != plan.approval.max_gas_wei
    {
        return Err(payment_api("Review the current storage and gas caps"));
    }
    assert_catalog_revision(&mut tx, plan.revision).await?;
    state
        .antd
        .approve_payment(&plan.approval)
        .await
        .map_err(payment_api)?;
    sqlx::query("UPDATE catalog_approvals SET state='approved' WHERE id=$1")
        .bind(&request.quote_id)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
    sqlx::query("INSERT INTO video_jobs(id,job_kind,status,max_attempts,run_after,payment_quote_id) VALUES($1,'finalize_catalog','queued',$2,$3,$4)")
        .bind(Uuid::new_v4()).bind(state.config.catalog_publish_job_max_attempts).bind(Utc::now()).bind(&request.quote_id).execute(&mut *tx).await.map_err(db_error)?;
    tx.commit().await.map_err(db_error)?;
    state
        .job_notify_tx
        .send_modify(|epoch| *epoch = epoch.saturating_add(1));
    Ok(())
}

pub(crate) async fn finalize(state: &AppState) -> Result<(), ApiError> {
    let job = state
        .active_job
        .as_ref()
        .ok_or_else(|| payment_api("Catalog publication requires a leased job"))?;
    let row=sqlx::query("SELECT c.plan,c.state FROM video_jobs j JOIN catalog_approvals c ON c.id=j.payment_quote_id WHERE j.id=$1").bind(job.id).fetch_one(&state.pool).await.map_err(db_error)?;
    let plan: CatalogPlan =
        serde_json::from_str(row.try_get("plan").map_err(db_error)?).map_err(payment_api)?;
    let status: String = row.try_get("state").map_err(db_error)?;
    if status == "complete" {
        return Ok(());
    }
    if !matches!(status.as_str(), "approved" | "uploading") {
        return Err(payment_api(
            "payment_recovery_required: catalog approval paused",
        ));
    }
    if catalog_revision(state).await? != plan.revision {
        return Err(payment_api("approval_required: catalog changed"));
    }
    execute_fenced(
        state,
        sqlx::query("UPDATE catalog_approvals SET state='uploading' WHERE id=$1")
            .bind(&plan.approval.quote_id),
    )
    .await?;
    let client = state.antd.with_approval(&plan.approval.quote_id);
    for (document, quote) in [
        (&plan.catalog, &plan.catalog_quote),
        (&plan.all_catalog, &plan.all_catalog_quote),
    ] {
        let result = client
            .data_put_public(
                &serde_json::to_vec(document).map_err(payment_api)?,
                &quote.payment_mode,
            )
            .await
            .map_err(payment_api)?;
        if result.address != quote.address {
            return Err(payment_api(
                "payment_recovery_required: catalog address mismatch",
            ));
        }
    }
    let _lock = state.catalog_lock.lock().await;
    let mut tx = begin_fenced(state).await?;
    assert_catalog_revision(&mut tx, plan.revision).await?;
    sqlx::query("UPDATE videos SET catalog_address=$1,all_catalog_address=$2 WHERE status='ready'")
        .bind(&plan.catalog_quote.address)
        .bind(&plan.all_catalog_quote.address)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
    sqlx::query("UPDATE catalog_approvals SET state='complete' WHERE id=$1")
        .bind(&plan.approval.quote_id)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
    sqlx::query("INSERT INTO application_state(key,value) VALUES('catalog_snapshot',$1) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
        .bind(json!({"published_address":plan.catalog_quote.address,"all_address":plan.all_catalog_quote.address,"published":plan.catalog,"all":plan.all_catalog}).to_string()).execute(&mut *tx).await.map_err(db_error)?;
    write_catalog_state(
        &state.config,
        Some(&plan.catalog_quote.address),
        Some(&plan.all_catalog_quote.address),
        Some(&plan.catalog),
        Some(&plan.all_catalog),
        false,
    )?;
    tx.commit().await.map_err(db_error)?;
    Ok(())
}

pub(crate) async fn restore_snapshot(state: &AppState) -> Result<(), ApiError> {
    let _lock = state.catalog_lock.lock().await;
    let mut tx = begin_immediate(&state.pool).await?;
    let snapshot: Option<String> =
        sqlx::query_scalar("SELECT value FROM application_state WHERE key='catalog_snapshot'")
            .fetch_optional(&mut *tx)
            .await
            .map_err(db_error)?;
    if let Some(snapshot) = snapshot {
        let value: Value = serde_json::from_str(&snapshot).map_err(payment_api)?;
        write_catalog_state(
            &state.config,
            value["published_address"].as_str(),
            value["all_address"].as_str(),
            Some(&value["published"]),
            Some(&value["all"]),
            value["publish_pending"].as_bool().unwrap_or(false),
        )?;
    }
    tx.commit().await.map_err(db_error)?;
    Ok(())
}

/// A fresh quote cannot hide an active or uncertain payment behind a new identity.
async fn require_reconciled_catalog(
    connection: &mut sqlx::SqliteConnection,
) -> Result<(), ApiError> {
    let pending: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM catalog_approvals WHERE state IN ('approved','uploading','payment_recovery_required'))")
        .fetch_one(connection).await.map_err(db_error)?;
    if pending {
        return Err(payment_api(
            "payment_recovery_required: resume or reconcile the existing catalog payment first",
        ));
    }
    Ok(())
}

pub(crate) async fn resume(state: &AppState) -> Result<(), ApiError> {
    let mut tx = begin_immediate(&state.pool).await?;
    let id: Option<String> = sqlx::query_scalar("SELECT id FROM catalog_approvals WHERE state='payment_recovery_required' ORDER BY created_at DESC,id DESC LIMIT 1")
        .fetch_optional(&mut *tx).await.map_err(db_error)?;
    let id = id.ok_or_else(|| payment_api("No paused catalog publication is available"))?;
    let resumed = sqlx::query("UPDATE video_jobs SET status='queued',run_after=$1,lease_owner=NULL,lease_expires_at=NULL,last_error=NULL,max_attempts=MAX(max_attempts,attempts+1),updated_at=$1 WHERE id=(SELECT id FROM video_jobs WHERE payment_quote_id=$2 AND job_kind='finalize_catalog' AND status='failed' ORDER BY created_at DESC,id DESC LIMIT 1)")
        .bind(Utc::now()).bind(&id).execute(&mut *tx).await.map_err(db_error)?.rows_affected();
    if resumed != 1 {
        return Err(payment_api(
            "The original catalog job is unavailable; reconcile its payment journal",
        ));
    }
    sqlx::query("UPDATE catalog_approvals SET state='approved',error_message=NULL WHERE id=$1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
    tx.commit().await.map_err(db_error)?;
    state
        .job_notify_tx
        .send_modify(|epoch| *epoch = epoch.saturating_add(1));
    Ok(())
}
