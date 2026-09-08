use std::path::Path;

use autvid_common::payments::{amount, PaymentApproval, PaymentLease};
use chrono::Utc;
use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
    Row, SqlitePool,
};

pub(super) struct Journal {
    _process_lock: std::fs::File,
    pub(super) pool: SqlitePool,
    pub(super) network: String,
}

impl Journal {
    pub(super) async fn open(path: &Path, network: String) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let mut lock_options = std::fs::OpenOptions::new();
        lock_options.create(true).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            lock_options.mode(0o600);
        }
        let process_lock = lock_options.open(path.with_extension("lock"))?;
        process_lock
            .try_lock()
            .map_err(|_| anyhow::anyhow!("payment journal is already owned by another gateway"))?;
        // Create the database privately before SQLite creates WAL/SHM companions.
        let _database = lock_options.open(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for file in [
                path.to_path_buf(),
                path.with_extension("lock"),
                std::path::PathBuf::from(format!("{}-wal", path.display())),
                std::path::PathBuf::from(format!("{}-shm", path.display())),
            ] {
                match tokio::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600)).await
                {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full)
            .busy_timeout(std::time::Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).await?;
        }
        let result = Self {
            pool,
            network,
            _process_lock: process_lock,
        };
        result.initialize().await?;
        Ok(result)
    }

    async fn initialize(&self) -> anyhow::Result<()> {
        sqlx::raw_sql(r#"
            CREATE TABLE IF NOT EXISTS payment_controls (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS payment_approvals (
                id TEXT PRIMARY KEY, definition TEXT NOT NULL, state TEXT NOT NULL DEFAULT 'open',
                reserved_storage TEXT NOT NULL DEFAULT '0', reserved_gas TEXT NOT NULL DEFAULT '0'
            );
            CREATE TABLE IF NOT EXISTS payment_leases (
                approval_id TEXT PRIMARY KEY REFERENCES payment_approvals(id), job_id TEXT NOT NULL, owner TEXT NOT NULL, generation INTEGER NOT NULL, expires_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS payment_uploads (
                id TEXT PRIMARY KEY, approval_id TEXT NOT NULL REFERENCES payment_approvals(id),
                sha256 TEXT NOT NULL, lease_key TEXT NOT NULL, state TEXT NOT NULL, result TEXT, detail TEXT,
                updated_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS payment_transactions (
                id TEXT PRIMARY KEY, hash TEXT, upload_id TEXT NOT NULL REFERENCES payment_uploads(id),
                approval_id TEXT NOT NULL REFERENCES payment_approvals(id), network TEXT NOT NULL,
                nonce INTEGER NOT NULL, raw_transaction TEXT,
                storage_reserved TEXT NOT NULL, gas_reserved TEXT NOT NULL,
                state TEXT NOT NULL DEFAULT 'reserved', receipt TEXT,
                UNIQUE(network, nonce), UNIQUE(network, hash)
            );
        "#).execute(&self.pool).await?;
        // Earlier development journals used global hash uniqueness. Identical
        // signed bytes can exist on separate local chain instances; identity is
        // the (network including genesis, hash) pair.
        let schema: String =
            sqlx::query_scalar("SELECT sql FROM sqlite_master WHERE name='payment_transactions'")
                .fetch_one(&self.pool)
                .await?;
        if schema.contains("hash TEXT UNIQUE") {
            let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
            sqlx::raw_sql(r#"CREATE TABLE payment_transactions_v2 (
                id TEXT PRIMARY KEY, hash TEXT, upload_id TEXT NOT NULL REFERENCES payment_uploads(id),
                approval_id TEXT NOT NULL REFERENCES payment_approvals(id), network TEXT NOT NULL,
                nonce INTEGER NOT NULL, raw_transaction TEXT,
                storage_reserved TEXT NOT NULL, gas_reserved TEXT NOT NULL,
                state TEXT NOT NULL DEFAULT 'reserved', receipt TEXT,
                UNIQUE(network, nonce), UNIQUE(network, hash)
            );"#).execute(&mut *tx).await?;
            sqlx::raw_sql("INSERT INTO payment_transactions_v2 SELECT * FROM payment_transactions; DROP TABLE payment_transactions; ALTER TABLE payment_transactions_v2 RENAME TO payment_transactions;").execute(&mut *tx).await?;
            tx.commit().await?;
        }
        // SDK prepare/resume objects cannot be restored from this journal. Preserve all
        // reservations and pause; a fresh request must never imply a second payment.
        sqlx::query("UPDATE payment_uploads SET state='payment_recovery_required', detail='Gateway restarted; retained SDK payment material must be reconciled', updated_at=$1 WHERE state IN ('preparing','paying','partial')")
            .bind(Utc::now().timestamp()).execute(&self.pool).await?;
        sqlx::query("UPDATE payment_approvals SET state='paused' WHERE id IN (SELECT approval_id FROM payment_uploads WHERE state='payment_recovery_required')")
            .execute(&self.pool).await?;
        Ok(())
    }

    pub(super) async fn approve(&self, approval: &PaymentApproval) -> anyhow::Result<()> {
        approval.validate(Utc::now().timestamp(), &self.network)?;
        let definition = serde_json::to_string(approval)?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let existing: Option<String> =
            sqlx::query_scalar("SELECT definition FROM payment_approvals WHERE id=$1")
                .bind(&approval.quote_id)
                .fetch_optional(&mut *tx)
                .await?;
        if let Some(existing) = existing {
            anyhow::ensure!(
                existing == definition,
                "quote ID already binds a different approval"
            );
        } else {
            sqlx::query("INSERT INTO payment_approvals(id, definition) VALUES($1,$2)")
                .bind(&approval.quote_id)
                .bind(definition)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub(super) async fn admit(
        &self,
        id: &str,
        approval_id: &str,
        sha256: &str,
        size: u64,
        mode: &str,
        lease_key: &str,
    ) -> anyhow::Result<bool> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let row = sqlx::query("SELECT definition, state FROM payment_approvals WHERE id=$1")
            .bind(approval_id)
            .fetch_one(&mut *tx)
            .await?;
        let approval: PaymentApproval = serde_json::from_str(row.try_get("definition")?)?;
        anyhow::ensure!(
            row.try_get::<String, _>("state")? == "open",
            "payment_recovery_required: approval is paused"
        );
        approval.validate(Utc::now().timestamp(), &self.network)?;
        anyhow::ensure!(
            approval.permits(sha256, size, mode),
            "approval_required: content or payment mode changed"
        );
        let valid: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM payment_leases WHERE approval_id=$1 AND job_id || ':' || owner || ':' || generation=$2 AND expires_at>$3)")
            .bind(approval_id).bind(lease_key).bind(Utc::now().timestamp()).fetch_one(&mut *tx).await?;
        anyhow::ensure!(
            valid,
            "payment_recovery_required: payment execution lease lost"
        );
        let inserted = sqlx::query("INSERT INTO payment_uploads(id, approval_id, sha256, state, updated_at, lease_key) VALUES($1,$2,$3,'preparing',$4,$5) ON CONFLICT(id) DO NOTHING")
            .bind(id).bind(approval_id).bind(sha256).bind(Utc::now().timestamp()).bind(lease_key).execute(&mut *tx).await?.rows_affected() == 1;
        tx.commit().await?;
        Ok(inserted)
    }

    /// Reserve worst-case costs before signing. The signed bytes are committed separately
    /// by record_signed; broadcasting is allowed only after that second commit.
    pub(super) async fn reserve(
        &self,
        transaction: &TransactionReservation<'_>,
    ) -> anyhow::Result<()> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let paused: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM payment_controls WHERE key='restore_reconciliation_required' AND value='1')")
            .fetch_one(&mut *tx).await?;
        anyhow::ensure!(!paused, "payment_recovery_required: restored payment journal requires operator reconciliation before signing");
        validate_execution(&mut tx, transaction.upload_id).await?;
        let row = sqlx::query("SELECT definition, state, reserved_storage, reserved_gas FROM payment_approvals WHERE id=$1")
            .bind(transaction.approval_id).fetch_one(&mut *tx).await?;
        let approval: PaymentApproval = serde_json::from_str(row.try_get("definition")?)?;
        approval.validate(Utc::now().timestamp(), &self.network)?;
        anyhow::ensure!(
            row.try_get::<String, _>("state")? == "open",
            "payment_recovery_required: approval paused"
        );
        let storage = checked_reservation(
            amount(row.try_get("reserved_storage")?)?,
            transaction.storage_upper,
            amount(&approval.max_storage_atto)?,
        )?;
        let gas = checked_reservation(
            amount(row.try_get("reserved_gas")?)?,
            transaction.gas_upper,
            amount(&approval.max_gas_wei)?,
        )?;
        // An uncertain nonce blocks further signing even when another approval is used.
        let uncertain: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM payment_transactions WHERE network=$1 AND state IN ('reserved','signed'))")
            .bind(&self.network).fetch_one(&mut *tx).await?;
        anyhow::ensure!(
            !uncertain,
            "payment_recovery_required: unresolved signed transaction"
        );
        sqlx::query("INSERT INTO payment_transactions(id, upload_id, approval_id, network, nonce, storage_reserved, gas_reserved) VALUES($1,$2,$3,$4,$5,$6,$7)")
            .bind(transaction.id).bind(transaction.upload_id).bind(transaction.approval_id).bind(&self.network)
            .bind(i64::try_from(transaction.nonce)?).bind(transaction.storage_upper.to_string()).bind(transaction.gas_upper.to_string())
            .execute(&mut *tx).await?;
        sqlx::query(
            "UPDATE payment_approvals SET reserved_storage=$1, reserved_gas=$2 WHERE id=$3",
        )
        .bind(storage.to_string())
        .bind(gas.to_string())
        .bind(transaction.approval_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE payment_uploads SET state='paying', updated_at=$1 WHERE id=$2")
            .bind(Utc::now().timestamp())
            .bind(transaction.upload_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    pub(super) async fn record_signed(
        &self,
        id: &str,
        hash: &str,
        raw: &str,
    ) -> anyhow::Result<()> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let upload: String =
            sqlx::query_scalar("SELECT upload_id FROM payment_transactions WHERE id=$1")
                .bind(id)
                .fetch_one(&mut *tx)
                .await?;
        validate_execution(&mut tx, &upload).await?;
        let updated = sqlx::query("UPDATE payment_transactions SET hash=$1, raw_transaction=$2, state='signed' WHERE id=$3 AND state='reserved'")
            .bind(hash).bind(raw).bind(id).execute(&mut *tx).await?.rows_affected();
        anyhow::ensure!(updated == 1, "transaction signing intent no longer owned");
        tx.commit().await?;
        Ok(())
    }

    pub(super) async fn settle(
        &self,
        hash: &str,
        storage: u128,
        gas: u128,
        receipt: &serde_json::Value,
    ) -> anyhow::Result<()> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let row = sqlx::query("SELECT approval_id, state, storage_reserved, gas_reserved FROM payment_transactions WHERE hash=$1 AND network=$2")
            .bind(hash).bind(&self.network).fetch_one(&mut *tx).await?;
        if row.try_get::<String, _>("state")? == "settled" {
            return Ok(());
        }
        let storage_upper = amount(row.try_get("storage_reserved")?)?;
        let gas_upper = amount(row.try_get("gas_reserved")?)?;
        anyhow::ensure!(
            storage <= storage_upper && gas <= gas_upper,
            "receipt exceeds signed reservation; manual reconciliation required"
        );
        let approval_id: String = row.try_get("approval_id")?;
        let current =
            sqlx::query("SELECT reserved_storage,reserved_gas FROM payment_approvals WHERE id=$1")
                .bind(&approval_id)
                .fetch_one(&mut *tx)
                .await?;
        let storage_used = amount(current.try_get("reserved_storage")?)?
            .checked_sub(storage_upper - storage)
            .ok_or_else(|| anyhow::anyhow!("invalid storage reservation"))?;
        let gas_used = amount(current.try_get("reserved_gas")?)?
            .checked_sub(gas_upper - gas)
            .ok_or_else(|| anyhow::anyhow!("invalid gas reservation"))?;
        sqlx::query("UPDATE payment_approvals SET reserved_storage=$1,reserved_gas=$2 WHERE id=$3")
            .bind(storage_used.to_string())
            .bind(gas_used.to_string())
            .bind(&approval_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE payment_transactions SET state='settled', receipt=$1 WHERE hash=$2 AND network=$3")
            .bind(receipt.to_string())
            .bind(hash)
            .bind(&self.network)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    pub(super) async fn finish(
        &self,
        id: &str,
        state: &str,
        result: Option<&serde_json::Value>,
        detail: Option<&str>,
    ) -> anyhow::Result<()> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query(
            "UPDATE payment_uploads SET state=$1, result=$2, detail=$3, updated_at=$4 WHERE id=$5",
        )
        .bind(state)
        .bind(result.map(serde_json::Value::to_string))
        .bind(detail)
        .bind(Utc::now().timestamp())
        .bind(id)
        .execute(&mut *tx)
        .await?;
        if state == "payment_recovery_required" {
            sqlx::query("UPDATE payment_approvals SET state='paused' WHERE id=(SELECT approval_id FROM payment_uploads WHERE id=$1)")
                .bind(id).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub(super) async fn has_storage_intent(&self, id: &str) -> anyhow::Result<bool> {
        Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM payment_transactions WHERE upload_id=$1 AND storage_reserved<>'0')").bind(id).fetch_one(&self.pool).await?)
    }

    pub(super) async fn signing_enabled(&self) -> anyhow::Result<bool> {
        Ok(!sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM payment_controls WHERE key='restore_reconciliation_required' AND value='1')")
            .fetch_one(&self.pool).await?)
    }

    pub(super) async fn is_partial(&self, id: &str) -> anyhow::Result<bool> {
        Ok(sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM payment_uploads WHERE id=$1 AND state='partial')",
        )
        .bind(id)
        .fetch_one(&self.pool)
        .await?)
    }

    pub(super) async fn result(&self, id: &str) -> anyhow::Result<Option<serde_json::Value>> {
        let result: Option<String> = sqlx::query_scalar(
            "SELECT result FROM payment_uploads WHERE id=$1 AND state='complete'",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .flatten();
        Ok(result.map(|v| serde_json::from_str(&v)).transpose()?)
    }

    pub(super) async fn status(&self, id: &str) -> anyhow::Result<serde_json::Value> {
        let row = sqlx::query(
            "SELECT state, result, detail, updated_at FROM payment_uploads WHERE id=$1",
        )
        .bind(id)
        .fetch_one(&self.pool)
        .await?;
        Ok(
            serde_json::json!({"upload_id":id, "state":row.try_get::<String,_>("state")?,
            "transaction_count":sqlx::query_scalar::<_,i64>("SELECT COUNT(*) FROM payment_transactions WHERE upload_id=$1").bind(id).fetch_one(&self.pool).await?,
            "result":row.try_get::<Option<serde_json::Value>,_>("result")?,
            "detail":row.try_get::<Option<String>,_>("detail")?, "updated_at":row.try_get::<i64,_>("updated_at")?}),
        )
    }

    pub(super) async fn activate(&self, lease: &PaymentLease) -> anyhow::Result<()> {
        let now = Utc::now().timestamp();
        anyhow::ensure!(
            lease.expires_at > now
                && lease.expires_at <= now + 3600
                && lease.generation > 0
                && !lease.job_id.is_empty()
                && !lease.owner.is_empty(),
            "invalid execution lease"
        );
        let updated = sqlx::query("INSERT INTO payment_leases(approval_id,job_id,owner,generation,expires_at) VALUES($1,$2,$3,$4,$5) ON CONFLICT(approval_id) DO UPDATE SET owner=excluded.owner,generation=excluded.generation,expires_at=excluded.expires_at WHERE payment_leases.job_id=excluded.job_id AND ((payment_leases.generation=excluded.generation AND payment_leases.owner=excluded.owner AND payment_leases.expires_at>$6) OR (payment_leases.generation<excluded.generation))")
            .bind(&lease.quote_id).bind(&lease.job_id).bind(&lease.owner).bind(lease.generation).bind(lease.expires_at).bind(now).execute(&self.pool).await?.rows_affected();
        anyhow::ensure!(
            updated == 1,
            "payment_recovery_required: stale payment execution lease"
        );
        Ok(())
    }

    pub(super) async fn check_storage(&self, approval_id: &str, upper: u128) -> anyhow::Result<()> {
        let row =
            sqlx::query("SELECT definition, reserved_storage FROM payment_approvals WHERE id=$1")
                .bind(approval_id)
                .fetch_one(&self.pool)
                .await?;
        let approval: PaymentApproval = serde_json::from_str(row.try_get("definition")?)?;
        approval.validate(Utc::now().timestamp(), &self.network)?;
        checked_reservation(
            amount(row.try_get("reserved_storage")?)?,
            upper,
            amount(&approval.max_storage_atto)?,
        )?;
        Ok(())
    }
}

async fn validate_execution(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    upload_id: &str,
) -> anyhow::Result<()> {
    let valid: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM payment_uploads u JOIN payment_leases l ON l.approval_id=u.approval_id WHERE u.id=$1 AND u.lease_key=l.job_id || ':' || l.owner || ':' || l.generation AND l.expires_at>$2)")
        .bind(upload_id).bind(Utc::now().timestamp()).fetch_one(&mut **tx).await?;
    anyhow::ensure!(
        valid,
        "payment_recovery_required: execution lease lost before signing"
    );
    Ok(())
}

