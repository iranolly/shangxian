// 与 Rust 后端的接口层。
// 在 Tauri 窗口里走真实命令；在普通浏览器里（纯前端预览）退回演示数据。

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import { MOCK_SERVERS, MOCK_SERVICES, MOCK_TUNNELS } from "./mock/services";
import type { ServiceStatus, TunnelStatus } from "./copy/status";

/** 最近一次失败记录：机器码 + 人话原因（D2 §4.1 FailureRecord） */
export interface FailureInfo {
  ts: number;
  reason: string;
  code: string;
}

/** 本机服务健康：端口在听 / 端口不通 / 还没探过（D2 §6.3） */
export type ServiceHealth = "up" | "down" | "unknown";

/** 隧道重试状态：看护会继续自动重连 / 已经停了（D2 §6.2） */
export type RetryState = "active" | "stopped";

/** 一台本机服务：这台电脑上的东西（端口 + 启动方式 + 看护开关） */
export interface ServiceView {
  id: string;
  name: string;
  localPort: number;
  status: ServiceStatus;
  watch: boolean;
  autostart: boolean;
  launchExe?: string | null;
  launchArgs?: string | null;
  launchWorkdir?: string | null;
  note?: string | null;
  /** 已经发布到几条隧道 */
  tunnelCount: number;
  /** 这些隧道的名字 */
  tunnelNames: string[];
  /** 出问题的原因（人话） */
  reason?: string | null;
  logPath: string;
  /** 新：端口是否在听；旧后端没有时 getSnapshot 兜底为 "unknown"（D2 §6.3） */
  health?: ServiceHealth;
  /** 新：最近一次失败（重启不失忆） */
  lastFailure?: FailureInfo | null;
  /** 新：程序日志是不是被截顶了（D0 §6） */
  logOverflowed?: boolean;
}

/** 一条发布隧道：把某台本机服务接到某台云端服务器上 */
export interface TunnelView {
  id: string;
  name: string;
  serviceId: string;
  serviceName: string;
  localPort: number;
  serverId: string;
  remotePort: number;
  url?: string | null;
  status: TunnelStatus;
  enabled: boolean;
  reason?: string | null;
  /** 访问保护：可同时有 "password" 和 "ip"；空数组/缺失 = 无保护 */
  access?: string[] | null;
  accessUser?: string | null;
  hasPassword: boolean;
  accessDetail?: string | null;
  /** 本机服务在不在跑（界面要标出来） */
  serviceOk: boolean;
  serviceStatus: string;
  keyPath?: string | null;
  /** 新：失败机器码（决定修复动作，如 remotePortBusy）；旧后端为 null */
  reasonCode?: string | null;
  /** 新：看护还会不会继续自动重连；旧后端按 enabled 兜底 */
  retryState?: RetryState;
  /** 新：连续失败次数 */
  attempt?: number;
  /** 新：距下次重试的秒数（向上取整）；不处于退避时为 null */
  retryInSecs?: number | null;
  /** 新：本次成功连接的建立时刻（epoch ms） */
  connectedSince?: number | null;
  /** 新：本次连接已持续秒数；未连接为 0 */
  uptimeSecs?: number;
  /** 新：本机今日重连次数 */
  reconnectsToday?: number;
  /** 新：最近一次失败（重启不失忆） */
  lastFailure?: FailureInfo | null;
  /** 新：最近一次云端保护核对结果（D2 §6.4，永远存在） */
  protectionCheck?: ProtectionCheck;
}

export interface ServerView {
  id: string;
  name: string;
  host: string;
  user: string;
  sshPort: number;
  keyPath?: string | null;
  /** 这台服务器上的站点目录（可空） */
  vhostDir?: string | null;
  /** 新：环境识别缓存（D3 §3，Wave 3；旧后端为 null） */
  cloudEnv?: CloudEnv | null;
}

/** 云端实际访问保护（D2 §4.1 CloudProtection） */
export interface ProtectionExpected {
  password: boolean;
  ip: boolean;
  allowIps: string[];
  /** 规范化后的域名 */
  url: string;
}

/** 最近一次云端保护核对结果（D2 §4.1；快照里永远存在，未知时为 unknown） */
export interface ProtectionCheck {
  status: "unknown" | "ok" | "drift" | "error";
  ts?: number | null;
  source?: "manual" | "auto" | null;
  cloud?: AccessCheck | null;
  expected?: ProtectionExpected | null;
  /** 漂移字段：conf / password / ip / allowIps / htpasswd */
  driftFields?: string[];
  error?: string | null;
  /** D3 扩展位：原样透传，前端只读 */
  extra?: unknown;
}

/** 检查更新结果（D2 §4.1 UpdateInfo，字段冻结） */
export type UpdateStatus = "unknown" | "upToDate" | "available" | "error";
export interface UpdateInfo {
  status: UpdateStatus;
  latest?: string | null;
  url?: string | null;
  checkedAt?: number | null;
  error?: string | null;
}

/** 界面设置（D2 §9；存 config，刷新频率默认 800ms） */
export interface QuietHours {
  enabled: boolean;
  start: string;
  end: string;
}
export interface Settings {
  refreshIntervalMs: number;
  quietHours: QuietHours;
  checkUpdateOnStart: boolean;
}

