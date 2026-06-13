use hmac::{Hmac, Mac};
use serde::Serialize;
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// 套餐目录（权威定价在服务端，前端只读）。price_cents 用分避免浮点；days=None → 终身。
#[derive(Clone, Serialize)]
pub struct Plan {
    pub id: &'static str,
    pub name: &'static str,
    pub price_cents: u32,
    pub days: Option<u32>,
    pub tagline: &'static str,
}

pub const PLANS: &[Plan] = &[
    Plan {
        id: "plus_month",
        name: "Plus 月卡",
        price_cents: 1800,
        days: Some(30),
        tagline: "无限 LOVEAI + 语音对话 + 生图",
    },
    Plan {
        id: "plus_year",
        name: "Plus 年卡",
        price_cents: 9800,
        days: Some(365),
        tagline: "全部 Plus + 优先新功能（≈¥8/月）",
    },
    Plan {
        id: "plus_life",
        name: "Plus 终身",
        price_cents: 19800,
        days: None,
        tagline: "全部 Plus + 永久解锁",
    },
];

pub fn find_plan(id: &str) -> Option<&'static Plan> {
    PLANS.iter().find(|p| p.id == id)
}

/// 当前支付渠道（env `PAY_PROVIDER`：alipay|wechat|stripe）。
/// 渠道名 + 必要密钥都齐才算「就绪」；否则 None → 下单走 needs_config。
/// 密钥只读 env（用户填服务器 .env，我绝不接触其值）。
/// Stripe 单独门控（PAY_PROVIDER=stripe + STRIPE_SECRET_KEY）；
/// 国内渠道（alipay/wechat）仍按 PAY_MERCHANT_ID + PAY_KEY 判定。
pub fn provider() -> Option<String> {
    let p = std::env::var("PAY_PROVIDER").ok()?;
    let p = p.trim().to_string();
    if p.is_empty() {
        return None;
    }
    let has = |k: &str| {
        std::env::var(k)
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false)
    };
    if p == "stripe" {
        // Stripe 只需 secret key 即可下单（webhook secret 验签时再校验）。
        if has("STRIPE_SECRET_KEY") {
            return Some(p);
        }
        return None;
    }
    // TODO P2 国内渠道：支付宝/微信仍走 PAY_MERCHANT_ID + PAY_KEY。
    if has("PAY_MERCHANT_ID") && has("PAY_KEY") {
        Some(p)
    } else {
        None
    }
}

pub enum OrderOutcome {
    /// 未配置商户密钥 → 诚实降级（绝不假成功）。
    NeedsConfig,
    /// 已下单：返回渠道 + 订单号 + 支付参数（二维码/跳转链）。
    Ready {
        provider: String,
        order_id: String,
        pay_payload: serde_json::Value,
    },
    /// 下单过程出错（网络/HTTP 非 200/解析失败）→ 如实上报，绝不假成功。
    Error(String),
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default.to_string())
}

/// 创建订单。渠道无关入口：换渠道只改此处实现，路由/模型/UI 不动。
/// - PAY_PROVIDER=stripe 且 STRIPE_SECRET_KEY 非空 → 真调 Stripe Checkout Session。
/// - 其它渠道（alipay/wechat）或未配置 → NeedsConfig（绝不假装下单成功）。
pub async fn create_order(client: &reqwest::Client, plan: &Plan, user: &str) -> OrderOutcome {
    let provider = std::env::var("PAY_PROVIDER")
        .ok()
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    if provider == "stripe" {
        let secret = std::env::var("STRIPE_SECRET_KEY")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let Some(secret) = secret else {
            return OrderOutcome::NeedsConfig;
        };
        return create_stripe_session(client, &secret, plan, user).await;
    }

    // TODO P2 国内渠道：支付宝/微信下单（返回二维码/跳转链）。落地前一律 NeedsConfig。
    OrderOutcome::NeedsConfig
}

