<script setup>
import { Icon } from "@iconify/vue";
import AccountAuthForm from "./AccountAuthForm.vue";

/* 首屏门禁：应用启动且未登录时盖在最上层（ADR-0048）。
 *
 * **可跳过**是刻意的：错题本、会话与记忆全部存在本机，不登录也该能用；
 * 而服务端一旦不可达（部署未就绪、断网），硬门禁会把整个应用锁死。
 * 跳过会被记住（localStorage），下次启动不再拦；想登录随时点侧栏左下角
 * 的「登录平台服务」，门禁会回来。 */
defineProps({
  kernel: { type: Object, required: true },
  serverUrl: { type: String, default: "" },
});
const emit = defineEmits(["authed", "skip"]);
</script>

<template>
  <div class="login-gate">
    <div class="gate-card card">
      <div class="gate-brand">
        <span class="brand-mark"><Icon icon="mdi:book-education-outline" width="26" /></span>
        <span class="brand-text">
          <span class="brand-name">错题 Agent</span>
          <span class="brand-sub">本地智能错题助手</span>
        </span>
      </div>

      <h1 class="gate-title">登录平台服务</h1>
      <p class="gate-lead">
        登录后模型调用走平台额度，不必自备 DeepSeek Key；错题本、会话与记忆始终存在本机。
      </p>

      <AccountAuthForm
        :kernel="kernel"
        :server-url="serverUrl"
        show-server-url
        @authed="emit('authed', $event)"
      />

      <button class="gate-skip" type="button" @click="emit('skip')">
        先跳过，用本地模式
      </button>
    </div>
  </div>
</template>
