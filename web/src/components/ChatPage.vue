<script setup>
import { computed, inject, nextTick, onMounted, onUnmounted, ref, watch } from "vue";
import { Icon } from "@iconify/vue";
import MessageBubble from "./MessageBubble.vue";
import AttachmentViewer from "./AttachmentViewer.vue";
import { runPython } from "../lib/pyodide";
import { attachmentUrl } from "../lib/attachments";
import {
  buildSessionView,
  getActiveChain,
  navigateBranch,
  renderPath,
} from "../lib/messages";
import { loadToolCatalog, toolIcon, toolList, toolTitle } from "../lib/tools";

const props = defineProps({
  kernel: { type: Object, required: true },
  ready: { type: Boolean, default: false },
});
const emit = defineEmits(["status", "navigate"]);

const inputText = ref("");
const busy = ref(false);
const toolStatus = ref(null); // { entry, message, icon }
const bubbles = ref([]);
const editingId = ref(null);
const currentStreamId = ref(null);
const sessionViews = ref({}); // sessionKey -> buildSessionView（含逐节点版本指针）
const activeSessionKey = ref(null);
const historyRefreshGen = ref(0); // 防重入：每次 refreshAllHistory 递增，仅最新世代生效
const tools = ref([]); // 用户可见工具（list_tools，供输入候选）
const suggestions = ref([]);
const activeSuggestion = ref(-1);
const armedTool = ref(null); // Tab 确认的待调用工具 { entry, title, icon }
const pendingAttachments = ref([]); // 选完未发送的附件列表（可多张/混合 PDF）
const cacheStats = ref(null); // 上下文缓存命中统计（get_cache_stats）
const inputEl = ref(null);
const overflowOpen = ref(false);

/* ──── 跨页面追问跳转 ──── */
const navigateToChatMessage = inject("navigateToChatMessage", ref(""));

/* ──── 工具交互（薄弱点→出题→批改） ──── */
async function onToolInteract(payload) {
  if (payload.action === "generate") {
    addBubble({ type: "user", text: `薄弱点练习：${payload.knowledge_point}` });
    busy.value = true;
    setStatus(true, "正在出题…");
    try {
      const result = await props.kernel.triggerCommand("practice::generate", {
        knowledge_point: payload.knowledge_point,
        difficulty: payload.difficulty || "basic",
      });
      addBubble({
        type: "tool",
        entry: "practice::generate",
        title: toolTitle("practice::generate") || "分层出题",
        toolOk: result.matched,
        toolIcon: toolIcon("practice::generate"),
        params: { knowledge_point: payload.knowledge_point, difficulty: payload.difficulty },
        result,
      });
    } catch (e) {
      addBubble({ type: "error", text: `出题失败：${e.message}` });
    } finally {
      busy.value = false;
      setStatus(false, "就绪");
      scrollBottom();
    }
    return;
  }

  if (payload.action === "practice-again") {
    if (payload.samePoint) {
      addBubble({ type: "user", text: `再来一题：${payload.knowledge_point}` });
      busy.value = true;
      setStatus(true, "正在出题…");
      try {
        const result = await props.kernel.triggerCommand("practice::generate", {
          knowledge_point: payload.knowledge_point,
          difficulty: payload.difficulty || "basic",
        });
        addBubble({
          type: "tool",
          entry: "practice::generate",
          title: toolTitle("practice::generate") || "分层出题",
          toolOk: result.matched,
          toolIcon: toolIcon("practice::generate"),
          params: { knowledge_point: payload.knowledge_point, difficulty: payload.difficulty },
          result,
        });
      } catch (e) {
        addBubble({ type: "error", text: `出题失败：${e.message}` });
      } finally {
        busy.value = false;
        setStatus(false, "就绪");
        scrollBottom();
      }
    } else {
      // 换知识点 → 重新分析薄弱点
      addBubble({ type: "user", text: "分析我的薄弱知识点" });
      busy.value = true;
      setStatus(true, "正在分析薄弱点…");
      try {
        const result = await props.kernel.triggerCommand("practice::gaps", {});
        addBubble({
          type: "tool",
          entry: "practice::gaps",
          title: toolTitle("practice::gaps") || "薄弱点定位",
          toolOk: true,
          toolIcon: toolIcon("practice::gaps"),
          params: {},
          result,
        });
      } catch (e) {
        addBubble({ type: "error", text: `薄弱点分析失败：${e.message}` });
      } finally {
        busy.value = false;
        setStatus(false, "就绪");
        scrollBottom();
      }
    }
  }
}

