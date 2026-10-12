// 模块级动作单例（D4 §3.1）：本波（E2）扩展出列表 / 卡片 / 批量 / 删除撤销 / 克隆用到的动作。
// 组件 / 页面把 actions.* 直接当回调传：引用稳定，卡片 memo 才有意义。
//
// 铁律：所有写操作都走这里 → runCommand（pending + 乐观 + 成功 toast + 失败就地）。

import {
  addTunnel,
  checkTunnelCloud,
  deleteServer as deleteServerApi,
  removeService as removeServiceApi,
  removeTunnel as removeTunnelApi,
  restoreDeleted,
  saveServer,
  saveService,
  serviceStart,
  serviceStop,
  setServiceAutostart,
  setServiceWatch,
  setSettings as saveSettingsApi,
  testServer,
  tunnelStart,
  tunnelStop,
  updateTunnel,
  type AccessCheck,
  type CloudCheckResult,
  type NewTunnelInput,
  type ServerInput,
  type ServiceInput,
  type ServiceStopResult,
  type Settings,
  type TunnelPreset,
  type UpdateTunnelInput,
} from "../api";
import { copyText as copyToClipboard } from "../copy/clipboard";
import { T } from "../copy/toasts";
import { errorText, runCommand } from "./commands";
import { setNotice } from "./noticeStore";
import { dropOverride, getState, refreshNow } from "./snapshotStore";
import { toast } from "./toastStore";

// ---------- 小工具 ----------

function serviceName(id: string): string {
  return getState().byServiceId[id]?.name ?? "这台服务";
}

function tunnelName(id: string): string {
  return getState().byTunnelId[id]?.name ?? "这条隧道";
}

function serverName(id: string): string {
  return getState().byServerId[id]?.name ?? "这台服务器";
}

/** 后端 remove_* / delete_server 的返回（api.ts 现在标成 void，运行时是真对象） */
interface DeleteResult {
  deleted?: { type: "tunnel" | "service" | "server"; id: string; name: string } | null;
  undoToken?: string | null;
  undoExpiresAt?: number | null;
}

function asDeleteResult(raw: unknown): DeleteResult | undefined {
  return raw && typeof raw === "object" ? (raw as DeleteResult) : undefined;
}

/** 撤销令牌 → 乐观覆盖键：撤销后要把「隐藏」的覆盖撤掉，实体才会重新出现 */
const undoKeys = new Map<string, string>();

function showDeletedToast(text: string, token: string | null, name: string): void {
  toast.success(text, {
    durationMs: 5000,
    key: token ? `deleted:${token}` : undefined,
    action: token ? { label: "撤销", run: () => void actions.undo(token, name) } : undefined,
  });
}

function cloudSummary(actual: AccessCheck | null | undefined): string {
  if (!actual) return "云端没有返回实际配置";
  if (actual.passwordOn && actual.ipOn) return "密码 + IP 白名单";
  if (actual.passwordOn) return "已加密";
  if (actual.ipOn) return `仅白名单 IP（${actual.allowIps.length} 个）`;
  return "无访问保护";
}

// ---------- 批量 ----------

export type BulkAction = "start" | "pause" | "remove";

export interface BulkOutcomeFailure {
  id: string;
  name: string;
  error: string;
}

export interface BulkOutcome {
  ok: number;
  succeeded: string[];
  failures: BulkOutcomeFailure[];
  tokens: { token: string; name: string }[];
  stopped: boolean;
}

export interface BulkOptions {
  onProgress?(done: number, total: number): void;
  /** true = 停止调度剩下的（已发出的不等） */
  isStopped?(): boolean;
}

/** 改访问保护时用：只提交 id + 访问三件套，发布字段由 actions 从最新快照补齐 */
export interface TunnelProtectionInput {
  id: string;
  access: string[];
  accessUser?: string | null;
  accessHash?: string | null;
  accessDetail?: string | null;
}

/** 克隆弹层交给 actions.tunnelClone 的草稿（serviceId 沿用源隧道，不在草稿里） */
export interface CloneDraft {
  name: string;
  serverId: string;
  remotePort: number;
  url: string | null;
  preset: TunnelPreset;
  access: string[];
  accessUser: string | null;
  accessHash: string | null;
  accessDetail: string | null;
}

type BulkItemResult = { ok: true; token?: string } | { ok: false; error: string };

