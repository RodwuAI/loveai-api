mod ai;
mod asr;
mod prompt;
mod state;
mod usage;

use axum::{
    extract::State,
    http::HeaderMap,
    routing::{get, post},
    Json, Router,
};
use serde_json::json;
use tower_http::cors::CorsLayer;

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
        .route("/ai/qa", post(qa_handler))
        .route("/ai/image", post(image_handler))
        .route("/ai/voice", get(voice_handler))
        .layer(CorsLayer::permissive())
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
    let user = headers
        .get("x-user-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "anonymous".to_string());

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

    // 配额闸：免费额度用完 → 引导订阅，不调 LLM。
    if used >= limit {
        return Json(json!({
            "status": "quota_exceeded",
            "message": "免费次数已用完，升级解锁更多 LOVEAI 关心",
            "model": "LOVEAI",
            "usage": {"used": used, "limit": limit, "remaining": 0},
            "audit": audit
        }));
    }

    match ai::call_ai(&st, &resolved, &model, &req.history).await {
        ai::AiOutcome::Answer(a) => {
            let new_used = st.usage.increment(&user);
            Json(json!({
                "status": "ok", "answer": a, "cached": false, "model": "LOVEAI",
                "usage": {"used": new_used, "limit": limit, "remaining": limit.saturating_sub(new_used)},
                "audit": audit
            }))
        }
        ai::AiOutcome::Cached(a) => Json(json!({
            "status": "ok", "answer": a, "cached": true, "model": "LOVEAI",
            "usage": {"used": used, "limit": limit, "remaining": limit.saturating_sub(used)},
            "audit": audit
        })),
        ai::AiOutcome::NeedsApiKey => Json(json!({
            "status": "needs_api_key",
            "message": "后端未配置 AI_API_KEY；填入后即返回真实回答（绝不使用假数据）。",
            "model": "LOVEAI",
            "usage": {"used": used, "limit": limit, "remaining": limit.saturating_sub(used)},
            "audit": audit
        })),
        ai::AiOutcome::Error(e) => Json(json!({
            "status": "error", "message": e, "model": "LOVEAI",
            "usage": {"used": used, "limit": limit, "remaining": limit.saturating_sub(used)},
            "audit": audit
        })),
    }
}
