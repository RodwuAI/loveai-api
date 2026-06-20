mod ai;
mod asr;
mod auth;
mod inspire;
mod pay;
mod prompt;
mod quiz;
mod state;
mod subscription;
mod usage;

use axum::{
    extract::State,
    http::{HeaderMap, HeaderName, Method, StatusCode},
    routing::{get, post},
    Json, Router,
};
use serde_json::json;
use tower_http::cors::{AllowOrigin, CorsLayer};

use state::AppState;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use futures::{SinkExt, StreamExt};
use tokio::sync::mpsc;

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    let app = Router::new()
        .route("/health", get(health))
        .route("/auth/register", post(auth_register_handler))
        .route("/auth/login", post(auth_login_handler))
        .route("/auth/me", get(auth_me_handler))
        .route("/ai/qa", post(qa_handler))
        .route("/ai/image", post(image_handler))
        .route("/ai/voice", get(voice_handler))
        .route("/sub/status", get(sub_status_handler))
        .route("/sub/order", post(sub_order_handler))
        .route("/sub/webhook", post(sub_webhook_handler))
        .route("/sub/grant", post(sub_grant_handler))
        .route("/ai/inspire", post(inspire_handler))
        .route("/ai/ghostwrite", post(ghostwrite_handler))
        .route("/ai/quiz/generate", post(quiz_generate_handler))
        .layer(cors_layer())
        .with_state(AppState::new());

    let port = std::env::var("PORT").unwrap_or_else(|_| "8800".to_string());
    let addr = format!("0.0.0.0:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    println!("xinshang_backend listening on {addr}");
    axum::serve(listener, app).await.unwrap();
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({"status": "ok", "service": "xinshang_backend"}))
}

/// CORS：收敛到已知前端来源（env `CORS_ALLOWED_ORIGINS` 逗号分隔可覆盖；默认生产域 + 本地开发）。
/// 仅放行 GET/POST + 业务自定义头；其它来源浏览器侧被拒。
/// 注：支付 webhook 是支付方服务器直连（无浏览器、无预检），不受 CORS 影响。
fn cors_layer() -> CorsLayer {
    let raw = std::env::var("CORS_ALLOWED_ORIGINS").unwrap_or_else(|_| {
        "https://www.loveaivip.com,https://loveaivip.com,http://localhost:8080,http://127.0.0.1:8080"
            .to_string()
    });
    let origins: Vec<axum::http::HeaderValue> = raw
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse().ok())
        .collect();
    CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([
            HeaderName::from_static("content-type"),
            HeaderName::from_static("x-user-id"),
            HeaderName::from_static("x-admin-token"),
            // 账号体系：登录后请求带 Authorization: Bearer <jwt>
            HeaderName::from_static("authorization"),
        ])
}

/// 生图（§9）：{prompt} → 图片 URL。未配置生图模型 → needs_config。
async fn image_handler(
    State(st): State<AppState>,
    Json(req): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    let prompt = req["prompt"].as_str().unwrap_or("").trim().to_string();
    if prompt.is_empty() {
        return Json(json!({"status": "error", "message": "提示词为空"}));
    }
    match ai::generate_image(&st, &prompt).await {
        ai::ImageOutcome::Url(u) => Json(json!({"status": "ok", "image_url": u})),
        ai::ImageOutcome::NeedsConfig => Json(json!({
            "status": "needs_config",
            "message": "未配置生图模型；在 .env 填 AI_IMAGE_MODEL（+ 可选 AI_IMAGE_KEY/BASE）后即可生图。"
        })),
        ai::ImageOutcome::Error(e) => Json(json!({"status": "error", "message": e})),
    }
}

/// 语音识别（火山流式 ASR）：浏览器 WS 上传 16k PCM，回传 partial/final 字幕。
/// 未配置火山 key → 立即回 error 事件并关闭（前端可降级到设备语音）。
async fn voice_handler(ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(handle_voice_socket)
}

