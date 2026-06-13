use serde::Serialize;

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
    if has("PAY_MERCHANT_ID") && has("PAY_KEY") {
        Some(p)
    } else {
        None
    }
}

pub enum OrderOutcome {
    /// 未配置商户密钥 → 诚实降级（绝不假成功）。
    NeedsConfig,
    /// 已下单：返回渠道 + 订单号 + 支付参数（二维码/跳转链）。P2 实接。
    Ready {
        provider: String,
        order_id: String,
        pay_payload: serde_json::Value,
    },
}

/// 创建订单。渠道无关入口：换渠道只改此处实现，路由/模型/UI 不动。
/// P1：无就绪渠道 → NeedsConfig。P2：按 provider 调真实下单 API。
pub fn create_order(_plan: &Plan, _user: &str) -> OrderOutcome {
    match provider() {
        None => OrderOutcome::NeedsConfig,
        Some(_p) => {
            // P2：此处按 provider 调支付下单（支付宝/微信/Stripe），返回二维码或跳转链。
            // 真实实现落地前一律 NeedsConfig，绝不假装下单成功。
            OrderOutcome::NeedsConfig
        }
    }
}

/// 校验回调签名并解析出 (user, plan_id)。P2 按 provider 实接验签。
/// 返回 None → 验签失败/未实现 → 不激活会员。
pub fn verify_webhook(_payload: &serde_json::Value) -> Option<(String, String)> {
    None
}
