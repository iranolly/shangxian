// 写命令的统一收尾（D4 §1.3 / §1.4）：pending + 乐观 + 成功 toast + 失败就地。
//
// 组件 / 页面不直接 await 后端函数，走 actions.*（actions 再调这里的 runCommand）。

import { selectServiceIds, selectTunnelIds } from "../components/list/selectors";
import { clearNotice, setNotice } from "./noticeStore";
import { isPending, setPending } from "./pendingStore";
import { beginOverride, confirmOverride, dropOverride, getState, refreshNow } from "./snapshotStore";
import { toast, type ToastSpec } from "./toastStore";

export interface OptimisticSpec {
  patch: Record<string, unknown>;
  expect: Record<string, unknown>;
  before: Record<string, unknown>;
  ttlMs?: number;
}

export interface CommandFailure {
  /** 就地错误挂到哪个键；默认 = key */
  inlineKey?: string;
  /** true = 失败继续抛给调用方（页面自己 catch 的场景，如停止服务的二次确认） */
  rethrow?: boolean;
  /** true = 不写就地错误（调用方自己展示） */
  silent?: boolean;
  /** 失败 toast 里的动作词（实体不可见时用），如 "启动"；不填按乐观 patch 猜 */
  action?: string;
  /** 额外再弹一条 toast（一般不用：实体失败优先就地） */
  toast?: ToastSpec;
}

export interface RunOptions<T> {
  /** pending / 乐观键：`tunnel:{id}` / `service:{id}` / `server:{id}` */
  key: string;
  run: () => Promise<T>;
  /** ToastSpec，或按结果算一条（返回 null = 不弹，如 needsConfirm） */
  success?: ToastSpec | ((result: T) => ToastSpec | null | undefined) | null;
  optimistic?: OptimisticSpec;
  failure?: CommandFailure;
  /** true = 允许在“配置读不出来”时执行（只读类调用） */
  allowUntrusted?: boolean;
}

/** 后端错误统一转成人话文本（Tauri 出错时 reject 的通常是字符串）。 */
export function errorText(error: unknown): string {
  const text = error instanceof Error ? error.message : String(error);
  const trimmed = text.trim().replace(/^Error:\s*/, "");
  return trimmed || "出现问题，请稍后重试";
}

type EntityKind = "tunnel" | "service" | "server";

function splitEntityKey(key: string): { kind: EntityKind; id: string } | null {
  const at = key.indexOf(":");
  if (at <= 0 || at === key.length - 1) return null;
  const kind = key.slice(0, at);
  if (kind !== "tunnel" && kind !== "service" && kind !== "server") return null;
  return { kind, id: key.slice(at + 1) };
}

function entityName(kind: EntityKind, id: string): string {
  const state = getState();
  if (kind === "tunnel") return state.byTunnelId[id]?.name ?? "这条隧道";
  if (kind === "service") return state.byServiceId[id]?.name ?? "这台服务";
  return state.byServerId[id]?.name ?? "这台服务器";
}

/**
 * 失败时实体在不在「可见列表」：与页面算 visibleIds 用同一套筛选选择器。
 * 被筛掉（搜索 / 状态 chips 不匹配）→ 就地卡片不会渲染，改走 toast + 定位。
 * 注意：这里只判筛选，不判当前页；不在当前页时 App 顶部横幅兜底。
 */
function isEntityVisible(kind: EntityKind, id: string): boolean {
  const state = getState();
  if (kind === "server") return Boolean(state.byServerId[id]);
  const ids = kind === "tunnel" ? selectTunnelIds(state) : selectServiceIds(state);
  return ids.includes(id);
}

/** 失败 toast 里的动作词：优先调用方给的，否则按乐观 patch 猜一个能读的。 */
function actionLabel(opts: { failure?: CommandFailure; optimistic?: OptimisticSpec }): string {
  if (opts.failure?.action) return opts.failure.action;
  const patch = opts.optimistic?.patch;
  if (!patch) return "操作";
  if (patch.__deleted === true) return "删除";
  if (patch.enabled === true) return "开始发布";
  if (patch.enabled === false) return "暂停发布";
  if (patch.watch !== undefined) return patch.watch ? "开启看护" : "关闭看护";
  if (patch.autostart !== undefined) return patch.autostart ? "开启自启" : "关闭自启";
  if (patch.status === "starting") return "启动";
  if (patch.status === "stopped") return "停止";
  if (patch.status === "paused") return "暂停";
  return "操作";
}

