<script setup>
import { computed, onBeforeUnmount, onMounted, provide, ref } from "vue";
import { Icon } from "@iconify/vue";
import { useKernel } from "./composables/useKernel";
import ChatPage from "./components/ChatPage.vue";
import MistakesPage from "./components/MistakesPage.vue";
import SettingsPage from "./components/SettingsPage.vue";
import SessionListPanel from "./components/SessionListPanel.vue";
import OobePage from "./components/OobePage.vue";

const kernel = useKernel();
provide("kernel", kernel);

/* ──── 跨页面跳转：错题本"追问" → 聊天页 ──── */
const navigateToChatMessage = ref("");
provide("navigateToChatMessage", navigateToChatMessage);

function navigateToChatWithMessage(msg) {
  navigateToChatMessage.value = msg;
  view.value = "chat";
}
provide("navigateToChatWithMessage", navigateToChatWithMessage);

const ready = ref(false);
const busy = ref(false);
const status = ref("准备中");
const view = ref("chat");
const oobeOpen = ref(false);
// 侧栏收起 = 只留图标条（宽度在 style.css），这里只管开关与记忆。
const SIDEBAR_COLLAPSED_KEY = "ma:sidebar-collapsed";
const sidebarCollapsed = ref(localStorage.getItem(SIDEBAR_COLLAPSED_KEY) === "1");
// 会话列表常驻在侧栏里，「当前会话」也就归 App 持有：ChatPage 只读它、不自己推导。
const activeSessionKey = ref(null);
const sessionList = ref(null);

function toggleSidebar() {
  sidebarCollapsed.value = !sidebarCollapsed.value;
  localStorage.setItem(SIDEBAR_COLLAPSED_KEY, sidebarCollapsed.value ? "1" : "0");
}

const navItems = [
  { id: "mistakes", label: "错题本", icon: "mdi:format-list-bulleted" },
  { id: "settings", label: "设置", icon: "mdi:cog-outline" },
];

/* ──── 左下角用户区：昵称（设置里可改）+ 一个占位菜单 ──── */
const nickname = ref("");
const displayName = computed(() => nickname.value.trim() || "同学");
const userBox = ref(null);
const userMenuOpen = ref(false);
const menuNotice = ref("");

// 三项都没有后端支撑：本地应用无账号、无服务端，也没有移动端。点了只如实说明。
const userMenuItems = [
  {
    id: "mobile",
    label: "下载手机端",
    icon: "mdi:cellphone-arrow-down",
    note: "移动端尚未支持：目前只有 Windows 桌面版。",
  },
  {
    id: "help",
    label: "帮助与反馈",
    icon: "mdi:help-circle-outline",
    note: "帮助与反馈尚未支持：反馈渠道还没接入。",
  },
  {
    id: "logout",
    label: "退出登录",
    icon: "mdi:logout",
    note: "无需登录：本应用纯本地运行，没有账号体系。",
  },
];

function toggleUserMenu() {
  userMenuOpen.value = !userMenuOpen.value;
  menuNotice.value = "";
}

function onMenuPick(item) {
  menuNotice.value = item.note;
}

/** 点菜单外面收起；Esc 也收（焦点在菜单里时）。 */
function onDocumentClick(e) {
  if (userMenuOpen.value && userBox.value && !userBox.value.contains(e.target)) {
    userMenuOpen.value = false;
  }
}

/** 读设置里的昵称。设置页保存后也会再调一次，让侧栏称呼立刻跟着变。 */
async function loadProfile() {
  try {
    const s = await kernel.call("get_settings", {}, 8000);
    nickname.value = s.nickname || "";
    return s;
  } catch {
    return null; // 读不到就用默认称呼，不阻塞界面。
  }
}

function onStatus(s) {
  busy.value = s.busy;
  status.value = s.text;
}

function navigate(viewId) {
  view.value = viewId;
}

/** 侧栏顶部的「新对话」：新建会话（面板负责落库与选中）→ select 回调里切到聊天页。 */
function newSession() {
  sessionList.value?.newSession();
}