/// 调 Stripe Checkout Session（mode=payment，一次性付款；CNY，单位分）。
/// 表单 application/x-www-form-urlencoded；成功取 JSON 的 id+url 回前端跳转。
async fn create_stripe_session(
    client: &reqwest::Client,
    secret: &str,
    plan: &Plan,
    user: &str,
) -> OrderOutcome {
    let success_url = env_or("PAY_SUCCESS_URL", "https://www.loveaivip.com/?pay=ok");
    let cancel_url = env_or("PAY_CANCEL_URL", "https://www.loveaivip.com/?pay=cancel");
    let unit_amount = plan.price_cents.to_string();

    // Stripe 用方括号嵌套表单字段；reqwest .form() 会自动 urlencode。
    let form = [
        ("mode", "payment"),
        ("line_items[0][price_data][currency]", "cny"),
        ("line_items[0][price_data][unit_amount]", unit_amount.as_str()),
        ("line_items[0][price_data][product_data][name]", plan.name),
        ("line_items[0][quantity]", "1"),
        ("success_url", success_url.as_str()),
        ("cancel_url", cancel_url.as_str()),
        ("metadata[user]", user),
        ("metadata[plan]", plan.id),
        ("client_reference_id", user),
    ];

    let resp = client
        .post("https://api.stripe.com/v1/checkout/sessions")
        .bearer_auth(secret)
        .form(&form)
        .send()
        .await;

    match resp {
        Ok(r) => {
            let status = r.status();
            if !status.is_success() {
                let t = r.text().await.unwrap_or_default();
                return OrderOutcome::Error(format!("Stripe HTTP {status}: {t}"));
            }
            match r.json::<serde_json::Value>().await {
                Ok(v) => {
                    let id = v["id"].as_str().unwrap_or("");
                    let url = v["url"].as_str().unwrap_or("");
                    if id.is_empty() || url.is_empty() {
                        return OrderOutcome::Error("Stripe 返回缺少 id/url".to_string());
                    }
                    OrderOutcome::Ready {
                        provider: "stripe".to_string(),
                        order_id: id.to_string(),
                        pay_payload: serde_json::json!({ "url": url }),
                    }
                }
                Err(e) => OrderOutcome::Error(format!("Stripe 响应解析失败: {e}")),
            }
        }
        Err(e) => OrderOutcome::Error(format!("Stripe 请求失败: {e}")),
    }
}

/// 常量时间比较两个等长字节切片（避免时序侧信道泄露签名）。
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// 解析 Stripe-Signature 头：`t=<ts>,v1=<sig>[,v1=<sig>...]`。
/// 返回 (timestamp, [v1 签名们])。命中任一 v1 即视为合法。
fn parse_sig_header(header: &str) -> Option<(String, Vec<String>)> {
    let mut t = None;
    let mut v1 = Vec::new();
    for part in header.split(',') {
        let part = part.trim();
        if let Some((k, val)) = part.split_once('=') {
            match k.trim() {
                "t" => t = Some(val.trim().to_string()),
                "v1" => v1.push(val.trim().to_string()),
                _ => {}
            }
        }
    }
    let t = t?;
    if v1.is_empty() {
        return None;
    }
    Some((t, v1))
}

/// 校验 Stripe webhook 签名并解析出 (user, plan_id)。
/// - 验签：signed_payload = "{t}.{raw_body}"，expected = hex(HMAC_SHA256(secret, signed_payload))，
///   与头里任一 v1 常量时间比对命中即通过。
/// - 通过后解析 raw_body：仅 type=="checkout.session.completed" 时取
///   data.object.metadata.{user,plan} 返回 Some；否则 None。
/// 返回 None → 验签失败/非目标事件/缺字段 → 不激活会员。
pub fn verify_webhook(secret: &str, sig_header: &str, raw_body: &str) -> Option<(String, String)> {
    if secret.trim().is_empty() {
        return None;
    }
    let (t, sigs) = parse_sig_header(sig_header)?;

    let signed_payload = format!("{t}.{raw_body}");
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).ok()?;
    mac.update(signed_payload.as_bytes());
    let expected = mac.finalize().into_bytes();
    let expected_hex = hex::encode(expected);

    let matched = sigs
        .iter()
        .any(|s| constant_time_eq(s.as_bytes(), expected_hex.as_bytes()));
    if !matched {
        return None;
    }

    let v: serde_json::Value = serde_json::from_str(raw_body).ok()?;
    if v["type"].as_str() != Some("checkout.session.completed") {
        return None;
    }
    let obj = &v["data"]["object"];
    let user = obj["metadata"]["user"].as_str()?.to_string();
    let plan = obj["metadata"]["plan"].as_str()?.to_string();
    if user.is_empty() || plan.is_empty() {
        return None;
    }
    Some((user, plan))
}

