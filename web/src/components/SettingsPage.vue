<script setup>
import { computed, onMounted, reactive, ref, watch } from "vue";
import { Icon } from "@iconify/vue";

const props = defineProps({ kernel: { type: Object, required: true } });
// 保存成功后通知外面（App 用它刷新侧栏左下角的称呼）。
const emit = defineEmits(["saved"]);

const loading = ref(true);
const saving = ref(false);
const error = ref("");
const saved = ref(false);
const balance = ref(null);
const balanceLoading = ref(false);
const balanceError = ref("");
const rules = ref(null);
const rulesError = ref("");
const rulesOpening = ref(false);
// 已登录平台服务时，下面那张卡的 DeepSeek 余额与学生无关（他花的是平台额度）。
const accountLoggedIn = ref(false);

const form = reactive({
  log_level: "info",
  english_mode: false,
  nickname: "",
  main: { api_url: "", model: "", transport: "responses", key_set: false, api_key: "" },
});

async function load() {
  loading.value = true;
  error.value = "";
  try {
    const v = await props.kernel.call("get_settings", {}, 10000);
    form.log_level = v.log_level || "info";
    form.english_mode = Boolean(v.english_mode);
    form.nickname = v.nickname || "";
    form.main.api_url = v.main_model?.api_url || "";
    form.main.model = v.main_model?.model || "";
    form.main.transport = v.main_model?.transport || "responses";
    form.main.key_set = Boolean(v.main_model?.key_set);
    form.main.api_key = "";
    accountLoggedIn.value = Boolean(v.account?.logged_in);
  } catch (e) {
    error.value = `读取设置失败：${e.message}`;
  } finally {
    loading.value = false;
  }
}

async function loadBalance() {
  balanceLoading.value = true;
  balanceError.value = "";
  try {
    balance.value = await props.kernel.call("check_balance", {}, 20000);
  } catch (e) {
    balanceError.value = `余额查询失败：${e.message}`;
  } finally {
    balanceLoading.value = false;
  }
}

const RULES_REASON_TEXT = {
  missing: "文件缺失或不可读（首次启动应已自动生成，可点击下方按钮检查）",
  too_large: "文件过大（超过 64KB 上限），已回退默认规则",
  invalid_utf8: "文件编码异常（需保存为 UTF-8），已回退默认规则",
};

async function loadRulesStatus() {
  rulesError.value = "";
  try {
    rules.value = await props.kernel.call("get_rules_status", {}, 10000);
  } catch (e) {
    rulesError.value = `读取规则状态失败：${e.message}`;
  }
}

async function openRulesFile() {
  rulesOpening.value = true;
  rulesError.value = "";
  try {
    await props.kernel.openRulesFile();
  } catch (e) {
    rulesError.value = `打开规则文件失败：${e.message}`;
  } finally {
    rulesOpening.value = false;
  }
}

async function save() {
  saving.value = true;
  error.value = "";
  saved.value = false;
  const patch = {
    log_level: form.log_level,
    english_mode: form.english_mode,
    // 空串 = 清空昵称（回到默认称呼），与 api_key 的「空串=保留」不同。
    nickname: form.nickname.trim(),
    main_model: {
      api_url: form.main.api_url.trim(),
      api_key: form.main.api_key,
      model: form.main.model.trim(),
      transport: form.main.transport || null,
    },
  };
  try {
    await props.kernel.call("set_settings", { patch }, 10000);
    saved.value = true;
    form.main.api_key = "";
    emit("saved");
    await load();
    await loadBalance();
  } catch (e) {
    error.value = `保存失败：${e.message}`;
  } finally {
    saving.value = false;
  }
}

function money(symbol, currency) {
  return currency === "CNY" ? `¥${symbol}` : `${symbol} ${currency || ""}`.trim();
}