/** 快照里的全局字段（D2 §6.1 AppView） */
export interface AppView {
  snapshotVersion: number;
  pid: number;
  appVersion: string;
  dataDir: string;
  startedAt: number;
  jobGuardOk: boolean;
  jobGuardError: string | null;
  configTrusted: boolean;
  updateInfo: UpdateInfo;
  settings: Settings;
  notifyEnabled: boolean;
  autostart: boolean;
}

/** 后端旧快照可能没有 app（旧版本 / 浏览器预览），读之前先兜底 */
type RawSnapshot = {
  app?: Partial<AppView> | null;
  services?: ServiceView[] | null;
  tunnels?: TunnelView[] | null;
  servers?: ServerView[] | null;
};

export interface Snapshot {
  app: AppView;
  services: ServiceView[];
  tunnels: TunnelView[];
  servers: ServerView[];
}

export interface LocalPort {
  port: number;
  pid?: number | null;
  /** 进程名（例：python.exe） */
  process?: string | null;
  /** 程序完整路径（添服务时自动填启动程序用） */
  exe?: string | null;
}

/** 自动读取启动方式时后端给的一条警告（code 是机器用的，text 是给人看的） */
export interface LaunchWarning {
  code: string;
  text: string;
}

/** 自动读取到的进程启动方式（点端口行时用） */
export interface LaunchInfo {
  port: number;
  pid?: number | null;
  /** 传进去的 pid 和端口现在的占用者一致吗 */
  pidMatched: boolean;
  exe?: string | null;
  args?: string | null;
  workdir?: string | null;
  /** 原始命令行（给人看，不做逐字还原） */
  rawCommandLine?: string | null;
  warnings: LaunchWarning[];
}

/** 停止服务的结果：kind=needsConfirm 时要让用户确认（带 pid/exe） */
export interface ServiceStopResult {
  kind: "stopped" | "needsConfirm" | "foreignStopped";
  pid?: number | null;
  exe?: string | null;
}

export interface ImportSuggestion {
  name: string;
  source: string;
  serverHost: string;
  serverUser: string;
  sshPort: number;
  localPort: number;
  remotePort: number;
  url?: string | null;
}

export interface ServerInput {
  id?: string | null;
  name: string;
  host: string;
  sshPort: number;
  user: string;
  keyPath?: string | null;
  /** 这台服务器上的站点目录（可空） */
  vhostDir?: string | null;
}

/** 新增/编辑一台本机服务（id 为空 = 新建） */
export interface ServiceInput {
  id?: string | null;
  name: string;
  localPort: number;
  launchExe?: string | null;
  launchArgs?: string | null;
  launchWorkdir?: string | null;
  watch: boolean;
  autostart: boolean;
  note?: string | null;
}

/** 新建一条隧道 */
export interface NewTunnelInput {
  name: string;
  serviceId: string;
  serverId: string;
  remotePort: number;
  url?: string | null;
  access: string[];
  accessUser?: string | null;
  accessHash?: string | null;
  accessDetail?: string | null;
}

/** 编辑一条隧道（accessHash 不填 = 不换密码） */
export interface UpdateTunnelInput {
  id: string;
  name: string;
  remotePort: number;
  url?: string | null;
  access: string[];
  accessUser?: string | null;
  accessHash?: string | null;
  accessDetail?: string | null;
}

const inTauri =
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

// ---------- 快照兜底（D2 §15.6：旧后端 / 浏览器预览可能缺新字段） ----------

/** 刷新频率默认值（D2 §9.1 冻结：800ms） */
export const DEFAULT_SETTINGS: Settings = {
  refreshIntervalMs: 800,
  quietHours: { enabled: false, start: "22:00", end: "08:00" },
  checkUpdateOnStart: true,
};

/** 还没查过时的更新信息 */
export const UNKNOWN_UPDATE_INFO: UpdateInfo = {
  status: "unknown",
  latest: null,
  url: null,
  checkedAt: null,
  error: null,
};

/** 还没核对过云端时的保护状态 */
export const UNKNOWN_PROTECTION: ProtectionCheck = {
  status: "unknown",
  ts: null,
  source: null,
  cloud: null,
  expected: null,
  driftFields: [],
  error: null,
};

export function defaultAppView(): AppView {
  return {
    snapshotVersion: 2,
    pid: 0,
    appVersion: "",
    dataDir: "",
    startedAt: 0,
    jobGuardOk: true,
    jobGuardError: null,
    configTrusted: true,
    updateInfo: { ...UNKNOWN_UPDATE_INFO },
    settings: { ...DEFAULT_SETTINGS, quietHours: { ...DEFAULT_SETTINGS.quietHours } },
    notifyEnabled: true,
    autostart: false,
  };
}

function normalizeSettings(raw?: Partial<Settings> | null): Settings {
  const quiet: Partial<QuietHours> = raw?.quietHours ?? {};
  const rawInterval = raw?.refreshIntervalMs;
  const interval =
    typeof rawInterval === "number" && Number.isFinite(rawInterval)
      ? Math.min(10_000, Math.max(500, Math.round(rawInterval)))
      : DEFAULT_SETTINGS.refreshIntervalMs;
  const start = typeof quiet.start === "string" && quiet.start ? quiet.start : DEFAULT_SETTINGS.quietHours.start;
  const end = typeof quiet.end === "string" && quiet.end ? quiet.end : DEFAULT_SETTINGS.quietHours.end;
  return {
    refreshIntervalMs: interval,
    quietHours: { enabled: quiet.enabled === true, start, end },
    checkUpdateOnStart: raw?.checkUpdateOnStart !== false,
  };
}

