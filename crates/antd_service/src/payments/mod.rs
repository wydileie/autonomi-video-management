//! Controlled payments. Every wallet write must pass the durable approval journal.
mod journal;
mod signer;

use std::{collections::HashMap, path::Path, sync::Arc, time::Duration};

use ant_core::data::{Client, FinalizeOutcome, FinalizeResume, PaymentMode, Visibility};
use autvid_common::payments::PaymentApproval;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    Json,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;
use tokio::sync::{watch, Mutex, Semaphore};
use zeroize::Zeroizing;

use crate::{config::Config, error::ApiError, state::AppState};
use journal::Journal;
use signer::{ControlledSigner, PaymentProofs};

type UploadResult = Result<UploadReceipt, String>;
type UploadReceiver = watch::Receiver<Option<UploadResult>>;

pub(crate) struct Payments {
    journal: Arc<Journal>,
    signer: ControlledSigner,
    running: Mutex<HashMap<String, UploadReceiver>>,
    resumes: Mutex<HashMap<String, ResumeSession>>,
    slots: Arc<Semaphore>,
}

pub(crate) struct UploadRequest {
    pub file: NamedTempFile,
    pub approval: String,
    pub lease_key: String,
    pub sha256: String,
    pub size: u64,
    pub mode: PaymentMode,
    pub verify: bool,
}

struct UploadContext<'a> {
    id: &'a str,
    approval: &'a str,
    sha256: &'a str,
    size: u64,
    mode: PaymentMode,
    verify: bool,
}

struct ResumeContext<'a> {
    id: &'a str,
    sha256: &'a str,
    size: u64,
    verify: bool,
    storage: u128,
    gas: u128,
}