/** 用户点列表里的会话：服务端归档旧 Active 并激活目标（ADR-0044）。 */
async function onSelectSession(key) {
  if (!key || busy.value) return;
  if (key !== activeSessionKey.value) {
    try {
      await kernel.call("open_session", { key }, 15000);
    } catch (e) {
      status.value = `切换会话失败：${e.message}`;
      return;
    }
  }
  activeSessionKey.value = key;
  view.value = "chat"; // 面板常驻侧栏：在别的页面点会话也回到聊天
}

/** 会话列表被改动（新建/重命名/删除）或回合结束后：让面板自己重列。 */
function refreshSessionList() {
  sessionList.value?.refreshList();
}

/** 服务端唯一 Active 会话即当前会话（单 Active 不变量），用它兜底同步选中项。 */
async function syncActiveSession() {
  if (!ready.value) return;
  try {
    const r = await kernel.call("list_sessions", {}, 8000);
    const arr = r.sessions || [];
    const active = arr.find((s) => s.status === "active");
    if (active?.key) activeSessionKey.value = active.key;
    else if (!arr.some((s) => s.key === activeSessionKey.value)) activeSessionKey.value = null;
  } catch {
    // 列表读不到不阻塞界面（会话为空时聊天区显示空状态）。
  }
}

/* ---- Ripple effect ---- */
let rippleCanvas = null;
let rippleCtx = null;
let ripples = [];
let rippleRaf = null;

function initRipple() {
  rippleCanvas = document.getElementById("ripple-canvas");
  if (!rippleCanvas) return;
  rippleCtx = rippleCanvas.getContext("2d");
  resizeRipple();
  window.addEventListener("resize", resizeRipple);
  document.addEventListener("click", onRippleClick);
  rippleRaf = requestAnimationFrame(animateRipples);
}

function resizeRipple() {
  if (!rippleCanvas) return;
  rippleCanvas.width = window.innerWidth;
  rippleCanvas.height = window.innerHeight;
}

function onRippleClick(e) {
  const x = e.clientX;
  const y = e.clientY;
  // spawn 6 rings
  for (let i = 0; i < 6; i++) {
    ripples.push({
      x,
      y,
      radius: 2,
      maxRadius: 30 + i * 14,
      opacity: 0.5,
      startTime: performance.now() + i * 40,
      speed: 0.6 + i * 0.08,
    });
  }
}

function animateRipples(now) {
  if (!rippleCtx || !rippleCanvas) {
    rippleRaf = requestAnimationFrame(animateRipples);
    return;
  }
  rippleCtx.clearRect(0, 0, rippleCanvas.width, rippleCanvas.height);

  ripples = ripples.filter((r) => {
    if (now < r.startTime) return true;
    const elapsed = now - r.startTime;
    const progress = elapsed / 800; // 0.8s lifetime
    if (progress >= 1) return false;

    r.radius += r.speed;
    r.opacity = 0.5 * (1 - progress);

    rippleCtx.beginPath();
    rippleCtx.arc(r.x, r.y, r.radius, 0, Math.PI * 2);
    rippleCtx.strokeStyle = `rgba(37,99,235,${r.opacity.toFixed(3)})`;
    rippleCtx.lineWidth = 1.5;
    rippleCtx.stroke();
    return true;
  });

  rippleRaf = requestAnimationFrame(animateRipples);
}

function destroyRipple() {
  if (rippleRaf) cancelAnimationFrame(rippleRaf);
  window.removeEventListener("resize", resizeRipple);
  document.removeEventListener("click", onRippleClick);
}

onMounted(async () => {
  initRipple();
  document.addEventListener("click", onDocumentClick);
  try {
    await kernel.start();
    ready.value = true;
    status.value = "就绪";
    await syncActiveSession();
    const s = await loadProfile();
    if (s && !s.main_model?.key_set) {
      oobeOpen.value = true;
    }
  } catch (e) {
    status.value = "内核异常";
    console.error("内核启动失败：", e);
  }
});

onBeforeUnmount(() => {
  destroyRipple();
  document.removeEventListener("click", onDocumentClick);
});
</script>