function normalizeUpdateInfo(raw?: Partial<UpdateInfo> | null): UpdateInfo {
  const status = raw?.status;
  return {
    status: status === "upToDate" || status === "available" || status === "error" ? status : "unknown",
    latest: raw?.latest ?? null,
    url: raw?.url ?? null,
    checkedAt: raw?.checkedAt ?? null,
    error: raw?.error ?? null,
  };
}

function normalizeProtection(raw?: Partial<ProtectionCheck> | null): ProtectionCheck {
  if (!raw) return { ...UNKNOWN_PROTECTION };
  const status = raw.status === "ok" || raw.status === "drift" || raw.status === "error" ? raw.status : "unknown";
  return {
    status,
    ts: raw.ts ?? null,
    source: raw.source === "manual" || raw.source === "auto" ? raw.source : null,
    cloud: raw.cloud ?? null,
    expected: raw.expected ?? null,
    driftFields: Array.isArray(raw.driftFields) ? raw.driftFields : [],
    error: raw.error ?? null,
    ...(raw.extra !== undefined ? { extra: raw.extra } : {}),
  };
}

function normalizeApp(raw?: Partial<AppView> | null): AppView {
  const fallback = defaultAppView();
  if (!raw) return fallback;
  return {
    snapshotVersion: typeof raw.snapshotVersion === "number" ? raw.snapshotVersion : 2,
    pid: typeof raw.pid === "number" ? raw.pid : 0,
    appVersion: raw.appVersion ?? "",
    dataDir: raw.dataDir ?? "",
    startedAt: typeof raw.startedAt === "number" ? raw.startedAt : 0,
    jobGuardOk: raw.jobGuardOk !== false,
    jobGuardError: raw.jobGuardError ?? null,
    configTrusted: raw.configTrusted !== false,
    updateInfo: normalizeUpdateInfo(raw.updateInfo),
    settings: normalizeSettings(raw.settings),
    notifyEnabled: raw.notifyEnabled !== false,
    autostart: raw.autostart === true,
  };
}

function normalizeService(raw: ServiceView): ServiceView {
  return {
    ...raw,
    health: raw.health === "up" || raw.health === "down" ? raw.health : "unknown",
    lastFailure: raw.lastFailure ?? null,
    logOverflowed: raw.logOverflowed === true,
  };
}

function normalizeTunnel(raw: TunnelView): TunnelView {
  const retryState: RetryState =
    raw.retryState === "active" || raw.retryState === "stopped" ? raw.retryState : raw.enabled ? "active" : "stopped";
  return {
    ...raw,
    reasonCode: raw.reasonCode ?? null,
    retryState,
    attempt: typeof raw.attempt === "number" ? raw.attempt : 0,
    retryInSecs: raw.retryInSecs ?? null,
    connectedSince: raw.connectedSince ?? null,
    uptimeSecs: typeof raw.uptimeSecs === "number" ? raw.uptimeSecs : 0,
    reconnectsToday: typeof raw.reconnectsToday === "number" ? raw.reconnectsToday : 0,
    lastFailure: raw.lastFailure ?? null,
    protectionCheck: normalizeProtection(raw.protectionCheck),
  };
}

/** 把后端快照补齐成界面能直接用的形状（app 缺失也有默认值） */
export function normalizeSnapshot(raw?: RawSnapshot | null): Snapshot {
  return {
    app: normalizeApp(raw?.app),
    services: (raw?.services ?? []).map(normalizeService),
    tunnels: (raw?.tunnels ?? []).map(normalizeTunnel),
    servers: raw?.servers ?? [],
  };
}

export async function getSnapshot(): Promise<Snapshot> {
  if (!inTauri) {
    return normalizeSnapshot({
      services: MOCK_SERVICES,
      tunnels: MOCK_TUNNELS,
      servers: MOCK_SERVERS,
    });
  }
  const raw = await invoke<RawSnapshot>("get_snapshot");
  return normalizeSnapshot(raw);
}

// ---------- 本机服务 ----------

/** 新增/编辑一台本机服务；返回服务 id。 */
export async function saveService(input: ServiceInput): Promise<string> {
  if (!inTauri) return input.id ?? "preview-service";
  return await invoke<string>("save_service", { input });
}

export async function removeService(id: string): Promise<void> {
  if (inTauri) await invoke("remove_service", { id });
}

export async function serviceStart(id: string): Promise<void> {
  if (inTauri) await invoke("service_start", { id });
}

export async function serviceStop(
  id: string,
  killForeign = false,
  expectedPid: number | null = null,
): Promise<ServiceStopResult> {
  if (!inTauri) return { kind: "stopped" };
  return await invoke<ServiceStopResult>("service_stop", {
    id,
    killForeign,
    expectedPid,
  });
}

/**
 * 自动读取某个端口现在被谁监听着、是怎么启动的。
 * 传 expectedPid（端口列表里那一行的进程号）用来核对占用者有没有变。
 */
export async function inspectProcessLaunchInfo(
  port: number,
  expectedPid?: number | null,
): Promise<LaunchInfo> {
  if (!inTauri) {
    return {
      port,
      pid: null,
      pidMatched: false,
      exe: null,
      args: null,
      workdir: null,
      rawCommandLine: null,
      warnings: [{ code: "preview", text: "浏览器预览模式：不会实际读取进程的启动方式。" }],
    };
  }
  return await invoke<LaunchInfo>("inspect_process_launch_info", {
    port,
    expectedPid: expectedPid ?? null,
  });
}

