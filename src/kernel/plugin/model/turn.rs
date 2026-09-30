//! 回合标识（ADR-0047 修订 R14 的客户端侧）：**一次学生提问 = 一个回合**。
//!
//! 服务端按回合计费：回合首个请求扣 1 次，同回合里的工具往返与会话标题等辅助调用不再扣次。
//! 所以客户端必须告诉它"哪些请求属于同一回合"。判断规则：
//!
//! - 请求里最后一条是 **User 消息** → 学生开了新回合，生成新 id；
//! - 否则（工具结果回填、助手续写）→ 沿用当前回合 id；
//! - 辅助调用（标题/摘要）**显式**取 [`current`]：它们自己知道不属于新回合，
//!   免得被上面的规则误判成"又问了一句"。
//!
//! 局限：回合 id 是**进程内单一**的当前值。同一时刻只会有一个活跃回合（kernel 串行执行
//! 用户回合），所以安全；万一两个会话真的并发，也只会把两次提问并成一个回合（少收，不会多收）。

use std::sync::{Mutex, OnceLock};

use crate::kernel::message::{Message, MessageKind};

static CURRENT: OnceLock<Mutex<Option<String>>> = OnceLock::new();

fn cell() -> &'static Mutex<Option<String>> {
    CURRENT.get_or_init(|| Mutex::new(None))
}

/// 开一个新回合（学生消息触发），返回它的 id。
pub fn begin() -> String {
    let id = uuid::Uuid::new_v4().simple().to_string();
    *cell().lock().expect("回合标识锁中毒") = Some(id.clone());
    id
}

/// 当前回合 id；没有活跃回合时为 `None`。
pub fn current() -> Option<String> {
    cell().lock().expect("回合标识锁中毒").clone()
}

/// 解析这次请求属于哪个回合：显式给了就用它，否则按"最后一条是不是用户消息"判断。
pub fn for_request(explicit: Option<&str>, messages: &[Message]) -> String {
    if let Some(id) = explicit.map(str::trim).filter(|id| !id.is_empty()) {
        return id.to_string();
    }
    let last_is_user = matches!(
        messages.last().map(|m| &m.kind),
        Some(MessageKind::User { .. })
    );
    if last_is_user {
        begin()
    } else {
        current().unwrap_or_else(begin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_message_starts_a_new_turn_and_tool_round_reuses_it() {
        let user = Message::user("你好");
        let first = for_request(None, std::slice::from_ref(&user));
        // 同一回合的第二次往返（最后一条不是用户消息）必须沿用同一个 id
        let tool_result = Message::tool_call_with_id(
            "memory::show",
            serde_json::json!({}),
            Ok(serde_json::json!({"ok": true})),
            "call_1".into(),
        );
        let second = for_request(None, &[user.clone(), tool_result]);
        assert_eq!(first, second, "工具往返不该自成一个回合");

        // 下一条学生消息才是新回合
        let next = for_request(None, &[Message::user("再问一句")]);
        assert_ne!(first, next, "新的学生消息应当开新回合");
    }

    #[test]
    fn explicit_turn_id_wins() {
        let messages = vec![Message::user("标题生成用的辅助调用")];
        let explicit = for_request(Some("turn-abc"), &messages);
        assert_eq!(explicit, "turn-abc", "辅助调用显式指定的回合优先");
        // 注意：显式指定**不**改全局当前回合——辅助调用只是借用当前回合，
        // 真正开新回合的是学生消息（见上一个用例）。
        // 空串/纯空白等于没给
        let generated = for_request(Some("  "), &messages);
        assert_ne!(generated, "  ");
    }
}
