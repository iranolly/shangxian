// 列表派生选择器（D4 §1.8：selectVisibleTunnelIds 等）+ 结构比较工具。
// 纯计算都在这里；页面只订阅结果（结构共享保证不必要不重渲染）。

import type { ServiceView, TunnelView } from "../../api";
import { SERVICE_ORDER, TUNNEL_ORDER } from "../../copy/status";
import type { AppState } from "../../store/snapshotStore";
import {
  getServicePrefs,
  getTunnelPrefs,
  SERVICE_FILTERS,
  TUNNEL_FILTERS,
  type ServiceListPrefs,
  type TunnelListPrefs,
} from "./listPrefs";

interface TunnelGroup {
  serverId: string;
  serverName: string;
  count: number;
  downCount: number;
  ids: string[];
}

function normalize(text: string): string {
  return text.trim().toLowerCase();
}

function tunnelServerName(state: AppState, tunnel: TunnelView): string {
  return state.byServerId[tunnel.serverId]?.name ?? "";
}

function matchesTunnel(state: AppState, tunnel: TunnelView, query: string): boolean {
  if (!query) return true;
  const q = normalize(query);
  const hay = [
    tunnel.name,
    tunnel.url ?? "",
    String(tunnel.remotePort),
    String(tunnel.localPort),
    tunnel.serviceName,
    tunnelServerName(state, tunnel),
  ]
    .join(" ")
    .toLowerCase();
  return hay.includes(q);
}

function matchesService(service: ServiceView, query: string): boolean {
  if (!query) return true;
  const q = normalize(query);
  const hay = [
    service.name,
    String(service.localPort),
    service.note ?? "",
    service.launchExe ?? "",
  ]
    .join(" ")
    .toLowerCase();
  return hay.includes(q);
}

function compareName(a: string, b: string, idA: string, idB: string): number {
  const byName = a.localeCompare(b, "zh-Hans-CN");
  if (byName !== 0) return byName;
  return idA < idB ? -1 : idA > idB ? 1 : 0;
}

function tunnelChangedAt(tunnel: TunnelView): number {
  return Math.max(
    tunnel.lastFailure?.ts ?? 0,
    tunnel.protectionCheck?.ts ?? 0,
    tunnel.connectedSince ?? 0,
  );
}

function sortTunnels(state: AppState, list: TunnelView[], prefs: TunnelListPrefs): TunnelView[] {
  const out = [...list];
  const byServer = (t: TunnelView) => tunnelServerName(state, t) || "\uffff";
  out.sort((a, b) => {
    switch (prefs.sort) {
      case "name":
        return compareName(a.name, b.name, a.id, b.id);
      case "port": {
        const d = a.remotePort - b.remotePort;
        return d !== 0 ? d : a.id < b.id ? -1 : 1;
      }
      case "server": {
        const d = compareName(byServer(a), byServer(b), a.id, b.id);
        return d !== 0 ? d : a.remotePort - b.remotePort;
      }
      case "changed": {
        const d = tunnelChangedAt(b) - tunnelChangedAt(a);
        return d !== 0 ? d : a.id < b.id ? -1 : 1;
      }
      default: {
        const d = TUNNEL_ORDER[a.status] - TUNNEL_ORDER[b.status];
        return d !== 0 ? d : a.id < b.id ? -1 : 1;
      }
    }
  });
  return out;
}

function sortServices(list: ServiceView[], prefs: ServiceListPrefs): ServiceView[] {
  const out = [...list];
  out.sort((a, b) => {
    switch (prefs.sort) {
      case "name":
        return compareName(a.name, b.name, a.id, b.id);
      case "port": {
        const d = a.localPort - b.localPort;
        return d !== 0 ? d : a.id < b.id ? -1 : 1;
      }
      default: {
        const d = SERVICE_ORDER[a.status] - SERVICE_ORDER[b.status];
        return d !== 0 ? d : a.id < b.id ? -1 : 1;
      }
    }
  });
  return out;
}

function tunnelStatusMatches(tunnel: TunnelView, prefs: TunnelListPrefs): boolean {
  if (prefs.status === "all") return true;
  return prefs.status.includes(tunnel.status);
}

