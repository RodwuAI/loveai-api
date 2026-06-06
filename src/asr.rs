//! 火山 Seed/Big ASR Streaming 协议封装（移植自已验证项目）。
//! 协议参考：https://www.volcengine.com/docs/6561/1303596
//! 通讯：WSS / binary 帧。
//!
//! 帧结构：[header 4 bytes] + [optional sequence 4 bytes] + [payload size 4 bytes] + [payload]
//! Audio 采样率 16000Hz / mono / 16-bit PCM little-endian。
//!
//! 站在浏览器和火山之间：
//!   - 浏览器 → 后端 WS：raw PCM binary（约 200ms/帧）；文本 {"type":"end"} 表示说完
//!   - 后端 → 火山 WS：首帧 FullClientRequest(JSON config)，之后 AudioOnlyRequest
//!   - 火山 → 后端：FullServerResponse JSON {result:{text,is_final,...}}
//!   - 后端 → 浏览器：JSON event {type:'partial'|'final'|'ready'|'error', text}

use anyhow::{anyhow, bail, Context, Result};
use futures::{SinkExt, StreamExt};
use serde::Serialize;
use std::env;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::{
    client::IntoClientRequest, http::header, protocol::Message as WsMsg,
};

const HEADER_LEN: usize = 4;

const MT_FULL_CLIENT: u8 = 0b0001;
const MT_AUDIO_ONLY: u8 = 0b0010;
const MT_FULL_SERVER: u8 = 0b1001;
const MT_SERVER_ERROR: u8 = 0b1111;

const FLAG_SEQ_POS: u8 = 0b0001;
const FLAG_SEQ_NEG: u8 = 0b0011;

const SER_JSON: u8 = 0b0001;
const SER_NONE: u8 = 0b0000;
const COMP_NONE: u8 = 0b0000;

#[derive(Debug, Clone, Serialize)]
pub struct AsrClientEvent {
    pub r#type: &'static str, // "partial" | "final" | "error" | "ready"
    pub text: String,
}

#[derive(Debug, Serialize)]
struct AsrConfig {
    user: AsrUser,
    audio: AsrAudio,
    request: AsrRequest,
}
#[derive(Debug, Serialize)]
struct AsrUser {
    uid: String,
}
#[derive(Debug, Serialize)]
struct AsrAudio {
    format: &'static str,
    rate: u32,
    bits: u32,
    channel: u32,
    codec: &'static str,
}
#[derive(Debug, Serialize)]
struct AsrRequest {
    model_name: String,
    enable_punc: bool,
    enable_itn: bool,
    enable_ddc: bool,
    show_utterances: bool,
}

/// 返回 true 表示已配置火山 ASR（用于优雅降级提示）。
pub fn is_configured() -> bool {
    (env::var("VOLC_ASR_APP_ID").is_ok() || env::var("VOLCENGINE_ASR_STREAM_APP_ID").is_ok())
        && (env::var("VOLC_ASR_TOKEN").is_ok() || env::var("VOLCENGINE_ASR_STREAM_API_KEY").is_ok())
}

