// 会话消息树 → 前端气泡（聊天页与会话历史详情共用同一渲染）。

import { toolIcon, toolTitle } from "./tools";

// 附件名截到（ 或 ( 为止：历史消息里 kernel 曾在名字后接「（…）」注记，
// 不截断会把注记吞进附件名。
const ATTACH_RE = /\n附件：(\S+)\|([^|\n（(]+)/g;
// 系统临时暂存路径（Unix + Windows）：展示时一律隐藏，不把路径暴露给学生。
const TMP_PATH_RE = /\/tmp\/mistake-agent-[^\s|（(]+/g;
const WIN_PATH_RE = /\b[A-Z]:\\[^\s|（(]*?mistake-agent[^\s|（(]*/gi;
// 老消息（无 display_text）：从「请调用工具 X 处理：Y」还原为「标题：Y」。
const FORCED_RE = /^请调用工具 (\S+) 处理[:：]?\s*(.*)$/s;
// AI 模型在附件/路径旁生成的说明文字（括号注记）：展示时一律隐藏。
const SYSTEM_NOTE_RE = /[（(]该路径仅用于界面展示[，,]\s*file\s*参数[必须请使用]*前面的暂存路径[）)]/g;

/** 从消息文本解析全部持久化附件标记（kernel 落盘的「附件：路径|名称」，可能多条）。 */
export function parseAttachments(text) {
  const out = [];
  const re = ATTACH_RE;
  re.lastIndex = 0;
  let m;
  while ((m = re.exec(String(text || "")))) {
    out.push({ path: m[1], name: m[2] });
  }
  return out;
}

/**
 * 构建会话视图：消息树 + 逐节点版本指针（参考 DeepSeek 网页版：
 * childIds / currentChildIndex / rootBranchIds / rootBranchIndex）。
 * activePath 来自服务端 meta.active_path，用于把指针初始化到服务端活跃链。
 */
export function buildSessionView(messages, activePath) {
  const list = messages || [];
  const nodes = new Map();
  for (const m of list) {
    nodes.set(String(m.id), { message: m, childIds: [], currentChildIndex: 0 });
  }
  const byTime = (ids) =>
    ids.slice().sort((a, b) => {
      const ta = new Date(nodes.get(a)?.message.created_at || 0);
      const tb = new Date(nodes.get(b)?.message.created_at || 0);
      return ta - tb;
    });
  const roots = [];
  for (const m of list) {
    const id = String(m.id);
    const p = m.parent_id ? String(m.parent_id) : null;
    if (p && nodes.has(p)) nodes.get(p).childIds.push(id);
    else roots.push(id);
  }
  let rootBranchIds = byTime(roots);
  for (const n of nodes.values()) n.childIds = byTime(n.childIds);

  // 沿服务端活跃路径回溯，把每层指针指向活跃子节点
  let end = activePath ? String(activePath) : null;
  if (!end && list.length) end = String(list[list.length - 1].id);
  const path = [];
  const seen = new Set();
  let cur = end;
  while (cur && nodes.has(cur) && !seen.has(cur)) {
    seen.add(cur);
    path.push(cur);
    cur = nodes.get(cur).message.parent_id
      ? String(nodes.get(cur).message.parent_id)
      : null;
  }
  path.reverse();
  let rootBranchIndex = 0;
  if (path.length) {
    const ri = rootBranchIds.indexOf(path[0]);
    if (ri >= 0) rootBranchIndex = ri;
    for (let i = 1; i < path.length; i += 1) {
      const parent = nodes.get(path[i - 1]);
      const ci = parent.childIds.indexOf(path[i]);
      if (ci >= 0) parent.currentChildIndex = ci;
    }
  }
  return { nodes, rootBranchIds, rootBranchIndex };
}

/** 活跃链：根版本 → 逐层 currentChildIndex 子节点 → 叶子。 */
export function getActiveChain(view) {
  const chain = [];
  let cur = view.rootBranchIds[view.rootBranchIndex];
  const seen = new Set();
  while (cur && view.nodes.has(cur) && !seen.has(cur)) {
    seen.add(cur);
    const node = view.nodes.get(cur);
    chain.push(node.message);
    cur = node.childIds[node.currentChildIndex] ?? null;
  }
  return chain;
}

/**
 * < / > 切换版本：对本消息所在版本列表取模循环。
 * 根层切 rootBranchIndex；非根层把父节点的 currentChildIndex 前/后移一位。
 */
export function navigateBranch(view, messageId, dir = 1) {
  const m = view.nodes.get(String(messageId));
  if (!m) return;
  const parentId = m.message.parent_id ? String(m.message.parent_id) : null;
  if (!parentId || !view.nodes.has(parentId)) {
    const n = view.rootBranchIds.length;
    if (n < 2) return;
    view.rootBranchIndex = (((view.rootBranchIndex + dir) % n) + n) % n;
  } else {
    const parent = view.nodes.get(parentId);
    const n = parent.childIds.length;
    if (n < 2) return;
    parent.currentChildIndex = (((parent.currentChildIndex + dir) % n) + n) % n;
  }
}

/**
 * 会话视图 → 前端气泡：只渲染活跃链（一次一个版本，DeepSeek 式）。
 * 系统消息中：`交接摘要：`（迁移前旧会话尾标记）隐藏；`上一会话梗概：`（会话边界）
 * 渲染为折叠分隔气泡；其余保留原文，展示文本优先取 display_text。
 * 每个气泡带版本元数据供 < / > 使用。
 */
export function renderPath(view, opts = {}) {
  return getActiveChain(view)
    .map((m) => {
      const parentKey = m.parent_id ? String(m.parent_id) : "__root__";
      const parentNode =
        parentKey === "__root__" ? null : view.nodes.get(parentKey) || null;
      const group = parentNode ? parentNode.childIds : view.rootBranchIds;
      const versionIndex = parentNode
        ? parentNode.currentChildIndex
        : view.rootBranchIndex;
      const base = {
        messageId: String(m.id),
        parentId: m.parent_id ? String(m.parent_id) : null,
        siblingIds: group.filter((g) => g !== String(m.id)),
        versions: group.map((gid) => ({
          id: gid,
          createdAt: view.nodes.get(gid)?.message.created_at || null,
        })),
        versionIndex,
        versionCount: group.length,
        createdAt: m.created_at,
        sessionKey: opts.sessionKey ?? null,
      };
      const kind = m.kind || {};
      if (kind.kind === "user") {
        const raw = kind.text || "";
        // ADR-0046：附件以路径引用持久化（attachment_refs），历史回放据此渲染；
        // 旧数据回退解析文本里的「附件：路径|名称」标记。
        const refs = kind.attachment_refs || [];
        const attachments = refs.length
          ? refs.map((r) => ({ path: r.name, name: r.display_name || r.name }))
          : parseAttachments(raw);
        let shown = (kind.display_text || raw)
          .replace(ATTACH_RE, "")
          .replace(TMP_PATH_RE, "")
          .replace(WIN_PATH_RE, "")
          .replace(SYSTEM_NOTE_RE, "")
          .trim();
        if (!kind.display_text) {
          const forced = shown.match(FORCED_RE);
          if (forced) {
            const title = toolTitle(forced[1]);
            const rest = forced[2].replace(TMP_PATH_RE, "").replace(WIN_PATH_RE, "").replace(SYSTEM_NOTE_RE, "").trim();
            shown = rest ? `${title}：${rest}` : title;
          }
        }
        const text = attachments.length
          ? shown.replace(ATTACH_RE, "").trim()
          : shown;
        return {
          ...base,
          type: "user",
          text,
          attachments,
        };
      }
      if (kind.kind === "assistant") {
        return { ...base, type: "assistant", text: kind.text || "" };
      }
      if (kind.kind === "system") {
        const raw = kind.text || "";
        // 老一代「旧会话尾部」标记：迁移后不再产生，历史回放一律隐藏。
        if (/^交接摘要[:：]/.test(raw)) return null;
        // 会话边界（交接摘要节点）：折叠分隔气泡，正文即摘要原文。
        const boundary = raw.match(/^上一会话梗概[:：]\s*/);
        if (boundary) {
          return {
            ...base,
            type: "divider",
            title: "上一会话梗概",
            text: raw.slice(boundary[0].length),
          };
        }
        if (kind.display_text === "") return null;
        return { ...base, type: "system", text: kind.display_text || raw };
      }
      if (kind.kind === "reasoning") {
        return { ...base, type: "reasoning", text: kind.text || "" };
      }
      if (kind.kind === "tool_call") {
        const ok = Boolean(kind.result?.Ok);
        return {
          ...base,
          type: "tool",
          entry: kind.entry || "",
          title: toolTitle(kind.entry || ""),
          toolOk: ok,
          toolIcon: toolIcon(kind.entry || ""),
          params: kind.params || {},
          result: ok ? kind.result?.Ok : kind.result?.Err || null,
        };
      }
      return null;
    })
    .filter(Boolean);
}