/** 处理从其他页面导航过来的结构化指令 */
async function handleNavigatePayload(payload) {
  if (payload.action === "variant-practice") {
    // 错题本抽屉"变式练习"：直接调用 practice::generate
    await onToolInteract({
      action: "generate",
      knowledge_point: payload.knowledge_point,
      difficulty: payload.difficulty || "variant",
    });
  } else if (payload.action === "ask-question") {
    // 错题本右键"追问"：发送聊天消息
    inputText.value = payload.text || "";
    await nextTick();
    sendMessage();
  }
}

/** 记录会话最近活动时间到 localStorage，供 SessionsPage 读取 */
const LS_ACTIVITY_PREFIX = "ma:last-activity:";
function recordSessionActivity(key) {
  if (!key) return;
  try {
    localStorage.setItem(LS_ACTIVITY_PREFIX + key, new Date().toISOString());
  } catch { /* quota 满时静默 */ }
}

let unsubscribe = null;
let assistantIndex = -1;
let reasoningText = "";
let reasoningIndex = -1;
let pendingSendId = null;

const canSend = computed(
  () =>
    props.ready &&
    !busy.value &&
    (inputText.value.trim() || armedTool.value || pendingAttachments.value.length),
);

const GROUP_ORDER = ["批改", "学习", "记忆", "其它", "调试"];

const MAX_VISIBLE_TOOLS = 5;
const visibleTools = computed(() => tools.value.slice(0, MAX_VISIBLE_TOOLS));
const overflowTools = computed(() => tools.value.slice(MAX_VISIBLE_TOOLS));

function toggleOverflow() {
  overflowOpen.value = !overflowOpen.value;
}

function closeOverflow() {
  overflowOpen.value = false;
}

const quickActions = [
  { id: "upload", label: "上传图片/PDF", desc: "看图提问、讲解或批改归档", icon: "mdi:upload", action: "upload" },
  { id: "mistakes", label: "查看错题本", desc: "按学科与知识点回顾错因", icon: "mdi:format-list-bulleted", action: "navigate" },
  { id: "settings", label: "配置模型", desc: "设置主模型与视觉模型密钥", icon: "mdi:cog-outline", action: "navigate" },
];

function onQuickAction(a) {
  if (a.action === "upload") {
    pickHomework();
  } else {
    emit("navigate", a.id);
  }
}

/** 输入时计算候选工具：匹配第一个 token（工具名/标题前缀）。 */
function computeSuggestions() {
  const firstToken = (inputText.value.match(/\S+/) || [""])[0];
  if (armedTool.value || !firstToken || firstToken.length < 2) {
    suggestions.value = [];
    return;
  }
  const q = firstToken.toLowerCase();
  const hasNs = firstToken.includes("::");
  suggestions.value = tools.value
    .filter(
      (t) =>
        t.entry.toLowerCase().includes(q) ||
        (t.title || "").toLowerCase().includes(q) ||
        (hasNs && t.entry.toLowerCase().startsWith(q)),
    )
    .slice(0, 8);
  activeSuggestion.value = suggestions.value.length ? 0 : -1;
}

/** 加载用户可见工具（候选数据全部来自后端 list_tools）。 */
async function loadTools() {
  if (!props.ready) return;
  await loadToolCatalog(props.kernel);
  tools.value = toolList().sort((a, b) => {
    const ga = GROUP_ORDER.indexOf(a.group || "其它");
    const gb = GROUP_ORDER.indexOf(b.group || "其它");
    return (
      (ga === -1 ? 99 : ga) - (gb === -1 ? 99 : gb) ||
      (a.title || a.entry).localeCompare(b.title || b.entry, "zh")
    );
  });
}

/** 聊天上下文缓存命中率（后端统计主模型回合调用，按会话 + 全局）。 */
async function loadCacheStats() {
  try {
    cacheStats.value = await props.kernel.call("get_cache_stats", {}, 8000);
  } catch {
    // 统计读取失败不影响聊天。
  }
}

function fmtTokens(n) {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(2)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
  return String(n);
}

