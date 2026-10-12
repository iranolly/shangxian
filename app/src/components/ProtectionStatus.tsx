// 访问保护状态（「访问保护」页每行下面用）：
// - 持久黄标「待核对」：从快照的 protectionCheck 读（unknown / 超过 24 小时 / drift）；
// - 漂移黄条：云端配置和设置对不上时给「打开云端配置 / 暂时不管」；
// - HTTPS 红黄灯：从检查结果的 protectionCheck.extra 读（D3 §4）；
// - 非 nginx 横幅：按 D3 §15 文案，给「重新识别 / 我手动确认过了」。

import { useEffect, useState } from "react";

import {
  checkTunnelAccess,
  checkTunnelCloud,
  scanServerEnv,
  type AccessCheck,
  type CloudCheckResult,
  type ProtectionCheck,
  type TunnelView,
} from "../api";
import { errorText } from "../store/commands";
import { useServer } from "../store/useStore";

type CheckState =
  | { phase: "idle" }
  | { phase: "loading" }
  | { phase: "done"; actual: AccessCheck; cloud: CloudCheckResult | null; at: number }
  | { phase: "error"; message: string };

/** 把 access 数组拆成两个开关，空数组/缺失 = 都没开。 */
export function accessFlags(access?: string[] | null): {
  password: boolean;
  ip: boolean;
} {
  const list = access ?? [];
  return { password: list.includes("password"), ip: list.includes("ip") };
}

/** 把 accessDetail / 云端 allowIps 里的 IP 拆出来（中英文逗号、空格都算分隔）。 */
function splitIps(detail?: string | null): string[] {
  if (!detail) return [];
  return detail.split(/[,，\s]+/).filter(Boolean);
}

/** 数一下 accessDetail 里填了几个 IP。 */
function countIps(detail?: string | null): number {
  return splitIps(detail).length;
}

/** 当前设置的完整说法（chip 用）：两者都开 / 只有密码 / 只有 IP / 无保护 */
export function protectionSummary(t: TunnelView): {
  tone: "ok" | "warn";
  label: string;
} {
  const { password, ip } = accessFlags(t.access);
  if (password && ip) {
    return {
      tone: t.hasPassword ? "ok" : "warn",
      label: t.hasPassword ? "密码 + IP 白名单" : "密码 + IP 白名单（还没设密码）",
    };
  }
  if (password) {
    return t.hasPassword
      ? { tone: "ok", label: `要密码（用户名 ${t.accessUser ?? "shangxian"}）` }
      : { tone: "warn", label: "选了要密码，但还没设" };
  }
  if (ip) {
    const n = countIps(t.accessDetail);
    return n > 0
      ? { tone: "ok", label: `仅白名单 IP（${n} 个）` }
      : { tone: "warn", label: "仅白名单 IP（还没填 IP）" };
  }
  return { tone: "warn", label: "无访问保护" };
}

/** 云端实际保护的说法（云端检查结果里用）。 */
export function cloudLabel(c: AccessCheck): string {
  if (c.passwordOn && c.ipOn) return "密码 + IP 白名单";
  if (c.passwordOn) return "要密码";
  if (c.ipOn) return `仅白名单 IP（${c.allowIps.length} 个）`;
  return "无访问保护";
}

/**
 * 把「检查云端」的结果和当前设置比一比，给出一句结论。
 * confFound=false 或和设置对不上 → warn；对得上 → ok。
 */
export function cloudCheck(
  t: TunnelView,
  c: AccessCheck,
): { tone: "ok" | "warn"; text: string; htpasswdMissing: boolean } {
  const { password, ip } = accessFlags(t.access);
  const htpasswdMissing = c.passwordOn && !c.htpasswdExists;
  if (!c.confFound) {
    return {
      tone: "warn",
      text: "云端没找到这条隧道的站点配置（可能还没在面板里建站）",
      htpasswdMissing,
    };
  }
  const sameFlags = password === c.passwordOn && ip === c.ipOn;
  // 两边都开着 IP 白名单时，顺便看名单内容是不是同一批。
  // 先各自去重再比：设置里重复填同一个 IP 不该被判成「不一致」。
  const cloudIps = [...new Set(c.allowIps.map((s) => s.trim()).filter(Boolean))].sort();
  const settingIps = [...new Set(splitIps(t.accessDetail))].sort();
  // ip 关掉时不参与名单对比；开着时两边都必须有内容，且集合完全相等才算一致。
  // 云端 allowIps 为空（只有 deny all）判不一致：那等于所有人都进不去。
  const sameIps =
    !ip ||
    (settingIps.length > 0 &&
      cloudIps.length > 0 &&
      settingIps.join(",") === cloudIps.join(","));
  const same = sameFlags && sameIps;
  return {
    tone: same ? "ok" : "warn",
    text: same
      ? `云端已生效：${cloudLabel(c)}`
      : `云端实际是：${cloudLabel(c)}（设置还没生效，请重新复制云端配置贴到面板里）`,
    htpasswdMissing,
  };
}

