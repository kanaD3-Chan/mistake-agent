<script setup>
import { onMounted, reactive, ref } from "vue";
import { Icon } from "@iconify/vue";

const props = defineProps({ kernel: { type: Object, required: true } });

const loading = ref(true);
const saving = ref(false);
const error = ref("");
const saved = ref(false);
const balance = ref(null);
const balanceLoading = ref(false);
const balanceError = ref("");

const form = reactive({
  log_level: "info",
  english_mode: false,
  main: { api_url: "", model: "", transport: "responses", key_set: false, api_key: "" },
  vision: { api_url: "", model: "", transport: "", key_set: false, api_key: "" },
});

async function load() {
  loading.value = true;
  error.value = "";
  try {
    const v = await props.kernel.call("get_settings", {}, 10000);
    form.log_level = v.log_level || "info";
    form.english_mode = Boolean(v.english_mode);
    form.main.api_url = v.main_model?.api_url || "";
    form.main.model = v.main_model?.model || "";
    form.main.transport = v.main_model?.transport || "responses";
    form.main.key_set = Boolean(v.main_model?.key_set);
    form.main.api_key = "";
    form.vision.api_url = v.vision_model?.api_url || "";
    form.vision.model = v.vision_model?.model || "";
    form.vision.transport = v.vision_model?.transport || "";
    form.vision.key_set = Boolean(v.vision_model?.key_set);
    form.vision.api_key = "";
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

async function save() {
  saving.value = true;
  error.value = "";
  saved.value = false;
  const patch = {
    log_level: form.log_level,
    english_mode: form.english_mode,
    main_model: {
      api_url: form.main.api_url.trim(),
      api_key: form.main.api_key,
      model: form.main.model.trim(),
      transport: form.main.transport || null,
    },
    vision_model: {
      api_url: form.vision.api_url.trim(),
      api_key: form.vision.api_key,
      model: form.vision.model.trim(),
      transport: form.vision.transport || null,
    },
  };
  try {
    await props.kernel.call("set_settings", { patch }, 10000);
    saved.value = true;
    form.main.api_key = "";
    form.vision.api_key = "";
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

onMounted(() => {
  load();
  loadBalance();
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
          <span class="section-icon"><Icon icon="mdi:wallet-outline" width="18" /></span>账户余额
        </h3>
        <button
          class="btn ghost"
          :disabled="balanceLoading"
          :title="'刷新余额'"
          @click="loadBalance"
        >
          <Icon icon="mdi:refresh" width="18" :class="{ spin: balanceLoading }" />
          {{ balanceLoading ? "查询中…" : "刷新" }}
        </button>
      </div>

      <p v-if="balanceError" class="alert" role="alert">
        <Icon icon="mdi:alert-circle-outline" width="18" />{{ balanceError }}
      </p>
      <div v-else-if="balance" class="balance-grid">
        <div class="balance-item">
          <span class="balance-label">
            <Icon icon="mdi:robot-outline" width="16" />DeepSeek（主模型）
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
        <div class="balance-item">
          <span class="balance-label">
            <Icon icon="mdi:image-search-outline" width="16" />SiliconFlow（视觉模型）
          </span>
          <template v-if="!balance.vision?.configured">
            <span class="balance-value muted">
              <Icon icon="mdi:key-off-outline" width="16" />未配置密钥
            </span>
          </template>
          <template v-else-if="balance.vision?.ok">
            <span class="balance-value">
              {{ money(balance.vision.data.charge_balance, "CNY") }}
            </span>
            <span class="balance-note">
              充值余额（实际可用） · 赠送
              {{ money(balance.vision.data.balance, "CNY") }} · 总额
              {{ money(balance.vision.data.total_balance, "CNY") }}
            </span>
          </template>
          <span v-else class="balance-value error-text">
            <Icon icon="mdi:alert-circle-outline" width="16" />{{ balance.vision.error }}
          </span>
        </div>
      </div>
      <div v-else class="empty">
        <Icon icon="mdi:loading" width="24" class="spin" />
        <p>正在查询余额…</p>
      </div>
    </section>

    <div v-if="loading" class="empty">
      <Icon icon="mdi:loading" width="28" class="spin" />
      <p>正在读取设置…</p>
    </div>

    <form v-else class="settings-form" @submit.prevent="save">
      <section class="card">
        <h3><span class="section-icon"><Icon icon="mdi:tune-variant" width="18" /></span>通用</h3>
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
        <h3><span class="section-icon"><Icon icon="mdi:robot-outline" width="18" /></span>主模型（对话与调度）</h3>
        <label class="field">
          <span>API 地址</span>
          <input v-model="form.main.api_url" type="url" required placeholder="https://api.deepseek.com" />
        </label>
        <label class="field">
          <span>模型 ID</span>
          <input v-model="form.main.model" placeholder="deepseek-v4-flash" />
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

      <section class="card">
        <h3><span class="section-icon"><Icon icon="mdi:image-search-outline" width="18" /></span>视觉模型（OCR / 图片理解）</h3>
        <label class="field">
          <span>API 地址</span>
          <input v-model="form.vision.api_url" type="url" required placeholder="https://api.siliconflow.cn/v1" />
        </label>
        <label class="field">
          <span>模型 ID</span>
          <input v-model="form.vision.model" placeholder="Qwen/Qwen3-VL-32B-Instruct" />
        </label>
        <label class="field">
          <span>接入方式</span>
          <select v-model="form.vision.transport">
            <option value="">Chat Completions（默认，支持图片）</option>
            <option value="responses">Responses API</option>
          </select>
        </label>
        <label class="field">
          <span>API Key</span>
          <input
            v-model="form.vision.api_key"
            type="password"
            autocomplete="off"
            :placeholder="form.vision.key_set ? '已配置（留空表示不修改）' : '未配置，请输入'"
          />
          <small v-if="form.vision.key_set">当前已配置密钥，出于安全不在此回显。</small>
          <small v-else>留空也可先体验，上传作业批改需要视觉模型密钥。</small>
        </label>
      </section>
    </form>
  </div>
</template>