/** 优先展示当前会话的命中率，没有会话样本时用全局累计。 */
const cacheRateText = computed(() => {
  const s = cacheStats.value;
  const session = s?.sessions?.find((x) => x.key === s.active_key) || s?.sessions?.[0];
  const src = session || s?.main;
  if (!src || src.hit_rate == null) return "—";
  return `${(src.hit_rate * 100).toFixed(1)}%`;
});

const cacheTitle = computed(() => {
  const s = cacheStats.value;
  const session = s?.sessions?.find((x) => x.key === s.active_key) || s?.sessions?.[0];
  const lines = [];
  if (session) {
    lines.push(
      `本会话：${session.calls} 次调用 · 命中 ${fmtTokens(session.hit_tokens)} · 未命中 ${fmtTokens(session.miss_tokens)} tokens`,
    );
  }
  if (s?.main?.calls) {
    lines.push(
      `累计：${s.main.calls} 次调用 · 命中率 ${s.main.hit_rate == null ? "—" : `${(s.main.hit_rate * 100).toFixed(1)}%`}`,
    );
  }
  lines.push("点击刷新");
  return lines.join("\n");
});

/** 武装工具：工具名只进徽章，输入框只留参数（避免「徽章 + 工具名文字」双份）。 */
function armTool(tool, stripToken = false) {
  if (stripToken) {
    // 从输入候选确认：移除已输入的触发词（工具名/标题），剩余文本作为参数。
    inputText.value = inputText.value.replace(/^\S+\s*/, "");
  }
  armedTool.value = {
    entry: tool.entry,
    title: tool.title || tool.entry,
    icon: toolIcon(tool.entry),
  };
  suggestions.value = [];
}

function unarmTool() {
  armedTool.value = null;
}

/** 输入变化：只重算候选与自动高度；武装状态由徽章 X 或再次点选工具解除。 */
function onInput() {
  computeSuggestions();
  autoResize();
}

/** 工具栏点击：选中/取消工具，进入待调用状态并聚焦输入框。 */
function pickTool(tool) {
  if (busy.value) return;
  if (armedTool.value?.entry === tool.entry) {
    unarmTool();
  } else {
    armTool(tool);
  }
  inputEl.value?.focus();
}

function onKeydown(e) {
  if (e.key === "Tab") {
    e.preventDefault();
    if (suggestions.value.length) {
      const t =
        suggestions.value[
          activeSuggestion.value >= 0 ? activeSuggestion.value : 0
        ];
      armTool(t, true);
    } else if (!armedTool.value) {
      // 候选框未弹出时兜底：输入的工具名精确匹配也直接确认。
      const firstToken = (inputText.value.match(/\S+/) || [""])[0];
      const exact = tools.value.find(
        (t) => t.entry.toLowerCase() === firstToken.toLowerCase(),
      );
      if (exact) armTool(exact, true);
    }
  } else if (e.key === "ArrowDown" && suggestions.value.length) {
    e.preventDefault();
    activeSuggestion.value =
      (activeSuggestion.value + 1) % suggestions.value.length;
  } else if (e.key === "ArrowUp" && suggestions.value.length) {
    e.preventDefault();
    activeSuggestion.value =
      (activeSuggestion.value - 1 + suggestions.value.length) %
      suggestions.value.length;
  }
}

function scrollBottom() {
  requestAnimationFrame(() => {
    const el = document.getElementById("messages");
    if (el) el.scrollTop = el.scrollHeight;
  });
}

function autoResize() {
  const el = inputEl.value;
  if (!el) return;
  el.style.height = "auto";
  el.style.height = el.scrollHeight + "px";
}

function addBubble(b) {
  bubbles.value.push({ ...b, createdAt: b.createdAt || new Date().toISOString() });
  scrollBottom();
}

function setStatus(b, text) {
  emit("status", { busy: b, text });
}

function ensureAssistant(messageId) {
  if (assistantIndex >= 0 && bubbles.value[assistantIndex]?.messageId === messageId) {
    return bubbles.value[assistantIndex];
  }
  assistantIndex = bubbles.value.length;
  addBubble({ type: "assistant", text: "", messageId });
  currentStreamId.value = messageId;
  return bubbles.value[assistantIndex];
}

