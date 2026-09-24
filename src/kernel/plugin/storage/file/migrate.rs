//! 存量会话迁移（启动时一次性，ADR-0044 收尾）。
//!
//! 老设计里模型会自行切换话题：一个会话文件内以 `上一会话梗概：` 系统消息为边界，
//! 后面挂着新话题的消息。现在会话边界只由用户决定，这些边界必须变成独立会话，
//! 否则侧栏列表会把多个话题显示成一条。
//!
//! 非致命：任何一步失败只 `log::warn`，启动流程继续（照 `FileMemoryService` 的降级范式）。
//! 幂等：成功后原文件改名为 `<key>.jsonl.bak`（扩展名不是 `.jsonl`，下次扫描自然跳过），
//! 且迁移后的每个段落最多含一个边界节点、必在段首——按「非空段数 ≥ 2」判定即可避免重复拆分。
//! 可回退：`.bak` 即原文件完整字节。

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::kernel::agent::session::{Goal, SessionKey, SessionMeta, SessionStatus};
use crate::kernel::message::{Message, MessageId, MessageKind};

use super::atomic_write_str;

/// 会话边界前缀：`SessionScheduler::create_new_session` 挂的交接摘要系统消息。
const BOUNDARY_PREFIX: &str = "上一会话梗概：";
/// 迁移时派生 `goal` / `title` 的截断长度（与调度层 `Goal` 口径一致）。
const DERIVE_CHARS: usize = 40;

/// 扫描 `sessions/` 下的会话文件，逐个尝试迁移；单个文件失败不影响其余文件与启动。
pub(crate) fn migrate_legacy_sessions(sessions_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(sessions_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        match migrate_file(&path) {
            Ok(0) => {}
            Ok(n) => log::info!("会话迁移：{} 拆分为 {n} 条独立会话", path.display()),
            Err(e) => log::warn!("会话迁移跳过 {}：{e}", path.display()),
        }
    }
}

/// 迁移单个会话文件；返回拆分出的会话数（0 = 无需迁移）。
fn migrate_file(path: &Path) -> Result<usize, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let meta_line = text.lines().next().ok_or("空文件")?;
    let old_meta: SessionMeta =
        serde_json::from_str(meta_line).map_err(|e| format!("元数据解析失败：{e}"))?;
    let messages: Vec<Message> = text
        .lines()
        .skip(1)
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();

    let groups = split_segments(&messages);
    // 段数 = 边界节点数 + （头部非空 ? 1 : 0）。只有「多话题挤在一个文件里」（非空段 ≥ 2）
    // 才拆；单一话题的文件（无边界，或已迁移过的「首条即边界」段落）一律不动。
    //
    // 注意不能写成 `groups.len() <= 1 || groups[0].is_empty()`：真实数据里第一个话题
    // 恰好以边界节点开头（老设计给新会话挂的交接摘要就在最前），那样会整个跳过迁移。
    // 「已迁移」由**非空段数**判定：迁移后的每段最多含一个边界节点，且必在段首。
    if groups.iter().filter(|g| !g.is_empty()).count() <= 1 {
        return Ok(0);
    }

    let now = chrono::Utc::now();
    let mut written: Vec<(SessionKey, Vec<Message>)> = Vec::new();
    for (idx, group) in groups.into_iter().enumerate() {
        if group.is_empty() {
            continue;
        }
        let (key, meta, messages) = build_segment(idx, group, &old_meta, now)?;
        let target = path.with_file_name(format!("{key}.jsonl"));
        atomic_write_str(&target, &serialize_session(&meta, &messages)?)
            .map_err(|e| e.to_string())?;
        written.push((key, messages));
    }

    // 全部段落盘成功后才把原文件降级为备份；失败则保留原文件（下次启动重试）。
    let backup = path.with_extension("jsonl.bak");
    std::fs::rename(path, &backup).map_err(|e| format!("备份原文件失败：{e}"))?;
    Ok(written.len())
}

