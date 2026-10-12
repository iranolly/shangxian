// 快照轮询 + 唯一数据源（D4 §1.2）。
//
// - 间隔读 app.settings.refreshIntervalMs（默认 800）；setInterval 保留，按最新间隔节流。
// - 窗口隐藏 → 停表；恢复可见 → 立刻刷一次（⑭）。
// - 连续失败只记数，不弹 toast；≥3 次由 App 画侧栏细条；首帧失败 5 次才弹一条 error。
// - 结构共享（optimistic.reconcileSnapshot）+ 乐观覆盖合并都在这里落到 state。
//
// raw = 后端最新一份（不含乐观）；state = raw 合并覆盖后的界面视图。

import {
  defaultAppView,
  getSnapshot,
  type AppView,
  type ServerView,
  type ServiceView,
  type Snapshot,
  type TunnelView,
} from "../api";
import { applyOverrides, reconcileSnapshot as reconcileEntities, type Override } from "./optimistic";
import { isPending, setPending } from "./pendingStore";
import { toast } from "./toastStore";

export interface PollingState {
  /** 当前生效的刷新间隔（毫秒） */
  intervalMs: number;
  /** 窗口是否隐藏 */
  hidden: boolean;
  /** 最近一次成功时刻（epoch ms；0 = 还没成功过） */
  lastOkAt: number;
  /** 连续失败次数（成功一次清零） */
  failStreak: number;
}

export interface AppState {
  app: AppView;
  services: ServiceView[];
  tunnels: TunnelView[];
  servers: ServerView[];
  byServiceId: Record<string, ServiceView>;
  byTunnelId: Record<string, TunnelView>;
  byServerId: Record<string, ServerView>;
  polling: PollingState;
  /** 首帧（还没拿到第一份快照） */
  loading: boolean;
}

const HEARTBEAT_MS = 200;
const FIRST_FAIL_TOAST_AFTER = 5;
const DEFAULT_OVERRIDE_TTL_MS = 30_000;
const CONFIRM_WINDOW_MS = 4_000;

function indexById<T extends { id: string }>(list: T[]): Record<string, T> {
  const out: Record<string, T> = {};
  for (const item of list) out[item.id] = item;
  return out;
}

function isDocumentHidden(): boolean {
  return typeof document !== "undefined" && document.hidden;
}

function buildState(snapshot: Snapshot, polling: PollingState, loading: boolean): AppState {
  return {
    app: snapshot.app,
    services: snapshot.services,
    tunnels: snapshot.tunnels,
    servers: snapshot.servers,
    byServiceId: indexById(snapshot.services),
    byTunnelId: indexById(snapshot.tunnels),
    byServerId: indexById(snapshot.servers),
    polling,
    loading,
  };
}

/** 超时提示里要一个能读的名字；找不到就退回 id。 */
function nameForKey(snapshot: Snapshot, key: string): string {
  const at = key.indexOf(":");
  if (at <= 0 || at === key.length - 1) return key;
  const kind = key.slice(0, at);
  const id = key.slice(at + 1);
  if (kind === "tunnel") return snapshot.tunnels.find((t) => t.id === id)?.name ?? id;
  if (kind === "service") return snapshot.services.find((s) => s.id === id)?.name ?? id;
  if (kind === "server") return snapshot.servers.find((s) => s.id === id)?.name ?? id;
  return id;
}

let raw: Snapshot = { app: defaultAppView(), services: [], tunnels: [], servers: [] };
let overrides = new Map<string, Override>();
let overrideSeq = 0;
let state: AppState = buildState(
  raw,
  { intervalMs: 800, hidden: false, lastOkAt: 0, failStreak: 0 },
  true,
);

const listeners = new Set<() => void>();

function emit(): void {
  for (const fn of listeners) fn();
}

export function getState(): AppState {
  return state;
}

export function subscribe(fn: () => void): () => void {
  listeners.add(fn);
  return () => {
    listeners.delete(fn);
  };
}

/** 用 raw + 覆盖表重算界面视图；默认保持 loading 不变。 */
function remerge(nextLoading: boolean = state.loading): void {
  const merged = applyOverrides(raw, overrides, Date.now());
  overrides = merged.overrides;
  // 快照已经给出结论（确认 / 被服务端改写）→ 命令不必再等，提前结束「处理中…」。
  // 超时：override 仍按 TTL 规则丢弃；命令还没结束就提示一次（pending 交给 commands 的 watchdog）。
  for (const item of merged.dropped) {
    if (item.reason === "confirmed" || item.reason === "superseded") {
      setPending(item.override.key, false);
    } else if (item.reason === "timeout" && isPending(item.override.key)) {
      toast.warn(
        `操作没有回音：${nameForKey(merged.snapshot, item.override.key)} 可能没成功，等一下再看看`,
        {
          key: `cmd-timeout:${item.override.key}`,
          action: { label: "刷新", run: () => refreshNow() },
        },
      );
    }
  }
  const snapshot = merged.snapshot;
  const intervalMs = snapshot.app.settings.refreshIntervalMs || 800;
  const hidden = isDocumentHidden();
  const polling =
    state.polling.intervalMs === intervalMs && state.polling.hidden === hidden
      ? state.polling
      : { ...state.polling, intervalMs, hidden };
  // 列表引用没变时连索引也一起复用，避免每次 tick 都重建整张索引表
  const byServiceId =
    snapshot.services === state.services ? state.byServiceId : indexById(snapshot.services);
  const byTunnelId =
    snapshot.tunnels === state.tunnels ? state.byTunnelId : indexById(snapshot.tunnels);
  const byServerId =
    snapshot.servers === state.servers ? state.byServerId : indexById(snapshot.servers);
  state = {
    app: snapshot.app,
    services: snapshot.services,
    tunnels: snapshot.tunnels,
    servers: snapshot.servers,
    byServiceId,
    byTunnelId,
    byServerId,
    polling,
    loading: nextLoading,
  };
  emit();
}