function ensureReasoning(delta) {
  reasoningText += delta;
  if (reasoningIndex < 0) {
    reasoningIndex = bubbles.value.length;
    addBubble({ type: "reasoning", text: reasoningText });
  } else {
    bubbles.value[reasoningIndex].text = reasoningText;
  }
}

function finalize() {
  if (assistantIndex >= 0 && !bubbles.value[assistantIndex]?.text) {
    bubbles.value.splice(assistantIndex, 1);
  }
  assistantIndex = -1;
  reasoningIndex = -1;
  reasoningText = "";
  currentStreamId.value = null;
}

/** 用缓存的会话视图重建合并气泡流（聊天页：各会话只渲染活跃链，副本按 id 去重）。
 *  与本地已渲染气泡做合并（按 messageId 或用户消息文本去重），避免
 *  turn_end 后全量替换时因后端数据缺失导致刚发出的消息消失。 */
function renderMergedBubbles() {
  const seen = new Set();
  const all = [];
  for (const [key, view] of Object.entries(sessionViews.value)) {
    for (const b of renderPath(view, { sessionKey: key })) {
      const id = String(b.messageId);
      if (seen.has(id)) continue;
      seen.add(id);
      all.push(b);
    }
  }
  all.sort((a, b) => new Date(a.createdAt || 0) - new Date(b.createdAt || 0));
  console.log("[renderMergedBubbles] all:", all.length, "bubbles:", bubbles.value.length); if (!all.length) { console.warn("[renderMergedBubbles] all empty, skip"); return; }

  // 合并策略：后端已有的按 ID/文本匹配更新；本地独有的保留不丢
  const backendIds = new Set(all.map((b) => String(b.messageId)));
  const backendUserTexts = new Set(
    all.filter((b) => b.type === "user").map((b) => b.text),
  );

  // 第一遍：将本地已有气泡更新为后端版本（匹配 messageId 或同文本用户消息）
  const consumedTexts = new Set();
  for (let i = 0; i < bubbles.value.length; i++) {
    const lb = bubbles.value[i];
    const lbId = lb.messageId ? String(lb.messageId) : null;
    if (lbId && backendIds.has(lbId)) {
      // 直接替换为后端气泡（含完整 messageId / versions 等元数据）
      const backend = all.find((b) => String(b.messageId) === lbId);
      if (backend) bubbles.value[i] = backend;
      continue;
    }
    // 用户消息无 messageId（刚由 addBubble 添加）：按文本匹配后端版本
    if (!lbId && lb.type === "user" && backendUserTexts.has(lb.text) && !consumedTexts.has(lb.text)) {
      const backend = all.find(
        (b) => b.type === "user" && b.text === lb.text,
      );
      if (backend) {
        bubbles.value[i] = backend;
        consumedTexts.add(lb.text);
      }
    }
  }

  // 第二遍：追加后端独有的气泡（本次回合新增的 assistant / tool / reasoning 等）
  console.log("[renderMergedBubbles] pass1 done, bubbles:", bubbles.value.map(b => `${b.type}:${b.messageId ? String(b.messageId).slice(-8) : "no-id"}:${(b.text||"").slice(0,20)}`)); const existingIds = new Set(bubbles.value.map((b) => (b.messageId ? String(b.messageId) : null)).filter(Boolean));
  for (const b of all) {
    if (!existingIds.has(String(b.messageId))) {
      bubbles.value.push(b);
    }
  }

  // 按时间排序（本地气泡无 createdAt 的排在前面保持插入顺序）
  bubbles.value.sort(
    (a, b) => new Date(a.createdAt || 0) - new Date(b.createdAt || 0),
  );
  scrollBottom();
}