async fn handle_voice_socket(socket: WebSocket) {
    let (mut ws_tx, mut ws_rx) = socket.split();

    if !asr::is_configured() {
        let _ = ws_tx
            .send(Message::Text(
                "{\"type\":\"error\",\"text\":\"asr_not_configured\"}".to_string(),
            ))
            .await;
        return;
    }

    let (audio_tx, audio_rx) = mpsc::channel::<Vec<u8>>(64);
    let (event_tx, mut event_rx) = mpsc::channel::<asr::AsrClientEvent>(64);

    let pipe = tokio::spawn(async move {
        if let Err(e) = asr::pipe(0, audio_rx, event_tx).await {
            eprintln!("asr pipe error: {e}");
        }
    });

    let to_browser = tokio::spawn(async move {
        while let Some(ev) = event_rx.recv().await {
            let j = serde_json::to_string(&ev).unwrap_or_default();
            if ws_tx.send(Message::Text(j)).await.is_err() {
                break;
            }
        }
    });

    while let Some(Ok(msg)) = ws_rx.next().await {
        match msg {
            Message::Binary(b) => {
                if audio_tx.send(b).await.is_err() {
                    break;
                }
            }
            Message::Text(t) => {
                if t.contains("end") {
                    let _ = audio_tx.send(Vec::new()).await;
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
    drop(audio_tx);
    let _ = pipe.await;
    to_browser.abort();
}

/// 免费额度（终身），env 可调，默认 10 次 AI 问答。
fn free_limit() -> u32 {
    std::env::var("AI_FREE_LIMIT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10)
}

/// 关系问答：用户配额闸 → 组装提示词 → 调 AI（缓存/限流/重试）。
/// 免费额度用完返回 quota_exceeded（不调 LLM）；权威计量在服务端，客户端改不了。
/// 仅真实 LLM 调用(Answer)计 1 次；缓存命中/无 Key/出错不计费。
async fn qa_handler(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<prompt::QaRequest>,
) -> Json<serde_json::Value> {
    // 登录则用可信账号 id；未登录回退设备 id（过渡期）。
    let user = auth_user(&headers).unwrap_or_else(|| user_from(&headers));

    let premium = st.subscription.is_premium(&user);
    let limit = free_limit();
    let used = st.usage.get(&user);

    let resolved = prompt::resolve_qa(&req);
    // 场景 → 模型路由（qa/analysis/generate→Pro，classify/tag→Lite，report→Long）。
    let model = ai::resolve_model(req.scenario.as_deref());
    let audit = json!({
        "system_prompt_keys": resolved.audit.system_prompt_keys,
        "context_memory_count": resolved.audit.context_memory_count,
        "sent_chars": resolved.audit.sent_chars,
    });

    // 配额闸：仅对免费用户生效；会员（premium）跳过 → 无限 LOVEAI。
    if !premium && used >= limit {
        return Json(json!({
            "status": "quota_exceeded",
            "message": "免费次数已用完，升级解锁更多 LOVEAI 关心",
            "model": "LOVEAI",
            "usage": usage_json(premium, used, limit),
            "audit": audit
        }));
    }

    match ai::call_ai(&st, &resolved, &model, &req.history).await {
        ai::AiOutcome::Answer(a) => {
            let new_used = st.usage.increment(&user);
            Json(json!({
                "status": "ok", "answer": a, "cached": false, "model": "LOVEAI",
                "usage": usage_json(premium, new_used, limit),
                "audit": audit
            }))
        }
        ai::AiOutcome::Cached(a) => Json(json!({
            "status": "ok", "answer": a, "cached": true, "model": "LOVEAI",
            "usage": usage_json(premium, used, limit),
            "audit": audit
        })),
        ai::AiOutcome::NeedsApiKey => Json(json!({
            "status": "needs_api_key",
            "message": "后端未配置 AI_API_KEY；填入后即返回真实回答（绝不使用假数据）。",
            "model": "LOVEAI",
            "usage": usage_json(premium, used, limit),
            "audit": audit
        })),
        ai::AiOutcome::Error(e) => Json(json!({
            "status": "error", "message": e, "model": "LOVEAI",
            "usage": usage_json(premium, used, limit),
            "audit": audit
        })),
    }
}

/// 从 X-User-Id 头解析用户标识；缺省 anonymous。权威计量/会员都以此为键。
fn user_from(headers: &HeaderMap) -> String {
    headers
        .get("x-user-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "anonymous".to_string())
}

/// 解析可信账号身份：Authorization: Bearer <jwt> → 校验 → Some(账号 id)；
/// 无 token / 非法 / 过期 → None（调用方回退 user_from 设备 id）。
/// 这是根治「X-User-Id 可伪造」的核心：账号 id 由签名 token 背书，客户端改不了。
fn auth_user(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get("authorization").and_then(|v| v.to_str().ok())?;
    let token = raw.strip_prefix("Bearer ").or_else(|| raw.strip_prefix("bearer "))?;
    auth::verify(token.trim())
}

/// 设备 → 账号迁移（注册/登录成功后调）：把请求头里的过渡期设备 id（X-User-Id）
/// 的用量与会员搬到账号 id 上。幂等，可重复执行。device 缺省/等于账号则跳过。
fn migrate_device_to_account(st: &AppState, headers: &HeaderMap, account_id: &str) {
    let device = user_from(headers);
    if device == account_id || device == "anonymous" {
        return;
    }
    // usage：账号计数取 max(账号, 设备)，免费次数不丢。
    st.usage.merge_max(&device, account_id);
    // subscription：设备会员更优则搬到账号（到期更晚/终身优先）。
    st.subscription.merge_better(&device, account_id);
}

/// POST /auth/register {email,password}：注册并签发 token。
/// 200 {token,user} | 409 {error:"email_taken"} | 400 {error:"invalid"}
async fn auth_register_handler(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<serde_json::Value>,
) -> (StatusCode, Json<serde_json::Value>) {
    let email = req["email"].as_str().unwrap_or("");
    let password = req["password"].as_str().unwrap_or("");
    match st.users.register(email, password) {
        Ok(user) => {
            migrate_device_to_account(&st, &headers, &user.id);
            let token = auth::issue(&user.id);
            (
                StatusCode::OK,
                Json(json!({"token": token, "user": {"id": user.id, "email": user.email}})),
            )
        }
        Err("email_taken") => (
            StatusCode::CONFLICT,
            Json(json!({"error": "email_taken"})),
        ),
        Err(_) => (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid"}))),
    }
}

/// POST /auth/login {email,password}：校验并签发 token。
/// 200 {token,user} | 401 {error:"bad_credentials"}
async fn auth_login_handler(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<serde_json::Value>,
) -> (StatusCode, Json<serde_json::Value>) {
    let email = req["email"].as_str().unwrap_or("");
    let password = req["password"].as_str().unwrap_or("");
    match st.users.login(email, password) {
        Some(user) => {
            migrate_device_to_account(&st, &headers, &user.id);
            let token = auth::issue(&user.id);
            (
                StatusCode::OK,
                Json(json!({"token": token, "user": {"id": user.id, "email": user.email}})),
            )
        }
        None => (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "bad_credentials"})),
        ),
    }
}

/// GET /auth/me（Authorization: Bearer <token>）：返回当前账号。
/// 200 {id,email} | 401（无/无效 token 或账号不存在）
async fn auth_me_handler(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> (StatusCode, Json<serde_json::Value>) {
    let Some(uid) = auth_user(&headers) else {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "unauthorized"})));
    };
    match st.users.get(&uid) {
        Some(user) => (
            StatusCode::OK,
            Json(json!({"id": user.id, "email": user.email})),
        ),
        None => (StatusCode::UNAUTHORIZED, Json(json!({"error": "unauthorized"}))),
    }
}

/// 统一用量 JSON。会员 remaining=-1（前端识别为「无限」）。
fn usage_json(premium: bool, used: u32, limit: u32) -> serde_json::Value {
    if premium {
        json!({"premium": true, "used": used, "limit": limit, "remaining": -1})
    } else {
        json!({"premium": false, "used": used, "limit": limit, "remaining": limit.saturating_sub(used)})
    }
}

/// 套餐目录 JSON（前端订阅页渲染用）。
fn plans_json() -> serde_json::Value {
    json!(pay::PLANS)
}

/// GET /sub/status：当前会员态 + 套餐目录 + 支付是否就绪。
async fn sub_status_handler(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Json<serde_json::Value> {
    let user = auth_user(&headers).unwrap_or_else(|| user_from(&headers));
    let rec = st.subscription.status(&user);
    let premium = st.subscription.is_premium(&user);
    Json(json!({
        "status": "ok",
        "plan": rec.plan,
        "premium": premium,
        "expires_at": rec.expires_at,
        "pay_ready": pay::provider().is_some(),
        "plans": plans_json(),
    }))
}

/// POST /sub/order {plan}：创建订单。
/// - PAY_PROVIDER=stripe 且配 STRIPE_SECRET_KEY → 真调 Stripe，返回跳转 url。
/// - 未配置/国内渠道未实现 → needs_config（诚实降级，绝不假成功）。
/// - 下单出错（网络/HTTP）→ error（如实上报）。
async fn sub_order_handler(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    let user = auth_user(&headers).unwrap_or_else(|| user_from(&headers));
    let plan_id = req["plan"].as_str().unwrap_or("").trim();
    let Some(plan) = pay::find_plan(plan_id) else {
        return Json(json!({"status": "error", "message": "未知套餐"}));
    };
    match pay::create_order(&st.client, plan, &user).await {
        pay::OrderOutcome::NeedsConfig => Json(json!({
            "status": "needs_config",
            "message": "支付即将开通：商户号配置好后即可购买。",
            "plan": plan.id,
            "price_cents": plan.price_cents,
        })),
        pay::OrderOutcome::Ready { provider, order_id, pay_payload } => Json(json!({
            "status": "ok",
            "provider": provider,
            "order_id": order_id,
            "pay": pay_payload,
        })),
        pay::OrderOutcome::Error(e) => Json(json!({
            "status": "error",
            "message": e,
        })),
    }
}

/// POST /sub/webhook：Stripe 异步回调 → 原始 body + Stripe-Signature 验签 → 激活会员。
/// 幂等：Stripe 会重发同一事件，按 session/event id 去重避免重复续期叠加天数。
/// 验签失败/非目标事件/缺密钥 → 400（绝不假激活）。注意 body 提取器必须放最后。
async fn sub_webhook_handler(
    State(st): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> (StatusCode, Json<serde_json::Value>) {
    let secret = std::env::var("STRIPE_WEBHOOK_SECRET").unwrap_or_default();
    let sig = headers
        .get("stripe-signature")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    match pay::verify_webhook(&secret, sig, &body) {
        Some((user, plan_id)) => {
            let Some(plan) = pay::find_plan(&plan_id) else {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"status": "error", "message": "未知套餐"})),
                );
            };
            // 幂等去重：同一 session/event 重发只续期一次。
            let key = pay::event_dedup_key(&body).unwrap_or_else(|| format!("{user}:{plan_id}"));
            if st.subscription.mark_processed(&key) {
                st.subscription.activate(&user, "plus", plan.days);
            }
            // 已处理过也回 200，让 Stripe 停止重发。
            (StatusCode::OK, Json(json!({"status": "ok"})))
        }
        None => (
            StatusCode::BAD_REQUEST,
            Json(json!({"status": "error", "message": "验签失败或未配置"})),
        ),
    }
}

