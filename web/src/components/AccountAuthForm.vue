<script setup>
import { computed, ref } from "vue";
import { Icon } from "@iconify/vue";

const props = defineProps({
  kernel: { type: Object, required: true },
  /** 当前设置里的平台服务地址（用于判断用户是否改过）。 */
  serverUrl: { type: String, default: "" },
  /** 只有首屏门禁才让改地址：进来之后再改地址要重新登录，入口收在一处。 */
  showServerUrl: { type: Boolean, default: false },
  initialEmail: { type: String, default: "" },
  initialMode: { type: String, default: "login" },
});
const emit = defineEmits(["authed"]);

const mode = ref(props.initialMode);
const email = ref(props.initialEmail);
const password = ref("");
const displayName = ref("");
const serverUrl = ref(props.serverUrl);
const busy = ref(false);
const error = ref("");
const notice = ref("");

const isRegister = computed(() => mode.value === "register");
const submitLabel = computed(() => {
  if (busy.value) return isRegister.value ? "注册中…" : "登录中…";
  return isRegister.value ? "注册" : "登录";
});

/* 故意不写原生 `required`/`type="email"`：浏览器自带校验会先说英文、并和内核返回的
 * 中文文案（"邮箱与口令都不能为空"）打架，同一个按钮点出两种提示。空值与格式一律
 * 让服务端说——它的 message 本来就是中文（docs/server-api.md §1）。 */
function switchMode(next) {
  if (mode.value === next || busy.value) return;
  mode.value = next;
  error.value = "";
  notice.value = "";
}

/** 地址只在**改过**时才落盘，免得每次点登录都写一遍 settings.json。 */
async function persistServerUrl() {
  const next = serverUrl.value.trim();
  if (!props.showServerUrl) return;
  if (next === (props.serverUrl || "").trim()) return;
  await props.kernel.call("set_settings", { patch: { account: { server_url: next } } }, 10000);
}

async function submit() {
  if (busy.value) return;
  error.value = "";
  notice.value = "";
  busy.value = true;
  try {
    await persistServerUrl();
    if (isRegister.value) {
      await props.kernel.call(
        "register",
        {
          email: email.value.trim(),
          password: password.value,
          display_name: displayName.value.trim() || null,
        },
        25000,
      );
      // 注册成功**不自动登录**（服务端不签发令牌）：切到登录态、留住邮箱，下一步就是登录。
      mode.value = "login";
      password.value = "";
      displayName.value = "";
      notice.value = "注册成功，请用刚才的邮箱登录。";
      return;
    }
    const view = await props.kernel.call(
      "login",
      { email: email.value.trim(), password: password.value },
      25000,
    );
    password.value = "";
    emit("authed", view);
  } catch (e) {
    // 服务端的文案已是面向用户的中文，原样展示；只有"该邮箱已注册"顺手把用户
    // 带到登录态——那是最常见的一次性错误，少让他点一次。
    error.value = e.message || "操作失败，请重试";
    if (e.code === "email_taken") mode.value = "login";
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <form class="auth-form" @submit.prevent="submit">
    <div class="segmented" role="tablist" aria-label="登录或注册">
      <button
        type="button"
        role="tab"
        :class="{ active: !isRegister }"
        :aria-selected="!isRegister"
        @click="switchMode('login')"
      >
        登录
      </button>
      <button
        type="button"
        role="tab"
        :class="{ active: isRegister }"
        :aria-selected="isRegister"
        @click="switchMode('register')"
      >
        注册
      </button>
    </div>

    <label v-if="showServerUrl" class="field">
      <span>平台服务地址</span>
      <input
        v-model="serverUrl"
        type="text"
        inputmode="url"
        autocomplete="off"
        spellcheck="false"
        placeholder="http://8.131.146.250:8080"
      />
      <small>模型调用指向这个地址；连不上就先跳过，用本地模式。</small>
    </label>

    <label class="field">
      <span>邮箱</span>
      <input
        v-model="email"
        type="text"
        inputmode="email"
        autocomplete="username"
        spellcheck="false"
        placeholder="you@example.com"
      />
    </label>

    <label class="field">
      <span>密码</span>
      <input
        v-model="password"
        type="password"
        :autocomplete="isRegister ? 'new-password' : 'current-password'"
        placeholder="至少 8 位"
      />
    </label>

    <label v-if="isRegister" class="field">
      <span>昵称（可选）</span>
      <input
        v-model="displayName"
        type="text"
        maxlength="24"
        autocomplete="nickname"
        placeholder="小张"
      />
      <small>只用于平台侧显示，不影响本机侧栏的称呼。</small>
    </label>

    <p v-if="error" class="alert" role="alert">
      <Icon icon="mdi:alert-circle-outline" width="18" />{{ error }}
    </p>
    <p v-else-if="notice" class="alert success" role="status">
      <Icon icon="mdi:check-circle-outline" width="18" />{{ notice }}
    </p>

    <button class="btn primary auth-submit" type="submit" :disabled="busy">
      <Icon :icon="isRegister ? 'mdi:account-plus-outline' : 'mdi:login-variant'" width="18" />
      {{ submitLabel }}
    </button>
  </form>
</template>