struct ResumeSession {
    handle: FinalizeResume,
    receipt: UploadReceipt,
    sha256: String,
    verify: bool,
    expires: std::time::Instant,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct UploadReceipt {
    pub address: String,
    pub byte_size: u64,
    pub chunks_stored: usize,
    pub total_chunks: usize,
    pub chunks_failed: usize,
    pub storage_cost_atto: String,
    pub estimated_gas_cost_wei: String,
    pub payment_mode_used: String,
    pub verified: bool,
    pub upload_id: String,
}

pub(crate) use autvid_common::payments::ContentQuote;

impl Payments {
    pub(crate) async fn new(config: &Config) -> anyhow::Result<Arc<Self>> {
        let key = crate::client::wallet_key()?.map(Zeroizing::new);
        let key = key.as_ref().map(|k| k.parse()).transpose()?;
        let signer = ControlledSigner::new(crate::client::evm_network()?, key).await?;
        let journal = Arc::new(Journal::open(&config.payment_db_path, signer.identity()).await?);
        let payments = Arc::new(Self {
            journal,
            signer,
            running: Mutex::new(HashMap::new()),
            resumes: Mutex::new(HashMap::new()),
            slots: Arc::new(Semaphore::new(4)),
        });
        let weak = Arc::downgrade(&payments);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            loop {
                interval.tick().await;
                let Some(payments) = weak.upgrade() else {
                    break;
                };
                payments
                    .resumes
                    .lock()
                    .await
                    .retain(|_, session| session.expires > std::time::Instant::now());
            }
        });
        Ok(payments)
    }

    pub(crate) async fn can_write(&self) -> bool {
        self.signer.key.is_some() && self.journal.signing_enabled().await.unwrap_or(false)
    }

    pub(crate) fn identity(&self) -> &str {
        &self.journal.network
    }

    pub(crate) async fn quote(
        &self,
        client: &Client,
        path: &Path,
        mode: PaymentMode,
    ) -> anyhow::Result<ContentQuote> {
        let _permit = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| anyhow::anyhow!("quote capacity exhausted"))?;
        let (size, sha256) = autvid_common::antd::sha256_file_async(path).await?;
        validate_buffered_payment_mode(mode, size)?;
        let prepared = client
            .file_prepare_upload_with_mode(path, Visibility::Public, mode, None)
            .await?;
        let cost = signer::storage_upper(&prepared, &self.signer.network)?;
        let calls = match &prepared.payment_info {
            ant_core::data::ExternalPaymentInfo::WaveBatch { payment_intent, .. } => {
                payment_intent.payments.len().div_ceil(256)
            }
            ant_core::data::ExternalPaymentInfo::Merkle {
                prepared_batches, ..
            } => prepared_batches.len(),
        };
        let gas = if cost == 0 {
            0
        } else {
            let fees = self.signer.provider.estimate_eip1559_fees().await?;
            // A displayed gas ceiling, never a promise of an exact fee. The signer
            // estimates each actual call and checks the aggregate cap before signing.
            fees.max_fee_per_gas
                .checked_mul(2)
                .and_then(|f| f.checked_mul(5_000_000))
                .and_then(|g| g.checked_mul((calls + 1) as u128))
                .ok_or_else(|| anyhow::anyhow!("quote gas ceiling overflow"))?
        };
        Ok(ContentQuote {
            cost: cost.to_string(),
            estimated_gas_cost_wei: gas.to_string(),
            file_size: size,
            chunk_count: prepared.total_chunks,
            payment_mode: format_mode(mode).into(),
            address: hex::encode(
                prepared
                    .data_map_address
                    .ok_or_else(|| anyhow::anyhow!("public preparation has no address"))?,
            ),
            content_sha256: sha256,
            network: self.identity().into(),
            confidence: if cost == 0 {
                "verified_all_already_stored"
            } else {
                "prepared_storage_upper_bound"
            }
            .into(),
        })
    }

    pub(crate) async fn upload(
        self: &Arc<Self>,
        client: Arc<Client>,
        request: UploadRequest,
    ) -> anyhow::Result<UploadReceipt> {
        let UploadRequest {
            file,
            approval,
            lease_key,
            sha256,
            size,
            mode,
            verify,
        } = request;
        validate_buffered_payment_mode(mode, size)?;
        let id = hex::encode(Sha256::digest(serde_json::to_vec(&(
            &approval,
            &sha256,
            format_mode(mode),
        ))?));
        if let Some(result) = self.journal.result(&id).await? {
            return Ok(serde_json::from_value(result)?);
        }
        if self.journal.is_partial(&id).await? {
            return self.resume(client, id).await;
        }
        let mut running = self.running.lock().await;
        let receiver = if let Some(receiver) = running.get(&id) {
            receiver.clone()
        } else {
            let permit = self
                .slots
                .clone()
                .try_acquire_owned()
                .map_err(|_| anyhow::anyhow!("upload capacity exhausted"))?;
            let fresh = self
                .journal
                .admit(&id, &approval, &sha256, size, format_mode(mode), &lease_key)
                .await?;
            anyhow::ensure!(fresh, "payment_recovery_required: existing upload must be reconciled; no second payment was started");
            let (sender, receiver) = watch::channel(None);
            running.insert(id.clone(), receiver.clone());
            let payments = self.clone();
            let id = id.clone();
            tokio::spawn(async move {
                let _permit = permit;
                let result = tokio::time::timeout(
                    Duration::from_secs(3600),
                    payments.upload_inner(
                        &client,
                        file.path(),
                        UploadContext {
                            id: &id,
                            approval: &approval,
                            sha256: &sha256,
                            size,
                            mode,
                            verify,
                        },
                    ),
                )
                .await
                .map_err(|_| anyhow::anyhow!("payment_recovery_required: upload deadline exceeded"))
                .and_then(|result| result);
                let result = match result {
                    Ok(receipt) => match serde_json::to_value(&receipt) {
                        Ok(value) => payments
                            .journal
                            .finish(&id, "complete", Some(&value), None)
                            .await
                            .map(|_| receipt)
                            .map_err(|e| e.to_string()),
                        Err(error) => Err(error.to_string()),
                    },
                    Err(error) => {
                        let mut detail = error.to_string();
                        if detail.starts_with("approval_required:")
                            && payments
                                .journal
                                .has_storage_intent(&id)
                                .await
                                .unwrap_or(true)
                        {
                            detail = format!("payment_recovery_required: budget changed after storage payment intent; {detail}");
                        }
                        let state = if detail.starts_with("approval_required:") {
                            "approval_required"
                        } else if detail.starts_with("partial_upload:") {
                            "partial"
                        } else {
                            "payment_recovery_required"
                        };
                        let _ = payments
                            .journal
                            .finish(&id, state, None, Some(&detail))
                            .await;
                        Err(detail)
                    }
                };
                sender.send_replace(Some(result));
                payments.running.lock().await.remove(&id);
            });
            receiver
        };
        drop(running);
        await_upload(receiver).await.map_err(anyhow::Error::msg)
    }

    async fn upload_inner(
        &self,
        client: &Client,
        path: &Path,
        context: UploadContext<'_>,
    ) -> anyhow::Result<UploadReceipt> {
        let UploadContext {
            id,
            approval,
            sha256,
            size,
            mode,
            verify,
        } = context;
        let prepared = client
            .file_prepare_upload_with_mode(path, Visibility::Public, mode, None)
            .await?;
        let paid = self
            .signer
            .pay(&self.journal, id, approval, &prepared)
            .await?;
        let mut outcome = match paid.proofs {
            PaymentProofs::Wave(map) => client.finalize_upload_resumable(prepared, &map).await?,
            PaymentProofs::Merkle(winners) => {
                client
                    .finalize_upload_merkle_multi_resumable(prepared, winners)
                    .await?
            }
        };
        let mut previous_failed = usize::MAX;
        for _ in 0..3 {
            match outcome {
                FinalizeOutcome::Complete(result) => {
                    anyhow::ensure!(
                        result.chunks_failed == 0 && result.chunks_stored == result.total_chunks,
                        "payment_recovery_required: incomplete storage result"
                    );
                    let verified = if verify {
                        verify_content(
                            client,
                            &result.data_map,
                            path.parent().unwrap_or_else(|| Path::new("/tmp")),
                            sha256,
                            size,
                        )
                        .await?;
                        true
                    } else {
                        false
                    };
                    return Ok(UploadReceipt {
                        address: hex::encode(
                            result
                                .data_map_address
                                .ok_or_else(|| anyhow::anyhow!("public upload missing address"))?,
                        ),
                        byte_size: size,
                        chunks_stored: result.chunks_stored,
                        total_chunks: result.total_chunks,
                        chunks_failed: 0,
                        storage_cost_atto: paid.storage.to_string(),
                        estimated_gas_cost_wei: paid.gas.to_string(),
                        payment_mode_used: format_mode(result.payment_mode_used).into(),
                        verified,
                        upload_id: id.into(),
                    });
                }
                FinalizeOutcome::Partial { result, resume } => {
                    if result.chunks_failed >= previous_failed {
                        return self
                            .keep_resume(
                                result,
                                resume,
                                ResumeContext {
                                    id,
                                    sha256,
                                    size,
                                    verify,
                                    storage: paid.storage,
                                    gas: paid.gas,
                                },
                            )
                            .await;
                    }
                    previous_failed = result.chunks_failed;
                    outcome = client.finalize_resume(resume).await?;
                }
            }
        }
        match outcome {
            FinalizeOutcome::Partial { result, resume } => {
                self.keep_resume(
                    result,
                    resume,
                    ResumeContext {
                        id,
                        sha256,
                        size,
                        verify,
                        storage: paid.storage,
                        gas: paid.gas,
                    },
                )
                .await
            }
            FinalizeOutcome::Complete(result) => {
                anyhow::ensure!(
                    result.chunks_failed == 0 && result.chunks_stored == result.total_chunks,
                    "payment_recovery_required: incomplete storage result"
                );
                if verify {
                    verify_content(
                        client,
                        &result.data_map,
                        path.parent().unwrap_or_else(|| Path::new("/tmp")),
                        sha256,
                        size,
                    )
                    .await?;
                }
                Ok(UploadReceipt {
                    address: hex::encode(
                        result
                            .data_map_address
                            .ok_or_else(|| anyhow::anyhow!("public upload missing address"))?,
                    ),
                    byte_size: size,
                    chunks_stored: result.chunks_stored,
                    total_chunks: result.total_chunks,
                    chunks_failed: result.chunks_failed,
                    storage_cost_atto: paid.storage.to_string(),
                    estimated_gas_cost_wei: paid.gas.to_string(),
                    payment_mode_used: format_mode(result.payment_mode_used).into(),
                    verified: verify,
                    upload_id: id.into(),
                })
            }
        }
    }

    async fn keep_resume(
        &self,
        result: ant_core::data::FileUploadResult,
        handle: FinalizeResume,
        context: ResumeContext<'_>,
    ) -> anyhow::Result<UploadReceipt> {
        let ResumeContext {
            id,
            sha256,
            size,
            verify,
            storage,
            gas,
        } = context;
        let mut resumes = self.resumes.lock().await;
        resumes.retain(|_, session| session.expires > std::time::Instant::now());
        anyhow::ensure!(
            resumes.len() < 4,
            "payment_recovery_required: partial upload recovery capacity exhausted"
        );
        let receipt = UploadReceipt {
            address: hex::encode(
                result
                    .data_map_address
                    .ok_or_else(|| anyhow::anyhow!("public partial upload missing address"))?,
            ),
            byte_size: size,
            chunks_stored: result.chunks_stored,
            total_chunks: result.total_chunks,
            chunks_failed: result.chunks_failed,
            storage_cost_atto: storage.to_string(),
            estimated_gas_cost_wei: gas.to_string(),
            payment_mode_used: format_mode(result.payment_mode_used).into(),
            verified: false,
            upload_id: id.into(),
        };
        resumes.insert(
            id.into(),
            ResumeSession {
                handle,
                receipt,
                sha256: sha256.into(),
                verify,
                expires: std::time::Instant::now() + Duration::from_secs(3600),
            },
        );
        anyhow::bail!(
            "partial_upload: {id}; storage can resume against the original payment within one hour"
        )
    }

    async fn resume(
        self: &Arc<Self>,
        client: Arc<Client>,
        id: String,
    ) -> anyhow::Result<UploadReceipt> {
        if let Some(result) = self.journal.result(&id).await? {
            return Ok(serde_json::from_value(result)?);
        }
        let mut running = self.running.lock().await;
        let receiver = if let Some(receiver) = running.get(&id) {
            receiver.clone()
        } else {
            let permit = self.slots.clone().try_acquire_owned()?;
            let session = self.resumes.lock().await.remove(&id).ok_or_else(|| anyhow::anyhow!("payment_recovery_required: no retained recovery material; no new payment was started"))?;
            anyhow::ensure!(
                session.expires > std::time::Instant::now(),
                "payment_recovery_required: retained recovery material expired"
            );
            let (sender, receiver) = watch::channel(None);
            running.insert(id.clone(), receiver.clone());
            let payments = self.clone();
            tokio::spawn(async move {
                let _permit = permit;
                let result = tokio::time::timeout(Duration::from_secs(3600), async {
                    match client.finalize_resume(session.handle).await? {
                        FinalizeOutcome::Complete(result) => {
                            anyhow::ensure!(
                                result.chunks_failed == 0
                                    && result.chunks_stored == result.total_chunks,
                                "payment_recovery_required: incomplete resumed upload"
                            );
                            if session.verify {
                                verify_content(
                                    &client,
                                    &result.data_map,
                                    &std::env::temp_dir(),
                                    &session.sha256,
                                    session.receipt.byte_size,
                                )
                                .await?;
                            }
                            let receipt = UploadReceipt {
                                chunks_stored: result.chunks_stored,
                                chunks_failed: 0,
                                verified: session.verify,
                                ..session.receipt
                            };
                            payments
                                .journal
                                .finish(
                                    &id,
                                    "complete",
                                    Some(&serde_json::to_value(&receipt)?),
                                    None,
                                )
                                .await?;
                            Ok(receipt)
                        }
                        FinalizeOutcome::Partial { result, resume } => {
                            payments
                                .keep_resume(
                                    result,
                                    resume,
                                    ResumeContext {
                                        id: &id,
                                        sha256: &session.sha256,
                                        size: session.receipt.byte_size,
                                        verify: session.verify,
                                        storage: autvid_common::payments::amount(
                                            &session.receipt.storage_cost_atto,
                                        )?,
                                        gas: autvid_common::payments::amount(
                                            &session.receipt.estimated_gas_cost_wei,
                                        )?,
                                    },
                                )
                                .await
                        }
                    }
                })
                .await
                .map_err(|_| anyhow::anyhow!("payment_recovery_required: resume deadline exceeded"))
                .and_then(|r| r);
                if let Err(error) = &result {
                    let detail = error.to_string();
                    let state = if detail.starts_with("partial_upload:") {
                        "partial"
                    } else {
                        "payment_recovery_required"
                    };
                    let _ = payments
                        .journal
                        .finish(&id, state, None, Some(&detail))
                        .await;
                }
                sender.send_replace(Some(result.map_err(|e| e.to_string())));
                payments.running.lock().await.remove(&id);
            });
            receiver
        };
        drop(running);
        await_upload(receiver).await.map_err(anyhow::Error::msg)
    }
}