// ---------- 本地「我已知晓」记录（ui.dismissed） ----------

interface Dismissed {
  [key: string]: number;
}

function readDismissed(): Dismissed {
  try {
    const raw = localStorage.getItem("ui.dismissed");
    if (!raw) return {};
    const parsed = JSON.parse(raw) as unknown;
    return parsed && typeof parsed === "object" ? (parsed as Dismissed) : {};
  } catch {
    return {};
  }
}

function isDismissed(key: string, ttlMs: number): boolean {
  const at = readDismissed()[key];
  return typeof at === "number" && Date.now() - at < ttlMs;
}

function dismiss(key: string): void {
  try {
    const all = readDismissed();
    all[key] = Date.now();
    localStorage.setItem("ui.dismissed", JSON.stringify(all));
  } catch {
    // 记不住就这次会话里不显示
  }
}

const DAY_MS = 24 * 60 * 60 * 1000;
const HTTPS_DISMISS_MS = 24 * 60 * 60 * 1000;
const PANEL_DISMISS_MS = 7 * DAY_MS;
const FRESH_MS = 24 * 60 * 60 * 1000;

// ---------- protectionCheck.extra 里的 HTTPS / warnings ----------

export interface ExtraHttps {
  enabled?: boolean;
  certDays?: number | null;
  redirect80?: boolean;
}

export function readExtra(check?: ProtectionCheck | null): {
  https: ExtraHttps | null;
  warnings: string[];
} {
  const extra = check?.extra;
  if (!extra || typeof extra !== "object") return { https: null, warnings: [] };
  const obj = extra as Record<string, unknown>;
  const rawHttps = obj.https;
  const https =
    rawHttps && typeof rawHttps === "object" ? (rawHttps as ExtraHttps) : null;
  const warnings = Array.isArray(obj.warnings)
    ? obj.warnings.filter((w): w is string => typeof w === "string")
    : [];
  return { https, warnings };
}

export function httpsLight(
  t: TunnelView,
  https: ExtraHttps | null,
): { level: "red" | "yellow"; text: string } | null {
  if (!https) return null;
  const { password, ip } = accessFlags(t.access);
  const urlHttps = (t.url ?? "").startsWith("https://");
  const plainOpen = https.enabled === true && https.redirect80 === false;
  if (urlHttps && https.enabled === false) {
    return {
      level: "red",
      text: "网址写的是 https，但服务器上没有找到 443 的站点。外面打不开，先回面板把证书和 https 开上。",
    };
  }
  if (typeof https.certDays === "number" && https.certDays <= 0) {
    return {
      level: "red",
      text: "证书已经过期了，网页会报警告。去面板点一下「续签证书」。",
    };
  }
  if (typeof https.certDays === "number" && https.certDays <= 7) {
    return {
      level: "red",
      text: `证书还剩 ${https.certDays} 天，快到期了。去面板点一下「续签证书」；续期用的 /.well-known 那一小块要留着别删。`,
    };
  }
  if (password && plainOpen) {
    return {
      level: "red",
      text: "密码现在能用 http:// 明文提交，等于把密码写在明信片上。请到面板打开「强制 HTTPS」（80 跳 443）。",
    };
  }
  if (ip && plainOpen) {
    return {
      level: "yellow",
      text: "网站用 http:// 打开时内容不加密。虽然只有白名单 IP 能进，还是建议在面板打开「强制 HTTPS」。",
    };
  }
  if (!password && !ip && plainOpen) {
    return {
      level: "yellow",
      text: "这个网站 http:// 和 https:// 都能开。建议在面板打开「强制 HTTPS」，让 http:// 自动跳到 https://。",
    };
  }
  if (typeof https.certDays === "number" && https.certDays <= 30) {
    return {
      level: "yellow",
      text: `证书还剩 ${https.certDays} 天，注意一下。面板一般会自动续，续不上会红。`,
    };
  }
  return null;
}

// ---------- 漂移字段 → 人话 ----------

const DRIFT_TEXT: Record<string, string> = {
  conf: "云端没找到这条隧道的站点配置",
  password: "密码开关不一样",
  ip: "IP 白名单开关不一样",
  allowIps: "白名单里的 IP 不一样",
  htpasswd: "服务器上还没有密码文件",
};

function driftText(fields: string[]): string {
  const list = fields.map((f) => DRIFT_TEXT[f] ?? f).join("、");
  return list || "云端和设置对不上";
}