// ── 平台额度（登录态那张卡，ADR-0048 修订 R10）──
// 数据源是服务端 `GET /api/v1/me/quota`（5 小时 / 7 天 / 30 天三个滑动窗口）。
// 登录后**不再显示与平台无关的 DeepSeek 余额**：那是学生自己的账，与积分卡无关。
const quota = ref(null);
const quotaLoading = ref(false);
const quotaError = ref("");

const QUOTA_REASON_TEXT = {
  not_logged_in: "未登录平台账号。",
  token_invalid: "登录已失效，请重新登录平台账号。",
  unreachable: "连不上平台服务，稍后可点「刷新」重试。",
  server_error: "平台服务暂时不可用，稍后可点「刷新」重试。",
};
const quotaReasonText = computed(
  () => QUOTA_REASON_TEXT[quota.value?.reason] || "额度暂时取不到。",
);

const WINDOW_LABELS = { five_hour: "近 5 小时", week: "近 7 天", month: "近 30 天" };
function windowLabel(key) {
  return WINDOW_LABELS[key] || key;
}

// 使用百分比 = 已用 / 上限。不限次数的窗口返回 0（页面显示「不限」而不是一条空进度条）。
function windowPercent(w) {
  if (!w || !w.limit) return 0;
  return Math.max(0, Math.min(100, Math.round((w.used / w.limit) * 100)));
}

