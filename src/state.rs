use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Semaphore;

use crate::subscription::SubscriptionStore;
use crate::usage::UsageStore;

/// 共享状态：复用的 HTTP 客户端（带超时 + 连接池）、并发信号量、回答缓存、每用户用量、订阅。
/// 所有 AI 调用收口于此 → 超时/重试/限流/缓存/配额/会员集中管理。
#[derive(Clone)]
pub struct AppState {
    pub client: reqwest::Client,
    pub sem: Arc<Semaphore>,
    pub cache: Arc<Mutex<HashMap<u64, String>>>,
    pub usage: Arc<UsageStore>,
    pub subscription: Arc<SubscriptionStore>,
}

impl AppState {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(45))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .expect("build http client");
        Self {
            client,
            // 最多 4 个并发 AI 调用，守住厂商 RPM/TPM。
            sem: Arc::new(Semaphore::new(4)),
            cache: Arc::new(Mutex::new(HashMap::new())),
            usage: Arc::new(UsageStore::load("usage.json")),
            subscription: Arc::new(SubscriptionStore::load("subscription.json")),
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}
