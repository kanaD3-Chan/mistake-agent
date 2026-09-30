<script setup>
import { computed, onBeforeUnmount, onMounted, provide, ref } from "vue";
import { Icon } from "@iconify/vue";
import { useKernel } from "./composables/useKernel";
import ChatPage from "./components/ChatPage.vue";
import MistakesPage from "./components/MistakesPage.vue";
import SettingsPage from "./components/SettingsPage.vue";
import SessionListPanel from "./components/SessionListPanel.vue";
import OobePage from "./components/OobePage.vue";
import LoginGate from "./components/LoginGate.vue";

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

// 只缓存聊天页（见模板里的 KeepAlive 注释）。写成常量而不是模板里的数组字面量：
// 字面量每次渲染都是新引用，会让 KeepAlive 内部的 include/exclude watcher 白跑一趟。
const KEEP_ALIVE_VIEWS = ["ChatPage"];

/* ──── 左下角用户区：昵称（设置里可改）+ 账号菜单 ──── */
const nickname = ref("");
const displayName = computed(() => nickname.value.trim() || "同学");
const userBox = ref(null);
const userMenuOpen = ref(false);
const menuNotice = ref("");

/* ──── 平台账号（ADR-0048）──── */
const account = ref({ logged_in: false, server_url: "", email: "", role: "", sync_enabled: false });
const loggingOut = ref(false);
/** 跳过首屏登录页后记住：本地优先的应用不该每次启动都拦一道。 */
const GATE_SKIP_KEY = "ma:gate-skipped";
const gateSkipped = ref(localStorage.getItem(GATE_SKIP_KEY) === "1");
/** 首屏门禁：内核就绪 + 未登录 + 没跳过过。 */
const gateVisible = computed(() => ready.value && !account.value.logged_in && !gateSkipped.value);
const ROLE_TEXT = { user: "学生", teacher: "教师", admin: "管理员" };
const roleText = computed(() => ROLE_TEXT[account.value.role] || account.value.role || "");

/** 账号视图（get_settings 的 account 段、各账号 RPC 的回执、account_changed 事件）合并进本地状态。
 *  事件只带 logged_in/email，合并式更新才不会把已有的 role/server_url 抹掉。 */
function applyAccount(view) {
  if (view) account.value = { ...account.value, ...view };
}

// 菜单随登录状态变形：未登录给「登录平台服务」，已登录给「退出登录」。
// 后两项没有后端支撑，点了只如实说明。
const userMenuItems = computed(() => {
  const items = [];
  if (!account.value.logged_in) {
    items.push({ id: "login", label: "登录平台服务", icon: "mdi:login-variant", action: "login" });
  }
  items.push({
    id: "mobile",
    label: "下载手机端",
    icon: "mdi:cellphone-arrow-down",
    note: "移动端尚未支持：目前只有 Windows 桌面版。",
  });
  items.push({
    id: "help",
    label: "帮助与反馈",
    icon: "mdi:help-circle-outline",
    note: "帮助与反馈尚未支持：反馈渠道还没接入。",
  });
  if (account.value.logged_in) {
    items.push({ id: "logout", label: "退出登录", icon: "mdi:logout", action: "logout" });
  }
  return items;
});

function toggleUserMenu() {
  userMenuOpen.value = !userMenuOpen.value;
  menuNotice.value = "";
}

/** 菜单里点「登录平台服务」：把首屏门禁叫回来——跳过标记只是不再自动拦，这扇门没焊死。 */
function openGate() {
  gateSkipped.value = false;
  userMenuOpen.value = false;
  menuNotice.value = "";
}

/** 退出登录：清本地令牌、模型链路切回自备 Key。**错题本 / 会话 / 记忆一个字节都不动。** */
async function doLogout() {
  if (loggingOut.value) return;
  loggingOut.value = true;
  menuNotice.value = "";
  try {
    applyAccount(await kernel.call("logout", {}, 25000));
    userMenuOpen.value = false;
    status.value = "已退出登录";
  } catch (e) {
    menuNotice.value = `退出登录失败：${e.message}`;
  } finally {
    loggingOut.value = false;
  }
}

function onMenuPick(item) {
  if (item.action === "login") {
    openGate();
    return;
  }
  if (item.action === "logout") {
    doLogout();
    return;
  }
  menuNotice.value = item.note;
}

function onGateSkip() {
  gateSkipped.value = true;
  localStorage.setItem(GATE_SKIP_KEY, "1");
}

function onGateAuthed(view) {
  applyAccount(view);
  // 走平台额度就不需要自备 Key 了：把新手向导收掉（它本来就是为"没配 Key"准备的）。
  oobeOpen.value = false;
  status.value = "就绪";
}

