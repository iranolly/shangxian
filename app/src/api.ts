// 与 Rust 后端的接口层。
// 在 Tauri 窗口里走真实命令；在普通浏览器里（纯前端预览）退回演示数据。

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import { MOCK_SERVERS, MOCK_SERVICES, MOCK_TUNNELS } from "./mock/services";
import type { ServiceStatus, TunnelStatus } from "./copy/status";

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
}

export interface Snapshot {
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

export async function getSnapshot(): Promise<Snapshot> {
  if (!inTauri) {
    return { services: MOCK_SERVICES, tunnels: MOCK_TUNNELS, servers: MOCK_SERVERS };
  }
  return await invoke<Snapshot>("get_snapshot");
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
      warnings: [{ code: "preview", text: "浏览器预览模式：不会真的去读进程的启动方式。" }],
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

/** 立刻重连。 */
export async function tunnelRestart(id: string): Promise<void> {
  if (inTauri) await invoke("tunnel_restart", { id });
}

/** 隧道日志（id 是隧道 id）。 */
export async function getLogs(id: string, lines = 200): Promise<string> {
  if (!inTauri) return "（浏览器预览没有日志）";
  return await invoke<string>("get_logs", { id, lines });
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
  if (!inTauri) return "（浏览器预览不会真的连服务器）";
  return await invoke<string>("setup_passwordless", { serverId, password });
}

/**
 * 测试能不能连上这台服务器（真实去 SSH 连一次）。
 * 兼容两种参数名：后端用 id 或 serverId 都能取到。
 */
export async function testServer(id: string): Promise<string> {
  if (!inTauri) return "（浏览器预览不会真的连服务器）";
  return await invoke<string>("test_server", { id, serverId: id });
}

export async function deleteServer(id: string): Promise<void> {
  if (inTauri) await invoke("delete_server", { id });
}

export async function hashPassword(
  serverId: string,
  password: string,
): Promise<string> {
  if (!inTauri) return `preview-hash-${serverId}-${password.length}`;
  return await invoke<string>("hash_password", { serverId, password });
}

export interface NginxLine {
  text: string;
  /** true = 这行是新增内容（界面高亮）；false = 原配置的样子（界面灰显） */
  isNew: boolean;
}

export interface NginxSnippet {
  /** 要新增的几行（复制按钮复制的就是它）；没有访问保护时为 null */
  snippet?: string | null;
  /** 证书自动续期要加的一行（放进 location /.well-known/ 里） */
  acmeExempt?: string | null;
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
      acmeExempt: "auth_basic off;",
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
    handler(msg || "配置文件读不出来，已保留原文件；请到「设置」页打开配置文件夹看看。");
  });
}
