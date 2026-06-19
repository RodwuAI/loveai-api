use std::collections::{HashMap, HashSet};
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
    /// 已处理过的支付回调事件 id（Stripe 会重发 webhook）。仅内存去重，
    /// 避免同一笔支付被重复续期叠加天数。进程重启会清空 → 极端情况最多重复
    /// 续期一次（activate 幂等续期，业务可接受）；上量再落 Redis/DB。
    processed: Mutex<HashSet<String>>,
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
            processed: Mutex::new(HashSet::new()),
        }
    }

    /// 幂等去重：首次见到该事件 key 返回 true（应处理）；重复返回 false（跳过）。
    pub fn mark_processed(&self, event_key: &str) -> bool {
        self.processed.lock().unwrap_or_else(|e| e.into_inner()).insert(event_key.to_string())
    }

    pub fn status(&self, user: &str) -> SubRecord {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(user)
            .cloned()
            .unwrap_or_else(SubRecord::free)
    }

    pub fn is_premium(&self, user: &str) -> bool {
        self.status(user).is_premium(now_secs())
    }

    /// 激活/续期会员。days=None → 终身；有效期未过则在其上叠加（续期）。返回新记录。
    pub fn activate(&self, user: &str, plan: &str, days: Option<u32>) -> SubRecord {
        let mut m = self.map.lock().unwrap_or_else(|e| e.into_inner());
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

    /// 设备 → 账号迁移：若设备的会员「更优」，则搬到账号上。
    /// 「更优」判定：设备是会员 且（账号非会员 或 设备到期更晚 / 终身优先）。
    /// 到期时间比较：None（终身）视为最优；都有则取更晚那个。
    /// 幂等：可重复执行（仅在设备更优时覆盖，已搬过再调不变）。
    pub fn merge_better(&self, from_device: &str, to_account: &str) {
        if from_device == to_account {
            return;
        }
        let mut m = self.map.lock().unwrap_or_else(|e| e.into_inner());
        let now = now_secs();
        let dev = match m.get(from_device).cloned() {
            Some(r) if r.is_premium(now) => r,
            _ => return, // 设备不是有效会员，无可搬
        };
        let acct = m.get(to_account).cloned().unwrap_or_else(SubRecord::free);
        let acct_premium = acct.is_premium(now);

        // 设备更优？账号非会员 → 是；都会员 → 比到期（None=终身最优，否则更晚的赢）。
        let device_better = if !acct_premium {
            true
        } else {
            match (dev.expires_at, acct.expires_at) {
                (None, _) => true,           // 设备终身
                (_, None) => false,          // 账号终身
                (Some(d), Some(a)) => d > a,  // 设备到期更晚
            }
        };

        if device_better {
            m.insert(to_account.to_string(), dev);
            if let Ok(s) = serde_json::to_string(&*m) {
                let _ = std::fs::write(&self.path, s);
            }
        }
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

    #[test]
    fn mark_processed_dedups() {
        let path = std::env::temp_dir()
            .join(format!("xs_sub_dedup_{}.json", std::process::id()))
            .to_string_lossy()
            .to_string();
        let _ = std::fs::remove_file(&path);

        let s = SubscriptionStore::load(&path);
        // 首次 → true（应处理）；重发 → false（跳过）。
        assert!(s.mark_processed("cs_test_1"));
        assert!(!s.mark_processed("cs_test_1"));
        assert!(s.mark_processed("cs_test_2"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn merge_better_moves_device_membership() {
        let path = std::env::temp_dir()
            .join(format!("xs_sub_merge_{}.json", std::process::id()))
            .to_string_lossy()
            .to_string();
        let _ = std::fs::remove_file(&path);

        let s = SubscriptionStore::load(&path);

        // 设备会员、账号免费 → 搬到账号
        s.activate("dev", "plus", Some(30));
        s.merge_better("dev", "acct");
        assert!(s.is_premium("acct"));

        // 幂等：重复搬不报错、结果不变
        s.merge_better("dev", "acct");
        assert!(s.is_premium("acct"));

        // 设备终身 vs 账号月卡 → 终身更优，覆盖
        s.activate("dev2", "plus", None); // 终身
        s.merge_better("dev2", "acct");
        assert!(s.status("acct").expires_at.is_none()); // 变终身

        // 设备月卡 vs 账号终身 → 不降级
        s.activate("dev3", "plus", Some(30));
        s.merge_better("dev3", "acct");
        assert!(s.status("acct").expires_at.is_none()); // 仍终身

        // 设备非会员 → 无可搬（不影响账号）
        s.merge_better("nobody", "acct");
        assert!(s.is_premium("acct"));

        // 设备非会员且账号也免费 → 账号仍免费
        s.merge_better("ghost", "fresh");
        assert!(!s.is_premium("fresh"));

        let _ = std::fs::remove_file(&path);
    }
}