/** 全量历史：每个会话建消息树视图（DeepSeek 式逐节点版本指针），只渲染活跃链。 */
async function refreshAllHistory() {
	  const gen = ++historyRefreshGen.value;
  try {
    const list = await props.kernel.call("list_sessions", {}, 8000);
    const arr = list.sessions || [];
    const views = {};
    activeSessionKey.value =
      arr.find((s) => s.status === "active")?.key || null;
    for (const s of arr) {
      try {
        const detail = await props.kernel.call("read_session", { key: s.key }, 8000);
        console.log("[refreshAllHistory] session", s.key, "status", s.status, "msgs:", detail.messages.length, "active_path:", detail.meta?.active_path); views[s.key] = buildSessionView(
          detail.messages,
          detail.meta?.active_path || null,
        );
      } catch {
        // 单个会话读取失败不阻断整体历史。
      }
    }
    if (gen !== historyRefreshGen.value) { console.warn("[refreshAllHistory] stale gen=", gen, "current=", historyRefreshGen.value, "- discarding"); return; }
	    sessionViews.value = views;
    renderMergedBubbles();
    // 同步活跃会话的活动时间到 localStorage
    if (activeSessionKey.value) recordSessionActivity(activeSessionKey.value);
  } catch (e) {
    // list_sessions/read_session 尚未接通时，聊天仍可用，只是没有分支/编辑入口。
    if (e.code !== "not_implemented") console.warn("会话回读失败：", e);
  }
}

let historyLoaded = false;
async function ensureHistory() {
  if (historyLoaded || !props.ready) return;
  historyLoaded = true;
  await refreshAllHistory();
}

async function handleComputeRequest(req) {
  toolStatus.value = {
    entry: "compute::verify",
    message: "正在运行 Python 验算…",
    icon: "mdi:calculator-variant",
  };
  try {
    const r = await runPython(req.code);
    await props.kernel.call("compute_result", {
      compute_id: req.id,
      stdout: r.stdout,
      stderr: r.stderr,
      duration_ms: r.durationMs,
    });
  } catch (e) {
    await props.kernel.call("compute_result", {
      compute_id: req.id,
      stdout: "",
      stderr: String(e?.message ?? e),
      duration_ms: 0,
    });
  } finally {
    if (toolStatus.value?.entry === "compute::verify") toolStatus.value = null;
  }
}

function handleFrame(frame) {
  if (frame.type === "response") {
    if (frame.id === pendingSendId && frame.error) {
      addBubble({ type: "error", text: `请求失败：${frame.error.message}` });
      busy.value = false;
      finalize();
      setStatus(false, "就绪");
    }
    return;
  }
  if (frame.type !== "event") return;
  const e = frame.event;
  switch (e.event) {
    case "message_delta":
      ensureAssistant(e.message_id).text += e.delta;
      scrollBottom();
      break;
    case "reasoning_delta":
      ensureReasoning(e.delta);
      break;
    case "tool_start":
      toolStatus.value = { entry: e.entry, message: "执行中", icon: e.icon };
      break;
    case "tool_progress":
      toolStatus.value = { entry: e.entry, message: e.message, icon: e.icon };
      break;
    case "tool_end":
      toolStatus.value = { entry: e.entry, message: e.ok ? "完成" : "失败", icon: toolIcon(e.entry) };
      break;
    case "compute_request":
      handleComputeRequest(e);
      break;
    case "turn_end":
      finalize();
      toolStatus.value = null;
      busy.value = false;
      setStatus(false, "就绪");
      refreshAllHistory();
      break;
    case "cache_stats_updated":
      cacheStats.value = e.stats;
      break;
    case "error":
      finalize();
      addBubble({ type: "error", text: e.message });
      busy.value = false;
      setStatus(false, "异常");
      refreshAllHistory();
      break;
  }
}

async function sendMessage() {
  const text = inputText.value.trim();
  if (busy.value) return;
  if (!text && !armedTool.value && !pendingAttachments.value.length) return;
  // 即时记录活动时间（不等 turn_end 异步回调）
  if (activeSessionKey.value) recordSessionActivity(activeSessionKey.value);
  const attachments = pendingAttachments.value.map((a) => ({
    path: a.asset_path,
    name: a.name,
  }));
  const extra = pendingAttachments.value.length
    ? {
        file: pendingAttachments.value.map((a) => a.temp_path),
        asset: attachments,
      }
    : {};
  if (armedTool.value) {
    const tool = armedTool.value;
    armedTool.value = null;
    pendingAttachments.value = [];
    const hint = inputText.value.trim();
    inputText.value = "";
    nextTick(autoResize);
    const display = tool.title + (hint ? `：${hint}` : "");
    addBubble({
      type: "user",
      text: display,
      toolIcon: tool.icon,
      attachments,
    });
    busy.value = true;
    setStatus(true, "正在调用工具");
    try {
      pendingSendId = await props.kernel.sendLine("send_user_message", {
        text: hint,
        force_tool: { entry: tool.entry, hint, display },
        ...extra,
      });
    } catch (err) {
      addBubble({ type: "error", text: `发送失败：${err}` });
      busy.value = false;
      setStatus(false, "异常");
    }
    return;
  }
  pendingAttachments.value = [];
  inputText.value = "";
  nextTick(autoResize);
  addBubble({ type: "user", text: text || "我上传了图片/PDF", attachments });
  busy.value = true;
  setStatus(true, "正在回答");
  try {
    pendingSendId = await props.kernel.sendLine("send_user_message", { text, ...extra });
  } catch (err) {
    addBubble({ type: "error", text: `发送失败：${err}` });
    busy.value = false;
    setStatus(false, "异常");
  }
}

