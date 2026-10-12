// 局部订阅小钩子（D4 §1.2）。不引第三方状态库。

import { useCallback, useRef, useSyncExternalStore } from "react";

import type { AppView, ServerView, ServiceView, TunnelView } from "../api";
import { getState, subscribe, type AppState, type PollingState } from "./snapshotStore";

/**
 * 订阅 store 的一小块。select 建议用模块级函数；
 * 结果用 ref 缓存 + isEqual 比较，避免每次返回新数组导致无限循环。
 */
export function useStore<T>(
  select: (s: AppState) => T,
  isEqual?: (a: T, b: T) => boolean,
): T {
  const selectRef = useRef(select);
  selectRef.current = select;
  const isEqualRef = useRef(isEqual);
  isEqualRef.current = isEqual;
  const cache = useRef<{ value: T } | null>(null);

  const getSelection = useCallback(() => {
    const next = selectRef.current(getState());
    const prev = cache.current;
    if (prev) {
      const eq = isEqualRef.current;
      if (eq ? eq(prev.value, next) : Object.is(prev.value, next)) return prev.value;
    }
    cache.current = { value: next };
    return next;
  }, []);

  return useSyncExternalStore(subscribe, getSelection, getSelection);
}

function sameArray<T>(a: T[], b: T[]): boolean {
  if (a === b) return true;
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) {
    if (!Object.is(a[i], b[i])) return false;
  }
  return true;
}

export function useApp(): AppView {
  return useStore((s) => s.app);
}

export function usePolling(): PollingState {
  return useStore((s) => s.polling);
}

export function useTunnel(id: string): TunnelView | undefined {
  return useStore((s) => s.byTunnelId[id]);
}

export function useService(id: string): ServiceView | undefined {
  return useStore((s) => s.byServiceId[id]);
}

export function useServer(id: string): ServerView | undefined {
  return useStore((s) => s.byServerId[id]);
}

/** id 数组：集合不变时引用不变（列表父组件只订阅它）。 */
export function useTunnelIds(): string[] {
  return useStore((s) => s.tunnels.map((t) => t.id), sameArray);
}

export function useServiceIds(): string[] {
  return useStore((s) => s.services.map((t) => t.id), sameArray);
}

export function useServerIds(): string[] {
  return useStore((s) => s.servers.map((t) => t.id), sameArray);
}