<template>
  <div class="app">
    <OobePage v-if="oobeOpen" :kernel="kernel" @done="oobeOpen = false" />

    <aside class="sidebar" :class="{ collapsed: sidebarCollapsed }">
      <div class="brand">
        <span class="brand-mark">
          <Icon icon="mdi:book-education-outline" width="22" />
        </span>
        <span class="brand-text">
          <span class="brand-name">错题 Agent</span>
          <span class="brand-sub">本地智能错题助手</span>
        </span>
        <button
          class="sidebar-toggle"
          type="button"
          :title="sidebarCollapsed ? '展开侧栏' : '收起侧栏'"
          :aria-label="sidebarCollapsed ? '展开侧栏' : '收起侧栏'"
          :aria-expanded="!sidebarCollapsed"
          @click="toggleSidebar"
        >
          <Icon :icon="sidebarCollapsed ? 'mdi:chevron-right' : 'mdi:chevron-left'" width="18" />
        </button>
      </div>
      <nav class="sidebar-top" aria-label="主操作">
        <button
          class="btn primary new-chat"
          type="button"
          title="新对话"
          aria-label="新对话"
          :disabled="busy"
          @click="newSession"
        >
          <Icon icon="mdi:plus" width="18" />
          <span>新对话</span>
        </button>
      </nav>
      <div class="sidebar-sessions">
        <span class="nav-label">会话</span>
        <SessionListPanel
          ref="sessionList"
          :kernel="kernel"
          :active-key="activeSessionKey"
          :busy="busy"
          @select="onSelectSession"
          @changed="refreshSessionList"
        />
      </div>

      <div class="sidebar-foot">
        <nav class="nav" aria-label="次级导航">
          <button
            v-for="item in navItems"
            :key="item.id"
            class="nav-item"
            :class="{ active: view === item.id }"
            :aria-current="view === item.id ? 'page' : undefined"
            :title="item.label"
            @click="view = item.id"
          >
            <Icon :icon="item.icon" width="20" />
            <span>{{ item.label }}</span>
          </button>
        </nav>

        <div ref="userBox" class="user-box" @keydown.esc="userMenuOpen = false">
          <div v-if="userMenuOpen" class="user-menu card" role="menu">
            <button
              v-for="item in userMenuItems"
              :key="item.id"
              class="user-menu-item"
              :class="{ danger: item.id === 'logout' }"
              type="button"
              role="menuitem"
              @click="onMenuPick(item)"
            >
              <Icon :icon="item.icon" width="18" />
              <span>{{ item.label }}</span>
            </button>
            <p v-if="menuNotice" class="user-menu-note">{{ menuNotice }}</p>
          </div>

          <button
            class="user-row"
            type="button"
            aria-haspopup="menu"
            :aria-expanded="userMenuOpen"
            title="账户与帮助"
            @click="toggleUserMenu"
          >
            <span class="avatar" :class="{ busy, ready: ready && !busy }">
              {{ displayName.slice(0, 1) }}
            </span>
            <span class="user-meta">
              <span class="user-name">{{ displayName }}</span>
              <span class="user-status">{{ status }}</span>
            </span>
            <Icon
              class="user-caret"
              :icon="userMenuOpen ? 'mdi:chevron-down' : 'mdi:chevron-up'"
              width="16"
            />
          </button>
        </div>
      </div>
    </aside>

    <section class="main">
      <div class="view-host">
        <Transition name="view" mode="out-in">
          <ChatPage
            v-if="view === 'chat'"
            :key="'chat'"
            :kernel="kernel"
            :ready="ready"
            v-model:active-key="activeSessionKey"
            @status="onStatus"
            @navigate="navigate"
            @sessions-dirty="refreshSessionList"
          />
          <MistakesPage v-else-if="view === 'mistakes'" :key="'mistakes'" :kernel="kernel" />
          <SettingsPage v-else :key="'settings'" :kernel="kernel" @saved="loadProfile" />
        </Transition>
      </div>
    </section>
  </div>
</template>
