// 实体级「就地错误」：命令失败时挂在对应卡片 / 弹层上（D4 §1.5）。
// 键与 pending / 乐观覆盖一致：`tunnel:{id}` / `service:{id}` / `server:{id}`。
// App 顶部横幅也订阅「最近一条」，作为页面还没接就地错误前的过渡展示。
//
// visible：挂这条错误时实体能不能就地显示（卡片在筛选结果里、当前页挂着）。
// commands.ts 失败分流和 App 横幅收敛都据此避免重复提示；不传默认 true，老调用方行为不变。

import { useSyncExternalStore } from "react";

export interface LatestNotice {
  key: string;
  text: string;
  at: number;
  /** 挂上这条错误时，实体是否可见（就地能显示） */
  visible: boolean;
}

interface Notice {
  text: string;
  visible: boolean;
}

const notices = new Map<string, Notice>();
let latest: LatestNotice | null = null;
const listeners = new Set<() => void>();

function emit(): void {
  for (const fn of listeners) fn();
}

export function subscribeNotice(fn: () => void): () => void {
  listeners.add(fn);
  return () => {
    listeners.delete(fn);
  };
}

/** 给某个实体挂一条错误（后到的覆盖先到的）。visible = 就地能不能显示，默认 true。 */
export function setNotice(key: string, text: string, visible: boolean = true): void {
  notices.set(key, { text, visible });
  latest = { key, text, at: Date.now(), visible };
  emit();
}

export function clearNotice(key: string): void {
  if (!notices.delete(key)) return;
  if (latest?.key === key) latest = null;
  emit();
}

export function clearAllNotices(): void {
  if (notices.size === 0 && latest === null) return;
  notices.clear();
  latest = null;
  emit();
}

export function getLatestNotice(): LatestNotice | null {
  return latest;
}

/** 某个实体当前的错误原文（卡片就地展示用）。 */
export function useNotice(key: string): string | undefined {
  return useSyncExternalStore(subscribeNotice, () => notices.get(key)?.text);
}

/** 最近一条错误（App 顶部横幅用；没有就返回 null）。 */
export function useLatestNotice(): LatestNotice | null {
  return useSyncExternalStore(subscribeNotice, getLatestNotice);
}
