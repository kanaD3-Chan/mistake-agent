<script setup>
import { computed, ref, watch } from "vue";
import { Icon } from "@iconify/vue";
import PracticeQuestion from "./PracticeQuestion.vue";
import WeakPointList from "./WeakPointList.vue";

const props = defineProps({
  bubble: { type: Object, required: true },
  streaming: { type: Boolean, default: false },
  editing: { type: Boolean, default: false },
});

const emit = defineEmits([
  "edit",
  "switch-branch",
  "copy",
  "open-attachment",
  "save-edit",
  "cancel-edit",
  "tool-interact",
]);

const editText = ref("");
watch(
  () => props.editing,
  (on) => {
    if (on) editText.value = props.bubble.text || "";
  },
);

function saveEdit() {
  const text = editText.value.trim();
  if (!text) return;
  emit("save-edit", text);
}

function attachmentIcon(att) {
  if (/\.pdf$/i.test(att.name || att.path)) return "mdi:file-pdf-box";
  if (/\.(png|jpe?g|webp|bmp)$/i.test(att.name || att.path)) return "mdi:image";
  return "mdi:file-outline";
}

/* ──── 工具结果辅助 ──── */

/** 从 practice::gaps 结果中提取薄弱点数组（兼容数组/包裹对象）。 */
function extractWeakPoints(result) {
  if (!result) return [];
  if (Array.isArray(result)) return result;
  for (const k of ["gaps", "weaknesses", "points", "items"]) {
    if (Array.isArray(result[k])) return result[k];
  }
  return [];
}

/** 检测结果是否像薄弱点数据 */
function isGapsResult(bubble) {
  const e = (bubble.entry || "").toLowerCase();
  if (e === "practice::gaps" || e.includes("gaps")) return true;
  if (bubble.result && extractWeakPoints(bubble.result).length) return true;
  return false;
}

/** 检测结果是否像练习题数据 */
function isPracticeQuestionResult(bubble) {
  const e = (bubble.entry || "").toLowerCase();
  if (e === "practice::generate" || e.includes("generate")) return true;
  const r = bubble.result;
  if (!r) return false;
  // 按数据结构判断：有 question_text 的就是练习题
  if (r.question_text || r.item?.question_text) return true;
  return false;
}

/** 从 bubble 中提取知识点名 */
function getKnowledgePoint(bubble) {
  if (bubble.params?.knowledge_point) return bubble.params.knowledge_point;
  const r = bubble.result;
  if (!r) return "";
  if (r.knowledge_point) return r.knowledge_point;
  if (r.item?.knowledge_point) return r.item.knowledge_point;
  return "";
}

/** 提取练习题 item（兼容 item 包裹和直接字段） */
function getQuestionItem(bubble) {
  const r = bubble.result;
  if (!r) return null;
  if (r.item) return r.item;
  if (r.question_text) return r;
  return null;
}

/** 工具气泡是否渲染结果内容：仅练习卡片 / 薄弱点列表等交互组件，通用 JSON/Markdown 详情不再展示。 */
const hasToolBody = computed(() => {
  const r = props.bubble;
  if (!r.result || r.toolOk === false) return false;
  return isGapsResult(r) || isPracticeQuestionResult(r);
});
</script>