/// POST /sub/grant {plan, user?, days?}：管理员手动开通会员（赠送/客服补单/测试）。
/// 需 X-Admin-Token 头匹配 env ADMIN_TOKEN；未配置或不匹配 → 403。绝不公开开放。
async fn sub_grant_handler(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let admin = std::env::var("ADMIN_TOKEN").unwrap_or_default();
    let provided = headers
        .get("x-admin-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if admin.trim().is_empty() || provided != admin {
        return Err(StatusCode::FORBIDDEN);
    }
    let target = req["user"]
        .as_str()
        .map(|s| s.to_string())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| user_from(&headers));
    let plan_id = req["plan"].as_str().unwrap_or("plus_month").trim();
    // days 优先取请求覆盖，否则按套餐目录；都没有则按 plus_month。
    let days = req["days"]
        .as_u64()
        .map(|d| Some(d as u32))
        .unwrap_or_else(|| pay::find_plan(plan_id).map(|p| p.days).unwrap_or(Some(30)));
    let rec = st.subscription.activate(&target, "plus", days);
    Ok(Json(json!({
        "status": "ok",
        "user": target,
        "plan": rec.plan,
        "expires_at": rec.expires_at,
    })))
}

async fn inspire_handler(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<inspire::InspireRequest>,
) -> Json<serde_json::Value> {
    let user = user_from(&headers);
    let premium = st.subscription.is_premium(&user);
    let limit = free_limit();
    let used = st.usage.get(&user);
    if !premium && used >= limit {
        return Json(json!({"status": "quota_exceeded", "message": "免费次数已用完，升级解锁更多 LOVEAI 关心"}));
    }
    let resolved = inspire::resolve_inspire(&req);
    let model = ai::resolve_model(Some("generate"));
    match ai::call_ai(&st, &resolved, &model, &[]).await {
        ai::AiOutcome::Answer(a) => {
            let clean = a.trim().trim_start_matches("```json").trim_start_matches("```").trim_end_matches("```").trim();
            let parsed = serde_json::from_str::<serde_json::Value>(clean)
                .unwrap_or_else(|_| json!({"suggestions": []}));
            st.usage.increment(&user);
            Json(json!({"status": "ok", "suggestions": parsed["suggestions"]}))
        }
        ai::AiOutcome::Cached(a) => {
            let clean = a.trim().trim_start_matches("```json").trim_start_matches("```").trim_end_matches("```").trim();
            let parsed = serde_json::from_str::<serde_json::Value>(clean)
                .unwrap_or_else(|_| json!({"suggestions": []}));
            Json(json!({"status": "ok", "suggestions": parsed["suggestions"]}))
        }
        ai::AiOutcome::NeedsApiKey => Json(json!({"status": "needs_api_key"})),
        ai::AiOutcome::Error(e) => Json(json!({"status": "error", "message": e})),
    }
}