async function runBulkItem(
  kind: "tunnel" | "service",
  action: BulkAction,
  id: string,
): Promise<BulkItemResult> {
  const key = `${kind}:${id}`;
  try {
    if (action === "remove") {
      const raw = await runCommand<DeleteResult | undefined>({
        key,
        optimistic: { patch: { __deleted: true }, expect: {}, before: {} },
        run: () =>
          (kind === "tunnel"
            ? removeTunnelApi(id)
            : removeServiceApi(id)) as unknown as Promise<DeleteResult | undefined>,
        success: null,
        failure: { rethrow: true },
      });
      const token = asDeleteResult(raw)?.undoToken ?? null;
      if (token) undoKeys.set(token, key);
      return { ok: true, token: token ?? undefined };
    }

    if (kind === "tunnel") {
      const current = getState().byTunnelId[id];
      if (action === "start") {
        await runCommand({
          key,
          optimistic: {
            patch: { enabled: true, status: "connecting", lastFailure: null },
            expect: { enabled: true },
            before: {
              enabled: current?.enabled,
              status: current?.status,
              lastFailure: current?.lastFailure,
            },
          },
          run: () => tunnelStart(id),
          success: null,
          failure: { rethrow: true },
        });
      } else {
        await runCommand({
          key,
          optimistic: {
            patch: { enabled: false, status: "paused" },
            expect: { enabled: false },
            before: { enabled: current?.enabled, status: current?.status },
          },
          run: () => tunnelStop(id),
          success: null,
          failure: { rethrow: true },
        });
      }
      return { ok: true };
    }

    const current = getState().byServiceId[id];
    if (action === "start") {
      await runCommand({
        key,
        optimistic: {
          patch: { status: "starting" },
          expect: { status: "running" },
          before: { status: current?.status },
        },
        run: () => serviceStart(id),
        success: null,
        failure: { rethrow: true },
      });
      return { ok: true };
    }

    const stopped = await runCommand<ServiceStopResult>({
      key,
      optimistic: {
        patch: { status: "stopped" },
        expect: { status: "stopped" },
        before: { status: current?.status },
      },
      run: () => serviceStop(id),
      success: null,
      failure: { rethrow: true },
    });
    if (stopped?.kind === "needsConfirm") {
      dropOverride(key);
      return {
        ok: false,
        error: "这个端口不是本应用启动的程序，无法批量停止；请在卡片上单独停止",
      };
    }
    return { ok: true };
  } catch (e) {
    return { ok: false, error: errorText(e) };
  }
}

async function runBulk(
  kind: "tunnel" | "service",
  action: BulkAction,
  ids: string[],
  opts?: BulkOptions,
): Promise<BulkOutcome> {
  const nameOf = kind === "tunnel" ? tunnelName : serviceName;
  const queue = [...ids];
  const total = queue.length;
  let done = 0;
  let stopped = false;
  const succeeded: string[] = [];
  const failures: BulkOutcomeFailure[] = [];
  const tokens: { token: string; name: string }[] = [];

  const worker = async () => {
    for (;;) {
      if (opts?.isStopped?.()) {
        stopped = true;
        return;
      }
      const id = queue.shift();
      if (id === undefined) return;
      const result = await runBulkItem(kind, action, id);
      done += 1;
      if (result.ok) {
        succeeded.push(id);
        if (result.token) tokens.push({ token: result.token, name: nameOf(id) });
      } else {
        failures.push({ id, name: nameOf(id), error: result.error });
      }
      opts?.onProgress?.(done, total);
    }
  };

  await Promise.all(Array.from({ length: Math.min(3, total) }, worker));
  return { ok: succeeded.length, succeeded, failures, tokens, stopped };
}

// ---------- 动作表 ----------