async function abortTurn() {
  await props.kernel.sendLine("abort");
}

async function pickHomework() {
  const picked = await props.kernel.pickHomeworkFile();
  if (!picked) return;
  addPendingAttachment(picked);
  setStatus(false, "已添加附件，可继续选择或直接输入内容后发送");
}

/** 附件挂起（选文件 / 粘截图共用）：不立即发送，输入区上方预览，发送时一起带上。 */
function addPendingAttachment(picked) {
  const item = {
    temp_path: picked.temp_path,
    asset_path: picked.asset_path,
    name: picked.name,
    preview: null,
  };
  pendingAttachments.value.push(item);
  attachmentUrl(picked.asset_path, picked.name)
    .then((p) => {
      if (pendingAttachments.value.some((a) => a.asset_path === picked.asset_path)) {
        item.preview = p;
      }
    })
    .catch(() => {});
  inputEl.value?.focus();
}

/** 剪贴板粘贴截图（Ctrl+V / 右键粘贴）：图片直接进入附件暂存，复用 vision::read → 判分管线。 */
async function onPaste(event) {
  const items = event.clipboardData?.items;
  if (!items || !items.length) return;
  let imageFile = null;
  for (let i = 0; i < items.length; i++) {
    const item = items[i];
    if (item.kind === "file" && item.type?.startsWith("image/")) {
      const f = item.getAsFile();
      if (f) {
        imageFile = f;
        break;
      }
    }
  }
  if (!imageFile) return; // 文本粘贴：不拦截，走默认行为
  event.preventDefault();
  await stageClipboardFile(imageFile);
}

async function stageClipboardFile(file) {
  try {
    const dataUrl = await readFileAsDataUrl(file);
    const m = /^data:([^;,]+);base64,(.*)$/s.exec(dataUrl);
    if (!m) throw new Error("无法解析粘贴的图片");
    const picked = await props.kernel.stageClipboardImage(m[1], m[2]);
    if (!picked) return;
    addPendingAttachment(picked);
    setStatus(false, "已粘贴截图，可继续添加或直接发送");
  } catch (e) {
    addBubble({ type: "error", text: `粘贴图片失败：${e.message}` });
  }
}

function readFileAsDataUrl(file) {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(reader.result);
    reader.onerror = () => reject(reader.error || new Error("读取剪贴板图片失败"));
    reader.readAsDataURL(file);
  });
}

function removePendingAttachment(index) {
  pendingAttachments.value.splice(index, 1);
}

const viewer = ref(null);
function openAttachment(attachment) {
  viewer.value = attachment;
}

function startEdit(bubble) {
  editingId.value = bubble.messageId;
}

async function saveEdit(text) {
  const id = editingId.value;
  editingId.value = null;
  if (!id || !text) return;
  busy.value = true;
  setStatus(true, "正在重新回答");
  try {
    await props.kernel.call("edit_message", { message_id: id, text });
    // 从服务端重读：编辑后的版本就地替换，其余对话保持不塌缩。
    await refreshAllHistory();
  } catch (e) {
    busy.value = false;
    setStatus(false, "就绪");
    addBubble({ type: "error", text: `编辑失败：${e.message}` });
  }
}