/** 点菜单外面收起；Esc 也收（焦点在菜单里时）。 */
function onDocumentClick(e) {
  if (userMenuOpen.value && userBox.value && !userBox.value.contains(e.target)) {
    userMenuOpen.value = false;
  }
}

/** 读设置里的昵称与账号段。设置页保存后也会再调一次，让侧栏称呼立刻跟着变。 */
async function loadProfile() {
  try {
    const s = await kernel.call("get_settings", {}, 8000);
    nickname.value = s.nickname || "";
    applyAccount(s.account);
    return s;
  } catch {
    return null; // 读不到就用默认称呼，不阻塞界面。
  }
}

/** 启动时静默校一次令牌：本地缓存写着"已登录"不代表服务端还认（令牌 90 天到期、
 *  账号可能被停用）。**只有服务端明确说令牌无效才清本地令牌**——连不上就什么都不做，
 *  一次网络抖动不该把学生踢下线（内核侧 `AccountError::invalidates_token` 定的调）。 */
async function revalidateAccount() {
  if (!account.value.logged_in) return;
  try {
    const view = await kernel.call("get_account_status", { revalidate: true }, 25000);
    applyAccount(view);
    if (view.reason === "token_invalid") status.value = "登录已失效，请重新登录";
    else if (view.reason === "account_disabled") status.value = "账号已被停用，请联系管理员";
  } catch {
    // 校验本身失败不影响本地使用。
  }
}

/** 内核帧订阅：账号状态变了就跟着变（登录/登出/令牌失效都会发这一个事件）。 */
let unsubscribe = null;
function onFrame(frame) {
  if (frame.type !== "event") return;
  const e = frame.event;
  if (e?.event === "account_changed") {
    applyAccount({ logged_in: e.logged_in, email: e.email });
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

/** 用户点列表里的会话：服务端归档旧 Active 并激活目标（ADR-0044）。
 *  `busy` 只挡「换会话」这一个动作（后端本来就会以 turn_in_progress 拒掉），
 *  不挡「回聊天页」——否则回合在飞时切去设置页就再也回不来了。 */
async function onSelectSession(key) {
  if (!key) return;
  if (key !== activeSessionKey.value) {
    if (busy.value) {
      status.value = "回合进行中，结束后再切换会话";
      view.value = "chat"; // 回得去，只是还停在正在回答的那条会话上
      return;
    }
    try {
      await kernel.call("open_session", { key }, 15000);
    } catch (e) {
      status.value = `切换会话失败：${e.message}`;
      return;
    }
    activeSessionKey.value = key;
  }
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
    // 已登录就走平台额度，不需要自备 Key——别拿"配 DeepSeek Key"的向导拦他。
    if (s && !s.main_model?.key_set && !s.account?.logged_in) {
      oobeOpen.value = true;
    }
    unsubscribe = kernel.onFrame(onFrame);
    revalidateAccount();
  } catch (e) {
    status.value = "内核异常";
    console.error("内核启动失败：", e);
  }
});

onBeforeUnmount(() => {
  destroyRipple();
  document.removeEventListener("click", onDocumentClick);
  unsubscribe?.();
});
</script>

<template>
  <div class="app">
    <LoginGate
      v-if="gateVisible"
      :kernel="kernel"
      :server-url="account.server_url"
      @authed="onGateAuthed"
      @skip="onGateSkip"
    />

    <!-- 门禁盖在最上层时让新手向导等着：两屏同时在，按钮会被抢焦点。 -->
    <OobePage v-if="oobeOpen && !gateVisible" :kernel="kernel" @done="oobeOpen = false" />

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
          :disabled="busy || !ready"
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
          :ready="ready"
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
            <div v-if="account.logged_in" class="user-menu-account">
              <span class="user-menu-email" :title="account.email">{{ account.email }}</span>
              <span v-if="roleText" class="badge">{{ roleText }}</span>
            </div>
            <button
              v-for="item in userMenuItems"
              :key="item.id"
              class="user-menu-item"
              :class="{ danger: item.action === 'logout' }"
              type="button"
              role="menuitem"
              :disabled="item.action === 'logout' && loggingOut"
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
          <!-- 只缓存聊天页：它订阅着 kernel 帧，卸载即退订——回合在飞时切去设置/错题本，
               半截回答连同 busy 一起丢，而 busy 卡在 true 会让侧栏的会话再也点不动。
               缓存后切走只是 deactivate，流式回答照常往下写，切回来接着看。
               `include` 按组件名匹配（`<script setup>` 由文件名推断出 "ChatPage"）；
               错题本/设置不缓存，回读时要拿到最新数据。 -->
          <KeepAlive :include="KEEP_ALIVE_VIEWS">
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
          </KeepAlive>
        </Transition>
      </div>
    </section>
  </div>
</template>
