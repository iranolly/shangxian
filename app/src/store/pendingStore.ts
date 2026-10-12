// 写操作的「处理中…」状态（D4 §1.4）。
// 键的格式和乐观覆盖一致：`tunnel:{id}` / `service:{id}` / `server:{id}` / `bulk:{action}`。
// 组件用 usePending(key) 局部订阅：只有那张卡片重渲染。

import { useSyncExternalStore } from "react";

const pending = new Set<string>();
const listeners = new Set<() => void>();

function emit(): void {
  for (const fn of listeners) fn();
}

export function subscribePending(fn: () => void): () => void {
  listeners.add(fn);
  return () => {
    listeners.delete(fn);
  };
}

/** 开 / 关某个键的 pending。 */
export function setPending(key: string, on: boolean): void {
  if (on === pending.has(key)) return;
  if (on) pending.add(key);
  else pending.delete(key);
  emit();
}

export function isPending(key: string): boolean {
  return pending.has(key);
}

/** 同一个实体同时只允许一个写操作：调用方可先查这个再决定是否禁用。 */
export function pendingCount(): number {
  return pending.size;
}

export function usePending(key: string): boolean {
  return useSyncExternalStore(subscribePending, () => pending.has(key));
}

/** 一组键里任意一个在 pending（批量按钮用）。 */
export function useAnyPending(keys: string[]): boolean {
  return useSyncExternalStore(subscribePending, () => keys.some((k) => pending.has(k)));
}