/** 给人看的「实际会执行的命令行」预览（失败就静默不显示）。 */
export async function previewLaunchCommand(
  exe: string,
  args: string,
): Promise<string> {
  if (!inTauri) return [exe.trim(), args.trim()].filter(Boolean).join(" ");
  return await invoke<string>("preview_launch_command", { exe, args });
}

export async function setServiceWatch(
  id: string,
  watch: boolean,
): Promise<void> {
  if (inTauri) await invoke("set_service_watch", { id, watch });
}

export async function setServiceAutostart(
  id: string,
  autostart: boolean,
): Promise<void> {
  if (inTauri) await invoke("set_service_autostart", { id, autostart });
}

/** 本机服务自己的输出（程序日志 + 看护记录）。 */
export async function getServiceLogs(id: string, lines = 200): Promise<string> {
  if (!inTauri) return "（浏览器预览没有日志）";
  return await invoke<string>("get_service_logs", { id, lines });
}

// ---------- 发布隧道 ----------

/** 新建一条隧道；返回隧道 id。 */
export async function addTunnel(input: NewTunnelInput): Promise<string> {
  if (!inTauri) return "preview-tunnel";
  return await invoke<string>("add_tunnel", { input });
}

export async function updateTunnel(input: UpdateTunnelInput): Promise<void> {
  if (inTauri) await invoke("update_tunnel", { input });
}

/** 云端 nginx 实际生效的访问保护（点「检查云端」时后端去服务器上读配置）。 */
export interface AccessCheck {
  /** 云端有没有找到这条隧道的站点配置 */
  confFound: boolean;
  /** 云端配置里开着密码保护 */
  passwordOn: boolean;
  /** 云端配置里开着 IP 白名单 */
  ipOn: boolean;
  /** 云端配置里允许的 IP 列表 */
  allowIps: string[];
  /** 服务器上密码文件在不在 */
  htpasswdExists: boolean;
}

/** 浏览器预览模式：按隧道设置造一份演示结果，顺便覆盖几种要预警的情况。 */
function previewAccessCheck(id: string): AccessCheck {
  const t = MOCK_TUNNELS.find((x) => x.id === id);
  const access = t?.access ?? [];
  const passwordOn = access.includes("password");
  const ipOn = access.includes("ip");
  const allowIps =
    ipOn && t?.accessDetail
      ? t.accessDetail.split(/[,，\s]+/).filter(Boolean)
      : [];
  const consistent: AccessCheck = {
    confFound: true,
    passwordOn,
    ipOn,
    allowIps,
    htpasswdExists: passwordOn,
  };
  // 特意造出「配置没找到 / 缺密码文件 / 云端和设置不一致」三种情况，预览时能看全提示
  if (id === "tun-docs") {
    return {
      confFound: false,
      passwordOn: false,
      ipOn: false,
      allowIps: [],
      htpasswdExists: false,
    };
  }
  if (id === "tun-album") return { ...consistent, htpasswdExists: false };
  if (id === "tun-pi-web-spare") {
    return { ...consistent, passwordOn: false, htpasswdExists: false };
  }
  return consistent;
}

/** 去云端服务器读 nginx 配置，看看访问保护实际有没有生效。 */
export async function checkTunnelAccess(id: string): Promise<AccessCheck> {
  if (!inTauri) return previewAccessCheck(id);
  return await invoke<AccessCheck>("check_tunnel_access", { id });
}

export async function removeTunnel(id: string): Promise<void> {
  if (inTauri) await invoke("remove_tunnel", { id });
}

/** 开始发布。 */
export async function tunnelStart(id: string): Promise<void> {
  if (inTauri) await invoke("tunnel_start", { id });
}

/** 暂停发布。 */
export async function tunnelStop(id: string): Promise<void> {
  if (inTauri) await invoke("tunnel_stop", { id });
}

/** 隧道日志（id 是隧道 id）。 */
export async function getLogs(id: string, lines = 200): Promise<string> {
  if (!inTauri) return "（浏览器预览没有日志）";
  return await invoke<string>("get_logs", { id, lines });
}

// ---------- 新后端命令（D2 §15.6 / D0 §10） ----------
// 后端（K4 lane）还在接线：这里先按冻结的命令名与形状封装，TS 只依赖类型。
// 浏览器预览视情况给最小可用数据，不让界面报错。

/** 日志条目（D2 §15.3 LogEntry） */
export interface EventSubject {
  type: "app" | "tunnel" | "service" | "server";
  id: string;
  name: string;
}
export interface LogEntry {
  ts: number | null;
  source: string;
  origin: "events" | "appLog" | "tunnel" | "service" | string;
  level: string | null;
  code: string | null;
  subject?: EventSubject | null;
  message: string | null;
  raw: string;
}
export interface LogsQuery {
  source?: "all" | "tunnel" | "service" | "app";
  id?: string;
  lines?: number;
  archive?: 0 | 1 | 2;
  format?: "lines" | "json";
}
export interface LogsResult {
  source: string;
  archive: number;
  lines: number;
  truncated: boolean;
  sources: string[];
  entries: LogEntry[];
  text?: string;
}

/** 日志归档文件（Tauri 专用帮手，D0 §10） */
export interface LogArchive {
  name: string;
  path: string;
  bytes: number;
  modifiedAt: number;
}
export interface LogArchivesResult {
  items: LogArchive[];
}