use alloy::providers::Provider;

// Upstream single-payment preparation retains chunk bodies in memory. Merkle
// preparation spills them to disk, so large originals must use auto or merkle.
fn validate_buffered_payment_mode(mode: PaymentMode, size: u64) -> anyhow::Result<()> {
    anyhow::ensure!(!matches!(mode, PaymentMode::Single) || size <= 64 * 1024 * 1024,
        "approval_required: single payment mode is limited to 64 MiB; choose auto or merkle for larger files");
    Ok(())
}

pub(crate) fn format_mode(mode: PaymentMode) -> &'static str {
    match mode {
        PaymentMode::Auto => "auto",
        PaymentMode::Single => "single",
        PaymentMode::Merkle => "merkle",
    }
}

async fn await_upload(mut receiver: UploadReceiver) -> UploadResult {
    loop {
        if let Some(result) = receiver.borrow().clone() {
            return result;
        }
        if receiver.changed().await.is_err() {
            return Err("payment_recovery_required: upload task stopped".into());
        }
    }
}

async fn verify_content(
    client: &Client,
    map: &ant_core::data::DataMap,
    directory: &Path,
    sha256: &str,
    size: u64,
) -> anyhow::Result<()> {
    let file = NamedTempFile::new_in(directory)?;
    let downloaded = client.file_download(map, file.path()).await?;
    let (verified_size, verified_sha256) =
        autvid_common::antd::sha256_file_async(file.path()).await?;
    anyhow::ensure!(
        size == downloaded && size == verified_size && sha256 == verified_sha256,
        "payment_recovery_required: verification mismatch"
    );
    Ok(())
}

