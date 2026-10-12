// 乐观覆盖与快照的合并规则（D4 §1.3，纯函数，可单测）。
//
// 三句话：
//   快照旧值不撤乐观；快照新值（≠乐观目标）撤乐观；命令失败立刻撤乐观并就地报错。
//
// 结构共享（reconcileSnapshot）：每项做 JSON 形状的结构比较（对象 / 数组递归），
// 全等就复用上一份的对象引用；卡片用 React.memo 默认浅比较时，快照没变 → 0 次卡片重渲染。

import type { Snapshot } from "../api";

/** 一次乐观覆盖（命令生命周期内有效） */
export interface Override {
  /** `tunnel:{id}` / `service:{id}` / `server:{id}` */
  key: string;
  /** 单调自增，后者赢 */
  seq: number;
  /** 动作开始时刻（epoch ms） */
  startedAt: number;
  /** 乐观值（盖在快照上） */
  patch: Record<string, unknown>;
  /** 期望值（快照达到即“已确认”） */
  expect: Record<string, unknown>;
  /** 动作前值（判断“被服务端改写”用） */
  before: Record<string, unknown>;
  /** 兜底存活时长 */
  ttlMs: number;
  /** 命令成功后给的额外确认窗口（now + 4s；到点还没确认就信服务端） */
  confirmDeadline?: number;
}

export type DropReason = "missing" | "confirmed" | "superseded" | "timeout";

export interface DroppedOverride {
  override: Override;
  reason: DropReason;
}

export interface MergeResult {
  snapshot: Snapshot;
  overrides: Map<string, Override>;
  dropped: DroppedOverride[];
}

type Entity = { id: string } & Record<string, unknown>;

/** 值比较：JSON 形状的深比较（对象 / 数组递归，其余 Object.is）。
 * 后端每次快照都会新建对象 / 数组，只比引用就没法做结构共享。 */
function sameValue(a: unknown, b: unknown): boolean {
  if (Object.is(a, b)) return true;
  if (typeof a !== "object" || typeof b !== "object" || a === null || b === null) return false;
  if (Array.isArray(a) || Array.isArray(b)) {
    if (!Array.isArray(a) || !Array.isArray(b) || a.length !== b.length) return false;
    for (let i = 0; i < a.length; i++) {
      if (!sameValue(a[i], b[i])) return false;
    }
    return true;
  }
  const ak = Object.keys(a);
  const bk = Object.keys(b);
  if (ak.length !== bk.length) return false;
  for (const k of ak) {
    if (!sameValue((a as Record<string, unknown>)[k], (b as Record<string, unknown>)[k])) return false;
  }
  return true;
}

function reconcileList<T extends { id: string }>(prev: T[], next: T[]): T[] {
  if (prev === next) return prev;
  const byId = new Map<string, T>();
  for (const item of prev) byId.set(item.id, item);
  const out = next.map((item) => {
    const old = byId.get(item.id);
    return old && sameValue(old, item) ? old : item;
  });
  let unchanged = out.length === prev.length;
  if (unchanged) {
    for (let i = 0; i < out.length; i++) {
      if (out[i] !== prev[i]) {
        unchanged = false;
        break;
      }
    }
  }
  return unchanged ? prev : out;
}

/**
 * 结构共享：结构相等的项复用 prev 的对象引用；整体没变时连数组引用也复用。
 * 这是「200 条顺滑」的关键。
 */
export function reconcileSnapshot(prev: Snapshot, next: Snapshot): Snapshot {
  if (prev === next) return prev;
  const services = reconcileList(prev.services, next.services);
  const tunnels = reconcileList(prev.tunnels, next.tunnels);
  const servers = reconcileList(prev.servers, next.servers);
  const app = sameValue(prev.app, next.app) ? prev.app : next.app;
  if (services === prev.services && tunnels === prev.tunnels && servers === prev.servers && app === prev.app) {
    return prev;
  }
  return { app, services, tunnels, servers };
}

function isConfirmed(expect: Record<string, unknown>, entity: Entity): boolean {
  for (const k of Object.keys(expect)) {
    if (!Object.is(entity[k], expect[k])) return false;
  }
  return true;
}

function isSuperseded(override: Override, entity: Entity): boolean {
  // 检查 expect ∪ patch 的所有字段：只在 patch 里、不在 expect 里的字段（如 restart 的
  // lastFailure / attempt）也参与「被服务端改写」判断，否则它们永远不会被撤掉。
  // __deleted 只是删除标记，不参与字段比较。
  const keys = new Set([...Object.keys(override.expect), ...Object.keys(override.patch)]);
  for (const k of keys) {
    if (k === "__deleted") continue;
    const value = entity[k];
    if (!Object.is(value, override.before[k]) && !Object.is(value, override.expect[k])) {
      return true;
    }
  }
  return false;
}

