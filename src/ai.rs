use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use serde_json::{json, Value};
use tokio::time::{sleep, Duration};

use crate::prompt::{ChatTurn, ResolvedPrompt};
use crate::state::AppState;

const ARK_DEFAULT_BASE: &str = "https://ark.cn-beijing.volces.com/api/v3";
const DEFAULT_MODEL: &str = "doubao-pro-32k";

/// AI 调用结果。无任何 Key → NeedsApiKey（诚实「未配置」，绝不假数据）。
pub enum AiOutcome {
    Answer(String),
    Cached(String),
    NeedsApiKey,
    Error(String),
}

fn cache_key(r: &ResolvedPrompt, model: &str, history: &[ChatTurn]) -> u64 {
    let mut h = DefaultHasher::new();
    r.system.hash(&mut h);
    r.developer.hash(&mut h);
    r.user_context.hash(&mut h);
    r.user.hash(&mut h);
    model.hash(&mut h);
    for t in history {
        t.role.hash(&mut h);
        t.content.hash(&mut h);
    }
    h.finish()
}

/// 场景 → 优先使用的模型环境变量键（纯函数，便于测试）。
pub fn model_env_key(scenario: Option<&str>) -> &'static str {
    match scenario.unwrap_or("qa") {
        "classify" | "tag" => "AI_MODEL_LITE",
        "report" => "AI_MODEL_LONG",
        _ => "AI_MODEL_PRO",
    }
}

/// 解析场景对应的实际模型/接入点：场景专用键 → 通用 AI_MODEL → 默认 doubao-pro-32k。
pub fn resolve_model(scenario: Option<&str>) -> String {
    pick(model_env_key(scenario))
        .or_else(|| pick("AI_MODEL"))
        .unwrap_or_else(|| DEFAULT_MODEL.to_string())
}

/// 是否值得切到备用厂商：key 失效(401/403)、限流(429)、服务端错(5xx)。
/// 400 等请求错不切（切了也白切，纯浪费一次调用）。
pub fn is_failover_worthy(status: u16) -> bool {
    status == 401 || status == 403 || status == 429 || status >= 500
}

fn pick(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|s| !s.trim().is_empty())
}

struct Provider {
    base: String,
    key: String,
    model: String,
}

/// 构建「主→备」厂商链。
/// - 主：AI_API_KEY (+ AI_BASE_URL + 场景模型)
/// - 备：AI_API_KEY_BACKUP；最少只填这一项即「同厂商备 key」；
///   再填 AI_BASE_URL_BACKUP + AI_MODEL_BACKUP 即「跨厂商容灾」（如 Moonshot）。
fn build_providers(primary_model: &str) -> Vec<Provider> {
    let mut v = Vec::new();
    if let Some(key) = pick("AI_API_KEY") {
        v.push(Provider {
            base: pick("AI_BASE_URL").unwrap_or_else(|| ARK_DEFAULT_BASE.to_string()),
            key,
            model: primary_model.to_string(),
        });
    }
    if let Some(key) = pick("AI_API_KEY_BACKUP") {
        v.push(Provider {
            base: pick("AI_BASE_URL_BACKUP")
                .or_else(|| pick("AI_BASE_URL"))
                .unwrap_or_else(|| ARK_DEFAULT_BASE.to_string()),
            key,
            model: pick("AI_MODEL_BACKUP")
                .or_else(|| pick("AI_MODEL"))
                .unwrap_or_else(|| DEFAULT_MODEL.to_string()),
        });
    }
    v
}

enum ProviderResult {
    Ok(String),
    Failover(String), // 值得切备用
    Fatal(String),    // 切了也没用，直接返回错误
}

/// 调用单个厂商，自带 3 次指数退避重试（针对 429/5xx/网络抖动）。
async fn call_provider(client: &reqwest::Client, p: &Provider, messages: &Value) -> ProviderResult {
    let url = format!("{}/chat/completions", p.base);
    let body = json!({ "model": p.model, "messages": messages, "stream": false });
    let mut last = String::new();
    for attempt in 0..3u32 {
        match client.post(&url).bearer_auth(&p.key).json(&body).send().await {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    return match resp.json::<Value>().await {
                        Ok(v) => ProviderResult::Ok(
                            v["choices"][0]["message"]["content"]
                                .as_str()
                                .unwrap_or("")
                                .to_string(),
                        ),
                        Err(e) => ProviderResult::Failover(format!("解析失败: {e}")),
                    };
                }
                let code = status.as_u16();
                if !is_failover_worthy(code) {
                    let t = resp.text().await.unwrap_or_default();
                    return ProviderResult::Fatal(format!("HTTP {code}: {t}"));
                }
                last = format!("HTTP {code}");
                // key 失效重试无意义 → 立即切备用；429/5xx 才退避重试。
                if code == 401 || code == 403 {
                    return ProviderResult::Failover(last);
                }
            }
            Err(e) => last = format!("网络错误: {e}"),
        }
        if attempt < 2 {
            sleep(Duration::from_millis(400 * (1 << attempt))).await;
        }
    }
    ProviderResult::Failover(last)
}

