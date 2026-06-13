use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// 每用户订阅状态，JSON 持久化（权威在服务端，客户端改不了）。仿 UsageStore。
/// 低量够用；上量换 Redis/DB。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SubRecord {
    pub plan: String,            // "free" | "plus"
    pub expires_at: Option<i64>, // unix 秒；None = 终身 / 无
}

impl SubRecord {
    pub fn free() -> Self {
        Self {
            plan: "free".into(),
            expires_at: None,
        }
    }

    pub fn is_premium(&self, now: i64) -> bool {
        self.plan != "free" && self.expires_at.map_or(true, |e| e > now)
    }
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub struct SubscriptionStore {
    path: PathBuf,
    map: Mutex<HashMap<String, SubRecord>>,
}

impl SubscriptionStore {
    pub fn load(path: &str) -> Self {
        let p = PathBuf::from(path);
        let map = std::fs::read_to_string(&p)
            .ok()
            .and_then(|s| serde_json::from_str::<HashMap<String, SubRecord>>(&s).ok())
            .unwrap_or_default();
        Self {
            path: p,
            map: Mutex::new(map),
        }
    }

    pub fn status(&self, user: &str) -> SubRecord {
        self.map
            .lock()
            .unwrap()
            .get(user)
            .cloned()
            .unwrap_or_else(SubRecord::free)
    }

    pub fn is_premium(&self, user: &str) -> bool {
        self.status(user).is_premium(now_secs())
    }

    /// 激活/续期会员。days=None → 终身；有效期未过则在其上叠加（续期）。返回新记录。
    pub fn activate(&self, user: &str, plan: &str, days: Option<u32>) -> SubRecord {
        let mut m = self.map.lock().unwrap();
        let now = now_secs();
        let base = m
            .get(user)
            .and_then(|r| r.expires_at)
            .filter(|e| *e > now)
            .unwrap_or(now);
        let expires_at = days.map(|d| base + (d as i64) * 86_400);
        let rec = SubRecord {
            plan: plan.to_string(),
            expires_at,
        };
        m.insert(user.to_string(), rec.clone());
        if let Ok(s) = serde_json::to_string(&*m) {
            let _ = std::fs::write(&self.path, s);
        }
        rec
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activate_persist_and_premium() {
        let path = std::env::temp_dir()
            .join(format!("xs_sub_test_{}.json", std::process::id()))
            .to_string_lossy()
            .to_string();
        let _ = std::fs::remove_file(&path);

        let s = SubscriptionStore::load(&path);
        assert!(!s.is_premium("u1")); // 默认免费
        assert_eq!(s.status("u1").plan, "free");

        s.activate("u1", "plus", Some(30)); // 月卡
        assert!(s.is_premium("u1"));
        s.activate("u2", "plus", None); // 终身
        assert!(s.is_premium("u2"));

        // 重新加载 → 持久化生效
        let r = SubscriptionStore::load(&path);
        assert!(r.is_premium("u1"));
        assert!(r.is_premium("u2"));
        assert!(!r.is_premium("u3"));

        let _ = std::fs::remove_file(&path);
    }
}
