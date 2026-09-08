use std::{collections::HashMap, sync::Arc, time::Duration};

use alloy::{
    eips::{BlockNumberOrTag, Encodable2718},
    network::{EthereumWallet, TransactionBuilder},
    primitives::{Address, Bytes, B256, U256},
    providers::{DynProvider, Provider, ProviderBuilder},
    rpc::types::{TransactionReceipt, TransactionRequest},
    signers::local::PrivateKeySigner,
    sol,
    sol_types::SolCall,
};
use ant_core::data::{ExternalPaymentInfo, PreparedUpload};
use ant_protocol::evm::{
    contract::payment_vault::{
        handler::PaymentVaultHandler, interface::IPaymentVault, MAX_TRANSFERS_PER_TRANSACTION,
    },
    Network,
};
use tokio::sync::Mutex;

use super::journal::{Journal, TransactionReservation};

sol! {
    interface Token {
        function approve(address spender, uint256 amount) external returns (bool);
        function balanceOf(address owner) external view returns (uint256);
    }
}

pub(super) struct ControlledSigner {
    pub(super) provider: DynProvider,
    pub(super) key: Option<PrivateKeySigner>,
    pub(super) network: Network,
    chain_id: u64,
    genesis: B256,
    // Nonces, bounded allowances, and each batch's payment sequence share this lock.
    lock: Mutex<()>,
}

pub(super) enum PaymentProofs {
    Wave(HashMap<B256, B256>),
    Merkle(Vec<Option<[u8; 32]>>),
}

pub(super) struct PaidUpload {
    pub(super) proofs: PaymentProofs,
    pub(super) storage: u128,
    pub(super) gas: u128,
}

impl ControlledSigner {
    pub(super) async fn new(
        network: Network,
        key: Option<PrivateKeySigner>,
    ) -> anyhow::Result<Self> {
        let provider = ProviderBuilder::new()
            .connect_http(network.rpc_url().clone())
            .erased();
        let chain_id =
            tokio::time::timeout(Duration::from_secs(15), provider.get_chain_id()).await??;
        let expected = match network {
            Network::ArbitrumOne => Some(42161),
            Network::ArbitrumSepoliaTest => Some(421614),
            Network::Custom(_) => None,
        };
        anyhow::ensure!(
            chain_id != 0 && expected.is_none_or(|expected| chain_id == expected),
            "EVM RPC returned an unexpected chain ID"
        );
        let genesis = provider
            .get_block_by_number(BlockNumberOrTag::Earliest)
            .await?
            .ok_or_else(|| anyhow::anyhow!("RPC genesis block missing"))?
            .header
            .hash;
        Ok(Self {
            genesis,
            provider,
            key,
            network,
            chain_id,
            lock: Mutex::new(()),
        })
    }

    pub(super) fn identity(&self) -> String {
        format!(
            "{}:{}:{:#x}:{:#x}:{:#x}:{}",
            self.network.identifier(),
            self.chain_id,
            self.genesis,
            self.network.payment_token_address(),
            self.network.payment_vault_address(),
            self.key
                .as_ref()
                .map(|k| format!("{:#x}", k.address()))
                .unwrap_or_else(|| "read-only".into())
        )
    }

