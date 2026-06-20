use serde::Deserialize;
use crate::prompt::{MemoryItem, ResolvedPrompt, Audit};

#[derive(Debug, Deserialize)]
pub struct QuizRequest {
    pub person_name: String,
    #[serde(default)]
    pub memories: Vec<MemoryItem>,
}

const QUIZ_SYS: &str = r#"根据提供的记忆，生成最多 3 道关于这个人的单选题，测试用户对 ta 的了解程度。严格返回 JSON：{"questions":[{"question":"...","options":["A选项","B选项","C选项"],"answer_index":0,"memory_source":"来自哪条记忆"}]}。要求：options[answer_index] 是正确答案（来自记忆），其余选项是合理但错误的干扰项，题目有趣不要太容易，记忆不足则少出题。仅返回 JSON，不加任何其他文字。"#;

pub fn resolve_quiz(req: &QuizRequest) -> ResolvedPrompt {
    let memories_text: String = if req.memories.is_empty() {
        "暂无记录。\n".to_string()
    } else {
        req.memories
            .iter()
            .map(|m| format!("- {}\n", m.content))
            .collect()
    };
    let user_context = format!(
        "心上人：{}\n关于 ta 的记忆：\n{}",
        req.person_name, memories_text
    );
    let user = "请生成最多 3 道单选题，严格返回 JSON。".to_string();
    let sent_chars = QUIZ_SYS.len() + user_context.len() + user.len();
    ResolvedPrompt {
        system: QUIZ_SYS.to_string(),
        developer: String::new(),
        user_context,
        user,
        audit: Audit {
            system_prompt_keys: vec!["quiz".to_string()],
            context_memory_count: req.memories.len(),
            sent_chars,
        },
    }
}
