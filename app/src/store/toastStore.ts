// 右下角 toast（D4 §1.6）。
// 规则：最多同时 3 条，多的排队；同一 key 同文案 2 秒内只留一条（合并计数 ×2）；
// hover / 聚焦时暂停计时；error 单独插入时用 role="alert"（由 Toaster 渲染）。

import { useSyncExternalStore } from "react";

export type ToastKind = "success" | "info" | "warn" | "error";

export interface ToastAction {
  label: string;
  run: () => void;
  /** true = 点动作后不自动关掉这条 toast */
  keepOpen?: boolean;
}

export interface Toast {
  id: string;
  kind: ToastKind;
  text: string;
  detail?: string;
  action?: ToastAction;
  durationMs: number;
  key: string;
  /** 同 key 合并后的次数（用于「×2」） */
  count: number;
}

/** 生成一条 toast 用的参数（runCommand 的 success 直接给这个形状） */
export interface ToastSpec {
  kind: ToastKind;
  text: string;
  detail?: string;
  action?: ToastAction;
  durationMs?: number;
  key?: string;
  /** true = 不自动消失（系统提示用） */
  sticky?: boolean;
}

export interface ToastOptions {
  detail?: string;
  action?: ToastAction;
  durationMs?: number;
  key?: string;
  sticky?: boolean;
}

const DEFAULT_DURATION: Record<ToastKind, number> = {
  success: 3000,
  info: 4000,
  warn: 6000,
  error: 8000,
};

const MAX_VISIBLE = 3;
const DEDUP_MS = 2000;

interface Entry {
  toast: Toast;
  remaining: number;
  startedAt: number;
  timer: number | null;
  addedAt: number;
  paused: boolean;
  sticky: boolean;
}

const entries = new Map<string, Entry>();
let visible: Toast[] = [];
let seq = 0;
const listeners = new Set<() => void>();

function emit(): void {
  visible = Array.from(entries.values())
    .slice(0, MAX_VISIBLE)
    .map((e) => e.toast);
  for (const fn of listeners) fn();
}

/** 只给排在前 3 条（真正显示出来的）跑倒计时。 */
function syncTimers(): void {
  let i = 0;
  for (const [id, e] of entries) {
    const shown = i < MAX_VISIBLE;
    const shouldRun = shown && !e.paused && !e.sticky && e.remaining > 0;
    if (shouldRun && e.timer === null) {
      e.startedAt = Date.now();
      e.timer = window.setTimeout(() => dismiss(id), e.remaining);
    } else if (!shouldRun && e.timer !== null) {
      window.clearTimeout(e.timer);
      e.timer = null;
      e.remaining = Math.max(0, e.remaining - (Date.now() - e.startedAt));
    }
    i++;
  }
}

export function subscribeToasts(fn: () => void): () => void {
  listeners.add(fn);
  return () => {
    listeners.delete(fn);
  };
}

export function getVisibleToasts(): Toast[] {
  return visible;
}

function add(spec: ToastSpec): string {
  const key = spec.key ?? `${spec.kind}|${spec.text}`;
  const now = Date.now();
  for (const [id, e] of entries) {
    if (e.toast.key !== key || now - e.addedAt > DEDUP_MS) continue;
    // 2 秒内同 key：合并计数，并把倒计时放回满格
    e.toast = { ...e.toast, count: e.toast.count + 1 };
    e.addedAt = now;
    if (e.timer !== null) {
      window.clearTimeout(e.timer);
      e.timer = null;
    }
    e.paused = false;
    e.remaining = spec.durationMs ?? e.toast.durationMs;
    emit();
    syncTimers();
    return id;
  }
  const id = `toast-${++seq}`;
  const durationMs = spec.durationMs ?? DEFAULT_DURATION[spec.kind];
  const toast: Toast = {
    id,
    kind: spec.kind,
    text: spec.text,
    detail: spec.detail,
    action: spec.action,
    durationMs,
    key,
    count: 1,
  };
  entries.set(id, {
    toast,
    remaining: durationMs,
    startedAt: now,
    timer: null,
    addedAt: now,
    paused: false,
    sticky: spec.sticky === true || durationMs <= 0,
  });
  emit();
  syncTimers();
  return id;
}

export function dismiss(id: string): void {
  const e = entries.get(id);
  if (!e) return;
  if (e.timer !== null) window.clearTimeout(e.timer);
  entries.delete(id);
  emit();
  syncTimers();
}

export function pause(id: string): void {
  const e = entries.get(id);
  if (!e || e.paused) return;
  if (e.timer !== null) {
    window.clearTimeout(e.timer);
    e.timer = null;
    e.remaining = Math.max(0, e.remaining - (Date.now() - e.startedAt));
  }
  e.paused = true;
}

export function resume(id: string): void {
  const e = entries.get(id);
  if (!e || !e.paused) return;
  e.paused = false;
  syncTimers();
}

export function clearToasts(): void {
  for (const e of entries.values()) {
    if (e.timer !== null) window.clearTimeout(e.timer);
  }
  entries.clear();
  emit();
}

export const toast = {
  success: (text: string, opts?: ToastOptions) => add({ kind: "success", text, ...opts }),
  info: (text: string, opts?: ToastOptions) => add({ kind: "info", text, ...opts }),
  warn: (text: string, opts?: ToastOptions) => add({ kind: "warn", text, ...opts }),
  error: (text: string, opts?: ToastOptions) => add({ kind: "error", text, ...opts }),
  show: (spec: ToastSpec) => add(spec),
  dismiss,
  pause,
  resume,
  clear: clearToasts,
};

export function useToasts(): Toast[] {
  return useSyncExternalStore(subscribeToasts, getVisibleToasts);
}