    pub(super) async fn pay(
        &self,
        journal: &Arc<Journal>,
        upload_id: &str,
        approval_id: &str,
        prepared: &PreparedUpload,
    ) -> anyhow::Result<PaidUpload> {
        let _lock = self.lock.lock().await;
        let mut paid = PaidUpload {
            proofs: PaymentProofs::Wave(HashMap::new()),
            storage: 0,
            gas: 0,
        };
        let storage_upper = storage_upper(prepared, &self.network)?;
        journal.check_storage(approval_id, storage_upper).await?;
        if storage_upper > 0 {
            // Overwrite any legacy unlimited allowance with the bounded maximum for
            // this prepared upload. Approvals themselves consume the user's gas cap.
            let call = Token::approveCall {
                spender: *self.network.payment_vault_address(),
                amount: U256::from(storage_upper),
            };
            let receipt = self
                .transact(
                    journal,
                    upload_id,
                    approval_id,
                    *self.network.payment_token_address(),
                    call.abi_encode().into(),
                    0,
                )
                .await?;
            let gas = receipt_gas(&receipt)?;
            journal
                .settle(
                    &format!("{:#x}", receipt.transaction_hash),
                    0,
                    gas,
                    &serde_json::to_value(&receipt)?,
                )
                .await?;
            anyhow::ensure!(
                receipt.status(),
                "payment_recovery_required: token approval reverted"
            );
            paid.gas = gas;
        }
        let handler =
            PaymentVaultHandler::new(*self.network.payment_vault_address(), self.provider.clone());
        match &prepared.payment_info {
            ExternalPaymentInfo::WaveBatch { payment_intent, .. } => {
                let mut map = HashMap::new();
                for payments in payment_intent
                    .payments
                    .chunks(MAX_TRANSFERS_PER_TRANSACTION)
                {
                    let amount = payments
                        .iter()
                        .try_fold(0u128, |total, (_, _, amount)| {
                            total.checked_add(u128::try_from(*amount).ok()?)
                        })
                        .ok_or_else(|| anyhow::anyhow!("payment amount overflow"))?;
                    let (calldata, to) =
                        handler.pay_for_quotes_calldata(payments.iter().copied())?;
                    let receipt = self
                        .transact(journal, upload_id, approval_id, to, calldata, amount)
                        .await?;
                    let gas = receipt_gas(&receipt)?;
                    journal
                        .settle(
                            &format!("{:#x}", receipt.transaction_hash),
                            if receipt.status() { amount } else { 0 },
                            gas,
                            &serde_json::to_value(&receipt)?,
                        )
                        .await?;
                    anyhow::ensure!(
                        receipt.status(),
                        "payment_recovery_required: storage payment reverted"
                    );
                    paid.storage = paid
                        .storage
                        .checked_add(amount)
                        .ok_or_else(|| anyhow::anyhow!("storage amount overflow"))?;
                    paid.gas = paid
                        .gas
                        .checked_add(gas)
                        .ok_or_else(|| anyhow::anyhow!("gas amount overflow"))?;
                    for (quote_hash, _, _) in payments {
                        map.insert(*quote_hash, receipt.transaction_hash);
                    }
                }
                paid.proofs = PaymentProofs::Wave(map);
            }
            ExternalPaymentInfo::Merkle {
                prepared_batches, ..
            } => {
                let mut winners = Vec::with_capacity(prepared_batches.len());
                for batch in prepared_batches {
                    // A receipt outside this bound requires reconciliation even
                    // if the chain reports success; never silently enlarge a cap.
                    let upper = u128::try_from(
                        self.network
                            .estimate_merkle_payment_cost(batch.depth, &batch.pool_commitments),
                    )?;
                    let (calldata, to) = handler.pay_for_merkle_tree_calldata(
                        batch.depth,
                        batch.pool_commitments.clone(),
                        batch.merkle_payment_timestamp,
                    )?;
                    let receipt = self
                        .transact(journal, upload_id, approval_id, to, calldata, upper)
                        .await?;
                    let gas = receipt_gas(&receipt)?;
                    if !receipt.status() {
                        journal
                            .settle(
                                &format!("{:#x}", receipt.transaction_hash),
                                0,
                                gas,
                                &serde_json::to_value(&receipt)?,
                            )
                            .await?;
                        anyhow::bail!("payment_recovery_required: merkle payment reverted");
                    }
                    let mut matching = receipt
                        .inner
                        .logs()
                        .iter()
                        .filter(|log| log.address() == to)
                        .filter_map(|log| log.log_decode::<IPaymentVault::MerklePaymentMade>().ok())
                        .filter(|log| {
                            log.inner.data.depth == batch.depth
                                && log.inner.data.merklePaymentTimestamp
                                    == batch.merkle_payment_timestamp
                        });
                    let event = matching.next().ok_or_else(|| {
                        anyhow::anyhow!(
                            "payment_recovery_required: missing matching merkle receipt"
                        )
                    })?;
                    anyhow::ensure!(
                        matching.next().is_none(),
                        "payment_recovery_required: ambiguous merkle receipt"
                    );
                    let winner = event.inner.data.winnerPoolHash.0;
                    anyhow::ensure!(
                        batch
                            .pool_commitments
                            .iter()
                            .any(|pool| pool.pool_hash == winner),
                        "payment_recovery_required: unexpected merkle winner"
                    );
                    let amount = u128::try_from(event.inner.data.totalAmount)?;
                    journal
                        .settle(
                            &format!("{:#x}", receipt.transaction_hash),
                            amount,
                            gas,
                            &serde_json::to_value(&receipt)?,
                        )
                        .await?;
                    paid.storage = paid
                        .storage
                        .checked_add(amount)
                        .ok_or_else(|| anyhow::anyhow!("storage amount overflow"))?;
                    paid.gas = paid
                        .gas
                        .checked_add(gas)
                        .ok_or_else(|| anyhow::anyhow!("gas amount overflow"))?;
                    winners.push(Some(winner));
                }
                paid.proofs = PaymentProofs::Merkle(winners);
            }
        }
        Ok(paid)
    }