// 重置时刻按**本地时间**渲染：服务端给的是滑动窗口的 UTC 时刻，不是自然日整点。
function formatMoment(iso) {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "";
  const pad = (n) => String(n).padStart(2, "0");
  return `${d.getMonth() + 1}月${d.getDate()}日 ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

async function loadQuota() {
  quotaLoading.value = true;
  quotaError.value = "";
  try {
    quota.value = await props.kernel.call("get_account_quota", {}, 20000);
  } catch (e) {
    quotaError.value = `额度查询失败：${e.message}`;
  } finally {
    quotaLoading.value = false;
  }
}

// 登录状态变化（门禁里刚登录 / 侧栏登出）时切换卡片的取数目标。
watch(accountLoggedIn, (loggedIn) => {
  if (loggedIn) loadQuota();
});

onMounted(async () => {
  await load();
  loadRulesStatus();
  // 登录态只查平台额度：DeepSeek 余额与学生无关，那次请求也不必再发。
  if (accountLoggedIn.value) loadQuota();
  else loadBalance();
});
</script>

<template>
  <div class="page settings-page">
    <div class="page-head">
      <h2>设置</h2>
      <button class="btn primary" :disabled="saving || loading" @click="save">
        <Icon icon="mdi:content-save" width="18" />{{ saving ? "保存中…" : "保存设置" }}
      </button>
    </div>

    <p v-if="error" class="alert" role="alert">
      <Icon icon="mdi:alert-circle-outline" width="18" />{{ error }}
    </p>
    <p v-if="saved" class="alert success" role="status">
      <Icon icon="mdi:check-circle-outline" width="18" />已保存，模型配置即时生效。
    </p>

    <section class="card balance-card">
      <div class="balance-head">
        <h3>
          <span class="section-icon"><Icon icon="mdi:wallet-outline" width="18" /></span>
          {{ accountLoggedIn ? "平台额度" : "账户余额" }}
        </h3>
        <button
          class="btn ghost"
          :disabled="accountLoggedIn ? quotaLoading : balanceLoading"
          :title="accountLoggedIn ? '刷新额度' : '刷新余额'"
          @click="accountLoggedIn ? loadQuota() : loadBalance()"
        >
          <Icon
            icon="mdi:refresh"
            width="18"
            :class="{ spin: accountLoggedIn ? quotaLoading : balanceLoading }"
          />
          {{ (accountLoggedIn ? quotaLoading : balanceLoading) ? "查询中…" : "刷新" }}
        </button>
      </div>

      <!-- 登录态：平台额度的三个滑动窗口（ADR-0048 修订 R10） -->
      <template v-if="accountLoggedIn">
        <p v-if="quotaError" class="alert" role="alert">
          <Icon icon="mdi:alert-circle-outline" width="18" />{{ quotaError }}
        </p>
        <p v-else-if="quota && quota.reason" class="balance-note">
          <Icon icon="mdi:information-outline" width="16" />{{ quotaReasonText }}
        </p>
        <p v-else-if="quota && !quota.has_entitlement" class="balance-note">
          <Icon icon="mdi:ticket-confirmation-outline" width="16" />
          还没有可用的服务包——兑换后即可使用平台额度。
        </p>
        <div v-else-if="quota" class="quota-grid">
          <div v-for="w in quota.windows" :key="w.key" class="quota-item">
            <div class="quota-row">
              <span class="quota-label">{{ windowLabel(w.key) }}</span>
              <span class="quota-count">
                {{ w.used }}<template v-if="w.limit"> / {{ w.limit }}</template>
                <span v-if="w.limit" class="quota-pct">{{ windowPercent(w) }}%</span>
              </span>
            </div>
            <div
              class="quota-bar"
              role="progressbar"
              :aria-valuenow="windowPercent(w)"
              aria-valuemin="0"
              aria-valuemax="100"
              :aria-label="windowLabel(w.key)"
            >
              <div
                class="quota-fill"
                :class="{ warn: windowPercent(w) >= 80 }"
                :style="{ width: windowPercent(w) + '%' }"
              ></div>
            </div>
            <span class="quota-hint">
              <template v-if="w.limit">
                剩余 {{ w.remaining }} 次<template v-if="w.resets_at"
                  >（{{ formatMoment(w.resets_at) }} 起恢复）</template
                >
              </template>
              <template v-else>不限次数</template>
            </span>
          </div>
          <p v-if="quota.plan" class="balance-note">
            <Icon icon="mdi:card-account-details-outline" width="16" />
            当前套餐：{{ quota.plan.name }}
            <template v-if="quota.entitlement?.expires_at">
              （{{ formatMoment(quota.entitlement.expires_at) }} 到期）
            </template>
          </p>
        </div>
        <div v-else class="empty">
          <Icon icon="mdi:loading" width="24" class="spin" />
          <p>正在查询额度…</p>
        </div>
      </template>

      <!-- 未登录：保持原有的自备 Key 余额卡片 -->
      <template v-else>
        <p v-if="balanceError" class="alert" role="alert">
          <Icon icon="mdi:alert-circle-outline" width="18" />{{ balanceError }}
        </p>
        <div v-else-if="balance" class="balance-grid">
          <div class="balance-item">
            <span class="balance-label">
              <Icon icon="mdi:robot-outline" width="16" />DeepSeek
            </span>
            <template v-if="!balance.main?.configured">
              <span class="balance-value muted">
                <Icon icon="mdi:key-off-outline" width="16" />未配置密钥
              </span>
            </template>
            <template v-else-if="balance.main?.ok">
              <span class="balance-value">
                {{ money(balance.main.data.total_balance, balance.main.data.currency) }}
              </span>
              <span class="balance-note">
                可用：
                <Icon
                  v-if="balance.main.data.is_available"
                  icon="mdi:check-circle-outline"
                  width="14"
                />
                <Icon v-else icon="mdi:alert-circle-outline" width="14" />
                {{ balance.main.data.is_available ? "是" : "否" }}
              </span>
            </template>
            <span v-else class="balance-value error-text">
              <Icon icon="mdi:alert-circle-outline" width="16" />{{ balance.main.error }}
            </span>
          </div>
        </div>
        <div v-else class="empty">
          <Icon icon="mdi:loading" width="24" class="spin" />
          <p>正在查询余额…</p>
        </div>
      </template>
    </section>

    <div v-if="loading" class="empty">
      <Icon icon="mdi:loading" width="28" class="spin" />
      <p>正在读取设置…</p>
    </div>

    <form v-else class="settings-form" @submit.prevent="save">
      <section class="card">
        <h3><span class="section-icon"><Icon icon="mdi:tune-variant" width="18" /></span>通用</h3>
        <label class="field">
          <span>昵称</span>
          <input
            v-model="form.nickname"
            type="text"
            maxlength="24"
            placeholder="同学"
            autocomplete="off"
          />
          <small>显示在侧栏左下角；留空则用「同学」。</small>
        </label>
        <label class="field">
          <span>日志级别</span>
          <select v-model="form.log_level">
            <option value="debug">DEBUG</option>
            <option value="info">INFO</option>
            <option value="warn">WARN</option>
            <option value="error">ERROR</option>
            <option value="critical">CRITICAL</option>
          </select>
          <small>级别越高输出越少；排障时用 DEBUG。</small>
        </label>
        <div class="field toggle-field">
          <div class="toggle-row">
            <span>英语练习模式</span>
            <label class="switch">
              <input v-model="form.english_mode" type="checkbox" />
              <span class="slider"></span>
            </label>
          </div>
          <small>开启后模型对话、判分、出题与复盘统一使用英文；界面文字保持中文。</small>
        </div>
      </section>

      <section class="card">
        <h3><span class="section-icon"><Icon icon="mdi:file-document-edit-outline" width="18" /></span>教学规则（AGENTS.md）</h3>
        <p v-if="rulesError" class="alert" role="alert">
          <Icon icon="mdi:alert-circle-outline" width="18" />{{ rulesError }}
        </p>
        <template v-else-if="rules">
          <p class="rules-status" :class="rules.loaded ? 'ok' : 'warn'">
            <Icon
              v-if="rules.loaded"
              icon="mdi:check-circle-outline"
              width="18"
            />
            <Icon v-else icon="mdi:alert-circle-outline" width="18" />
            {{ rules.loaded ? "规则已加载：Agent 将按此文件辅导学生" : "规则未加载（已回退默认规则）" }}
          </p>
          <p v-if="!rules.loaded && rules.reason" class="rules-reason">
            {{ RULES_REASON_TEXT[rules.reason] || `未知原因：${rules.reason}` }}
          </p>
          <p v-else-if="rules.loaded && rules.bytes" class="rules-reason">
            当前规则 {{ rules.bytes }} 字节，保存后即时生效。
          </p>
          <p class="rules-path">{{ rules.path }}</p>
          <button class="btn ghost" :disabled="rulesOpening" @click="openRulesFile">
            <Icon icon="mdi:open-in-new" width="18" />
            {{ rulesOpening ? "正在打开…" : "打开规则文件编辑" }}
          </button>
        </template>
        <div v-else class="empty">
          <Icon icon="mdi:loading" width="24" class="spin" />
          <p>正在读取规则状态…</p>
        </div>
      </section>

      <section class="card">
        <h3><span class="section-icon"><Icon icon="mdi:robot-outline" width="18" /></span>DeepSeek 模型（对话 / 调度 / 图片理解）</h3>
        <label class="field">
          <span>API 地址</span>
          <input v-model="form.main.api_url" type="url" required placeholder="https://api.deepseek.com" />
        </label>
        <label class="field">
          <span>模型 ID</span>
          <input v-model="form.main.model" placeholder="deepseek-flash" />
        </label>
        <label class="field">
          <span>接入方式</span>
          <select v-model="form.main.transport">
            <option value="responses">Responses API（DeepSeek 官方）</option>
            <option value="chat_completions">Chat Completions（OpenAI 兼容）</option>
          </select>
        </label>
        <label class="field">
          <span>API Key</span>
          <input
            v-model="form.main.api_key"
            type="password"
            autocomplete="off"
            :placeholder="form.main.key_set ? '已配置（留空表示不修改）' : '未配置，请输入'"
          />
          <small v-if="form.main.key_set">当前已配置密钥，出于安全不在此回显。</small>
          <small v-else>密钥只保存在本机 settings.json，不会发送到其它地方。</small>
        </label>
      </section>
    </form>
  </div>
</template>