function applyPatch(entity: Entity, patch: Record<string, unknown>): Entity {
  let changed = false;
  for (const k of Object.keys(patch)) {
    if (k === "__deleted") continue;
    if (!Object.is(entity[k], patch[k])) {
      changed = true;
      break;
    }
  }
  return changed ? ({ ...entity, ...patch } as Entity) : entity;
}

function splitKey(key: string): { kind: string; id: string } | null {
  const at = key.indexOf(":");
  if (at <= 0 || at === key.length - 1) return null;
  return { kind: key.slice(0, at), id: key.slice(at + 1) };
}

function listFor(snapshot: Snapshot, kind: string): Entity[] | null {
  if (kind === "tunnel") return snapshot.tunnels as unknown as Entity[];
  if (kind === "service") return snapshot.services as unknown as Entity[];
  if (kind === "server") return snapshot.servers as unknown as Entity[];
  return null;
}

/**
 * 把覆盖表合并到已经过结构共享的 base 上。
 * 返回新的覆盖表（已确认 / 被改写 / 超时的会被丢掉）。
 */
export function applyOverrides(
  base: Snapshot,
  overrides: Map<string, Override>,
  now: number,
): MergeResult {
  if (overrides.size === 0) return { snapshot: base, overrides, dropped: [] };

  const dropped: DroppedOverride[] = [];
  const kept = new Map<string, Override>();
  const patches = new Map<string, Record<string, unknown>>();
  const deletions = new Set<string>();

  for (const [key, override] of overrides) {
    const parsed = splitKey(key);
    const list = parsed ? listFor(base, parsed.kind) : null;
    if (!parsed || !list) {
      kept.set(key, override);
      continue;
    }
    const entity = list.find((e) => e.id === parsed.id);
    const isDelete = override.patch.__deleted === true;

    // 2. 实体在 base 里不存在（被删）
    if (!entity) {
      if (isDelete) kept.set(key, override);
      else dropped.push({ override, reason: "missing" });
      continue;
    }
    // 3. 已确认：服务端认了（删除类覆盖没有字段可对，等实体真的消失或命令显式丢弃）
    if (!isDelete && isConfirmed(override.expect, entity)) {
      dropped.push({ override, reason: "confirmed" });
      continue;
    }
    // 4. 被服务端改写：以服务端为准
    if (isSuperseded(override, entity)) {
      if (typeof console !== "undefined") {
        console.debug("[optimistic] superseded", key);
      }
      dropped.push({ override, reason: "superseded" });
      continue;
    }
    // 5. 超时（命令成功后 4s 的确认窗口，或 30s 兜底）
    const deadline = override.confirmDeadline ?? override.startedAt + override.ttlMs;
    if (now > deadline) {
      dropped.push({ override, reason: "timeout" });
      continue;
    }
    // 6. 否则重新盖上去
    if (isDelete) {
      deletions.add(key);
      kept.set(key, override);
      continue;
    }
    patches.set(key, override.patch);
    kept.set(key, override);
  }

  const mergeList = <T extends { id: string }>(list: T[], kind: "tunnel" | "service" | "server"): { list: T[]; dirty: boolean } => {
    let dirty = false;
    const out: T[] = [];
    for (const item of list) {
      const key = `${kind}:${item.id}`;
      if (deletions.has(key)) {
        dirty = true;
        continue;
      }
      const patch = patches.get(key);
      if (!patch) {
        out.push(item);
        continue;
      }
      const merged = applyPatch(item as Entity, patch) as T;
      if (merged !== item) dirty = true;
      out.push(merged);
    }
    return { list: dirty ? out : list, dirty };
  };

  const services = mergeList(base.services, "service");
  const tunnels = mergeList(base.tunnels, "tunnel");
  const servers = mergeList(base.servers, "server");
  const dirty = services.dirty || tunnels.dirty || servers.dirty;
  const snapshot: Snapshot = dirty
    ? { app: base.app, services: services.list, tunnels: tunnels.list, servers: servers.list }
    : base;

  return { snapshot, overrides: kept, dropped };
}

/**
 * D4 §1.3 的公开签名：结构共享 + 乐观合并，纯函数。
 * 需要知道哪些覆盖被丢掉的调用方用 applyOverrides。
 */
export function mergeSnapshot(
  prev: Snapshot,
  next: Snapshot,
  overrides: Map<string, Override>,
  now: number,
): Snapshot {
  const base = reconcileSnapshot(prev, next);
  return applyOverrides(base, overrides, now).snapshot;
}
