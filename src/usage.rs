use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

/// 每用户 AI 用量计数，JSON 文件持久化（权威计量在服务端，客户端改不了）。
/// 低量够用；上量换 Redis/DB + spawn_blocking。
pub struct UsageStore {
    path: PathBuf,
    map: Mutex<HashMap<String, u32>>,
}

impl UsageStore {
    pub fn load(path: &str) -> Self {
        let p = PathBuf::from(path);
        let map = std::fs::read_to_string(&p)
            .ok()
            .and_then(|s| serde_json::from_str::<HashMap<String, u32>>(&s).ok())
            .unwrap_or_default();
        Self {
            path: p,
            map: Mutex::new(map),
        }
    }

    pub fn get(&self, user: &str) -> u32 {
        *self.map.lock().unwrap_or_else(|e| e.into_inner()).get(user).unwrap_or(&0)
    }

    /// 计数 +1，持久化，返回新值。
    pub fn increment(&self, user: &str) -> u32 {
        let mut m = self.map.lock().unwrap_or_else(|e| e.into_inner());
        let c = m.entry(user.to_string()).or_insert(0);
        *c += 1;
        let v = *c;
        if let Ok(s) = serde_json::to_string(&*m) {
            let _ = std::fs::write(&self.path, s);
        }
        v
    }

    /// 设备 → 账号迁移：把账号的已用计数设为 max(账号, 设备)。
    /// 老设备用户注册/登录后，已消耗的免费次数不会因换键而「清零」。
    /// 幂等：可重复执行（取 max，重复调用结果不变）。返回迁移后账号计数。
    pub fn merge_max(&self, from_device: &str, to_account: &str) -> u32 {
        if from_device == to_account {
            return self.get(to_account);
        }
        let mut m = self.map.lock().unwrap_or_else(|e| e.into_inner());
        let dev = *m.get(from_device).unwrap_or(&0);
        let acct = m.entry(to_account.to_string()).or_insert(0);
        if dev > *acct {
            *acct = dev;
        }
        let v = *acct;
        if let Ok(s) = serde_json::to_string(&*m) {
            let _ = std::fs::write(&self.path, s);
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_and_persists() {
        let dir = std::env::temp_dir();
        let path = dir
            .join(format!("xs_usage_test_{}.json", std::process::id()))
            .to_string_lossy()
            .to_string();
        let _ = std::fs::remove_file(&path);

        let store = UsageStore::load(&path);
        assert_eq!(store.get("u1"), 0);
        assert_eq!(store.increment("u1"), 1);
        assert_eq!(store.increment("u1"), 2);
        assert_eq!(store.get("u1"), 2);
        assert_eq!(store.get("u2"), 0);

        // 重新加载 → 持久化生效
        let reloaded = UsageStore::load(&path);
        assert_eq!(reloaded.get("u1"), 2);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn merge_max_takes_higher_and_is_idempotent() {
        let path = std::env::temp_dir()
            .join(format!("xs_usage_merge_{}.json", std::process::id()))
            .to_string_lossy()
            .to_string();
        let _ = std::fs::remove_file(&path);

        let store = UsageStore::load(&path);
        // 设备已用 5，账号已用 2 → 取 max=5
        store.increment("dev"); store.increment("dev"); store.increment("dev");
        store.increment("dev"); store.increment("dev");
        store.increment("acct"); store.increment("acct");
        assert_eq!(store.merge_max("dev", "acct"), 5);
        assert_eq!(store.get("acct"), 5);
        // 幂等：重复迁移结果不变
        assert_eq!(store.merge_max("dev", "acct"), 5);
        // 账号本就更高时不下调
        store.increment("acct"); // acct=6
        assert_eq!(store.merge_max("dev", "acct"), 6);
        // 同键无操作
        assert_eq!(store.merge_max("acct", "acct"), 6);

        let _ = std::fs::remove_file(&path);
    }
}
