// Tauri ↔ sidecar 桥接：Channel 收帧，kernel_send 发请求。
import { Channel, invoke } from "@tauri-apps/api/core";

export function useKernel() {
  let nextId = 1;
  const listeners = new Set();
  const pending = new Map();

  function handleFrame(frame) {
    if (frame.type === "response") {
      const waiter = pending.get(frame.id);
      if (waiter) {
        pending.delete(frame.id);
        clearTimeout(waiter.timer);
        if (frame.error) {
          const err = new Error(frame.error.message || frame.error.code);
          err.code = frame.error.code;
          waiter.reject(err);
        } else {
          waiter.resolve(frame.result ?? {});
        }
      }
    }
    for (const cb of listeners) cb(frame);
  }

  function onFrame(cb) {
    listeners.add(cb);
    return () => listeners.delete(cb);
  }

  async function start() {
    const channel = new Channel();
    channel.onmessage = (payload) => {
      let frame = payload;
      if (typeof payload === "string") {
        try {
          frame = JSON.parse(payload);
        } catch {
          return;
        }
      }
      handleFrame(frame);
    };
    await invoke("start_kernel", { onFrame: channel });
    // 全链路自检：get_state 回执到达后才解锁发送。
    await sendLine("get_state");
  }

  /**
   * 请求-响应 RPC：按 id 关联回执，支持超时。
   * 事件帧（无 id）会同时广播给 onFrame 订阅者。
   */
  function call(method, extra = {}, timeoutMs = 20000) {
    const id = nextId++;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        if (pending.delete(id)) {
          const err = new Error(`${method} 请求超时`);
          err.code = "timeout";
          reject(err);
        }
      }, timeoutMs);
      pending.set(id, { resolve, reject, timer });
      invoke("kernel_send", {
        line: JSON.stringify({ id, method, ...extra }),
      }).catch((e) => {
        if (pending.delete(id)) {
          clearTimeout(timer);
          reject(e);
        }
      });
    });
  }

  async function sendLine(method, extra = {}) {
    const id = nextId++;
    await invoke("kernel_send", {
      line: JSON.stringify({ id, method, ...extra }),
    });
    return id;
  }

  /** 唯一命令通道（ADR-0013）：触发已注册 EntryPoint，用户侧调用。 */
  async function triggerCommand(entry, params = {}) {
    return call("trigger_command", { entry, params });
  }

  /** 用户可调工具/命令清单（显式 tool-calling 面板）。 */
  function listTools() {
    return call("list_tools", {}, 10000);
  }

  function pickHomeworkFile() {
    return invoke("pick_homework_file");
  }

  /** 教学规则（数据根 AGENTS.md）加载状态：{loaded, path, reason?, bytes?}。 */
  function getRulesStatus() {
    return call("get_rules_status", {}, 10000);
  }

  /** 用系统默认程序打开教学规则文件（家长/老师编辑用）。 */
  function openRulesFile() {
    return invoke("open_rules_file");
  }

  return {
    onFrame,
    start,
    call,
    sendLine,
    triggerCommand,
    listTools,
    pickHomeworkFile,
    getRulesStatus,
    openRulesFile,
  };
}