/** 后端来了一份新快照：结构共享 + 合并乐观，然后通知订阅者。 */
export function reconcileSnapshot(next: Snapshot): void {
  raw = reconcileEntities(raw, next);
  remerge(false);
}

// ---------- 乐观覆盖 API（commands.ts 用） ----------

export interface OverrideSpec {
  patch: Record<string, unknown>;
  expect: Record<string, unknown>;
  before: Record<string, unknown>;
  ttlMs?: number;
}

export function beginOverride(key: string, spec: OverrideSpec): void {
  overrides = new Map(overrides);
  overrides.set(key, {
    key,
    seq: ++overrideSeq,
    startedAt: Date.now(),
    patch: spec.patch,
    expect: spec.expect,
    before: spec.before,
    ttlMs: spec.ttlMs ?? DEFAULT_OVERRIDE_TTL_MS,
  });
  remerge();
}

/** 命令成功后给一个确认窗口：快照 4 秒内还没确认就信服务端。 */
export function confirmOverride(key: string, withinMs: number = CONFIRM_WINDOW_MS): void {
  const current = overrides.get(key);
  if (!current) return;
  overrides = new Map(overrides);
  overrides.set(key, { ...current, confirmDeadline: Date.now() + withinMs });
}

export function dropOverride(key: string): void {
  if (!overrides.has(key)) return;
  overrides = new Map(overrides);
  overrides.delete(key);
  remerge();
}

export function getOverrides(): Map<string, Override> {
  return overrides;
}

// ---------- 轮询 ----------

let started = false;
let timer: number | null = null;
let lastRequestAt = 0;
let inFlight = false;
let firstFailNotified = false;

function ensureTimer(): void {
  if (timer !== null || !started || isDocumentHidden()) return;
  timer = window.setInterval(() => {
    void tick(false);
  }, HEARTBEAT_MS);
}

function stopTimer(): void {
  if (timer === null) return;
  window.clearInterval(timer);
  timer = null;
}

async function tick(force: boolean): Promise<void> {
  if (!started || inFlight) return;
  if (isDocumentHidden() && !force) return;
  const now = Date.now();
  const interval = state.polling.intervalMs >= 500 ? state.polling.intervalMs : 800;
  if (!force && now - lastRequestAt < interval) return;
  inFlight = true;
  lastRequestAt = now;
  try {
    const next = await getSnapshot();
    if (!started) return;
    state = { ...state, polling: { ...state.polling, failStreak: 0, lastOkAt: Date.now() } };
    reconcileSnapshot(next);
    firstFailNotified = false;
  } catch {
    if (!started) return;
    const failStreak = state.polling.failStreak + 1;
    state = { ...state, polling: { ...state.polling, failStreak } };
    emit();
    if (state.loading && failStreak >= FIRST_FAIL_TOAST_AFTER && !firstFailNotified) {
      firstFailNotified = true;
      toast.error("连不上后台，界面上的状态可能不是最新的", {
        key: "snapshot-first-fail",
        durationMs: 12_000,
        action: { label: "重试", run: () => refreshNow() },
      });
    }
  } finally {
    inFlight = false;
  }
}

function onVisibilityChange(): void {
  const hidden = isDocumentHidden();
  state = { ...state, polling: { ...state.polling, hidden } };
  emit();
  if (hidden) {
    stopTimer();
  } else {
    ensureTimer();
    refreshNow();
  }
}

/** App 挂载时调用一次；返回停止函数（StrictMode 下重复挂载也安全）。 */
export function startPolling(): () => void {
  if (started) return () => undefined;
  started = true;
  document.addEventListener("visibilitychange", onVisibilityChange);
  ensureTimer();
  void tick(true);
  return () => {
    started = false;
    document.removeEventListener("visibilitychange", onVisibilityChange);
    stopTimer();
  };
}

/** 手动刷新（失败 toast 的「重试」、页面刷新按钮用）。 */
export function refreshNow(): void {
  void tick(true);
}