/** < / > 切换版本（DeepSeek 式）：本地改版本指针即时渲染；活跃会话同步服务端。 */
function switchBranch(bubble, dir = 1) {
  const view = bubble.sessionKey ? sessionViews.value[bubble.sessionKey] : null;
  if (!view) return;
  navigateBranch(view, bubble.messageId, dir);
  renderMergedBubbles();
  // 活跃会话：把新链末端同步给服务端，后续发送从所选版本继续。
  if (bubble.sessionKey === activeSessionKey.value) {
    const chain = getActiveChain(view);
    const end = chain.length ? String(chain[chain.length - 1].id) : null;
    if (end) {
      props.kernel
        .call("switch_branch", { message_id: end })
        .catch(() => {});
    }
  }
}

async function copyText(text) {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    // 剪贴板不可用时静默（不阻断主流程）。
  }
}

onMounted(async () => {
  unsubscribe = props.kernel.onFrame(handleFrame);
  loadTools();
  await ensureHistory();
  loadCacheStats();

  // 跨页面跳转：错题本"追问"带过来的消息
  if (navigateToChatMessage.value) {
    const payload = navigateToChatMessage.value;
    navigateToChatMessage.value = "";
    if (typeof payload === "object" && payload.action) {
      // 结构化导航：直接调用工具（如变式练习）
      await handleNavigatePayload(payload);
    } else {
      // 纯文本：作为聊天消息发送
      inputText.value = String(payload);
      await nextTick();
      sendMessage();
    }
  }
});
watch(
  () => props.ready,
  (v) => {
    if (v) ensureHistory();
    if (v) loadTools();
  },
);
onUnmounted(() => unsubscribe?.());
</script>