export const actions = {
  // ---------- 本机服务 ----------

  serviceStart(id: string): void {
    const current = getState().byServiceId[id];
    void runCommand({
      key: `service:${id}`,
      optimistic: {
        patch: { status: "starting" },
        expect: { status: "running" },
        before: { status: current?.status },
      },
      run: () => serviceStart(id),
      success: { kind: "success", text: T.serviceStarting(serviceName(id)) },
    });
  },

  /**
   * 停止：第一次可能返回 needsConfirm（不是本应用启动的进程），
   * 页面拿到结果后弹 ConfirmModal；失败会抛给调用方（runCommand 已把原因就地挂好）。
   */
  async serviceStop(
    id: string,
    killForeign = false,
    expectedPid: number | null = null,
  ): Promise<ServiceStopResult> {
    const current = getState().byServiceId[id];
    const result = await runCommand<ServiceStopResult>({
      key: `service:${id}`,
      optimistic: {
        patch: { status: "stopped" },
        expect: { status: "stopped" },
        before: { status: current?.status },
      },
      run: () => serviceStop(id, killForeign, expectedPid),
      success: (r) =>
        r.kind === "needsConfirm" ? null : { kind: "success", text: T.serviceStopped(serviceName(id)) },
      failure: { rethrow: true },
    });
    if (result === undefined) throw new Error("停止失败：上一步还在处理，请稍后重试");
    // 需要确认时其实什么都没停：把乐观值撤掉，别让卡片装成已停止
    if (result.kind === "needsConfirm") dropOverride(`service:${id}`);
    return result;
  },

  serviceToggleWatch(id: string, on: boolean): void {
    const current = getState().byServiceId[id];
    void runCommand({
      key: `service:${id}`,
      optimistic: {
        patch: { watch: on },
        expect: { watch: on },
        before: { watch: current?.watch },
      },
      run: () => setServiceWatch(id, on),
      success: { kind: "success", text: on ? T.watchOn(serviceName(id)) : T.watchOff(serviceName(id)) },
    });
  },

  serviceToggleAutostart(id: string, on: boolean): void {
    const current = getState().byServiceId[id];
    void runCommand({
      key: `service:${id}`,
      optimistic: {
        patch: { autostart: on },
        expect: { autostart: on },
        before: { autostart: current?.autostart },
      },
      run: () => setServiceAutostart(id, on),
      success: { kind: "success", text: on ? T.autostartOn() : T.autostartOff() },
    });
  },

  async serviceSave(input: ServiceInput): Promise<string | undefined> {
    return await runCommand<string>({
      key: input.id ? `service:${input.id}` : "service:new",
      run: () => saveService(input),
      success: { kind: "success", text: T.serviceSaved(input.name) },
      failure: { rethrow: true, silent: true },
    });
  },

  /** 删除服务：成功 toast + 5s 撤销；被隧道引用时后端会拒绝（就地错误） */
  async serviceRemove(id: string): Promise<void> {
    const name = serviceName(id);
    try {
      const raw = await runCommand<DeleteResult | undefined>({
        key: `service:${id}`,
        optimistic: { patch: { __deleted: true }, expect: {}, before: {} },
        run: () => removeServiceApi(id) as unknown as Promise<DeleteResult | undefined>,
        success: null,
        failure: { rethrow: true },
      });
      const token = asDeleteResult(raw)?.undoToken ?? null;
      if (token) undoKeys.set(token, `service:${id}`);
      showDeletedToast(T.serviceRemoved(name), token, name);
    } catch {
      // runCommand 已把原因挂在卡片上（就地错误）
    }
  },

  // ---------- 发布隧道 ----------

  tunnelStart(id: string): void {
    const current = getState().byTunnelId[id];
    void runCommand({
      key: `tunnel:${id}`,
      optimistic: {
        patch: { enabled: true, status: "connecting", lastFailure: null },
        expect: { enabled: true },
        before: { enabled: current?.enabled, status: current?.status, lastFailure: current?.lastFailure },
      },
      run: () => tunnelStart(id),
      success: { kind: "success", text: T.tunnelStarting(tunnelName(id)) },
    });
  },

  tunnelPause(id: string): void {
    const current = getState().byTunnelId[id];
    void runCommand({
      key: `tunnel:${id}`,
      optimistic: {
        patch: { enabled: false, status: "paused" },
        expect: { enabled: false },
        before: { enabled: current?.enabled, status: current?.status },
      },
      run: () => tunnelStop(id),
      success: { kind: "success", text: T.tunnelPaused(tunnelName(id)) },
    });
  },

  /** 删除隧道：成功 toast + 5s 撤销；失败就地（卡片修复带） */
  async tunnelRemove(id: string): Promise<void> {
    const name = tunnelName(id);
    try {
      const raw = await runCommand<DeleteResult | undefined>({
        key: `tunnel:${id}`,
        optimistic: { patch: { __deleted: true }, expect: {}, before: {} },
        run: () => removeTunnelApi(id) as unknown as Promise<DeleteResult | undefined>,
        success: null,
        failure: { rethrow: true },
      });
      const token = asDeleteResult(raw)?.undoToken ?? null;
      if (token) undoKeys.set(token, `tunnel:${id}`);
      showDeletedToast(T.tunnelRemoved(name), token, name);
    } catch {
      // runCommand 已把原因挂在卡片上（就地错误）
    }
  },

  /** 发布向导提交：新建隧道；失败抛给向导就地展示（成功屏自己负责告知） */
  async tunnelAdd(input: NewTunnelInput): Promise<string | undefined> {
    return await runCommand<string>({
      key: "tunnel:new",
      run: () => addTunnel(input),
      success: null,
      failure: { rethrow: true, silent: true },
    });
  },

  /**
   * 只改访问三件套（改访问保护专用）。
   * 名称 / 云端端口 / 网址一律从 store 取最新值：弹层打开时的旧快照不回传，
   * 免得把别处刚改好的发布字段覆盖回去。
   */
  async tunnelProtectionSave(input: TunnelProtectionInput): Promise<void> {
    const current = getState().byTunnelId[input.id];
    if (!current) throw new Error("这条隧道已不存在，请刷新后重试");
    await runCommand({
      key: `tunnel:${input.id}`,
      run: () =>
        updateTunnel({
          id: input.id,
          name: current.name,
          remotePort: current.remotePort,
          url: current.url ?? null,
          access: input.access,
          accessUser: input.accessUser ?? null,
          accessDetail: input.accessDetail ?? null,
          // 没给新密码就整个字段都不传：后端保持原密码不变。
          ...(input.accessHash ? { accessHash: input.accessHash } : {}),
        }),
      success: { kind: "success", text: T.protectSaved() },
      failure: { rethrow: true, silent: true },
    });
  },

  async tunnelSave(input: UpdateTunnelInput): Promise<boolean> {
    try {
      await runCommand({
        key: `tunnel:${input.id}`,
        run: () => updateTunnel(input),
        success: { kind: "success", text: T.tunnelSaved(input.name) },
        failure: { rethrow: true, silent: true },
      });
      return true;
    } catch (e) {
      setNotice(`tunnel:${input.id}`, errorText(e));
      return false;
    }
  },

  /** 克隆：建好后立刻暂停（D2 的 add_tunnel 没有 enabled 输入）；返回新隧道 id */
  async tunnelClone(
    sourceId: string,
    draft: CloneDraft,
    openEdit?: (id: string) => void,
  ): Promise<string | undefined> {
    const source = getState().byTunnelId[sourceId];
    if (!source) {
      toast.error("这条隧道已不存在，请刷新后重试");
      return undefined;
    }
    try {
      const id = await runCommand<string>({
        key: `tunnel:${sourceId}`,
        run: () =>
          addTunnel({
            name: draft.name,
            serviceId: source.serviceId,
            serverId: draft.serverId,
            remotePort: draft.remotePort,
            url: draft.url,
            access: draft.access,
            accessUser: draft.accessUser,
            accessHash: draft.accessHash,
            accessDetail: draft.accessDetail,
            preset: draft.preset,
          }),
        success: null,
        failure: { rethrow: true, silent: true },
      });
      if (!id) return undefined;
      try {
        await tunnelStop(id);
      } catch {
        // 建好了但没停住：仍然算克隆成功，快照会显示真实状态
      }
      toast.success(T.tunnelCloned(draft.name), {
        action: openEdit ? { label: "去编辑", run: () => openEdit(id) } : undefined,
      });
      refreshNow();
      return id;
    } catch (e) {
      setNotice(`tunnel:${sourceId}`, `克隆失败：${errorText(e)}`);
      throw e;
    }
  },

  // ---------- 云端 ----------

  /** 立刻核对云端访问保护；结果写快照，这里只给成功 / 失败提示 */
  async checkCloud(id: string): Promise<void> {
    const key = `tunnel:${id}`;
    try {
      await runCommand<CloudCheckResult>({
        key,
        allowUntrusted: true,
        run: () => checkTunnelCloud(id, "full"),
        success: (r) => {
          const status = r.check?.status;
          if (status === "drift") {
            return { kind: "info", text: T.protectDrift(cloudSummary(r.actual)) };
          }
          if (status === "error") {
            return { kind: "warn", text: `云端核对未通过：${r.check?.error ?? "原因不明"}` };
          }
          return { kind: "success", text: T.protectVerified(cloudSummary(r.actual)) };
        },
        failure: { rethrow: true, silent: true },
      });
      refreshNow();
    } catch (e) {
      setNotice(
        key,
        `检查失败：${errorText(e)}；也可能是服务器未启动、密钥不正确，请先点击「体检」查看`,
      );
    }
  },

  // ---------- 服务器（E4 的页面直接复用） ----------

  async serverSave(input: ServerInput): Promise<string | undefined> {
    return await runCommand<string>({
      key: input.id ? `server:${input.id}` : "server:new",
      run: () => saveServer(input),
      success: { kind: "success", text: T.serverSaved(input.name) },
      failure: { rethrow: true, silent: true },
    });
  },

  async serverRemove(id: string): Promise<void> {
    const name = serverName(id);
    try {
      const raw = await runCommand<DeleteResult | undefined>({
        key: `server:${id}`,
        optimistic: { patch: { __deleted: true }, expect: {}, before: {} },
        run: () => deleteServerApi(id) as unknown as Promise<DeleteResult | undefined>,
        success: null,
        failure: { rethrow: true, silent: true },
      });
      const token = asDeleteResult(raw)?.undoToken ?? null;
      if (token) undoKeys.set(token, `server:${id}`);
      showDeletedToast(T.serverRemoved(name), token, name);
    } catch (e) {
      toast.error(`删除失败：${errorText(e)}`);
    }
  },

  /** 测试连接：pending 走 `server:{id}`；成功返回后端那句人话，失败统一挂到卡片 */
  async serverTest(id: string): Promise<string | undefined> {
    try {
      return await runCommand<string>({
        key: `server:${id}`,
        run: () => testServer(id),
        success: { kind: "success", text: T.serverOk(serverName(id)) },
        failure: { rethrow: true, silent: true },
      });
    } catch (e) {
      setNotice(`server:${id}`, errorText(e));
      return undefined;
    }
  },

  // ---------- 撤销 / 批量 ----------

  async undo(token: string, name: string): Promise<void> {
    const key = undoKeys.get(token);
    try {
      const result = await restoreDeleted(token);
      undoKeys.delete(token);
      if (key) dropOverride(key);
      const type = result.restored?.type;
      const restoredName = result.restored?.name || name;
      const text =
        type === "service"
          ? T.serviceRestored(restoredName)
          : type === "server"
            ? T.serverRestored(restoredName)
            : T.tunnelRestored(restoredName);
      toast.success(text);
      refreshNow();
    } catch (e) {
      toast.error(errorText(e), { key: `undo-fail:${token}` });
    }
  },

  /** 批量删除后的「全部撤销」：顺序恢复，失败的逐条说明 */
  async undoMany(items: { token: string; name: string }[]): Promise<void> {
    let ok = 0;
    const failed: string[] = [];
    for (const item of items) {
      const key = undoKeys.get(item.token);
      try {
        await restoreDeleted(item.token);
        undoKeys.delete(item.token);
        if (key) dropOverride(key);
        ok += 1;
      } catch (e) {
        failed.push(`「${item.name}」：${errorText(e)}`);
      }
    }
    if (failed.length === 0) {
      toast.success(`已恢复 ${ok} 条`);
    } else {
      toast.warn(`${items.length} 条中有 ${failed.length} 条恢复失败`, {
        detail: failed.slice(0, 3).join("；"),
      });
    }
    refreshNow();
  },

  bulkTunnel(action: BulkAction, ids: string[], opts?: BulkOptions): Promise<BulkOutcome> {
    return runBulk("tunnel", action, ids, opts);
  },

  bulkService(action: BulkAction, ids: string[], opts?: BulkOptions): Promise<BulkOutcome> {
    return runBulk("service", action, ids, opts);
  },

  // ---------- 复制 / 设置 ----------

  async copyText(text: string, successText: string): Promise<void> {
    const ok = await copyToClipboard(text);
    if (ok) toast.success(successText);
    else toast.error("复制失败：请手动选中文字复制");
  },

  /** 整份 settings 写 config；失败就地挂到 `settings` 键（设置页订阅它） */
  async setSettings(next: Settings): Promise<void> {
    await runCommand<Settings>({
      key: "settings",
      run: () => saveSettingsApi(next),
      success: null,
    });
  },
};