/** 体检步骤 / 报告（D2 §14；状态含 warn，按 D0 裁决） */
export interface DoctorFix {
  label: string;
  text: string;
}
export interface DoctorStep {
  id: string;
  title: string;
  status: "pass" | "warn" | "fail" | "skip";
  detail: string;
  advice: string;
  children?: DoctorStep[];
  durationMs?: number;
  fix?: DoctorFix;
}
export interface DoctorSummary {
  pass: number;
  warn: number;
  fail: number;
  skip: number;
}
export interface DoctorReport {
  schemaVersion: number;
  checkedAt: number;
  appVersion: string | null;
  appRunning: boolean;
  mode: "quick" | "deep";
  steps: DoctorStep[];
  summary: DoctorSummary;
}

/** 诊断包结果（D2 §12） */
export interface DiagResult {
  path: string;
  bytes: number;
  redacted: true;
  deep: boolean;
  truncated: boolean;
}

/** 最近删除（墓碑）条目（D2 §10.2） */
export interface TrashItem {
  token: string;
  type: "tunnel" | "service" | "server";
  id: string;
  name: string;
  deletedAt: number;
  expiresAt: number;
}
export interface DeletedListResult {
  items: TrashItem[];
}
export interface RestoreResult {
  restored: { type: "tunnel" | "service" | "server"; id: string; name: string };
  enabled: false;
}

/** 导出 / 导入配置（D2 §11） */
export interface ExportConfigResult {
  redacted: boolean;
  config: unknown;
  path?: string | null;
}
export interface ImportCounts {
  servers: number;
  services: number;
  tunnels: number;
}
export interface ImportSummary {
  mode: "merge" | "replace";
  dryRun: boolean;
  added: ImportCounts;
  updated: ImportCounts;
  unchanged: ImportCounts;
  warnings: string[];
  backupPath?: string | null;
}
export interface ImportConfigParams {
  doc?: string;
  path?: string;
  mode?: "merge" | "replace";
  dryRun?: boolean;
}

/** 运行统计（D2 §8） */
export interface StatsTunnelItem {
  id: string;
  name: string;
  status: string;
  retryState: string;
  attempt: number;
  retryInSecs: number | null;
  connectedSince: number | null;
  uptimeSecs: number;
  reconnectsToday: number;
}
export interface StatsServiceItem {
  id: string;
  name: string;
  status: string;
  health: string;
}
export interface StatsResult {
  schemaVersion: number;
  generatedAt: number;
  app: {
    pid: number;
    appVersion: string;
    startedAt: number;
    uptimeSecs: number;
    dayKey: string;
    jobGuardOk: boolean;
    eventsSinceStart: number;
  };
  tunnels: {
    total: number;
    enabled: number;
    running: number;
    retrying: number;
    reconnectsToday: number;
    items: StatsTunnelItem[];
  };
  services: {
    total: number;
    up: number;
    down: number;
    unknown: number;
    items: StatsServiceItem[];
  };
  update: UpdateInfo;
}

/** 云端环境识别结果（D3 §3） */
export interface CloudEnv {
  panel: "bt" | "xp" | "none" | "unknown";
  panelEvidence?: string | null;
  panelConfidence?: "high" | "medium" | "low" | "unknown";
  web: "nginx" | "apache" | "both" | "none" | "unknown";
  webEvidence?: string | null;
  webConfidence?: "high" | "medium" | "low" | "unknown";
  nginxBin?: string | null;
  vhostDir?: string | null;
  vhostDirExists?: boolean;
  wsMap?: boolean | null;
  detectedAt?: number;
}

/** 云端端口回环核对（D3 §8） */
export interface CloudPortCheck {
  bound: "loopback" | "exposed" | "mixed" | "missing" | "unknown";
  tool: "ss" | "netstat" | "none";
  evidence: string[];
}

export interface AuditEntry {
  ip?: string | null;
  time?: string | null;
  method?: string | null;
  path?: string | null;
  status?: number | null;
  bytes?: number | null;
  raw?: string | null;
}

/** 访问审计（D3 §16；计数是“大概”） */
export interface AccessAudit {
  available: boolean;
  path?: string | null;
  window: number;
  count401: number;
  count403: number;
  topIps: Array<{ ip: string; count: number }>;
  entries: AuditEntry[];
  truncated: boolean;
  warnings: string[];
}

/** 云端完整检查返回（D3 §2.2 CloudCheckResult） */
export interface CloudCheckResult {
  check: ProtectionCheck;
  actual?: AccessCheck | null;
  port?: CloudPortCheck | null;
  files?: string[];
  env?: CloudEnv | null;
  clientIp?: string | null;
  audit?: AccessAudit | null;
}

export type CloudCheckMode = "full" | "drift" | "audit" | "env";

/** 公网 IP 助手（D3 §9） */
export interface PublicIpInfo {
  ip: string;
  source: string;
  at: number;
}

/** 数据目录来源（D0 §2-6：get_data_dir 保持返回字符串，这个帮手给来源） */
export interface DataDirInfo {
  path: string;
  kind: string;
}

/** 保存一个文本文件（Tauri 专用帮手，D0 §10） */
export interface SaveTextResult {
  path: string;
}

function emptyImportSummary(): ImportSummary {
  const zero: ImportCounts = { servers: 0, services: 0, tunnels: 0 };
  return { mode: "merge", dryRun: false, added: { ...zero }, updated: { ...zero }, unchanged: { ...zero }, warnings: [] };
}