function serviceStatusMatches(service: ServiceView, prefs: ServiceListPrefs): boolean {
  if (prefs.status === "all") return true;
  return prefs.status.includes(service.status);
}

/** 当前筛选 + 排序后的隧道清单（页面内部用） */
function pickTunnels(state: AppState): TunnelView[] {
  const prefs = getTunnelPrefs();
  const filtered = state.tunnels.filter(
    (t) => tunnelStatusMatches(t, prefs) && matchesTunnel(state, t, prefs.query),
  );
  return sortTunnels(state, filtered, prefs);
}

function pickServices(state: AppState): ServiceView[] {
  const prefs = getServicePrefs();
  const filtered = state.services.filter(
    (s) => serviceStatusMatches(s, prefs) && matchesService(s, prefs.query),
  );
  return sortServices(filtered, prefs);
}

// ---------- useStore 选择器 ----------

export function selectTunnelTotal(state: AppState): number {
  return state.tunnels.length;
}

export function selectTunnelIds(state: AppState): string[] {
  return pickTunnels(state).map((t) => t.id);
}

/** 分组结构（未分组时页面不用它，但保持订阅口径一致） */
export function selectTunnelGroups(state: AppState): TunnelGroup[] {
  const items = pickTunnels(state);
  const order: string[] = [];
  const buckets = new Map<string, TunnelView[]>();
  for (const t of items) {
    let list = buckets.get(t.serverId);
    if (!list) {
      list = [];
      buckets.set(t.serverId, list);
      order.push(t.serverId);
    }
    list.push(t);
  }
  const serverLabel = (serverId: string) => state.byServerId[serverId]?.name ?? "\uffff";
  order.sort((a, b) => compareName(serverLabel(a), serverLabel(b), a, b));
  return order.map((serverId) => {
    const list = buckets.get(serverId) ?? [];
    return {
      serverId,
      serverName: state.byServerId[serverId]?.name ?? "（服务器已删除）",
      count: list.length,
      downCount: list.filter((t) => t.status === "disconnected" || t.status === "error").length,
      ids: list.map((t) => t.id),
    };
  });
}

/**
 * 筛选 chips 上的计数：只受搜索词影响，不受状态筛选影响
 * （否则选中一个 chip 后其它 chip 会全部变 0，没法切换）。
 * 顺序与 TUNNEL_FILTERS 一致。
 */
export function selectTunnelStatusCounts(state: AppState): number[] {
  const prefs = getTunnelPrefs();
  const base = state.tunnels.filter((t) => matchesTunnel(state, t, prefs.query));
  return TUNNEL_FILTERS.map((f) =>
    f.value === "all" ? base.length : base.filter((t) => t.status === f.value).length,
  );
}

export function selectServiceIds(state: AppState): string[] {
  return pickServices(state).map((s) => s.id);
}

export function selectServiceStatusCounts(state: AppState): number[] {
  const prefs = getServicePrefs();
  const base = state.services.filter((s) => matchesService(s, prefs.query));
  return SERVICE_FILTERS.map((f) =>
    f.value === "all" ? base.length : base.filter((s) => s.status === f.value).length,
  );
}

// ---------- 结构比较（传给 useStore 的 isEqual） ----------

export function sameStringArray(a: string[], b: string[]): boolean {
  if (a === b) return true;
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) {
    if (a[i] !== b[i]) return false;
  }
  return true;
}

export function sameNumberArray(a: number[], b: number[]): boolean {
  if (a === b) return true;
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) {
    if (a[i] !== b[i]) return false;
  }
  return true;
}

export function sameTunnelGroups(a: TunnelGroup[], b: TunnelGroup[]): boolean {
  if (a === b) return true;
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) {
    const x = a[i];
    const y = b[i];
    if (
      x.serverId !== y.serverId ||
      x.serverName !== y.serverName ||
      x.count !== y.count ||
      x.downCount !== y.downCount ||
      !sameStringArray(x.ids, y.ids)
    ) {
      return false;
    }
  }
  return true;
}
