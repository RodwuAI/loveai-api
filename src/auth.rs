use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};

/// 账号记录（JSON 持久化）。权威身份键 = id；email 仅作登录索引。
/// pass_hash 为 argon2 PHC 字符串（含算法/参数/盐），明文密码绝不落盘。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct User {
    pub id: String,        // acct_<32 hex> —— 业务全程以此为键（usage/subscription）
    pub email: String,     // 规范化后的小写邮箱
    pub pass_hash: String, // argon2 PHC 哈希串
    pub created_at: i64,   // unix 秒
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 账号存储：内存 HashMap<email 小写, User> + JSON 落盘。仿 UsageStore/SubscriptionStore。
/// 低量够用；上量换 DB + spawn_blocking（argon2 是 CPU 密集，量大时应丢到阻塞线程池）。
pub struct UserStore {
    path: PathBuf,
    /// key = 规范化邮箱（trim + 小写）
    map: Mutex<HashMap<String, User>>,
}

impl UserStore {
    pub fn load(path: &str) -> Self {
        let p = PathBuf::from(path);
        let map = std::fs::read_to_string(&p)
            .ok()
            .and_then(|s| serde_json::from_str::<HashMap<String, User>>(&s).ok())
            .unwrap_or_default();
        Self {
            path: p,
            map: Mutex::new(map),
        }
    }

    fn persist(&self, m: &HashMap<String, User>) {
        if let Ok(s) = serde_json::to_string(m) {
            let _ = std::fs::write(&self.path, s);
        }
    }

    /// 注册：规范化邮箱 + 基本校验（含 @、密码≥6）；邮箱已存在 → Err("email_taken")；
    /// 否则 argon2 哈希（OsRng 盐）后落盘，返回新 User。
    pub fn register(&self, email: &str, password: &str) -> Result<User, &'static str> {
        let email = email.trim().to_lowercase();
        // 基本校验：含 @ 且 @ 不在首尾；密码长度 ≥ 6。
        if email.len() < 3 || !email.contains('@') || email.starts_with('@') || email.ends_with('@') {
            return Err("invalid");
        }
        if password.len() < 6 {
            return Err("invalid");
        }

        let mut m = self.map.lock().unwrap_or_else(|e| e.into_inner());
        if m.contains_key(&email) {
            return Err("email_taken");
        }

        // argon2 哈希：每次独立随机盐，输出 PHC 串。
        let salt = SaltString::generate(&mut OsRng);
        let pass_hash = Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map_err(|_| "invalid")?
            .to_string();

        let user = User {
            id: format!("acct_{}", uuid::Uuid::new_v4().simple()),
            email: email.clone(),
            pass_hash,
            created_at: now_secs(),
        };
        m.insert(email, user.clone());
        self.persist(&m);
        Ok(user)
    }

    /// 登录：按邮箱查账号 + argon2 校验密码。成功返回 User，否则 None。
    pub fn login(&self, email: &str, password: &str) -> Option<User> {
        let email = email.trim().to_lowercase();
        let m = self.map.lock().unwrap_or_else(|e| e.into_inner());
        let user = m.get(&email)?;
        let parsed = PasswordHash::new(&user.pass_hash).ok()?;
        Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .ok()?;
        Some(user.clone())
    }

    /// 按账号 id 查 User（线性扫描；低量够用，上量加 id 索引）。
    pub fn get(&self, id: &str) -> Option<User> {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .find(|u| u.id == id)
            .cloned()
    }
}

// ───────────────────────── JWT ─────────────────────────

/// JWT 载荷：sub = 账号 id；exp = 过期 unix 秒。P1 单 token，30 天有效，不做 refresh。
#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    sub: String,
    exp: i64,
}

/// token 有效期：30 天。
const TOKEN_TTL_SECS: i64 = 30 * 86_400;

/// 仅供本地无配置启动用的 dev 默认签名串。上线务必通过 env 设置真实长随机串。
const DEV_FALLBACK_SECRET: &str = "loveai-dev-insecure-secret-change-me";

/// 读 JWT 签名密钥：env `AUTH_JWT_SECRET`；为空时回退 dev 默认并打印警告。
fn jwt_secret() -> Vec<u8> {
    match std::env::var("AUTH_JWT_SECRET") {
        Ok(s) if !s.trim().is_empty() => s.into_bytes(),
        _ => {
            eprintln!(
                "[auth] 警告：AUTH_JWT_SECRET 未配置，正在使用不安全的 dev 默认密钥。上线前务必在 .env 设置长随机串！"
            );
            DEV_FALLBACK_SECRET.as_bytes().to_vec()
        }
    }
}

/// 为账号签发 token（HS256，30 天）。密钥取自 env（见 jwt_secret）。
pub fn issue(user_id: &str) -> String {
    issue_with(user_id, &jwt_secret())
}

