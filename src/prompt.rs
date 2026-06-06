use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct MemoryItem {
    pub content: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
}

/// 多轮对话历史中的一轮。role: "user" | "assistant"。
#[derive(Debug, Deserialize)]
pub struct ChatTurn {
    pub role: String,
    pub content: String,
}

/// /ai/qa 请求体。上下文（记忆）由 App 按引用范围挑好后发来——后端不读数据库，
/// 落实「最小必要发送」（ai-prompt-routing-master 表1 第5层）。
#[derive(Debug, Deserialize)]
pub struct QaRequest {
    pub person_name: String,
    #[serde(default)]
    pub relation: Option<String>,
    #[serde(default)]
    pub memories: Vec<MemoryItem>,
    pub question: String,
    #[serde(default)]
    pub tone: Option<String>,
    #[serde(default)]
    pub perspective: Option<String>,
    /// 场景，决定用哪一档模型：qa/analysis/generate→Pro，classify/tag→Lite，report→Long。
    #[serde(default)]
    pub scenario: Option<String>,
    /// 多轮对话历史（不含本次问题）；让 LOVEAI 记住上下文连续聊。
    #[serde(default)]
    pub history: Vec<ChatTurn>,
}

#[derive(Debug, Serialize)]
pub struct Audit {
    pub system_prompt_keys: Vec<String>,
    pub context_memory_count: usize,
    pub sent_chars: usize,
}

#[derive(Debug, Serialize)]
pub struct ResolvedPrompt {
    pub system: String,
    pub developer: String,
    pub user_context: String,
    pub user: String,
    pub audit: Audit,
}

/// 系统内置提示词注册表（ai-prompt-routing-master 表2 的安全/定位/情感语气层，不可被用户覆盖）。
const SYS_PROMPTS: &[(&str, &str)] = &[
    ("SYS.TRUTH_BOUNDARY", "区分事实、记忆与推断；没有记忆不要编造关系或事件；每个结论标注来自哪条记忆或「缺少记忆」。"),
    ("SYS.NO_MINDREADING", "不臆测对方内心、动机、是否爱/恨/出轨；只基于可观察的记忆。任何推测都标注「这是推测，建议直接沟通」，不下定论。"),
    ("SYS.EMOTIONAL_TONE", "温暖、导向行动；不制造愧疚、审判、焦虑或施压；给出下一步，并保留正反馈。"),
    ("SYS.PRIVACY_MINIMAL_SEND", "只使用本次随请求发送的内容；不要请求或假设未提供的资料。"),
    ("SYS.RELATIONSHIP_FOCUS", "你是心上 AI，关系的理解者与守护者；只围绕用户最重要的人，不做泛聊天。"),
    ("SYS.NO_HARM", "不提供操纵、监控、跟踪、套话手段；不做医疗或心理诊断；涉及风险建议温和、直接沟通或寻求专业帮助。"),
];

/// 组装一次关系问答的提示词（system + developer + 上下文 + 用户问题）。
pub fn resolve_qa(req: &QaRequest) -> ResolvedPrompt {
    let system = SYS_PROMPTS
        .iter()
        .map(|(k, v)| format!("[{k}] {v}"))
        .collect::<Vec<_>>()
        .join("\n");

    let developer = "任务：基于提供的记忆回答用户关于这个人的问题。\n\
输出契约：先给结论（标注来自哪条记忆或「缺少记忆」），再给一个温暖、可执行的下一步。\n\
记忆不足时直接说明缺什么、建议记录什么，绝不编造。"
        .to_string();

    let mut ctx = String::new();
    ctx.push_str(&format!("人物：{}", req.person_name));
    if let Some(r) = &req.relation {
        ctx.push_str(&format!("（关系：{r}）"));
    }
    ctx.push('\n');
    if let Some(p) = &req.perspective {
        ctx.push_str(&format!("视角：{p}\n"));
    }
    if let Some(t) = &req.tone {
        ctx.push_str(&format!("语气：{t}\n"));
    }
    if req.memories.is_empty() {
        ctx.push_str("（暂无可引用的记忆）\n");
    } else {
        ctx.push_str("可引用的记忆：\n");
        for (i, m) in req.memories.iter().enumerate() {
            let cat = m.category.as_deref().unwrap_or("未分类");
            let when = m
                .created_at
                .as_deref()
                .map(|d| format!("（记于 {d}）"))
                .unwrap_or_default();
            ctx.push_str(&format!("{}. [{}] {}{}\n", i + 1, cat, m.content, when));
        }
    }

    let user = req.question.clone();
    let sent_chars = system.chars().count()
        + developer.chars().count()
        + ctx.chars().count()
        + user.chars().count();

    let audit = Audit {
        system_prompt_keys: SYS_PROMPTS.iter().map(|(k, _)| k.to_string()).collect(),
        context_memory_count: req.memories.len(),
        sent_chars,
    };

    ResolvedPrompt {
        system,
        developer,
        user_context: ctx,
        user,
        audit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> QaRequest {
        QaRequest {
            person_name: "母亲".into(),
            relation: Some("母亲".into()),
            memories: vec![MemoryItem {
                content: "上周陪她体检，一切正常".into(),
                category: Some("健康".into()),
                created_at: None,
            }],
            question: "她最近怎么样？".into(),
            tone: None,
            perspective: None,
            scenario: None,
            history: vec![],
        }
    }

    #[test]
    fn includes_safety_prompts() {
        let r = resolve_qa(&sample());
        assert!(r.system.contains("SYS.NO_MINDREADING"));
        assert!(r.system.contains("SYS.TRUTH_BOUNDARY"));
        assert!(r.system.contains("SYS.EMOTIONAL_TONE"));
        assert_eq!(r.audit.system_prompt_keys.len(), 6);
    }

    #[test]
    fn minimal_send_only_provided_memories() {
        let r = resolve_qa(&sample());
        assert_eq!(r.audit.context_memory_count, 1);
        assert!(r.user_context.contains("体检"));
        assert!(r.user_context.contains("健康"));
    }

    #[test]
    fn empty_memories_states_no_context() {
        let mut q = sample();
        q.memories.clear();
        let r = resolve_qa(&q);
        assert_eq!(r.audit.context_memory_count, 0);
        assert!(r.user_context.contains("暂无可引用的记忆"));
    }
}