    async fn transact(
        &self,
        journal: &Journal,
        upload_id: &str,
        approval_id: &str,
        to: Address,
        input: Bytes,
        storage_upper: u128,
    ) -> anyhow::Result<TransactionReceipt> {
        let genesis = self
            .provider
            .get_block_by_number(BlockNumberOrTag::Earliest)
            .await?
            .ok_or_else(|| anyhow::anyhow!("RPC genesis block missing"))?
            .header
            .hash;
        anyhow::ensure!(
            genesis == self.genesis,
            "approval_required: EVM chain changed or local devnet restarted"
        );
        let signer = self
            .key
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("wallet is not configured"))?;
        let fees = self.provider.estimate_eip1559_fees().await?;
        let nonce = self
            .provider
            .get_transaction_count(signer.address())
            .pending()
            .await?;
        let mut request = TransactionRequest::default()
            .with_from(signer.address())
            .with_to(to)
            .with_input(input)
            .with_value(U256::ZERO)
            .with_chain_id(self.chain_id)
            .with_nonce(nonce)
            .with_max_fee_per_gas(fees.max_fee_per_gas)
            .with_max_priority_fee_per_gas(fees.max_priority_fee_per_gas);
        let estimate = self.provider.estimate_gas(request.clone()).await?;
        let gas_limit = estimate
            .checked_mul(3)
            .and_then(|n| n.checked_div(2))
            .ok_or_else(|| anyhow::anyhow!("gas limit overflow"))?;
        anyhow::ensure!(gas_limit > 0, "RPC returned zero gas limit");
        request.set_gas_limit(gas_limit);
        let gas_upper = u128::from(gas_limit)
            .checked_mul(fees.max_fee_per_gas)
            .ok_or_else(|| anyhow::anyhow!("gas cost overflow"))?;
        let intent_id = uuid::Uuid::new_v4().to_string();
        journal
            .reserve(&TransactionReservation {
                approval_id,
                upload_id,
                id: &intent_id,
                nonce,
                storage_upper,
                gas_upper,
            })
            .await?;
        let envelope = request.build(&EthereumWallet::from(signer.clone())).await?;
        let hash = format!("{:#x}", envelope.tx_hash());
        let raw = envelope.encoded_2718();
        journal
            .record_signed(&intent_id, &hash, &hex::encode(&raw))
            .await?;
        // Every retry/recovery refers to these exact signed bytes and hash. No wallet
        // filler, fee replacement, or retry loop can allocate another nonce.
        let pending = self
            .provider
            .send_raw_transaction(&raw)
            .await
            .map_err(|e| {
                anyhow::anyhow!("payment_recovery_required: broadcast outcome uncertain: {e}")
            })?;
        let receipt = tokio::time::timeout(Duration::from_secs(120), pending.get_receipt())
            .await
            .map_err(|_| anyhow::anyhow!("payment_recovery_required: receipt timeout"))??;
        anyhow::ensure!(
            format!("{:#x}", receipt.transaction_hash) == hash
                && receipt.from == signer.address()
                && receipt.to == Some(to),
            "payment_recovery_required: transaction receipt identity mismatch"
        );
        Ok(receipt)
    }
}

pub(super) fn storage_upper(prepared: &PreparedUpload, network: &Network) -> anyhow::Result<u128> {
    match &prepared.payment_info {
        ExternalPaymentInfo::WaveBatch { payment_intent, .. } => {
            Ok(u128::try_from(payment_intent.total_amount)?)
        }
        ExternalPaymentInfo::Merkle {
            prepared_batches, ..
        } => prepared_batches.iter().try_fold(0u128, |sum, batch| {
            let cost = u128::try_from(
                network.estimate_merkle_payment_cost(batch.depth, &batch.pool_commitments),
            )?;
            sum.checked_add(cost)
                .ok_or_else(|| anyhow::anyhow!("storage cost overflow"))
        }),
    }
}

fn receipt_gas(receipt: &TransactionReceipt) -> anyhow::Result<u128> {
    u128::from(receipt.gas_used)
        .checked_mul(receipt.effective_gas_price)
        .ok_or_else(|| anyhow::anyhow!("receipt gas overflow"))
}
