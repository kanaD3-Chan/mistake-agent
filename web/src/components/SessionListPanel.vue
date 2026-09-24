<script setup>
import { onMounted, ref } from "vue";
import { Icon } from "@iconify/vue";

const props = defineProps({
  kernel: { type: Object, required: true },
  activeKey: { type: String, default: null },
  busy: { type: Boolean, default: false },
});
const emit = defineEmits(["select", "changed"]);

const sessions = ref([]);
const loading = ref(false);
const error = ref("");
const renamingKey = ref(null);
const renameText = ref("");
const confirmKey = ref(null);
const working = ref(false);

/** 标题回退链：模型标题 → 学习目标 → 「新会话」（后端已保证旧数据 title 缺省）。 */
function sessionTitle(s) {
  const title = (s?.title || "").trim();
  if (title) return title;
  const goal = typeof s?.goal === "string" ? s.goal : s?.goal?.text;
  return (goal || "").trim() || "新会话";
}

/** 相对时间：刚刚 / N 分钟前 / N 小时前 / 昨天 / M-D。 */
function fmtRelative(iso) {
  if (!iso) return "";
  const t = new Date(iso).getTime();
  if (Number.isNaN(t)) return "";
  const diff = Date.now() - t;
  const min = Math.floor(diff / 60000);
  if (min < 1) return "刚刚";
  if (min < 60) return `${min} 分钟前`;
  const hour = Math.floor(min / 60);
  if (hour < 24) return `${hour} 小时前`;
  const day = Math.floor(hour / 24);
  if (day === 1) return "昨天";
  if (day < 30) return `${day} 天前`;
  const d = new Date(t);
  return `${d.getMonth() + 1}-${d.getDate()}`;
}

async function refreshList() {
  loading.value = true;
  error.value = "";
  try {
    const r = await props.kernel.call("list_sessions", {}, 10000);
    sessions.value = (r.sessions || [])
      .slice()
      .sort(
        (a, b) =>
          new Date(b.last_activity_at || b.created_at || 0) -
          new Date(a.last_activity_at || a.created_at || 0),
      );
  } catch (e) {
    error.value = e.code === "not_implemented" ? "会话接口尚未接通" : `加载失败：${e.message}`;
    sessions.value = [];
  } finally {
    loading.value = false;
  }
}

async function newSession() {
  if (props.busy || working.value) return;
  working.value = true;
  try {
    const r = await props.kernel.call("create_session", { carry_summary: false }, 20000);
    await refreshList();
    emit("changed");
    emit("select", r.session_key);
  } catch (e) {
    error.value = `新建会话失败：${e.message}`;
  } finally {
    working.value = false;
  }
}

function select(key) {
  if (key === props.activeKey) return;
  emit("select", key);
}

function startRename(s) {
  renamingKey.value = s.key;
  renameText.value = sessionTitle(s);
}

function cancelRename() {
  renamingKey.value = null;
  renameText.value = "";
}

async function saveRename() {
  const key = renamingKey.value;
  const title = renameText.value.trim();
  if (!key) return;
  renamingKey.value = null;
  if (!title) return;
  try {
    await props.kernel.call("rename_session", { key, title }, 10000);
    await refreshList();
    emit("changed");
  } catch (e) {
    error.value = `重命名失败：${e.message}`;
  }
}

async function doDelete() {
  const key = confirmKey.value;
  confirmKey.value = null;
  if (!key) return;
  try {
    const r = await props.kernel.call("delete_session", { key }, 20000);
    await refreshList();
    emit("changed");
    // 删掉的是当前会话：后端补建了空会话，切过去（否则聊天区停在已删除会话上）。
    if (r.replacement_session_key) emit("select", r.replacement_session_key);
    else if (key === props.activeKey && sessions.value.length) {
      emit("select", sessions.value[0].key);
    }
  } catch (e) {
    error.value = `删除失败：${e.message}`;
  }
}

defineExpose({ refreshList, newSession });
onMounted(refreshList);
</script>

<template>
  <aside class="session-panel">
    <p v-if="error" class="session-panel-error" role="alert">{{ error }}</p>

    <div class="session-panel-list">
      <p v-if="loading && !sessions.length" class="muted session-panel-empty">正在读取…</p>
      <p v-else-if="!sessions.length" class="muted session-panel-empty">还没有会话。</p>

      <div
        v-for="s in sessions"
        :key="s.key"
        class="session-item"
        :class="{ active: s.key === activeKey }"
      >
        <template v-if="renamingKey === s.key">
          <input
            v-model="renameText"
            class="edit-inline session-rename"
            aria-label="会话名称"
            @keydown.enter.prevent="saveRename"
            @keydown.esc="cancelRename"
          />
          <div class="session-item-actions">
            <button class="icon-btn" title="取消" @click="cancelRename">
              <Icon icon="mdi:close" width="16" />
            </button>
            <button class="icon-btn" title="保存" @click="saveRename">
              <Icon icon="mdi:check" width="16" />
            </button>
          </div>
        </template>

        <template v-else>
          <button class="session-item-main" :title="sessionTitle(s)" @click="select(s.key)">
            <span class="session-item-title">{{ sessionTitle(s) }}</span>
            <span class="session-item-time muted">{{ fmtRelative(s.last_activity_at) }}</span>
          </button>
          <div class="session-item-actions">
            <button
              class="icon-btn"
              aria-label="重命名会话"
              title="重命名"
              :disabled="busy"
              @click="startRename(s)"
            >
              <Icon icon="mdi:pencil-outline" width="16" />
            </button>
            <button
              class="icon-btn"
              aria-label="删除会话"
              title="删除"
              :disabled="busy"
              @click="confirmKey = s.key"
            >
              <Icon icon="mdi:trash-can-outline" width="16" />
            </button>
          </div>
        </template>
      </div>
    </div>

    <Teleport to="body">
      <div v-if="confirmKey" class="confirm-overlay" @click.self="confirmKey = null">
        <div class="confirm-dialog card">
          <p>
            <Icon icon="mdi:alert-outline" width="20" />
            删除这条会话？该会话的消息将不可恢复。
          </p>
          <div class="confirm-actions">
            <button class="btn ghost" @click="confirmKey = null">取消</button>
            <button class="btn danger" @click="doDelete">确认删除</button>
          </div>
        </div>
      </div>
    </Teleport>
  </aside>
</template>