/// 单段 → 独立会话：保留消息 id，段外 parent 置空（段内已是完整的一条链）。
fn build_segment(
    idx: usize,
    mut messages: Vec<Message>,
    old_meta: &SessionMeta,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(SessionKey, SessionMeta, Vec<Message>), String> {
    let ids: HashSet<MessageId> = messages.iter().map(|m| m.id).collect();
    for msg in &mut messages {
        if msg.parent_id.is_some_and(|p| !ids.contains(&p)) {
            msg.parent_id = None;
        }
    }

    let key = SessionKey::new();
    let mut meta = SessionMeta::new(key);
    meta.status = SessionStatus::Archived;
    meta.archived_at = Some(now);
    meta.created_at = messages.first().map(|m| m.created_at).unwrap_or(now);
    meta.last_activity_at = messages
        .iter()
        .map(|m| m.created_at)
        .max()
        .unwrap_or(meta.created_at);
    // 只有含老 active_path 的那一段继承老状态——不新增 Active，维持单 Active 不变量。
    meta.active_path = old_meta.active_path.filter(|a| ids.contains(a));
    if meta.active_path.is_some() {
        meta.status = old_meta.status;
        meta.archived_at = match old_meta.status {
            SessionStatus::Active => None,
            SessionStatus::Archived => old_meta.archived_at.or(Some(now)),
        };
    }
    // 头部段沿用老 goal；边界段取摘要文本（去掉前缀）前 40 字。
    meta.goal = if idx == 0 {
        old_meta.goal.clone()
    } else {
        boundary_summary(&messages[0]).map(|text| Goal {
            text: text.chars().take(DERIVE_CHARS).collect(),
        })
    };
    // 归档段不会再跑回合，直接给截断标题；继承 Active 的段留空，下一回合由模型生成。
    if meta.status == SessionStatus::Archived {
        meta.title = first_user_text(&messages).map(|t| t.chars().take(DERIVE_CHARS).collect());
    }
    Ok((key, meta, messages))
}

/// 按边界节点切分：index 0 = 首个边界之前的头部，1..=n = 各边界节点及其后代。
/// 无边界节点时只返回一个头部段；调用方按**非空段数**判断是否需要迁移。
fn split_segments(messages: &[Message]) -> Vec<Vec<Message>> {
    let mut parent_of: HashMap<MessageId, Option<MessageId>> = HashMap::new();
    let mut segment_of: HashMap<MessageId, usize> = HashMap::new();
    let mut boundary_count = 0usize;
    for msg in messages {
        parent_of.insert(msg.id, msg.parent_id);
        if boundary_summary(msg).is_some() {
            boundary_count += 1;
            segment_of.insert(msg.id, boundary_count);
        }
    }

    for msg in messages {
        if segment_of.contains_key(&msg.id) {
            continue;
        }
        // 沿 parent 链上溯，归入最近的边界祖先；无边界祖先者归头部段。
        let mut cur = msg.parent_id;
        let mut seen = HashSet::new();
        let segment = loop {
            match cur {
                Some(p) if seen.insert(p) => match segment_of.get(&p) {
                    Some(s) => break *s,
                    None => cur = parent_of.get(&p).copied().flatten(),
                },
                _ => break 0,
            }
        };
        segment_of.insert(msg.id, segment);
    }

    let mut groups: Vec<Vec<Message>> = vec![Vec::new(); boundary_count + 1];
    for msg in messages {
        let segment = segment_of[&msg.id];
        groups[segment].push(msg.clone());
    }
    groups
}

/// 边界节点判定：`System` 且文本以交接摘要前缀开头。
/// 明确排除 `上下文压缩摘要：`（压缩节点）与 `交接摘要：`（老一代旧会话尾标记）——
/// 二者都不是话题边界，留在原段内。
fn boundary_summary(msg: &Message) -> Option<&str> {
    match &msg.kind {
        MessageKind::System { text, .. } => text.strip_prefix(BOUNDARY_PREFIX),
        _ => None,
    }
}

fn first_user_text(messages: &[Message]) -> Option<String> {
    messages.iter().find_map(|m| match &m.kind {
        // 可见文本优先：forced_tool 的 `text` 是给模型的指令，做标题只会得到一串工具名。
        MessageKind::User { .. } => {
            let t = crate::kernel::message::visible_text(m)?.trim();
            (!t.is_empty()).then(|| t.to_string())
        }
        _ => None,
    })
}

/// 会话文件格式与 `FileStorage::persist_session_meta` 一致：首行 meta JSON，其后每行一条消息。
fn serialize_session(meta: &SessionMeta, messages: &[Message]) -> Result<String, String> {
    let mut out = String::new();
    out.push_str(&serde_json::to_string(meta).map_err(|e| format!("元数据序列化失败：{e}"))?);
    out.push('\n');
    for msg in messages {
        out.push_str(&serde_json::to_string(msg).map_err(|e| format!("消息序列化失败：{e}"))?);
        out.push('\n');
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 建数据根 + sessions 目录（迁移要在 `FileStorage::open` 之前放文件）。
    fn temp_root(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mistake-agent-mig-{name}-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(dir.join("sessions")).unwrap();
        dir
    }

    fn write_session_file(
        dir: &Path,
        meta: &SessionMeta,
        messages: &[Message],
    ) -> std::path::PathBuf {
        let path = dir.join("sessions").join(format!("{}.jsonl", meta.key));
        std::fs::write(&path, serialize_session(meta, messages).unwrap()).unwrap();
        path
    }

    /// 线性链：u1 → a1 → [边界 s] → u2 → a2。
    fn linear_with_boundary(key: SessionKey, active: bool) -> (SessionMeta, Vec<Message>) {
        let u1 = Message::user("第一题");
        let mut a1 = Message::assistant("第一题的讲解");
        a1.parent_id = Some(u1.id);
        let mut boundary = Message::system_with_display("上一会话梗概：上一轮做了三件事。", None);
        boundary.parent_id = Some(a1.id);
        let mut u2 = Message::user("换一道新题");
        u2.parent_id = Some(boundary.id);
        let mut a2 = Message::assistant("新题的讲解");
        a2.parent_id = Some(u2.id);

        let messages = vec![u1, a1, boundary, u2.clone(), a2];
        let mut meta = SessionMeta::new(key);
        meta.status = if active {
            SessionStatus::Active
        } else {
            SessionStatus::Archived
        };
        meta.active_path = Some(u2.id);
        (meta, messages)
    }

    /// 直接调用迁移函数（不经 `FileStorage::open`，便于断言文件层面的事实）。
    fn migrate(dir: &Path) -> usize {
        let mut n = 0;
        for entry in std::fs::read_dir(dir.join("sessions")).unwrap().flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                n += migrate_file(&path).unwrap();
            }
        }
        n
    }

    fn load(dir: &Path, key: SessionKey) -> Option<(SessionMeta, Vec<Message>)> {
        let text = std::fs::read_to_string(dir.join("sessions").join(format!("{key}.jsonl")))
            .ok()?;
        let meta = serde_json::from_str(text.lines().next()?).ok()?;
        let messages = text
            .lines()
            .skip(1)
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        Some((meta, messages))
    }

    fn session_files(dir: &Path) -> Vec<std::path::PathBuf> {
        let mut v: Vec<_> = std::fs::read_dir(dir.join("sessions"))
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .collect();
        v.sort();
        v
    }

    /// 迁移后的一段会话：key + 元数据 + 消息。
    type Segment = (SessionKey, SessionMeta, Vec<Message>);

    /// 迁移后的全部会话（按 key 排序），供断言按内容而非旧 key 定位段落。
    fn loaded_sessions(dir: &Path) -> Vec<Segment> {
        let mut out: Vec<_> = session_files(dir)
            .iter()
            .filter_map(|p| {
                let stem = p.file_stem()?.to_str()?;
                stem.parse::<uuid::Uuid>().ok().map(SessionKey)
            })
            .map(|k| {
                let (meta, messages) = load(dir, k).expect("新段落应可读回");
                (k, meta, messages)
            })
            .collect();
        out.sort_by_key(|(k, _, _)| k.0);
        out
    }

    /// 取出（头部段，边界段）：边界段的首条消息即 `上一会话梗概：` 节点。
    fn split_parts(dir: &Path) -> (Segment, Segment) {
        let mut sessions = loaded_sessions(dir);
        assert_eq!(sessions.len(), 2, "应拆成两条会话：{}", sessions.len());
        let tail_idx = sessions
            .iter()
            .position(|(_, _, msgs)| boundary_summary(&msgs[0]).is_some())
            .expect("应有一条以边界节点开头的会话");
        let tail = sessions.remove(tail_idx);
        let head = sessions.remove(0);
        (head, tail)
    }

    #[test]
    fn splits_linear_session_at_boundary_and_keeps_backup() {
        let dir = temp_root("split");
        let key = SessionKey::new();
        let (meta, messages) = linear_with_boundary(key, true);
        let original = write_session_file(&dir, &meta, &messages);
        let original_bytes = std::fs::read(&original).unwrap();

        assert_eq!(migrate(&dir), 2, "应拆成两条会话");

        // 原文件降级为 .bak，字节完全一致（可回退）；每个段落都拿新 key
        // （沿用旧 key 会在写新文件时覆盖掉原文件，先写后备份的顺序会失效）。
        assert!(!original.exists(), "原 <key>.jsonl 应已改名");
        let backup = dir.join("sessions").join(format!("{key}.jsonl.bak"));
        assert_eq!(std::fs::read(&backup).unwrap(), original_bytes);

        let ((head_key, head, head_msgs), (tail_key, tail, tail_msgs)) = split_parts(&dir);
        assert_ne!(head_key, key);
        assert_ne!(tail_key, key);

        // 头部段：2 条消息，段首 parent 为 None，状态一律 Archived。
        assert_eq!(head.status, SessionStatus::Archived);
        assert_eq!(head.active_path, None);
        assert_eq!(head_msgs.len(), 2);
        assert_eq!(head_msgs[0].parent_id, None, "段首段外 parent 应置空");
        assert_eq!(head_msgs[1].parent_id, Some(head_msgs[0].id));

        // 边界段：边界节点在前，其下挂新话题。
        assert_eq!(tail_msgs.len(), 3);
        assert_eq!(tail_msgs[0].id, messages[2].id, "边界消息 id 应保留");
        assert_eq!(tail_msgs[0].parent_id, None, "段首段外 parent 应置空");
        assert_eq!(tail_msgs[1].parent_id, Some(tail_msgs[0].id));
        // 含老 active_path 的段继承老状态与路径。
        assert_eq!(tail.status, SessionStatus::Active);
        assert_eq!(tail.active_path, Some(messages[3].id));
        assert!(tail.archived_at.is_none(), "继承 Active 的段不写归档时间");
        assert_eq!(
            tail.goal.as_ref().unwrap().text,
            "上一轮做了三件事。",
            "边界段 goal 取摘要正文前 40 字"
        );
        // 归档段直接给截断标题（不会再跑回合）；继承 Active 的段留空待模型生成。
        assert_eq!(head.title.as_deref(), Some("第一题"));
        assert!(tail.title.is_none());
    }

    #[test]
    fn migration_is_idempotent_on_second_run() {
        let dir = temp_root("idempotent");
        let key = SessionKey::new();
        let (meta, messages) = linear_with_boundary(key, true);
        write_session_file(&dir, &meta, &messages);

        assert_eq!(migrate(&dir), 2);
        let after_first = session_files(&dir);

        // 二次运行：没有可迁移的文件（.bak 扩展名不是 .jsonl），文件集合不变。
        assert_eq!(migrate(&dir), 0);
        assert_eq!(session_files(&dir), after_first);
        assert_eq!(
            after_first
                .iter()
                .filter(|p| p.to_string_lossy().ends_with(".jsonl.bak"))
                .count(),
            1
        );
    }

    #[test]
    fn keeps_sessions_without_boundary_untouched() {
        let dir = temp_root("noop");
        let key = SessionKey::new();
        let u1 = Message::user("只有一轮");
        let mut a1 = Message::assistant("回答");
        a1.parent_id = Some(u1.id);
        // 老式「交接摘要：」尾标记与压缩摘要节点都不是话题边界。
        let mut legacy = Message::system("交接摘要：旧一代尾标记。");
        legacy.parent_id = Some(a1.id);
        let mut compaction = Message::system("上下文压缩摘要：前文很长。");
        compaction.parent_id = Some(legacy.id);
        let mut meta = SessionMeta::new(key);
        meta.active_path = Some(compaction.id);

        let original = write_session_file(&dir, &meta, &[u1, a1, legacy, compaction]);
        assert_eq!(migrate(&dir), 0);

        let (loaded, msgs) = load(&dir, key).unwrap();
        assert_eq!(msgs.len(), 4, "消息一条不动");
        assert_eq!(loaded.status, SessionStatus::Active);
        assert!(original.exists(), "无边界节点不应迁移");
        assert!(!dir.join("sessions").join(format!("{key}.jsonl.bak")).exists());
    }

    #[test]
    fn skips_file_whose_first_message_is_a_boundary() {
        let dir = temp_root("headless");
        let key = SessionKey::new();
        let mut boundary = Message::system_with_display("上一会话梗概：首条即边界。", None);
        let mut u1 = Message::user("边界后的问题");
        u1.parent_id = Some(boundary.id);
        boundary.parent_id = None;

        let mut meta = SessionMeta::new(key);
        meta.status = SessionStatus::Archived;
        meta.goal = Some(Goal {
            text: "老目标".into(),
        });
        meta.active_path = Some(u1.id);
        let original = write_session_file(&dir, &meta, &[boundary, u1]);

        // 单一话题（唯一一个边界节点就在段首）= 已迁移段落的形态，不迁移（二次运行幂等）。
        assert_eq!(migrate(&dir), 0);
        assert!(original.exists());
        let (loaded, msgs) = load(&dir, key).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(loaded.goal.as_ref().unwrap().text, "老目标");
    }

    #[test]
    fn splits_file_whose_first_message_is_a_boundary_when_more_topics_follow() {
        // 真实数据的形态：老设计给会话挂的交接摘要节点就在**文件最前**，
        // 后面还有别的边界节点——仍是「多话题挤在一个文件里」，必须拆。
        let dir = temp_root("head-boundary-multi");
        let key = SessionKey::new();
        let s1 = Message::system_with_display("上一会话梗概：第一个话题的摘要。", None);
        let mut u1 = Message::user("第一个话题的问题");
        u1.parent_id = Some(s1.id);
        let mut s2 = Message::system_with_display("上一会话梗概：第二个话题的摘要。", None);
        s2.parent_id = Some(u1.id);
        let mut u2 = Message::user("第二个话题的问题");
        u2.parent_id = Some(s2.id);

        let mut meta = SessionMeta::new(key);
        meta.active_path = Some(u2.id);
        write_session_file(&dir, &meta, &[s1, u1, s2, u2]);

        assert_eq!(migrate(&dir), 2, "两条话题应拆成两条独立会话");
        assert!(dir.join("sessions").join(format!("{key}.jsonl.bak")).exists());

        let mut sessions = loaded_sessions(&dir);
        assert_eq!(sessions.len(), 2, "两条话题应拆成两条独立会话");
        // 两段都以边界节点开头，按摘要正文区分（key 是随机的，不能靠顺序）。
        let tail_idx = sessions
            .iter()
            .position(|(_, _, m)| boundary_summary(&m[0]) == Some("第二个话题的摘要。"))
            .expect("应有第二个话题的段落");
        let (_, tail_meta, tail) = sessions.remove(tail_idx);
        let (_, head_meta, head) = sessions.remove(0);

        // 头部段即第一个话题（边界节点 + 其下消息）；两个边界节点分属两段，各在段首。
        assert_eq!(head.len(), 2);
        assert_eq!(boundary_summary(&head[0]), Some("第一个话题的摘要。"));
        assert_eq!(tail.len(), 2);
        assert_eq!(boundary_summary(&tail[0]), Some("第二个话题的摘要。"));
        // 含老 active_path 的段落继承 Active 且留空标题；头部段归档并给截断标题。
        assert_eq!(tail_meta.status, SessionStatus::Active);
        assert!(tail_meta.title.is_none());
        assert_eq!(head_meta.status, SessionStatus::Archived);
        assert_eq!(head_meta.title.as_deref(), Some("第一个话题的问题"));
    }

    #[test]
    fn segment_title_prefers_display_text_of_forced_tool_message() {
        // 老会话首条用户消息常是 forced_tool 的模型指令；标题必须取学生看到的 display_text。
        let dir = temp_root("display-text");
        let key = SessionKey::new();
        let u1 = Message::user_with_display(
            "请调用工具 grading::list 处理当前请求。",
            Some("看看我的错题本".to_string()),
        );
        let mut s1 = Message::system_with_display("上一会话梗概：边界。", None);
        s1.parent_id = Some(u1.id);
        let mut u2 = Message::user("继续");
        u2.parent_id = Some(s1.id);

        let mut meta = SessionMeta::new(key);
        meta.active_path = Some(u2.id);
        write_session_file(&dir, &meta, &[u1, s1, u2]);

        assert_eq!(migrate(&dir), 2);
        let sessions = loaded_sessions(&dir);
        let head = sessions
            .iter()
            .find(|(_, _, m)| boundary_summary(&m[0]).is_none())
            .expect("应有头部段");
        assert_eq!(head.1.title.as_deref(), Some("看看我的错题本"));
    }

    #[test]
    fn splits_branched_descendants_by_nearest_boundary() {
        let dir = temp_root("branch");
        let key = SessionKey::new();
        // u1 → a1 → s1 → u2 → a2，其中 a2 另有兄弟分支 a2b（同一父 u2）。
        let u1 = Message::user("一");
        let mut a1 = Message::assistant("二");
        a1.parent_id = Some(u1.id);
        let mut s1 = Message::system_with_display("上一会话梗概：边界一。", None);
        s1.parent_id = Some(a1.id);
        let mut u2 = Message::user("三");
        u2.parent_id = Some(s1.id);
        let mut a2 = Message::assistant("四");
        a2.parent_id = Some(u2.id);
        let mut a2b = Message::assistant("四之分支");
        a2b.parent_id = Some(u2.id);
        let a2b_id = a2b.id;

        let mut meta = SessionMeta::new(key);
        meta.active_path = Some(a2b_id);
        write_session_file(&dir, &meta, &[u1, a1, s1, u2, a2, a2b]);

        assert_eq!(migrate(&dir), 2);
        let ((_, _, head), (tail_key, tail_meta, tail)) = split_parts(&dir);
        assert_eq!(head.len(), 2, "边界之前的消息归头部段");
        assert_ne!(tail_key, key);

        assert_eq!(tail.len(), 4, "边界及其全部后代（含兄弟分支）归同一段");
        assert!(
            tail.iter().any(|m| m.id == a2b_id),
            "兄弟分支也随父节点归入边界段"
        );
        assert_eq!(tail_meta.active_path, Some(a2b_id), "存活分支指针保留");
    }

}