<template>
  <div class="bubble-row" :class="bubble.type">
    <details v-if="bubble.type === 'reasoning'" class="reasoning">
      <summary>
        <Icon icon="mdi:brain" width="18" />
        思考过程（点击折叠）
      </summary>
      <div class="reasoning-body">{{ bubble.text }}</div>
    </details>

    <!-- 会话边界：上一会话梗概（迁移后每条拆分会话的首条消息），折叠展示。 -->
    <details v-else-if="bubble.type === 'divider'" class="session-divider">
      <summary>
        <Icon icon="mdi:content-cut" width="16" />
        {{ bubble.title }}（点击展开）
      </summary>
      <div class="session-divider-body">{{ bubble.text }}</div>
    </details>

    <div
      v-else
      class="bubble"
      :class="[bubble.type, { streaming }]"
      :data-message-id="bubble.messageId"
    >
      <div
        v-if="bubble.type === 'assistant'"
        class="md-body"
        v-html-smiles="bubble.text"
      ></div>
      <div v-else-if="bubble.type === 'system'" class="md-body system-note">{{ bubble.text }}</div>
      <div v-else-if="bubble.type === 'tool'" class="tool-card">
        <div class="tool-card-head">
          <Icon v-if="bubble.toolIcon" :icon="bubble.toolIcon" width="18" />
          <span class="tool-entry">{{ bubble.title || bubble.entry }}</span>
          <span
            class="tool-result"
            :class="{ ok: bubble.toolOk, fail: bubble.toolOk === false }"
          >
            {{ bubble.toolOk === true ? "完成" : bubble.toolOk === false ? "失败" : "进行中" }}
          </span>
        </div>

        <!-- 工具结果交互式内容（通用 JSON/Markdown 详情不渲染，只保留状态徽章） -->
        <div v-if="hasToolBody" class="tool-card-body">
          <!-- practice::gaps → 薄弱点列表 -->
          <WeakPointList
            v-if="isGapsResult(bubble) && extractWeakPoints(bubble.result).length"
            :points="extractWeakPoints(bubble.result)"
            @generate="(p) => emit('tool-interact', { action: 'generate', ...p })"
          />
          <!-- practice::gaps 结果为空 -->
          <p v-else-if="isGapsResult(bubble)" class="muted" style="text-align:center;padding:12px 0;">
            <Icon icon="mdi:emoticon-happy-outline" width="18" /> 近期没有发现薄弱知识点，继续保持！
          </p>

          <!-- practice::generate 成功 → 练习卡片 -->
          <PracticeQuestion
            v-else-if="isPracticeQuestionResult(bubble) && getQuestionItem(bubble)"
            :item="getQuestionItem(bubble)"
            :knowledge-point="getKnowledgePoint(bubble)"
            @practice-again="(p) => emit('tool-interact', { action: 'practice-again', ...p })"
          />
          <!-- practice::generate 未命中 → 展示后端 message -->
          <div
            v-else-if="isPracticeQuestionResult(bubble)"
            class="tool-unmatched"
          >
            <Icon icon="mdi:information-outline" width="18" />
            <span>{{ bubble.result.message || "暂未找到合适的题目，换个知识点试试。" }}</span>
          </div>

        </div>
      </div>
      <template v-else>
        <template v-if="editing">
          <textarea
            v-model="editText"
            class="edit-inline"
            rows="3"
            aria-label="编辑消息内容"
            @keydown.esc="emit('cancel-edit')"
            @keydown.ctrl.enter="saveEdit()"
          ></textarea>
          <div class="edit-actions">
            <button class="btn ghost" @click="emit('cancel-edit')">取消</button>
            <button class="btn primary" :disabled="!editText.trim()" @click="saveEdit">
              保存并派生新分支
            </button>
          </div>
        </template>
        <template v-else>
          <Icon v-if="bubble.toolIcon" :icon="bubble.toolIcon" width="16" class="user-tool-icon" />
          <span>{{ bubble.text }}</span>
          <div v-if="bubble.attachments?.length" class="bubble-attachments">
            <button
              v-for="att in bubble.attachments"
              :key="att.path"
              class="attachment-chip"
              :aria-label="`查看附件 ${att.name}`"
              :title="att.name"
              @click="emit('open-attachment', att)"
            >
              <Icon :icon="attachmentIcon(att)" width="16" />
              <span class="attachment-chip-name">{{ att.name }}</span>
            </button>
          </div>
        </template>
      </template>
    </div>

    <div class="bubble-actions">
      <button
        v-if="bubble.type === 'user' && bubble.messageId && !editing"
        class="icon-btn"
        aria-label="编辑这条消息"
        title="编辑"
        @click="emit('edit', bubble)"
      >
        <Icon icon="mdi:pencil-outline" />
      </button>
      <span v-if="bubble.versionCount > 1" class="version-nav">
        <button
          class="icon-btn"
          aria-label="上一个版本"
          title="查看上一个版本"
          @click="emit('switch-branch', bubble, -1)"
        >
          <Icon icon="mdi:chevron-left" />
        </button>
        <span class="version-count" aria-hidden="true">
          {{ bubble.versionIndex + 1 }}/{{ bubble.versionCount }}
        </span>
        <button
          class="icon-btn"
          aria-label="下一个版本"
          title="查看下一个版本"
          @click="emit('switch-branch', bubble, 1)"
        >
          <Icon icon="mdi:chevron-right" />
        </button>
      </span>
      <button
        v-if="bubble.type === 'assistant'"
        class="icon-btn"
        aria-label="复制回答"
        title="复制"
        @click="emit('copy', bubble.text)"
      >
        <Icon icon="mdi:content-copy" />
      </button>
    </div>
  </div>
</template>
