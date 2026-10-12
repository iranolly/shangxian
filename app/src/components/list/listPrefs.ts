// 列表偏好（搜索 / 筛选 / 排序 / 分组）+ 批量选择状态（D4 §1.8 / §3.1 的等价实现）。
//
// 位置说明：D4 把它们放在 app/src/store/；本波（E2）允许新建的文件只有列表组件与
// styles/lists.css，所以放在这里。语义、localStorage 键名、持久化内容都照 D4 §1.8 / §1.10。

import { useSyncExternalStore } from "react";

import type { ServiceStatus, TunnelStatus } from "../../copy/status";

export type ListPage = "tunnels" | "services";

type TunnelSort = "default" | "name" | "changed" | "port" | "server";
type ServiceSort = "default" | "name" | "port";

export interface TunnelListPrefs {
  query: string;
  /** 多选 chips，OR 语义；"all" = 全选 */
  status: TunnelStatus[] | "all";
  sort: TunnelSort;
  groupByServer: boolean;
  collapsedServers: string[];
}

export interface ServiceListPrefs {
  query: string;
  status: ServiceStatus[] | "all";
  sort: ServiceSort;
}

/** 筛选 chips 的顺序与文案（D4 §4.4 按钮 / 筛选 chips） */
export const TUNNEL_FILTERS: { value: TunnelStatus | "all"; label: string }[] = [
  { value: "all", label: "全部" },
  { value: "running", label: "正在发布" },
  { value: "disconnected", label: "掉线重连中" },
  { value: "error", label: "出错" },
  { value: "paused", label: "已暂停" },
  { value: "connecting", label: "连接中" },
];

export const SERVICE_FILTERS: { value: ServiceStatus | "all"; label: string }[] = [
  { value: "all", label: "全部" },
  { value: "running", label: "在跑" },
  { value: "stopped", label: "没启动" },
  { value: "starting", label: "正在启动" },
  { value: "not_listening", label: "端口不通" },
  { value: "failed", label: "起不来" },
];

export const TUNNEL_SORTS: { value: TunnelSort; label: string }[] = [
  { value: "default", label: "默认排序（有问题的排前面）" },
  { value: "name", label: "按名字" },
  { value: "port", label: "按云端端口" },
  { value: "server", label: "按服务器" },
  { value: "changed", label: "最近有变化" },
];

export const SERVICE_SORTS: { value: ServiceSort; label: string }[] = [
  { value: "default", label: "默认排序（有问题的排前面）" },
  { value: "name", label: "按名字" },
  { value: "port", label: "按本机端口" },
];

const TUNNEL_STORAGE_KEY = "listPrefs.tunnels";
const SERVICE_STORAGE_KEY = "listPrefs.services";

const TUNNEL_STATUS_VALUES: TunnelStatus[] = [
  "running",
  "connecting",
  "disconnected",
  "error",
  "paused",
];
const SERVICE_STATUS_VALUES: ServiceStatus[] = [
  "stopped",
  "starting",
  "running",
  "not_listening",
  "failed",
];

const DEFAULT_TUNNEL_PREFS: TunnelListPrefs = {
  query: "",
  status: "all",
  sort: "default",
  groupByServer: false,
  collapsedServers: [],
};

const DEFAULT_SERVICE_PREFS: ServiceListPrefs = {
  query: "",
  status: "all",
  sort: "default",
};

function isTunnelStatus(value: unknown): value is TunnelStatus {
  return typeof value === "string" && (TUNNEL_STATUS_VALUES as string[]).includes(value);
}

function isServiceStatus(value: unknown): value is ServiceStatus {
  return typeof value === "string" && (SERVICE_STATUS_VALUES as string[]).includes(value);
}

function isTunnelSort(value: unknown): value is TunnelSort {
  return value === "default" || value === "name" || value === "changed" || value === "port" || value === "server";
}

function isServiceSort(value: unknown): value is ServiceSort {
  return value === "default" || value === "name" || value === "port";
}

function readJson(key: string): unknown {
  try {
    const raw = localStorage.getItem(key);
    return raw ? (JSON.parse(raw) as unknown) : null;
  } catch {
    return null;
  }
}

function writeJson(key: string, value: unknown): void {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch {
    // 存不了就算了：偏好丢失不影响使用
  }
}

function loadTunnelPrefs(): TunnelListPrefs {
  const raw = readJson(TUNNEL_STORAGE_KEY) as Partial<TunnelListPrefs> | null;
  if (!raw || typeof raw !== "object") return { ...DEFAULT_TUNNEL_PREFS };
  const status =
    raw.status === "all"
      ? "all"
      : Array.isArray(raw.status)
        ? raw.status.filter(isTunnelStatus)
        : "all";
  return {
    query: typeof raw.query === "string" ? raw.query : "",
    status: Array.isArray(status) && status.length === 0 ? "all" : status,
    sort: isTunnelSort(raw.sort) ? raw.sort : "default",
    groupByServer: raw.groupByServer === true,
    collapsedServers: Array.isArray(raw.collapsedServers)
      ? raw.collapsedServers.filter((x): x is string => typeof x === "string")
      : [],
  };
}