<template>
  <div class="chat-page" @paste="onPaste">
    <div class="chat-topbar">
      <button
        v-if="false"
        class="cache-chip"
        :title="cacheTitle"
        aria-label="聊天上下文缓存命中率"
        @click="loadCacheStats"
      >
        <Icon icon="mdi:database-sync-outline" width="15" />
        <span>上下文缓存命中 {{ cacheRateText }}</span>
      </button>
    </div>

    <AttachmentViewer
      v-if="viewer"
      :attachment="viewer"
      @close="viewer = null"
    />

    <main id="messages">
      <div v-if="!bubbles.length && !busy" class="empty chat-empty">
        <span class="empty-icon">
          <Icon icon="mdi:school-outline" width="36" />
        </span>
        <h2>开始你的学习吧</h2>
        <p>上传一份作业让 Agent 批改，或直接提问：错题会归档进错题本，跨会话记得住。</p>
        <div class="quick-actions">
          <button
            v-for="a in quickActions"
            :key="a.id"
            class="quick-card"
            @click="onQuickAction(a)"
          >
            <span class="quick-icon">
              <Icon :icon="a.icon" width="22" />
            </span>
            <span>
              <span class="quick-title">{{ a.label }}</span>
              <span class="quick-desc">{{ a.desc }}</span>
            </span>
            <Icon icon="mdi:chevron-right" width="18" class="quick-arrow" />
          </button>
        </div>
      </div>

      <TransitionGroup name="msg" tag="div" class="bubbles">
        <MessageBubble
          v-for="(b, i) in bubbles"
          :key="b.messageId || i"
          :bubble="b"
          :streaming="b.type === 'assistant' && currentStreamId && b.messageId === currentStreamId"
          :editing="editingId === b.messageId"
          @edit="startEdit"
          @switch-branch="switchBranch"
          @copy="copyText"
          @open-attachment="openAttachment"
          @save-edit="saveEdit"
          @cancel-edit="editingId = null"
          @tool-interact="onToolInteract"
        />
      </TransitionGroup>
    </main>

    <footer class="chat-footer">
      <Transition name="fade">
        <div v-if="toolStatus" class="tool-status">
          <span class="spinner" aria-hidden="true"></span>
          <Icon :icon="toolStatus.icon || 'mdi:toolbox-outline'" width="18" />
          <span>{{ toolStatus.entry }}：{{ toolStatus.message }}</span>
        </div>
      </Transition>

      <div v-if="tools.length && !busy" class="tool-bar" role="toolbar" aria-label="工具">
        <button
          v-for="t in visibleTools"
          :key="t.entry"
          class="tool-chip"
          :class="{ active: armedTool?.entry === t.entry }"
          :title="t.description"
          @click="pickTool(t)"
        >
          <Icon :icon="t.icon || 'mdi:toolbox-outline'" width="16" />
          <span>{{ t.title || t.entry }}</span>
        </button>
      </div>

      <div class="input-wrap">
        <Transition name="fade">
          <div
            v-if="suggestions.length"
            class="tool-suggest"
            role="listbox"
            aria-label="工具候选"
          >
            <button
              v-for="(t, i) in suggestions"
              :key="t.entry"
              class="tool-suggest-item"
              :class="{ active: i === activeSuggestion }"
              role="option"
              :aria-selected="i === activeSuggestion"
              @mousedown.prevent="armTool(t, true)"
            >
              <Icon :icon="t.icon || 'mdi:toolbox-outline'" width="16" />
              <span class="ts-title">{{ t.title || t.entry }}</span>
              <span class="ts-entry">{{ t.entry }}</span>
            </button>
            <p class="tool-suggest-hint">按 Tab 确认调用 · ↑↓ 选择 · 也可点选</p>
          </div>
        </Transition>

        <div v-if="overflowTools.length" class="overflow-floating">
          <Transition name="drop">
            <div v-if="overflowOpen" class="tool-overflow-menu" @mouseleave="closeOverflow">
              <button
                v-for="t in overflowTools"
                :key="t.entry"
                class="tool-overflow-item"
                :class="{ active: armedTool?.entry === t.entry }"
                :title="t.title + '：' + t.description"
                @click="pickTool(t); closeOverflow()"
              >
                <span class="tool-overflow-icon">
                  <Icon :icon="t.icon || 'mdi:toolbox-outline'" width="20" />
                </span>
                <span class="tool-overflow-title">{{ t.title || t.entry }}</span>
              </button>
            </div>
          </Transition>
        </div>

        <div v-if="pendingAttachments.length" class="pending-attach-bar">
          <div
            v-for="(a, i) in pendingAttachments"
            :key="a.asset_path"
            class="pending-attach"
          >
            <img
              v-if="a.preview?.kind === 'image'"
              class="pending-attach-thumb"
              :src="a.preview.url"
              alt="待发送附件"
            />
            <Icon v-else-if="a.preview?.kind === 'pdf'" icon="mdi:file-pdf-box" width="22" />
            <Icon v-else icon="mdi:file-outline" width="22" />
            <span class="pending-attach-name">{{ a.name }}</span>
            <button
              class="armed-tool-x"
              aria-label="移除附件"
              title="移除附件"
              @click="removePendingAttachment(i)"
            >
              <Icon icon="mdi:close" width="14" />
            </button>
          </div>
        </div>
        <div class="input-shell" :class="{ armed: armedTool }">
          <button
            v-if="overflowTools.length"
            class="input-plus-btn"
            :class="{ active: overflowOpen }"
            title="更多功能"
            @click="toggleOverflow"
          >
            <Icon icon="mdi:plus" width="20" />
          </button>
          <span v-if="armedTool" class="armed-tool">
            <Icon :icon="armedTool.icon" width="16" />
            <span>{{ armedTool.title }}</span>
            <button
              class="armed-tool-x"
              aria-label="取消工具调用"
              @click="unarmTool"
            >
              <Icon icon="mdi:close" width="14" />
            </button>
          </span>
          <textarea
            ref="inputEl"
            v-model="inputText"
            rows="1"
            :placeholder="armedTool ? '<可选参数>' : '发消息，或输入功能名（如：生成练习题）按 Tab 确认'"
            autocomplete="off"
            aria-label="消息输入框"
            @input="onInput"
            @keydown="onKeydown"
            @keydown.enter.exact.prevent="sendMessage"
          ></textarea>
          <span
            v-if="armedTool && !inputText.trim()"
            class="param-hint"
            aria-hidden="true"
          >&lt;可选参数&gt;</span>
          <button class="action-btn attach-btn" aria-label="选择图片/PDF" title="选择图片/PDF" @click="pickHomework()">
            <Icon icon="mdi:paperclip" width="18" />
          </button>
          <button v-if="!busy" class="action-btn send-btn" :disabled="!canSend" @click="sendMessage">
            <Icon icon="mdi:arrow-up" width="20" />
          </button>
          <button v-if="busy" class="action-btn stop-btn" @click="abortTurn">
            <Icon icon="mdi:stop-circle" width="18" />
          </button>
        </div>
      </div>
    </footer>
  </div>
</template>
