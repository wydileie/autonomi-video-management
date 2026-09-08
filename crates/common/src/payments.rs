//! Versioned content and spending contract shared by the admin and gateway.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApprovedContent {
    pub sha256: String,
    pub byte_size: u64,
    pub payment_mode: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PaymentApproval {
    pub quote_id: String,
    pub content_digest: String,
    pub network: String,
    pub expires_at: i64,
    pub max_storage_atto: String,
    pub max_gas_wei: String,
    pub contents: Vec<ApprovedContent>,
}

pub fn amount(raw: &str) -> anyhow::Result<u128> {
    anyhow::ensure!(
        !raw.is_empty() && raw.bytes().all(|b| b.is_ascii_digit()),
        "invalid unsigned payment amount"
    );
    Ok(raw.parse()?)
}

pub fn content_digest(network: &str, contents: &[ApprovedContent]) -> anyhow::Result<String> {
    Ok(crate::antd::hex_lower(&Sha256::digest(serde_json::to_vec(
        &(network, contents),
    )?)))
}

impl PaymentApproval {
    pub fn validate(&self, now: i64, network: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.network == network && !network.is_empty() && network.len() <= 512,
            "approval network changed"
        );
        anyhow::ensure!(
            self.expires_at > now && self.expires_at <= now.saturating_add(86_400),
            "approval expired or exceeds one day"
        );
        anyhow::ensure!(
            !self.quote_id.is_empty() && self.quote_id.len() <= 128,
            "invalid quote ID"
        );
        anyhow::ensure!(
            !self.contents.is_empty() && self.contents.len() <= 65_540,
            "invalid approval content count"
        );
        amount(&self.max_storage_atto)?;
        amount(&self.max_gas_wei)?;
        for item in &self.contents {
            anyhow::ensure!(
                item.sha256.len() == 64
                    && item
                        .sha256
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
                "invalid content SHA-256"
            );
            anyhow::ensure!(
                item.byte_size >= 3
                    && matches!(item.payment_mode.as_str(), "auto" | "single" | "merkle"),
                "invalid approved content"
            );
        }
        anyhow::ensure!(
            self.content_digest == content_digest(network, &self.contents)?,
            "approval content changed"
        );
        Ok(())
    }

    pub fn permits(&self, sha256: &str, byte_size: u64, mode: &str) -> bool {
        self.contents.iter().any(|item| {
            item.sha256 == sha256 && item.byte_size == byte_size && item.payment_mode == mode
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_amounts_never_become_free_uploads() {
        for raw in [
            "",
            "-1",
            "+1",
            " 1",
            "1.0",
            "340282366920938463463374607431768211456",
        ] {
            assert!(amount(raw).is_err());
        }
        assert_eq!(amount("0").expect("zero"), 0);
    }
    #[test]
    fn approval_binds_bytes_mode_network_and_expiry() {
        let contents = vec![ApprovedContent {
            sha256: "a".repeat(64),
            byte_size: 3,
            payment_mode: "auto".into(),
        }];
        let mut approval = PaymentApproval {
            quote_id: "quote".into(),
            content_digest: content_digest("chain:1", &contents).expect("digest"),
            network: "chain:1".into(),
            expires_at: 100,
            max_storage_atto: "10".into(),
            max_gas_wei: "20".into(),
            contents,
        };
        assert!(approval.validate(99, "chain:1").is_ok());
        assert!(approval.validate(100, "chain:1").is_err());
        assert!(approval.validate(99, "chain:2").is_err());
        assert!(!approval.permits(&"a".repeat(64), 4, "auto"));
        assert!(!approval.permits(&"a".repeat(64), 3, "merkle"));
        approval.contents[0].sha256 = "b".repeat(64);
        assert!(approval.validate(99, "chain:1").is_err());
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContentQuote {
    pub cost: String,
    pub estimated_gas_cost_wei: String,
    pub file_size: u64,
    pub chunk_count: usize,
    pub payment_mode: String,
    pub address: String,
    pub content_sha256: String,
    pub network: String,
    pub confidence: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaymentLease {
    pub quote_id: String,
    pub job_id: String,
    pub owner: String,
    pub generation: i32,
    pub expires_at: i64,
}
impl PaymentLease {
    pub fn key(&self) -> String {
        format!("{}:{}:{}", self.job_id, self.owner, self.generation)
    }
}