pub(crate) async fn approve(
    State(state): State<AppState>,
    Json(approval): Json<PaymentApproval>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state
        .payments
        .journal
        .approve(&approval)
        .await
        .map_err(payment_error)?;
    Ok(Json(
        serde_json::json!({"quote_id": approval.quote_id, "approved": true}),
    ))
}

pub(crate) async fn resume(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<UploadReceipt>, ApiError> {
    state
        .payments
        .resume(state.client, id)
        .await
        .map(Json)
        .map_err(payment_error)
}

pub(crate) async fn cancel_unpaid(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    let lease = lease_header(&headers)?;
    let cancelled = state
        .payments
        .journal
        .cancel_unpaid(&id, &lease)
        .await
        .map_err(payment_error)?;
    Ok(Json(serde_json::json!({"cancelled_unpaid": cancelled})))
}

pub(crate) async fn status(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state
        .payments
        .journal
        .status(&id)
        .await
        .map(Json)
        .map_err(payment_error)
}

pub(crate) fn approval_header(headers: &HeaderMap) -> Result<String, ApiError> {
    headers
        .get("x-payment-approval")
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty() && v.len() <= 128)
        .map(str::to_owned)
        .ok_or_else(|| {
            ApiError::with_code(
                StatusCode::CONFLICT,
                "APPROVAL_REQUIRED",
                "A content-bound storage and gas approval is required",
            )
        })
}