/** 运行统计（ctl `stats` 的 Tauri 镜像）。 */
export async function getStats(): Promise<StatsResult> {
  if (!inTauri) {
    const tunnelItems: StatsTunnelItem[] = MOCK_TUNNELS.map((t) => ({
      id: t.id,
      name: t.name,
      status: t.status,
      retryState: t.retryState ?? (t.enabled ? "active" : "stopped"),
      attempt: t.attempt ?? 0,
      retryInSecs: t.retryInSecs ?? null,
      connectedSince: t.connectedSince ?? null,
      uptimeSecs: t.uptimeSecs ?? 0,
      reconnectsToday: t.reconnectsToday ?? 0,
    }));
    const serviceItems: StatsServiceItem[] = MOCK_SERVICES.map((s) => ({
      id: s.id,
      name: s.name,
      status: s.status,
      health: s.health ?? "unknown",
    }));
    return {
      schemaVersion: 1,
      generatedAt: Date.now(),
      app: { pid: 0, appVersion: "", startedAt: 0, uptimeSecs: 0, dayKey: "", jobGuardOk: true, eventsSinceStart: 0 },
      tunnels: {
        total: tunnelItems.length,
        enabled: tunnelItems.filter((t) => t.retryState === "active").length,
        running: tunnelItems.filter((t) => t.status === "running").length,
        retrying: tunnelItems.filter((t) => t.retryInSecs != null).length,
        reconnectsToday: tunnelItems.reduce((n, t) => n + t.reconnectsToday, 0),
        items: tunnelItems,
      },
      services: {
        total: serviceItems.length,
        up: serviceItems.filter((s) => s.health === "up").length,
        down: serviceItems.filter((s) => s.health === "down").length,
        unknown: serviceItems.filter((s) => s.health === "unknown").length,
        items: serviceItems,
      },
      update: { ...UNKNOWN_UPDATE_INFO },
    };
  }
  return await invoke<StatsResult>("get_stats");
}

/** 按来源读日志（事件 / 应用 / 隧道 / 服务，可含归档）。 */
export async function queryLogs(params: LogsQuery = {}): Promise<LogsResult> {
  if (!inTauri) {
    const lines = params.lines ?? 200;
    return {
      source: params.source ?? "all",
      archive: params.archive ?? 0,
      lines,
      truncated: false,
      sources: [],
      entries: [],
      ...(params.format === "lines" ? { text: "（浏览器预览没有日志）" } : {}),
    };
  }
  return await invoke<LogsResult>("query_logs", {
    source: params.source,
    id: params.id,
    lines: params.lines,
    archive: params.archive,
    format: params.format,
  });
}

/** 体检（9 步；deep 会连服务器与公网）。 */
export async function runDoctor(params: { mode?: "quick" | "deep" } = {}): Promise<DoctorReport> {
  const mode = params.mode ?? "quick";
  if (!inTauri) {
    return {
      schemaVersion: 1,
      checkedAt: Date.now(),
      appVersion: null,
      appRunning: false,
      mode,
      steps: [],
      summary: { pass: 0, warn: 0, fail: 0, skip: 0 },
    };
  }
  return await invoke<DoctorReport>("run_doctor", { mode });
}

/** 打一个脱敏诊断包（固定落 <dataDir>/diag/）。 */
export async function buildDiag(params: { out?: string; deep?: boolean } = {}): Promise<DiagResult> {
  if (!inTauri) {
    return {
      path: "（浏览器预览模式不会实际打包）",
      bytes: 0,
      redacted: true,
      deep: params.deep === true,
      truncated: false,
    };
  }
  return await invoke<DiagResult>("build_diag", { out: params.out, deep: params.deep });
}

/** 最近删除（10 分钟内可撤销）。 */
export async function listDeleted(): Promise<DeletedListResult> {
  if (!inTauri) return { items: [] };
  return await invoke<DeletedListResult>("list_deleted");
}

/** 撤销删除（隧道恢复为暂停状态）。 */
export async function restoreDeleted(token: string): Promise<RestoreResult> {
  if (!inTauri) {
    return { restored: { type: "tunnel", id: token, name: "（浏览器预览）" }, enabled: false };
  }
  return await invoke<RestoreResult>("restore_deleted", { token });
}

/** 放弃撤销（清掉墓碑）。 */
export async function purgeDeleted(token: string): Promise<{ ok: true }> {
  if (!inTauri) return { ok: true };
  return await invoke<{ ok: true }>("purge_deleted", { token });
}

/** 导出配置（默认脱敏，不含密码哈希 / 密钥路径）。 */
export async function exportConfig(
  params: { redacted?: boolean; out?: string } = {},
): Promise<ExportConfigResult> {
  if (!inTauri) {
    return { redacted: params.redacted !== false, config: null, path: params.out ?? null };
  }
  return await invoke<ExportConfigResult>("export_config", { redacted: params.redacted, out: params.out });
}

/** 导入配置（默认合并；dryRun 只校验不写盘）。 */
export async function importConfig(params: ImportConfigParams = {}): Promise<ImportSummary> {
  if (!inTauri) return emptyImportSummary();
  return await invoke<ImportSummary>("import_config", {
    doc: params.doc,
    path: params.path,
    mode: params.mode,
    dryRun: params.dryRun,
  });
}

/** 检查有没有新版本（不自动下载 / 安装）。 */
export async function checkUpdate(): Promise<UpdateInfo> {
  if (!inTauri) return { ...UNKNOWN_UPDATE_INFO };
  return await invoke<UpdateInfo>("check_update");
}