pub(super) struct TransactionReservation<'a> {
    pub approval_id: &'a str,
    pub upload_id: &'a str,
    pub id: &'a str,
    pub nonce: u64,
    pub storage_upper: u128,
    pub gas_upper: u128,
}

fn checked_reservation(used: u128, additional: u128, cap: u128) -> anyhow::Result<u128> {
    let total = used
        .checked_add(additional)
        .ok_or_else(|| anyhow::anyhow!("approval_required: amount overflow"))?;
    anyhow::ensure!(
        total <= cap,
        "approval_required: aggregate spending cap exceeded"
    );
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use autvid_common::payments::{content_digest, ApprovedContent};
    async fn fixture() -> (tempfile::TempDir, Journal, PaymentApproval, PaymentLease) {
        let dir = tempfile::tempdir().expect("tempdir");
        let journal = Journal::open(&dir.path().join("payments.sqlite3"), "test:1".into())
            .await
            .expect("journal");
        let contents = vec![ApprovedContent {
            sha256: "a".repeat(64),
            byte_size: 3,
            payment_mode: "auto".into(),
        }];
        let approval = PaymentApproval {
            quote_id: "quote".into(),
            network: "test:1".into(),
            content_digest: content_digest("test:1", &contents).expect("digest"),
            expires_at: Utc::now().timestamp() + 3600,
            max_storage_atto: "10".into(),
            max_gas_wei: "20".into(),
            contents,
        };
        journal.approve(&approval).await.expect("approve");
        let lease = PaymentLease {
            quote_id: "quote".into(),
            job_id: "job".into(),
            owner: "one".into(),
            generation: 1,
            expires_at: Utc::now().timestamp() + 60,
        };
        journal.activate(&lease).await.expect("lease");
        (dir, journal, approval, lease)
    }
    fn reservation<'a>(
        id: &'a str,
        upload: &'a str,
        storage: u128,
        gas: u128,
    ) -> TransactionReservation<'a> {
        TransactionReservation {
            id,
            upload_id: upload,
            approval_id: "quote",
            nonce: 0,
            storage_upper: storage,
            gas_upper: gas,
        }
    }
    #[tokio::test]
    async fn restored_journal_blocks_new_signatures_and_hashes_are_scoped_to_network() {
        let (_dir, mut journal, _approval, lease) = fixture().await;
        journal
            .admit("upload", "quote", &"a".repeat(64), 3, "auto", &lease.key())
            .await
            .expect("admit");
        sqlx::query("INSERT INTO payment_controls VALUES('restore_reconciliation_required','1')")
            .execute(&journal.pool)
            .await
            .expect("restore guard");
        assert!(!journal.signing_enabled().await.expect("signing status"));
        assert!(journal
            .reserve(&reservation("intent", "upload", 1, 1))
            .await
            .is_err());
        sqlx::query("DELETE FROM payment_controls")
            .execute(&journal.pool)
            .await
            .expect("test reconciliation");
        journal
            .reserve(&reservation("intent", "upload", 1, 1))
            .await
            .expect("reserve");
        journal
            .record_signed("intent", "same-hash", "raw")
            .await
            .expect("first network");
        journal
            .settle("same-hash", 1, 1, &serde_json::json!({}))
            .await
            .expect("settle first");
        // The same signed bytes are possible after an explicit local-chain reset.
        journal.network = "test:2".into();
        sqlx::query("INSERT INTO payment_transactions(id,hash,upload_id,approval_id,network,nonce,storage_reserved,gas_reserved,state) VALUES('second','same-hash','upload','quote','test:2',0,'1','1','signed')")
            .execute(&journal.pool).await.expect("second chain hash namespace");
        journal
            .settle("same-hash", 1, 1, &serde_json::json!({"second":true}))
            .await
            .expect("settle second");
        let settled: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM payment_transactions WHERE state='settled'")
                .fetch_one(&journal.pool)
                .await
                .expect("states");
        assert_eq!(settled, 2);
    }

    #[tokio::test]
    async fn approval_identity_content_and_duplicate_uploads_are_enforced() {
        let (_dir, journal, mut approval, lease) = fixture().await;
        approval.max_storage_atto = "11".into();
        assert!(journal.approve(&approval).await.is_err());
        assert!(journal
            .admit("upload", "quote", &"b".repeat(64), 3, "auto", &lease.key())
            .await
            .is_err());
        assert!(journal
            .admit("upload", "quote", &"a".repeat(64), 4, "auto", &lease.key())
            .await
            .is_err());
        assert!(journal
            .admit(
                "upload",
                "quote",
                &"a".repeat(64),
                3,
                "merkle",
                &lease.key()
            )
            .await
            .is_err());
        assert!(journal
            .admit("upload", "quote", &"a".repeat(64), 3, "auto", &lease.key())
            .await
            .expect("admit"));
        assert!(!journal
            .admit("upload", "quote", &"a".repeat(64), 3, "auto", &lease.key())
            .await
            .expect("duplicate"));
    }
    #[tokio::test]
    async fn aggregate_caps_and_uncertain_transactions_block_signing() {
        let (_dir, journal, _approval, lease) = fixture().await;
        journal
            .admit("upload", "quote", &"a".repeat(64), 3, "auto", &lease.key())
            .await
            .expect("admit");
        assert!(journal
            .reserve(&reservation("too-much-storage", "upload", 11, 1))
            .await
            .is_err());
        assert!(journal
            .reserve(&reservation("too-much-gas", "upload", 1, 21))
            .await
            .is_err());
        journal
            .reserve(&reservation("intent", "upload", 10, 20))
            .await
            .expect("reserve");
        assert!(journal
            .reserve(&reservation("duplicate", "upload", 0, 0))
            .await
            .is_err());
        journal
            .record_signed("intent", "hash", "raw")
            .await
            .expect("journal signed bytes");
        journal
            .settle("hash", 7, 15, &serde_json::json!({"status":true}))
            .await
            .expect("receipt");
        journal
            .settle("hash", 7, 15, &serde_json::json!({"status":true}))
            .await
            .expect("receipt replay");
        let row = sqlx::query("SELECT reserved_storage,reserved_gas FROM payment_approvals")
            .fetch_one(&journal.pool)
            .await
            .expect("read");
        assert_eq!(row.get::<String, _>("reserved_storage"), "7");
        assert_eq!(row.get::<String, _>("reserved_gas"), "15");
        let mut next = reservation("second", "upload", 4, 1);
        next.nonce = 1;
        assert!(journal.reserve(&next).await.is_err());
        next.storage_upper = 3;
        next.gas_upper = 6;
        assert!(journal.reserve(&next).await.is_err());
        next.gas_upper = 5;
        journal.reserve(&next).await.expect("remaining exact cap");
    }
    #[tokio::test]
    async fn expired_or_reclaimed_workers_cannot_admit_reserve_or_sign() {
        let (_dir, journal, _approval, mut lease) = fixture().await;
        let old_key = lease.key();
        journal
            .admit("upload", "quote", &"a".repeat(64), 3, "auto", &old_key)
            .await
            .expect("admit");
        journal
            .reserve(&reservation("intent", "upload", 1, 1))
            .await
            .expect("reserve");
        sqlx::query("UPDATE payment_leases SET expires_at=0")
            .execute(&journal.pool)
            .await
            .expect("expire");
        assert!(journal.activate(&lease).await.is_err());
        assert!(journal
            .record_signed("intent", "hash", "raw")
            .await
            .is_err());
        lease.owner = "two".into();
        lease.generation = 2;
        journal.activate(&lease).await.expect("new owner");
        assert!(journal
            .admit("other", "quote", &"a".repeat(64), 3, "auto", &old_key)
            .await
            .is_err());
        assert!(journal
            .record_signed("intent", "hash", "raw")
            .await
            .is_err());
        assert!(journal
            .reserve(&reservation("stale", "upload", 1, 1))
            .await
            .is_err());
    }
    #[tokio::test]
    async fn restart_preserves_reservations_and_pauses_without_repayment() {
        let (dir, journal, approval, lease) = fixture().await;
        journal
            .admit("upload", "quote", &"a".repeat(64), 3, "auto", &lease.key())
            .await
            .expect("admit");
        journal
            .reserve(&reservation("intent", "upload", 5, 6))
            .await
            .expect("reserve");
        journal
            .record_signed("intent", "hash", "raw")
            .await
            .expect("signed");
        assert!(
            Journal::open(&dir.path().join("payments.sqlite3"), "test:1".into())
                .await
                .is_err()
        );
        journal.pool.close().await;
        drop(journal);
        let reopened = Journal::open(&dir.path().join("payments.sqlite3"), "test:1".into())
            .await
            .expect("reopen");
        assert_eq!(
            reopened.status("upload").await.expect("status")["state"],
            "payment_recovery_required"
        );
        reopened
            .approve(&approval)
            .await
            .expect("idempotent approval");
        assert!(reopened
            .admit("second", "quote", &"a".repeat(64), 3, "auto", &lease.key())
            .await
            .is_err());
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT raw_transaction FROM payment_transactions WHERE id='intent'"
            )
            .fetch_one(&reopened.pool)
            .await
            .expect("raw"),
            "raw"
        );
    }
    #[test]
    fn reservations_fail_closed_on_caps_and_overflow() {
        assert_eq!(checked_reservation(5, 5, 10).expect("within cap"), 10);
        assert!(checked_reservation(5, 6, 10).is_err());
        assert!(checked_reservation(u128::MAX, 1, u128::MAX).is_err());
    }
}