/// 把浏览器 PCM 转发到火山，把识别结果回传到 event_tx。
pub async fn pipe(
    user_id: u64,
    mut audio_rx: mpsc::Receiver<Vec<u8>>,
    event_tx: mpsc::Sender<AsrClientEvent>,
) -> Result<()> {
    let app_id = env::var("VOLC_ASR_APP_ID")
        .or_else(|_| env::var("VOLCENGINE_ASR_STREAM_APP_ID"))
        .map_err(|_| anyhow!("asr_not_configured: VOLC_ASR_APP_ID 未设置"))?;
    let token = env::var("VOLC_ASR_TOKEN")
        .or_else(|_| env::var("VOLCENGINE_ASR_STREAM_API_KEY"))
        .map_err(|_| anyhow!("asr_not_configured: VOLC_ASR_TOKEN 未设置"))?;
    let resource = env::var("VOLC_ASR_RESOURCE_ID")
        .or_else(|_| env::var("VOLCENGINE_ASR_STREAM_RESOURCE_ID"))
        .unwrap_or_else(|_| "volc.bigasr.sauc.duration".into());
    let url = env::var("VOLC_ASR_WS_URL")
        .unwrap_or_else(|_| "wss://openspeech.bytedance.com/api/v3/sauc/bigmodel".into());
    let model_name = env::var("VOLC_ASR_MODEL").unwrap_or_else(|_| "bigmodel".into());

    let request_id = uuid::Uuid::new_v4().to_string();

    let mut req = url.as_str().into_client_request().context("ASR url 非法")?;
    let h = req.headers_mut();
    h.insert("X-Api-App-Key", header::HeaderValue::from_str(&app_id)?);
    h.insert("X-Api-Access-Key", header::HeaderValue::from_str(&token)?);
    h.insert("X-Api-Resource-Id", header::HeaderValue::from_str(&resource)?);
    h.insert("X-Api-Request-Id", header::HeaderValue::from_str(&request_id)?);
    h.insert(
        "X-Api-Connect-Id",
        header::HeaderValue::from_str(&format!("xinshang-{user_id}-{}", uuid::Uuid::new_v4()))?,
    );

    let (mut ws, _resp) = tokio_tungstenite::connect_async(req)
        .await
        .map_err(|e| anyhow!("ASR ws connect failed: {e}"))?;

    let _ = event_tx
        .send(AsrClientEvent { r#type: "ready", text: String::new() })
        .await;

    // 1. 配置帧
    let cfg = AsrConfig {
        user: AsrUser { uid: format!("xinshang_user_{user_id}") },
        audio: AsrAudio { format: "pcm", rate: 16000, bits: 16, channel: 1, codec: "raw" },
        request: AsrRequest {
            model_name,
            enable_punc: true,
            enable_itn: true,
            enable_ddc: true,
            show_utterances: true,
        },
    };
    let cfg_json = serde_json::to_vec(&cfg)?;
    let frame = build_frame(MT_FULL_CLIENT, FLAG_SEQ_POS, SER_JSON, COMP_NONE, Some(1), &cfg_json);
    ws.send(WsMsg::Binary(frame)).await.context("send config frame")?;

    let mut seq: i32 = 1;

    loop {
        tokio::select! {
            audio = audio_rx.recv() => {
                match audio {
                    Some(pcm) if pcm.is_empty() => {
                        seq += 1;
                        let frame = build_frame(MT_AUDIO_ONLY, FLAG_SEQ_NEG, SER_NONE, COMP_NONE, Some(-seq), &[]);
                        let _ = ws.send(WsMsg::Binary(frame)).await;
                    }
                    Some(pcm) => {
                        seq += 1;
                        let frame = build_frame(MT_AUDIO_ONLY, FLAG_SEQ_POS, SER_NONE, COMP_NONE, Some(seq), &pcm);
                        if ws.send(WsMsg::Binary(frame)).await.is_err() { break; }
                    }
                    None => {
                        seq += 1;
                        let frame = build_frame(MT_AUDIO_ONLY, FLAG_SEQ_NEG, SER_NONE, COMP_NONE, Some(-seq), &[]);
                        let _ = ws.send(WsMsg::Binary(frame)).await;
                        break;
                    }
                }
            }
            msg = ws.next() => {
                match msg {
                    Some(Ok(WsMsg::Binary(bin))) => {
                        if let Ok(parsed) = parse_frame(&bin) {
                            if parsed.message_type == MT_FULL_SERVER {
                                if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&parsed.payload) {
                                    let text = extract_text(&json);
                                    let is_final = json.get("result").and_then(|r| r.get("is_final")).and_then(|v| v.as_bool()).unwrap_or(false);
                                    let r#type = if is_final { "final" } else { "partial" };
                                    let _ = event_tx.send(AsrClientEvent { r#type, text }).await;
                                }
                            } else if parsed.message_type == MT_SERVER_ERROR {
                                let txt = String::from_utf8_lossy(&parsed.payload).to_string();
                                let _ = event_tx.send(AsrClientEvent { r#type: "error", text: txt }).await;
                                bail!("server error frame");
                            }
                        }
                    }
                    Some(Ok(WsMsg::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(e)) => {
                        let _ = event_tx.send(AsrClientEvent { r#type: "error", text: format!("ws error: {e}") }).await;
                        break;
                    }
                }
            }
        }
    }
    Ok(())
}

fn extract_text(json: &serde_json::Value) -> String {
    if let Some(t) = json.get("result").and_then(|r| r.get("text")).and_then(|v| v.as_str()) {
        if !t.is_empty() {
            return t.to_string();
        }
    }
    if let Some(arr) = json.get("result").and_then(|r| r.get("utterances")).and_then(|v| v.as_array()) {
        return arr
            .iter()
            .filter_map(|u| u.get("text").and_then(|v| v.as_str()))
            .collect::<Vec<_>>()
            .join("");
    }
    String::new()
}

fn build_frame(
    message_type: u8,
    flags: u8,
    serialization: u8,
    compression: u8,
    sequence: Option<i32>,
    payload: &[u8],
) -> Vec<u8> {
    let mut buf = Vec::with_capacity(HEADER_LEN + 8 + payload.len());
    buf.push(0x11);
    buf.push((message_type << 4) | (flags & 0x0f));
    buf.push((serialization << 4) | (compression & 0x0f));
    buf.push(0x00);
    if let Some(s) = sequence {
        buf.extend_from_slice(&s.to_be_bytes());
    }
    buf.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    buf.extend_from_slice(payload);
    buf
}

struct ParsedFrame {
    message_type: u8,
    payload: Vec<u8>,
}

fn parse_frame(bin: &[u8]) -> Result<ParsedFrame> {
    if bin.len() < HEADER_LEN {
        bail!("frame too short");
    }
    let header_size = (bin[0] & 0x0f) as usize * 4;
    let message_type = (bin[1] >> 4) & 0x0f;
    let flags = bin[1] & 0x0f;

    let mut cursor = header_size;
    let has_seq = matches!(flags, FLAG_SEQ_POS | FLAG_SEQ_NEG)
        || (message_type == MT_FULL_SERVER && flags & 0x01 != 0);
    if has_seq {
        if bin.len() < cursor + 4 {
            bail!("missing seq");
        }
        cursor += 4;
    }
    if message_type == MT_SERVER_ERROR {
        if bin.len() < cursor + 4 {
            bail!("missing error code");
        }
        cursor += 4;
    }
    if bin.len() < cursor + 4 {
        bail!("missing payload size");
    }
    let psize = u32::from_be_bytes([bin[cursor], bin[cursor + 1], bin[cursor + 2], bin[cursor + 3]]) as usize;
    cursor += 4;
    if bin.len() < cursor + psize {
        bail!("payload truncated");
    }
    Ok(ParsedFrame { message_type, payload: bin[cursor..cursor + psize].to_vec() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn t_build_frame_min() {
        let f = build_frame(MT_FULL_CLIENT, FLAG_SEQ_POS, SER_JSON, COMP_NONE, Some(1), b"{}");
        assert_eq!(f.len(), 14);
        assert_eq!(f[0], 0x11);
        assert_eq!(f[1], (MT_FULL_CLIENT << 4) | FLAG_SEQ_POS);
        assert_eq!(f[2], SER_JSON << 4);
    }
}