/** 读界面设置（刷新频率 / 免打扰时段 / 启动检查更新）。 */
export async function getSettings(): Promise<Settings> {
  if (!inTauri) return { ...DEFAULT_SETTINGS, quietHours: { ...DEFAULT_SETTINGS.quietHours } };
  return normalizeSettings(await invoke<Settings>("get_settings"));
}

/** 整份保存设置项（一次原子写；非法值后端会拒绝）。 */
export async function setSettings(settings: Settings): Promise<Settings> {
  if (!inTauri) return normalizeSettings(settings);
  return normalizeSettings(await invoke<Settings>("set_settings", { settings }));
}

/** 云端完整检查（Wave 3；未上线前调用方回退 checkTunnelAccess）。 */
export async function checkTunnelCloud(id: string, mode: CloudCheckMode = "full"): Promise<CloudCheckResult> {
  if (!inTauri) {
    return {
      check: { ...UNKNOWN_PROTECTION },
      actual: previewAccessCheck(id),
      port: { bound: "unknown", tool: "none", evidence: [] },
      files: [],
      env: null,
      clientIp: null,
      audit: null,
    };
  }
  return await invoke<CloudCheckResult>("check_tunnel_cloud", { id, mode });
}

/** 识别服务器上的面板与网站服务（Wave 3）。 */
export async function scanServerEnv(serverId: string): Promise<CloudEnv> {
  if (!inTauri) {
    return { panel: "unknown", web: "unknown", panelConfidence: "unknown", webConfidence: "unknown" };
  }
  return await invoke<CloudEnv>("scan_server_env", { serverId });
}

/** 访问审计（最近 1000 行，计数是“大概”）（Wave 3）。 */
export async function runAccessAudit(id: string): Promise<AccessAudit> {
  if (!inTauri) {
    return {
      available: false,
      window: 1000,
      count401: 0,
      count403: 0,
      topIps: [],
      entries: [],
      truncated: false,
      warnings: ["浏览器预览模式：不会实际读取服务器日志。"],
    };
  }
  // ctl 的 run_access_audit 返回的是整包（check/actual/audit…），这里只取 audit 那一段。
  // 注意：曾直接把整包当 AccessAudit 用，data.topIps 为 undefined，界面会直接崩成空白。
  const result = await invoke<CloudCheckResult>("run_access_audit", { id });
  return (
    result.audit ?? {
      available: false,
      window: 1000,
      count401: 0,
      count403: 0,
      topIps: [],
      entries: [],
      truncated: false,
      warnings: [],
    }
  );
}

/** 查本机公网 IP（Wave 3；未上线前从检查结果的 clientIp 取）。 */
export async function getPublicIp(): Promise<PublicIpInfo> {
  if (!inTauri) return { ip: "", source: "preview", at: Date.now() };
  return await invoke<PublicIpInfo>("get_public_ip");
}

/** 保存一个文本文件（日志导出用；Tauri 专用帮手）。 */
export async function saveTextFile(name: string, content: string): Promise<SaveTextResult> {
  if (!inTauri) return { path: name };
  return await invoke<SaveTextResult>("save_text_file", { name, content });
}

/** 打开日志文件夹（实现优先用 opener 的 reveal，不可用则退回打开数据目录）。 */
export async function openLogFolder(): Promise<void> {
  if (inTauri) await invoke("open_log_folder");
}

/** 列日志归档文件（当前 / 上一份 / 上上份）。 */
export async function getLogArchives(): Promise<LogArchivesResult> {
  if (!inTauri) return { items: [] };
  return await invoke<LogArchivesResult>("get_log_archives");
}

/** 数据目录路径 + 来源（便携模式等，D0 §2-6）。 */
export async function getDataDirInfo(): Promise<DataDirInfo> {
  if (!inTauri) return { path: "%APPDATA%\\Shangxian", kind: "preview" };
  return await invoke<DataDirInfo>("get_data_dir_info");
}

// ---------- 其它 ----------

export async function listLocalPorts(): Promise<LocalPort[]> {
  if (!inTauri) {
    return [
      { port: 30141, pid: 4108, process: "python.exe", exe: "C:\\Python311\\python.exe" },
      { port: 8787, pid: 8123, process: "python.exe", exe: "C:\\Python311\\python.exe" },
      { port: 3000, pid: 9911, process: "node.exe", exe: "C:\\Program Files\\nodejs\\node.exe" },
      { port: 8080, pid: 7520, process: "cloudflared.exe", exe: "C:\\tools\\cloudflared.exe" },
    ];
  }
  return await invoke<LocalPort[]>("list_local_ports");
}

export async function scanImports(): Promise<ImportSuggestion[]> {
  if (!inTauri) return [];
  return await invoke<ImportSuggestion[]>("scan_imports");
}

export async function importServices(
  items: ImportSuggestion[],
): Promise<number> {
  if (!inTauri) return items.length;
  return await invoke<number>("import_services", { items });
}

export async function saveServer(input: ServerInput): Promise<string> {
  if (!inTauri) return input.id ?? "preview-server";
  return await invoke<string>("save_server", { input });
}

/// 用密码登录一次，让助手自动配好免密登录（密码只在内存里用一次）。
export async function setupPasswordless(
  serverId: string,
  password: string,
): Promise<string> {
  if (!inTauri) return "（浏览器预览模式不会实际连接服务器）";
  return await invoke<string>("setup_passwordless", { serverId, password });
}