// ---------- 环境横幅文案（D3 §15） ----------

export function envText(web: string): string {
  if (web === "apache") {
    return "这台服务器上跑的是 Apache，不是 nginx。这个应用生成的是 nginx 的配置片段，贴了不会生效。两个办法：① 在面板里新建站点时选 nginx（宝塔/小皮都支持）；② 先把片段留着，换成 nginx 再用。";
  }
  if (web === "none") {
    return "没找到 nginx，也没有 Apache。可能网站服务没装，或者装在特殊路径。这种情况先别贴片段。";
  }
  if (web === "unknown") {
    return "没认出来这台服务器用的什么面板和网站服务（nginx 还是 Apache 都没找到）。先别贴片段，怕贴错地方；可以在服务器上执行 nginx -v 或 httpd -v 看看，再点「重新识别」。";
  }
  if (web === "both") {
    return "这台机器上 nginx 和 Apache 都在。贴之前先确认你的站点是哪一个在管（面板里的网站列表一般会写 nginx 还是 Apache）。";
  }
  return "";
}

function timeText(at: number): string {
  return new Date(at).toLocaleString("zh-CN", {
    month: "numeric",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
  });
}

interface Props {
  tunnel: TunnelView;
  /** 打开这条隧道的「云端配置」弹层（漂移条的按钮） */
  onOpenNginx?: (tunnel: TunnelView) => void;
}