/// 从已验签的 raw_body 取 Stripe 事件/会话 id，作为幂等去重 key。
/// 优先 data.object.id（一个 session 只激活一次）；退回顶层 event id。
pub fn event_dedup_key(raw_body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(raw_body).ok()?;
    if let Some(id) = v["data"]["object"]["id"].as_str() {
        if !id.is_empty() {
            return Some(id.to_string());
        }
    }
    v["id"].as_str().filter(|s| !s.is_empty()).map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 用已知 secret/body 造一个合法的 Stripe-Signature 头（复用生产验签逻辑的逆向）。
    fn sign(secret: &str, ts: &str, body: &str) -> String {
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(format!("{ts}.{body}").as_bytes());
        let sig = hex::encode(mac.finalize().into_bytes());
        format!("t={ts},v1={sig}")
    }

    fn completed_body(user: &str, plan: &str) -> String {
        serde_json::json!({
            "type": "checkout.session.completed",
            "id": "evt_test_123",
            "data": { "object": {
                "id": "cs_test_abc",
                "metadata": { "user": user, "plan": plan }
            }}
        })
        .to_string()
    }

    #[test]
    fn verify_ok_returns_user_and_plan() {
        let secret = "whsec_test_secret";
        let body = completed_body("u1", "plus_month");
        let header = sign(secret, "1718000000", &body);
        let got = verify_webhook(secret, &header, &body);
        assert_eq!(got, Some(("u1".to_string(), "plus_month".to_string())));
    }

    #[test]
    fn verify_rejects_tampered_body() {
        let secret = "whsec_test_secret";
        let body = completed_body("u1", "plus_month");
        let header = sign(secret, "1718000000", &body);
        // 篡改 body：签名对不上 → None。
        let tampered = completed_body("attacker", "plus_life");
        assert_eq!(verify_webhook(secret, &header, &tampered), None);
    }

    #[test]
    fn verify_rejects_wrong_secret() {
        let body = completed_body("u1", "plus_month");
        let header = sign("whsec_real", "1718000000", &body);
        assert_eq!(verify_webhook("whsec_wrong", &header, &body), None);
    }

    #[test]
    fn verify_ignores_non_completed_event() {
        let secret = "whsec_test_secret";
        // 合法签名，但事件类型不是 checkout.session.completed → None。
        let body = serde_json::json!({
            "type": "payment_intent.created",
            "data": { "object": { "id": "pi_1", "metadata": { "user": "u1", "plan": "plus_month" } } }
        })
        .to_string();
        let header = sign(secret, "1718000000", &body);
        assert_eq!(verify_webhook(secret, &header, &body), None);
    }

    #[test]
    fn verify_handles_multiple_v1_signatures() {
        let secret = "whsec_test_secret";
        let body = completed_body("u9", "plus_year");
        // 头里混入一个错误的 v1 + 一个正确的 v1：命中其一即通过。
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(format!("1718000000.{body}").as_bytes());
        let good = hex::encode(mac.finalize().into_bytes());
        let header = format!("t=1718000000,v1=deadbeef,v1={good}");
        assert_eq!(
            verify_webhook(secret, &header, &body),
            Some(("u9".to_string(), "plus_year".to_string()))
        );
    }

    #[test]
    fn verify_rejects_empty_secret_or_malformed_header() {
        let body = completed_body("u1", "plus_month");
        assert_eq!(verify_webhook("", "t=1,v1=abc", &body), None);
        // 缺 v1。
        assert_eq!(verify_webhook("s", "t=1", &body), None);
        // 缺 t。
        assert_eq!(verify_webhook("s", "v1=abc", &body), None);
    }

    #[test]
    fn dedup_key_prefers_object_id() {
        let body = completed_body("u1", "plus_month");
        assert_eq!(event_dedup_key(&body), Some("cs_test_abc".to_string()));
    }

    #[test]
    fn plan_catalog_amounts_in_cents() {
        // 套餐金额映射（分）：月 1800 / 年 9800 / 终身 19800；终身 days=None。
        assert_eq!(find_plan("plus_month").unwrap().price_cents, 1800);
        assert_eq!(find_plan("plus_month").unwrap().days, Some(30));
        assert_eq!(find_plan("plus_year").unwrap().price_cents, 9800);
        assert_eq!(find_plan("plus_year").unwrap().days, Some(365));
        assert_eq!(find_plan("plus_life").unwrap().price_cents, 19800);
        assert_eq!(find_plan("plus_life").unwrap().days, None);
        assert!(find_plan("nope").is_none());
    }
}