pub(crate) fn payment_error(error: impl std::fmt::Display) -> ApiError {
    let detail = error.to_string();
    let code = if detail.starts_with("approval_required:") {
        "APPROVAL_REQUIRED"
    } else if detail.starts_with("partial_upload:") {
        "PARTIAL_UPLOAD"
    } else {
        "PAYMENT_RECOVERY_REQUIRED"
    };
    ApiError::with_code(StatusCode::CONFLICT, code, detail)
}

pub(crate) async fn activate(
    State(state): State<AppState>,
    Json(lease): Json<autvid_common::payments::PaymentLease>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state
        .payments
        .journal
        .activate(&lease)
        .await
        .map_err(payment_error)?;
    Ok(Json(serde_json::json!({"active":true})))
}
pub(crate) fn lease_header(headers: &HeaderMap) -> Result<String, ApiError> {
    headers
        .get("x-payment-lease")
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty() && v.len() <= 256)
        .map(str::to_owned)
        .ok_or_else(|| payment_error("payment_recovery_required: payment execution lease required"))
}

#[cfg(test)]
mod resource_tests {
    use super::*;
    #[test]
    fn large_originals_use_disk_backed_payment_preparation() {
        assert!(validate_buffered_payment_mode(PaymentMode::Single, 64 * 1024 * 1024).is_ok());
        assert!(validate_buffered_payment_mode(PaymentMode::Single, 64 * 1024 * 1024 + 1).is_err());
        assert!(
            validate_buffered_payment_mode(PaymentMode::Merkle, 1024 * 1024 * 1024 + 1).is_ok()
        );
    }
}