export function ProtectionStatus({ tunnel, onOpenNginx }: Props) {
  const [state, setState] = useState<CheckState | null>(null);
  const [driftHidden, setDriftHidden] = useState(false);
  const [envBusy, setEnvBusy] = useState(false);
  const [envMsg, setEnvMsg] = useState("");
  /** 浏览器预览 / 刚识别完：识别结果显示用（快照可能还没更新） */
  const [envOverride, setEnvOverride] = useState<string | null | undefined>(undefined);
  const server = useServer(tunnel.serverId);
  const summary = protectionSummary(tunnel);
  const { password, ip } = accessFlags(tunnel.access);
  const check = tunnel.protectionCheck;
  const now = Date.now();

  // 把设置拼成一个字符串：快照每 800ms 刷新一次，内容没变时不要清掉检查结果。
  const settingKey = `${tunnel.id}|${password ? "p" : ""}${ip ? "i" : ""}|${
    tunnel.hasPassword ? 1 : 0
  }|${tunnel.accessDetail ?? ""}|${tunnel.url ?? ""}|${tunnel.accessUser ?? ""}`;

  useEffect(() => {
    setState(null);
  }, [settingKey]);

  const run = async () => {
    setState({ phase: "loading" });
    try {
      const cloud = await checkTunnelCloud(tunnel.id, "full");
      const actual =
        cloud.actual ??
        cloud.check.cloud ?? {
          confFound: false,
          passwordOn: false,
          ipOn: false,
          allowIps: [],
          htpasswdExists: false,
        };
      setState({ phase: "done", actual, cloud, at: Date.now() });
    } catch {
      // 新命令没上线 / 浏览器预览：退回旧的 check_tunnel_access
      try {
        const actual = await checkTunnelAccess(tunnel.id);
        setState({ phase: "done", actual, cloud: null, at: Date.now() });
      } catch (e) {
        setState({ phase: "error", message: errorText(e) });
      }
    }
  };

  const env =
    envOverride !== undefined ? envOverride : (server?.cloudEnv?.web ?? null);
  const nonNginx = env !== null && env !== "nginx";
  const panelDismissedKey = `panel:${tunnel.serverId}`;
  const [panelDismissed, setPanelDismissed] = useState(() =>
    isDismissed(panelDismissedKey, PANEL_DISMISS_MS),
  );

  const rescanEnv = async () => {
    if (!server) return;
    setEnvBusy(true);
    setEnvMsg("");
    try {
      const result = await scanServerEnv(server.id);
      setEnvOverride(result.web);
      setEnvMsg(`重新识别完成：网站服务是 ${result.web}`);
    } catch (e) {
      setEnvMsg(`识别没成功：${errorText(e)}`);
    } finally {
      setEnvBusy(false);
    }
  };

  const freshCheck =
    state?.phase === "done" && state.cloud ? state.cloud.check : null;
  const { https, warnings } = readExtra(freshCheck ?? check);
  const light = httpsLight(tunnel, https);
  const httpsDismissKey = `https:${tunnel.id}`;
  const [httpsDismissed, setHttpsDismissed] = useState(() =>
    isDismissed(httpsDismissKey, HTTPS_DISMISS_MS),
  );

  // 持久 chip：unknown / 超过 24 小时 / drift 都算「待核对」
  const checkStatus = check?.status ?? "unknown";
  const checkTs = check?.ts ?? null;
  const stale = checkTs === null || now - checkTs > FRESH_MS;
  let chipTone: "ok" | "warn" | "danger";
  let chipLabel: string;
  if (!password && !ip) {
    chipTone = "danger";
    chipLabel = "无访问保护";
  } else if (checkStatus === "ok" && !stale) {
    chipTone = "ok";
    chipLabel = `✓ ${summary.label}`;
  } else if (checkStatus === "error") {
    chipTone = "warn";
    chipLabel = `${summary.label} · 核不上`;
  } else {
    chipTone = "warn";
    chipLabel = `${summary.label} · 待核对`;
  }
  const chipTitle =
    checkTs === null
      ? "还没核对过云端，点「检查云端」才算数"
      : `上次核对：${timeText(checkTs)}`;

  const result = state?.phase === "done" ? cloudCheck(tunnel, state.actual) : null;
  const showDrift = checkStatus === "drift" && !driftHidden;

  return (
    <div>
      <div className="protect-row">
        <span
          className={`chip ${chipTone === "danger" ? "danger" : chipTone}`}
          title={chipTitle}
        >
          {chipLabel}
        </span>
        <button
          className="link-btn"
          title={nonNginx ? "自动检查用不了，请按面板手册核对" : undefined}
          disabled={state?.phase === "loading"}
          onClick={() => void run()}
        >
          {state?.phase === "loading"
            ? "检查中…"
            : nonNginx
              ? "手动核对"
              : "检查云端"}
        </button>
        {state?.phase === "done" && (
          <span className="protect-result" style={{ color: "var(--text-3)" }}>
            上次检查 {timeText(state.at)}：
          </span>
        )}
        {result && (
          <span className={`protect-result ${result.tone}`}>
            {result.tone === "ok" ? "✓ " : "⚠ "}
            {result.text}
          </span>
        )}
        {result?.htpasswdMissing && (
          <span className="protect-result warn">
            ⚠ 服务器上还没有密码文件（htpasswd），密码可能进不去
          </span>
        )}
        {state?.phase === "error" && (
          <span className="protect-result warn">⚠ 检查失败：{state.message}</span>
        )}
      </div>

      {showDrift && (
        <div className="drift-bar">
          <span>
            云端和设置对不上：{driftText(check?.driftFields ?? [])}。重新贴一次云端配置。
          </span>
          {onOpenNginx && (
            <button className="link-btn" onClick={() => onOpenNginx(tunnel)}>
              打开云端配置
            </button>
          )}
          <button className="link-btn" onClick={() => setDriftHidden(true)}>
            暂时不管
          </button>
        </div>
      )}

      {checkStatus === "error" && (
        <div className="drift-bar">
          <span>云端核不上：{check?.error ?? "不知道原因"}</span>
          <button className="link-btn" onClick={() => void run()}>
            再检查一次
          </button>
          <button className="link-btn" onClick={() => setDriftHidden(true)}>
            暂时不管
          </button>
        </div>
      )}

      {light && !httpsDismissed && (
        <div className={light.level === "red" ? "error-line" : "warn-line"}>
          {light.level === "red" ? "红灯： " : "注意： "}
          {light.text}
          {onOpenNginx && (
            <>
              {" "}
              <button className="link-btn" onClick={() => onOpenNginx(tunnel)}>
                看怎么办
              </button>
            </>
          )}
          {light.level === "red" && (
            <>
              {" "}
              <button
                className="link-btn"
                onClick={() => {
                  dismiss(httpsDismissKey);
                  setHttpsDismissed(true);
                }}
              >
                我暂时不管
              </button>
            </>
          )}
        </div>
      )}

      {warnings.map((text, index) => (
        <div className="warn-line" key={index}>
          {text}
        </div>
      ))}

      {server && !panelDismissed && env === null && (
        <div className="env-banner">
          <span className="env-text">还没识别这台服务器的环境。</span>
          <button
            className="link-btn"
            disabled={envBusy}
            onClick={() => void rescanEnv()}
          >
            {envBusy ? "识别中…" : "识别一下"}
          </button>
        </div>
      )}
      {server && !panelDismissed && env !== null && env !== "nginx" && (
        <div className="env-banner">
          <span className="env-text">{envText(env ?? "")}</span>
          <button
            className="link-btn"
            disabled={envBusy}
            onClick={() => void rescanEnv()}
          >
            {envBusy ? "识别中…" : "重新识别"}
          </button>
          <button
            className="link-btn"
            onClick={() => {
              dismiss(panelDismissedKey);
              setPanelDismissed(true);
            }}
          >
            我手动确认过了
          </button>
        </div>
      )}
      {envMsg && <div className="field-hint">{envMsg}</div>}
    </div>
  );
}