/// 厂商无关的 AI 调用：缓存 → 限流 → 主厂商(重试) → 失效自动切备用厂商。
/// 对外永不暴露用了哪个模型/厂商。
pub async fn call_ai(
    state: &AppState,
    resolved: &ResolvedPrompt,
    model: &str,
    history: &[ChatTurn],
) -> AiOutcome {
    let providers = build_providers(model);
    if providers.is_empty() {
        return AiOutcome::NeedsApiKey;
    }

    // 缓存命中（同请求 + 同逻辑模型 + 同历史）→ 不调 LLM。
    let ck = cache_key(resolved, model, history);
    if let Some(ans) = state.cache.lock().unwrap_or_else(|e| e.into_inner()).get(&ck).cloned() {
        return AiOutcome::Cached(ans);
    }

    // 并发限流。
    let _permit = state.sem.acquire().await.expect("semaphore");

    // 组装消息：系统规则 + 人物上下文(一次) + 多轮历史 + 本次问题。
    let mut msgs = vec![
        json!({"role": "system", "content": resolved.system}),
        json!({"role": "system", "content": format!("{}\n\n{}", resolved.developer, resolved.user_context)}),
    ];
    for t in history {
        let role = if t.role == "assistant" {
            "assistant"
        } else {
            "user"
        };
        msgs.push(json!({"role": role, "content": t.content}));
    }
    msgs.push(json!({"role": "user", "content": resolved.user}));
    let messages = Value::Array(msgs);

    // 主 → 备 依次尝试；遇 Fatal 直接返回，遇 Failover 切下一个。
    let mut last = String::new();
    for p in &providers {
        match call_provider(&state.client, p, &messages).await {
            ProviderResult::Ok(content) => {
                {
                    let mut c = state.cache.lock().unwrap_or_else(|e| e.into_inner());
                    if c.len() > 500 {
                        c.clear();
                    }
                    c.insert(ck, content.clone());
                }
                return AiOutcome::Answer(content);
            }
            ProviderResult::Fatal(e) => return AiOutcome::Error(e),
            ProviderResult::Failover(e) => last = e,
        }
    }
    AiOutcome::Error(format!("主备厂商均失败：{last}"))
}

pub enum ImageOutcome {
    Url(String),
    NeedsConfig,
    Error(String),
}

/// 生图（§9）：OpenAI 兼容 /images/generations。需 AI_IMAGE_MODEL + key（独立或复用 AI_API_KEY）。
/// 未配置生图模型 → NeedsConfig（对外仍只显示 LOVEAI）。
pub async fn generate_image(state: &AppState, prompt: &str) -> ImageOutcome {
    let key = pick("AI_IMAGE_KEY").or_else(|| pick("AI_API_KEY"));
    let key = match key {
        Some(k) => k,
        None => return ImageOutcome::NeedsConfig,
    };
    let model = match pick("AI_IMAGE_MODEL") {
        Some(m) => m,
        None => return ImageOutcome::NeedsConfig,
    };
    let base = pick("AI_IMAGE_BASE_URL")
        .or_else(|| pick("AI_BASE_URL"))
        .unwrap_or_else(|| ARK_DEFAULT_BASE.to_string());
    let url = format!("{base}/images/generations");
    let body = json!({"model": model, "prompt": prompt, "n": 1, "size": "1024x1024"});
    match state.client.post(&url).bearer_auth(&key).json(&body).send().await {
        Ok(resp) => {
            let status = resp.status();
            if !status.is_success() {
                let t = resp.text().await.unwrap_or_default();
                return ImageOutcome::Error(format!("HTTP {status}: {t}"));
            }
            match resp.json::<Value>().await {
                Ok(v) => {
                    if let Some(u) = v["data"][0]["url"].as_str() {
                        ImageOutcome::Url(u.to_string())
                    } else if let Some(b) = v["data"][0]["b64_json"].as_str() {
                        ImageOutcome::Url(format!("data:image/png;base64,{b}"))
                    } else {
                        ImageOutcome::Error("无图返回".into())
                    }
                }
                Err(e) => ImageOutcome::Error(format!("解析失败: {e}")),
            }
        }
        Err(e) => ImageOutcome::Error(format!("请求失败: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt::{resolve_qa, QaRequest};

    fn rp() -> ResolvedPrompt {
        resolve_qa(&QaRequest {
            person_name: "母亲".into(),
            relation: None,
            memories: vec![],
            question: "在吗".into(),
            tone: None,
            perspective: None,
            scenario: None,
            history: vec![],
        })
    }

    #[test]
    fn cache_key_stable_and_distinct() {
        assert_eq!(cache_key(&rp(), "m1", &[]), cache_key(&rp(), "m1", &[]));
        assert_ne!(cache_key(&rp(), "m1", &[]), cache_key(&rp(), "m2", &[]));
        let h = vec![ChatTurn {
            role: "user".into(),
            content: "之前聊过的".into(),
        }];
        assert_ne!(cache_key(&rp(), "m1", &[]), cache_key(&rp(), "m1", &h));
    }

    #[test]
    fn model_routing_by_scenario() {
        assert_eq!(model_env_key(Some("qa")), "AI_MODEL_PRO");
        assert_eq!(model_env_key(Some("classify")), "AI_MODEL_LITE");
        assert_eq!(model_env_key(Some("report")), "AI_MODEL_LONG");
        assert_eq!(model_env_key(None), "AI_MODEL_PRO");
        assert_eq!(model_env_key(Some("unknown")), "AI_MODEL_PRO");
    }

    #[test]
    fn failover_classification() {
        for s in [401u16, 403, 429, 500, 503, 502] {
            assert!(is_failover_worthy(s), "{s} 应切备用");
        }
        for s in [200u16, 400, 404, 422] {
            assert!(!is_failover_worthy(s), "{s} 不应切备用");
        }
    }
}