/**
 * 测试能不能连上这台服务器（真实去 SSH 连一次）。
 * 兼容两种参数名：后端用 id 或 serverId 都能取到。
 */
export async function testServer(id: string): Promise<string> {
  if (!inTauri) return "（浏览器预览模式不会实际连接服务器）";
  return await invoke<string>("test_server", { id, serverId: id });
}

export async function deleteServer(id: string): Promise<void> {
  if (inTauri) await invoke("delete_server", { id });
}

/** 访问密码哈希（后端自适应：优先 bcrypt；服务器 nginx 验不了时用兼容的 apr1） */
export interface HashPasswordResult {
  hash: string;
  format: "bcrypt" | "apr1";
  /** 用了兼容格式/没探准时的一句说明（可展示给用户） */
  note?: string;
}

export async function hashPassword(
  serverId: string,
  password: string,
): Promise<HashPasswordResult> {
  if (!inTauri) {
    return { hash: `preview-hash-${serverId}-${password.length}`, format: "bcrypt" };
  }
  const raw = await invoke<{ hash: string; format?: string; note?: string }>(
    "hash_password",
    { serverId, password },
  );
  return {
    hash: raw.hash,
    format: raw.format === "apr1" ? "apr1" : "bcrypt",
    note: raw.note,
  };
}

export interface NginxLine {
  text: string;
  /** true = 这行是新增内容（界面高亮）；false = 原配置的样子（界面灰显） */
  isNew: boolean;
}

export interface NginxSnippet {
  /** 要新增的几行（复制按钮复制的就是它）；没有访问保护时为 null */
  snippet?: string | null;
  /**
   * 证书自动续期要加的几行：**多行字符串**（D0 §13；密码有 auth_basic off，白名单还有 allow all）。
   * 调用方按原文整体展示 / 复制，不要按分号拆行。放进 location /.well-known/ { } 里。
   */
  acmeExempt?: string | null;
  /** 可选附加行（如 noindex 提示行），原样展示 / 复制；没有就是空（undefined 或空数组） */
  optional?: string[] | null;
  /** 简短示意：灰 = 原配置的样子，高亮 = 新增内容的位置 */
  preview: NginxLine[];
  htpasswd?: string | null;
  steps: string[];
}

/** id 是隧道 id。 */
export async function getNginxSnippet(id: string): Promise<NginxSnippet> {
  if (!inTauri) {
    const gray = (text: string) => ({ text, isNew: false });
    const add = (text: string) => ({ text, isNew: true });
    return {
      snippet:
        '    # 访问密码（由「上线助手」新增）\n    auth_basic "需要密码";\n    auth_basic_user_file /xp/panel/vhost/演示.example.com.htpasswd;',
      // 多行字符串：密码场景只放开续期用的一小块；真实运行时后端按 D3 §5 生成
      acmeExempt:
        "# 证书续期专用：把下面一行放进已有的 location /.well-known/ { } 里\n" +
        "# （续 Let's Encrypt 证书要用；只放开这一小块，别的地址还是要过密码）\n" +
        "auth_basic off;",
      preview: [
        gray("server {"),
        gray("    ..."),
        gray("    server_name 演示.example.com;"),
        add("    # ① 新增：访问密码（放在 server_name 下面）"),
        add('    auth_basic "需要密码";'),
        add(
          "    auth_basic_user_file /xp/panel/vhost/演示.example.com.htpasswd;",
        ),
        gray("    ..."),
        gray("    # SSL证书验证目录"),
        gray("    location /.well-known/ {"),
        add("        # ② 新增：证书续期用（放进这个目录里）"),
        add("        auth_basic off;"),
        gray("        allow all;"),
        gray("    }"),
        gray("    ..."),
        gray("}"),
      ],
      htpasswd: null,
      steps: [
        "浏览器预览模式：这里是演示内容，真实运行时会给这条隧道生成要新增的几行。",
      ],
    };
  }
  return await invoke<NginxSnippet>("get_nginx_snippet", { id });
}

export async function getAutostart(): Promise<boolean> {
  if (!inTauri) return false;
  return await invoke<boolean>("get_autostart");
}

export async function setAutostart(enabled: boolean): Promise<void> {
  if (inTauri) await invoke("set_autostart", { enabled });
}

export async function getDataDir(): Promise<string> {
  if (!inTauri) return "%APPDATA%\\Shangxian";
  return await invoke<string>("get_data_dir");
}

export async function openDataDir(): Promise<void> {
  if (inTauri) await invoke("open_data_dir");
}

export async function getNotifyEnabled(): Promise<boolean> {
  if (!inTauri) return true;
  return await invoke<boolean>("get_notify_enabled");
}

export async function setNotifyEnabled(enabled: boolean): Promise<void> {
  if (inTauri) await invoke("set_notify_enabled", { enabled });
}

export async function testNotification(): Promise<void> {
  if (inTauri) await invoke("test_notification");
}

/**
 * 监听后端的「配置读不出来」事件：后端发 config-error 时把大白话错误交给界面。
 * 返回取消监听的函数；浏览器预览下什么也不做。
 */
export async function onConfigError(
  handler: (message: string) => void,
): Promise<UnlistenFn> {
  if (!inTauri) return () => {};
  return await listen<{ message?: string }>("config-error", (e) => {
    const msg = e.payload?.message?.trim();
    handler(msg || "配置文件无法读取，已保留原文件；请到「设置」页打开数据目录查看。");
  });
}