/** 实体不在可见位置：toast 里给「去看」，点了跳页 + 清筛选定位（页面自己处理）。 */
function toastFailureOutsideView(kind: EntityKind, id: string, action: string, reason: string): void {
  const name = entityName(kind, id);
  const page = kind === "tunnel" ? "tunnels" : kind === "service" ? "services" : "servers";
  toast.warn(`「${name}」${action}失败：${reason}`, {
    key: `cmd-fail:${kind}:${id}`,
    action: {
      label: "去看",
      run: () => {
        // F6 契约：nav-intent {locate}（App 处理）；shangxian:locate 是 App 现在就在走的通道，
        // 两者都发，F6 落地前后「去看」都能真的跳页 + 清筛选。
        window.dispatchEvent(
          new CustomEvent("shangxian:nav-intent", {
            detail: { kind: "locate", page, entity: { type: kind, id, name } },
          }),
        );
        window.dispatchEvent(
          new CustomEvent("shangxian:locate", { detail: { type: kind, id, name } }),
        );
      },
    },
  });
}

/** 后端卡住、命令永不 resolve 时，不能让「处理中…」一直转（30s 清 pending + 提示一次）。 */
const WATCHDOG_MS = 30_000;

function startWatchdog(key: string): () => void {
  const timer = window.setTimeout(() => {
    if (!isPending(key)) return;
    setPending(key, false);
    const target = splitEntityKey(key);
    const name = target ? entityName(target.kind, target.id) : "这项操作";
    toast.warn(`操作无响应：${name} 可能未成功，请稍后查看`, {
      key: `cmd-timeout:${key}`,
      action: { label: "刷新", run: () => refreshNow() },
    });
  }, WATCHDOG_MS);
  return () => window.clearTimeout(timer);
}

/**
 * 执行一个写命令。同一个键已有 pending 时直接返回 undefined（防连点，UI 也应同时禁用）。
 */
export async function runCommand<T>(opts: RunOptions<T>): Promise<T | undefined> {
  const key = opts.key;
  if (!getState().app.configTrusted && !opts.allowUntrusted) {
    const text = "配置文件无法读取，请先修复后再修改设置";
    if (opts.failure?.rethrow) throw new Error(text);
    setNotice(opts.failure?.inlineKey ?? key, text);
    return undefined;
  }
  if (isPending(key)) {
    if (opts.failure?.rethrow) throw new Error("上一步还在处理，请稍后重试");
    return undefined;
  }

  setPending(key, true);
  if (opts.optimistic) beginOverride(key, opts.optimistic);
  // 新动作开始 = 上一次的报错已经过时，先收掉
  clearNotice(opts.failure?.inlineKey ?? key);
  const stopWatchdog = startWatchdog(key);
  try {
    const result = await opts.run();
    stopWatchdog();
    setPending(key, false);
    if (opts.optimistic) confirmOverride(key, 4000);
    const spec = typeof opts.success === "function" ? opts.success(result) : opts.success;
    if (spec) toast.show(spec);
    return result;
  } catch (error) {
    stopWatchdog();
    setPending(key, false);
    if (opts.optimistic) dropOverride(key);
    const failure = opts.failure ?? {};
    const reason = errorText(error);
    if (!failure.silent) {
      const noticeKey = failure.inlineKey ?? key;
      const target = splitEntityKey(noticeKey);
      const visible = target ? isEntityVisible(target.kind, target.id) : true;
      setNotice(noticeKey, reason, visible);
      if (target && !visible) {
        toastFailureOutsideView(target.kind, target.id, actionLabel(opts), reason);
      }
    }
    if (failure.toast) toast.show(failure.toast);
    if (failure.rethrow) throw error;
    return undefined;
  }
}
