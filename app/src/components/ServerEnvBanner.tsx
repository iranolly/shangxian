// 「发布隧道」页的服务器环境提醒（D3 §15 文案 / D4 ServerEnvBanner）。
//
// 数据：后端把环境识别结果（CloudEnv）放在每条隧道 protectionCheck.extra.env 里，
// 这里按服务器去重、取最新一次；没有数据 / 环境正常时不显示。
// 版式从 Servers 页 / ProtectionStatus 的小横幅搬了一份，不 import 它们，避免页面耦合。

import { useState } from "react";

import { scanServerEnv, type CloudEnv } from "../api";
import { errorText } from "../store/commands";
import { useServer, useStore } from "../store/useStore";
import { envNeedsAttention, selectServerEnv } from "./list/selectors";

interface Props {
  /** 只给服务器 id：组件自己去该服务器各隧道的保护检查结果里取环境 */
  serverId: string;
}

/** 「我手动确认过了」的冷静期：7 天内不再显示（与 Servers 页共用 ui.dismissed 的键） */
const UI_DISMISSED_KEY = "ui.dismissed";
const PANEL_DISMISS_TTL_MS = 7 * 24 * 60 * 60 * 1000;

function dismissedKey(serverId: string): string {
  return `panel:${serverId}`;
}

function isDismissed(serverId: string): boolean {
  try {
    const raw = localStorage.getItem(UI_DISMISSED_KEY);
    if (!raw) return false;
    const parsed = JSON.parse(raw) as unknown;
    if (!parsed || typeof parsed !== "object") return false;
    const at = (parsed as Record<string, unknown>)[dismissedKey(serverId)];
    return typeof at === "number" && Date.now() - at < PANEL_DISMISS_TTL_MS;
  } catch {
    return false;
  }
}

function dismiss(serverId: string): void {
  let all: Record<string, unknown> = {};
  try {
    const raw = localStorage.getItem(UI_DISMISSED_KEY);
    const parsed = raw ? (JSON.parse(raw) as unknown) : null;
    if (parsed && typeof parsed === "object") all = parsed as Record<string, unknown>;
  } catch {
    // 读不出来就只记这次会话
  }
  all[dismissedKey(serverId)] = Date.now();
  try {
    localStorage.setItem(UI_DISMISSED_KEY, JSON.stringify(all));
  } catch {
    // 存不了就算了，最多下次再提示一遍
  }
}

/** 按 D3 §15 / Servers 页文案（这里只处理需要提醒的几种环境） */
function envText(env: CloudEnv): string {
  if (env.web === "apache") {
    return "这台服务器上跑的是 Apache，不是 nginx。这个应用生成的是 nginx 的配置片段，贴了不会生效。两个办法：① 在面板里新建站点时选 nginx（宝塔/小皮都支持）；② 先把片段留着，换成 nginx 再用。";
  }
  if (env.web === "both") {
    return "这台机器上 nginx 和 Apache 都在。贴之前先确认你的站点是哪一个在管（面板里的网站列表一般会写 nginx 还是 Apache）。";
  }
  if (env.web === "none") {
    return "没找到 nginx，也没有 Apache。可能网站服务没装，或者装在特殊路径。这种情况先别贴片段。";
  }
  if (env.web === "unknown") {
    return "没认出来这台服务器用的什么面板和网站服务（nginx 还是 Apache 都没找到）。先别贴片段，怕贴错地方；可以在服务器上执行 nginx -v 或 httpd -v 看看，再点「重新识别」。";
  }
  if (env.panel === "none") {
    return "这台服务器没发现面板，但装了 nginx。配置要自己改 nginx 的站点文件；下面给的片段还是能用的。";
  }
  return "没认出来这台服务器用的什么面板；不过网站服务是 nginx，配置片段能用，站点目录可能要手动确认。";
}

export function ServerEnvBanner({ serverId }: Props) {
  const stored = useStore((s) => selectServerEnv(s, serverId));
  const server = useServer(serverId);
  const [fresh, setFresh] = useState<CloudEnv | null>(null);
  const [dismissed, setDismissed] = useState(() => isDismissed(serverId));
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState("");

  // 刚识别完用本地结果先显示（服务端缓存要等下一次云端检查才会进快照）
  const env = fresh ?? stored;
  if (!env || !envNeedsAttention(env) || dismissed) return null;

  const rescan = async () => {
    setBusy(true);
    setMsg("");
    try {
      const next = await scanServerEnv(serverId);
      setFresh(next);
    } catch (e) {
      setMsg(`识别没成功：${errorText(e)}`);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className={`env-banner ${env.web === "apache" ? "error" : "warn"}`}>
      <p>
        {server ? `${server.name}：` : ""}
        {envText(env)}
      </p>
      <div className="env-banner-actions">
        <button className="link-btn" disabled={busy} onClick={() => void rescan()}>
          {busy ? "识别中…" : "重新识别"}
        </button>
        <button
          className="link-btn"
          onClick={() => {
            dismiss(serverId);
            setDismissed(true);
          }}
        >
          我手动确认过了
        </button>
      </div>
      {msg && <div className="field-hint">{msg}</div>}
    </div>
  );
}
