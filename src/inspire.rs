// backend/src/inspire.rs
use serde::Deserialize;
use crate::prompt::{MemoryItem, ResolvedPrompt, Audit};

#[derive(Debug, Deserialize)]
pub struct InspireRequest {
    pub person_name: String,
    #[serde(default)]
    pub relation: Option<String>,
    pub scene: String, // romantic | share | express | micro | occasion
    #[serde(default)]
    pub memories: Vec<MemoryItem>,
}

#[derive(Debug, Deserialize)]
pub struct GhostwriteRequest {
    pub person_name: String,
    #[serde(default)]
    pub relation: Option<String>,
    pub action: String,
    #[serde(default)]
    pub memories: Vec<MemoryItem>,
}

const INSPIRE_SYS: &str = r#"你是心上 AI 的"关心顾问"。根据提供的心上人记忆，为指定场景生成 3 条具体可执行的关心行动建议。要求：①具体（不是"多关心"，而是"今晚发给 ta 那首 ta 说过很喜欢的歌"）②有温度（引用记忆细节；无记忆则给走心的通用建议）③好玩高级（不土气）④说明"这样做的理由"。严格返回 JSON：{"suggestions":[{"action":"...","why":"..."},{"action":"...","why":"..."},{"action":"...","why":"..."}]}"#;

const GHOSTWRITE_SYS: &str = r#"你是心上 AI 的代笔人。根据关心行动和对方记忆，写出一段可以直接发出去的文字。要求：①像用户自己写的，不像 AI（无"首先""此外"等书面语）②温暖自然，口语化③融入至少一个记忆细节④长度 50-120 字。直接输出文字内容，不加任何解释。"#;

fn scene_label(scene: &str) -> &'static str {
    match scene {
        "romantic" => "浪漫一击（惊喜、甜蜜）",
        "share" => "分享好东西（在想你）",
        "express" => "说出口（表达爱意）",
        "micro" => "小行动大感受（细心）",
        "occasion" => "特殊时刻（仪式感）",
        _ => "关心表达",
    }
}

fn memories_block(memories: &[MemoryItem]) -> String {
    if memories.is_empty() {
        return "暂无记录，请给走心的通用建议。\n".to_string();
    }
    memories
        .iter()
        .map(|m| {
            if let Some(cat) = &m.category {
                format!("- {} [{}]\n", m.content, cat)
            } else {
                format!("- {}\n", m.content)
            }
        })
        .collect()
}

fn person_header(name: &str, relation: Option<&str>) -> String {
    format!(
        "心上人：{}{}",
        name,
        relation.map(|r| format!("（{}）", r)).unwrap_or_default()
    )
}

pub fn resolve_inspire(req: &InspireRequest) -> ResolvedPrompt {
    let user_context = format!(
        "{}\n关于 ta 的记忆：\n{}",
        person_header(&req.person_name, req.relation.as_deref()),
        memories_block(&req.memories)
    );
    let user = format!(
        "场景：{}\n请生成 3 条专属行动建议，严格返回 JSON。",
        scene_label(&req.scene)
    );
    let sent_chars = INSPIRE_SYS.len() + user_context.len() + user.len();
    ResolvedPrompt {
        system: INSPIRE_SYS.to_string(),
        developer: String::new(),
        user_context,
        user,
        audit: Audit {
            system_prompt_keys: vec!["inspire".to_string()],
            context_memory_count: req.memories.len(),
            sent_chars,
        },
    }
}

pub fn resolve_ghostwrite(req: &GhostwriteRequest) -> ResolvedPrompt {
    let user_context = format!(
        "{}\n关于 ta 的记忆：\n{}",
        person_header(&req.person_name, req.relation.as_deref()),
        memories_block(&req.memories)
    );
    let user = format!("要做的关心行动：{}\n请帮我写出可以直接发给 ta 的文字。", req.action);
    let sent_chars = GHOSTWRITE_SYS.len() + user_context.len() + user.len();
    ResolvedPrompt {
        system: GHOSTWRITE_SYS.to_string(),
        developer: String::new(),
        user_context,
        user,
        audit: Audit {
            system_prompt_keys: vec!["ghostwrite".to_string()],
            context_memory_count: req.memories.len(),
            sent_chars,
        },
    }
}