function loadServicePrefs(): ServiceListPrefs {
  const raw = readJson(SERVICE_STORAGE_KEY) as Partial<ServiceListPrefs> | null;
  if (!raw || typeof raw !== "object") return { ...DEFAULT_SERVICE_PREFS };
  const status =
    raw.status === "all"
      ? "all"
      : Array.isArray(raw.status)
        ? raw.status.filter(isServiceStatus)
        : "all";
  return {
    query: typeof raw.query === "string" ? raw.query : "",
    status: Array.isArray(status) && status.length === 0 ? "all" : status,
    sort: isServiceSort(raw.sort) ? raw.sort : "default",
  };
}

let tunnelPrefs: TunnelListPrefs = loadTunnelPrefs();
let servicePrefs: ServiceListPrefs = loadServicePrefs();

const listeners = new Set<() => void>();

function emit(): void {
  for (const fn of listeners) fn();
}

function subscribeListState(fn: () => void): () => void {
  listeners.add(fn);
  return () => {
    listeners.delete(fn);
  };
}

export function getTunnelPrefs(): TunnelListPrefs {
  return tunnelPrefs;
}

export function getServicePrefs(): ServiceListPrefs {
  return servicePrefs;
}

export function setTunnelPrefs(patch: Partial<TunnelListPrefs>): void {
  tunnelPrefs = { ...tunnelPrefs, ...patch };
  writeJson(TUNNEL_STORAGE_KEY, tunnelPrefs);
  emit();
}

export function setServicePrefs(patch: Partial<ServiceListPrefs>): void {
  servicePrefs = { ...servicePrefs, ...patch };
  writeJson(SERVICE_STORAGE_KEY, servicePrefs);
  emit();
}

/** 空态「清空筛选」：搜索词与状态 chips 一起清掉（排序 / 分组保留） */
export function clearListFilters(page: ListPage): void {
  if (page === "tunnels") setTunnelPrefs({ query: "", status: "all" });
  else setServicePrefs({ query: "", status: "all" });
}

export function useTunnelPrefs(): TunnelListPrefs {
  return useSyncExternalStore(subscribeListState, getTunnelPrefs, getTunnelPrefs);
}

export function useServicePrefs(): ServiceListPrefs {
  return useSyncExternalStore(subscribeListState, getServicePrefs, getServicePrefs);
}

// ---------- 批量模式与勾选（D4 §3.1 bulkStore 的等价实现） ----------

const bulkMode: Record<ListPage, boolean> = { tunnels: false, services: false };
const selection: Record<ListPage, Set<string>> = {
  tunnels: new Set<string>(),
  services: new Set<string>(),
};

function subscribeBulk(fn: () => void): () => void {
  return subscribeListState(fn);
}

export function useBulkMode(page: ListPage): boolean {
  return useSyncExternalStore(
    subscribeBulk,
    () => bulkMode[page],
    () => bulkMode[page],
  );
}

export function useBulkSelection(page: ListPage): Set<string> {
  return useSyncExternalStore(
    subscribeBulk,
    () => selection[page],
    () => selection[page],
  );
}

/** 单张卡片只看自己有没有被勾选：勾选别人时它不用重渲染 */
export function useIsSelected(page: ListPage, id: string): boolean {
  return useSyncExternalStore(
    subscribeBulk,
    () => selection[page].has(id),
    () => selection[page].has(id),
  );
}

export function setBulkMode(page: ListPage, on: boolean): void {
  if (bulkMode[page] === on) return;
  bulkMode[page] = on;
  if (!on) selection[page] = new Set<string>();
  emit();
}

export function toggleSelect(page: ListPage, id: string, next?: boolean): void {
  const set = new Set(selection[page]);
  const want = next ?? !set.has(id);
  if (want) set.add(id);
  else set.delete(id);
  selection[page] = set;
  emit();
}

export function selectAll(page: ListPage, ids: string[]): void {
  selection[page] = new Set(ids);
  emit();
}

export function clearSelection(page: ListPage): void {
  if (selection[page].size === 0) return;
  selection[page] = new Set<string>();
  emit();
}

/** 批量动作结束后：处理成功的从勾选里去掉（失败的留着方便重试） */
export function removeFromSelection(page: ListPage, ids: string[]): void {
  if (ids.length === 0) return;
  const set = new Set(selection[page]);
  let changed = false;
  for (const id of ids) {
    if (set.delete(id)) changed = true;
  }
  if (!changed) return;
  selection[page] = set;
  emit();
}