async fn ghostwrite_handler(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<inspire::GhostwriteRequest>,
) -> Json<serde_json::Value> {
    let user = user_from(&headers);
    let premium = st.subscription.is_premium(&user);
    let limit = free_limit();
    let used = st.usage.get(&user);
    if !premium && used >= limit {
        return Json(json!({"status": "quota_exceeded", "message": "免费次数已用完"}));
    }
    let resolved = inspire::resolve_ghostwrite(&req);
    let model = ai::resolve_model(Some("generate"));
    match ai::call_ai(&st, &resolved, &model, &[]).await {
        ai::AiOutcome::Answer(a) => {
            st.usage.increment(&user);
            Json(json!({"status": "ok", "content": a.trim()}))
        }
        ai::AiOutcome::Cached(a) => {
            Json(json!({"status": "ok", "content": a.trim()}))
        }
        ai::AiOutcome::NeedsApiKey => Json(json!({"status": "needs_api_key"})),
        ai::AiOutcome::Error(e) => Json(json!({"status": "error", "message": e})),
    }
}

async fn quiz_generate_handler(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<quiz::QuizRequest>,
) -> Json<serde_json::Value> {
    let user = user_from(&headers);
    let premium = st.subscription.is_premium(&user);
    let limit = free_limit();
    let used = st.usage.get(&user);
    if !premium && used >= limit {
        return Json(json!({"status": "quota_exceeded", "message": "免费次数已用完"}));
    }
    let resolved = quiz::resolve_quiz(&req);
    let model = ai::resolve_model(Some("generate"));
    match ai::call_ai(&st, &resolved, &model, &[]).await {
        ai::AiOutcome::Answer(a) => {
            let clean = a.trim().trim_start_matches("```json").trim_start_matches("```").trim_end_matches("```").trim();
            let parsed = serde_json::from_str::<serde_json::Value>(clean)
                .unwrap_or_else(|_| json!({"questions": []}));
            st.usage.increment(&user);
            Json(json!({"status": "ok", "questions": parsed["questions"]}))
        }
        ai::AiOutcome::Cached(a) => {
            let clean = a.trim().trim_start_matches("```json").trim_start_matches("```").trim_end_matches("```").trim();
            let parsed = serde_json::from_str::<serde_json::Value>(clean)
                .unwrap_or_else(|_| json!({"questions": []}));
            Json(json!({"status": "ok", "questions": parsed["questions"]}))
        }
        ai::AiOutcome::NeedsApiKey => Json(json!({"status": "needs_api_key"})),
        ai::AiOutcome::Error(e) => Json(json!({"status": "error", "message": e})),
    }
}
