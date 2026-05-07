use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use ant_core::data::Client as CoreClient;
use autvid_common::HttpMetrics;

use crate::routes::data::DataCostResponse;

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) client: Arc<CoreClient>,
    pub(crate) network: String,
    pub(crate) metrics: Arc<HttpMetrics>,
    pub(crate) cost_cache: Arc<CostCache>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct CostCacheKey {
    pub(crate) sha256: [u8; 32],
    pub(crate) byte_len: usize,
    pub(crate) payment_mode: String,
}

pub(crate) struct CostCache {
    ttl: Duration,
    max_entries: usize,
    inner: Mutex<CostCacheInner>,
}

#[derive(Default)]
struct CostCacheInner {
    entries: HashMap<CostCacheKey, CostCacheEntry>,
    order: VecDeque<CostCacheKey>,
}

struct CostCacheEntry {
    value: DataCostResponse,
    expires_at: Instant,
}

impl CostCache {
    pub(crate) fn new(ttl: Duration, max_entries: usize) -> Self {
        Self {
            ttl,
            max_entries,
            inner: Mutex::new(CostCacheInner::default()),
        }
    }

    pub(crate) fn get(&self, key: &CostCacheKey) -> Option<DataCostResponse> {
        if self.disabled() {
            return None;
        }
        let now = Instant::now();
        let mut inner = self.inner.lock().ok()?;
        match inner.entries.get(key) {
            Some(entry) if entry.expires_at > now => Some(entry.value.clone()),
            Some(_) => {
                inner.entries.remove(key);
                inner.order.retain(|candidate| candidate != key);
                None
            }
            None => None,
        }
    }

    pub(crate) fn insert(&self, key: CostCacheKey, value: DataCostResponse) {
        if self.disabled() {
            return;
        }
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        if inner.entries.contains_key(&key) {
            inner.order.retain(|candidate| candidate != &key);
        }
        inner.order.push_back(key.clone());
        inner.entries.insert(
            key,
            CostCacheEntry {
                value,
                expires_at: Instant::now() + self.ttl,
            },
        );
        while inner.entries.len() > self.max_entries {
            let Some(oldest) = inner.order.pop_front() else {
                break;
            };
            inner.entries.remove(&oldest);
        }
    }

    fn disabled(&self) -> bool {
        self.ttl.is_zero() || self.max_entries == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(cost: &str) -> DataCostResponse {
        DataCostResponse {
            cost: cost.to_string(),
            file_size: 3,
            chunk_count: 1,
            estimated_gas_cost_wei: "10".to_string(),
            payment_mode: "auto".to_string(),
        }
    }

    fn key(seed: u8) -> CostCacheKey {
        CostCacheKey {
            sha256: [seed; 32],
            byte_len: 3,
            payment_mode: "auto".to_string(),
        }
    }

    #[test]
    fn cost_cache_hits_and_evicts_oldest_entry() {
        let cache = CostCache::new(Duration::from_secs(60), 1);
        let first = key(1);
        let second = key(2);

        cache.insert(first.clone(), response("111"));
        assert_eq!(cache.get(&first).unwrap().cost, "111");

        cache.insert(second.clone(), response("222"));
        assert!(cache.get(&first).is_none());
        assert_eq!(cache.get(&second).unwrap().cost, "222");
    }
}