/// 校验 token：签名 + 过期检查通过 → Some(账号 id)；篡改/过期/无效 → None。
pub fn verify(token: &str) -> Option<String> {
    verify_with(token, &jwt_secret())
}

/// 显式密钥版（便于单测隔离，不依赖共享进程 env）。
fn issue_with(user_id: &str, secret: &[u8]) -> String {
    let claims = Claims {
        sub: user_id.to_string(),
        exp: now_secs() + TOKEN_TTL_SECS,
    };
    encode(&Header::default(), &claims, &EncodingKey::from_secret(secret)).unwrap_or_default()
}

fn verify_with(token: &str, secret: &[u8]) -> Option<String> {
    let data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret),
        &Validation::default(), // 默认 HS256 + 校验 exp
    )
    .ok()?;
    Some(data.claims.sub)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_store(tag: &str) -> (UserStore, String) {
        let path = std::env::temp_dir()
            .join(format!("xs_users_{}_{}.json", tag, std::process::id()))
            .to_string_lossy()
            .to_string();
        let _ = std::fs::remove_file(&path);
        (UserStore::load(&path), path)
    }

    #[test]
    fn register_success_and_duplicate() {
        let (st, path) = tmp_store("reg");
        let u = st.register("Alice@Example.com", "secret1").unwrap();
        assert_eq!(u.email, "alice@example.com"); // 规范化为小写
        assert!(u.id.starts_with("acct_"));
        assert!(!u.pass_hash.is_empty());
        assert_ne!(u.pass_hash, "secret1"); // 不存明文

        // 重复邮箱（大小写不敏感）→ Err
        assert_eq!(st.register("alice@example.com", "another6").err(), Some("email_taken"));
        // 非法输入
        assert_eq!(st.register("noat", "secret1").err(), Some("invalid"));
        assert_eq!(st.register("bob@x.com", "123").err(), Some("invalid")); // 密码过短

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn login_correct_and_wrong_password() {
        let (st, path) = tmp_store("login");
        st.register("bob@example.com", "hunter2").unwrap();
        assert!(st.login("bob@example.com", "hunter2").is_some());
        assert!(st.login("BOB@example.com", "hunter2").is_some()); // 大小写不敏感
        assert!(st.login("bob@example.com", "wrongpw").is_none()); // 错密码
        assert!(st.login("nobody@example.com", "hunter2").is_none()); // 不存在
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn get_by_id() {
        let (st, path) = tmp_store("get");
        let u = st.register("carol@example.com", "passw0rd").unwrap();
        assert_eq!(st.get(&u.id).map(|x| x.email), Some("carol@example.com".to_string()));
        assert!(st.get("acct_nope").is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn persists_across_reload() {
        let (st, path) = tmp_store("persist");
        let u = st.register("dave@example.com", "passw0rd").unwrap();
        let reloaded = UserStore::load(&path);
        assert!(reloaded.login("dave@example.com", "passw0rd").is_some());
        assert_eq!(reloaded.get(&u.id).map(|x| x.email), Some("dave@example.com".to_string()));
        let _ = std::fs::remove_file(&path);
    }

    // JWT 单测用显式密钥（issue_with/verify_with），避免并行测试共享进程 env 互相污染。
    #[test]
    fn jwt_roundtrip() {
        let secret = b"test-secret-roundtrip";
        let token = issue_with("acct_abc123", secret);
        assert!(!token.is_empty());
        assert_eq!(verify_with(&token, secret), Some("acct_abc123".to_string()));
    }

    #[test]
    fn jwt_tampered_fails() {
        let secret = b"test-secret-tamper";
        let token = issue_with("acct_xyz", secret);
        // 篡改最后一个字符 → 签名校验失败
        let mut chars: Vec<char> = token.chars().collect();
        let last = chars.len() - 1;
        chars[last] = if chars[last] == 'a' { 'b' } else { 'a' };
        let tampered: String = chars.into_iter().collect();
        assert_eq!(verify_with(&tampered, secret), None);
        // 用不同密钥校验同一 token → 失败（模拟伪造/换密钥）
        assert_eq!(verify_with(&token, b"different-secret"), None);
        assert_eq!(verify_with("not.a.jwt", secret), None);
    }

    #[test]
    fn jwt_expired_fails() {
        let secret = b"test-secret-expired";
        // 手工签一个已过期 token。
        // 注意：jsonwebtoken 默认有 60s leeway 容忍时钟偏差，过期需远超它。
        let claims = Claims {
            sub: "acct_old".into(),
            exp: now_secs() - 3600,
        };
        let token = encode(&Header::default(), &claims, &EncodingKey::from_secret(secret)).unwrap();
        assert_eq!(verify_with(&token, secret), None);
    }
}
