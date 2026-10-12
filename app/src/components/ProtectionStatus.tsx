// 访问保护状态（隧道卡片上用）：
// - 持久黄标「待核对」：从快照的 protectionCheck 读（unknown / 超过 24 小时 / drift）；
// - 漂移黄条：云端配置和设置对不上时给「打开云端配置 / 暂时不管」；
// - HTTPS 红黄灯：从检查结果的 protectionCheck.extra 读（D3 §4）。

import { useEffect, useState } from "react";

import {
  checkTunnelAccess,
  checkTunnelCloud,
  type AccessCheck,
  type CloudCheckResult,
  type ProtectionCheck,
  type TunnelView,
} from "../api";
import { errorText } from "../store/commands";

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
      label: t.hasPassword ? "密码 + IP 白名单" : "密码 + IP 白名单（尚未设置密码）",
    };
  }
  if (password) {
    return t.hasPassword
      ? { tone: "ok", label: `已加密（用户名 ${t.accessUser ?? "shangxian"}）` }
      : { tone: "warn", label: "已启用密码保护，但尚未设置密码" };
  }
  if (ip) {
    const n = countIps(t.accessDetail);
    return n > 0
      ? { tone: "ok", label: `仅白名单 IP（${n} 个）` }
      : { tone: "warn", label: "仅白名单 IP（尚未填写 IP）" };
  }
  return { tone: "warn", label: "无访问保护" };
}

/** 待核对 = 没查过 / unknown / 上次核对超过 24 小时（和旧「访问保护」页口径一致） */
export function protectionPending(t: TunnelView): boolean {
  const p = t.protectionCheck;
  if (!p || p.status === "unknown") return true;
  if (p.status !== "ok") return false;
  if (typeof p.ts !== "number") return true;
  return Date.now() - p.ts > FRESH_MS;
}

/** 云端实际保护的说法（云端检查结果里用）。 */
export function cloudLabel(c: AccessCheck): string {
  if (c.passwordOn && c.ipOn) return "密码 + IP 白名单";
  if (c.passwordOn) return "已加密";
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
): { tone: "ok" | "warn"; text: string; hint?: string; htpasswdMissing: boolean } {
  const { password, ip } = accessFlags(t.access);
  const htpasswdMissing = c.passwordOn && !c.htpasswdExists;
  if (!c.confFound) {
    return {
      tone: "warn",
      text: "云端未找到站点配置",
      hint: "可能尚未在面板中建站；建站后再检查一次",
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
    text: same ? `云端已生效：${cloudLabel(c)}` : `云端实际是：${cloudLabel(c)}`,
    hint: same ? undefined : "设置尚未生效，请重新复制云端配置并粘贴到面板",
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

const HTTPS_DISMISS_MS = 24 * 60 * 60 * 1000;
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
): { level: "red" | "yellow"; text: string; hint?: string } | null {
  if (!https) return null;
  const { password, ip } = accessFlags(t.access);
  const urlHttps = (t.url ?? "").startsWith("https://");
  const plainOpen = https.enabled === true && https.redirect80 === false;
  if (urlHttps && https.enabled === false) {
    return {
      level: "red",
      text: "网址是 https，但服务器上没有 443 站点",
      hint: "外部（访客）无法访问；请先到面板启用证书和 https",
    };
  }
  if (typeof https.certDays === "number" && https.certDays <= 0) {
    return {
      level: "red",
      text: "证书已过期",
      hint: "浏览器会提示警告；请到面板点击「续签证书」",
    };
  }
  if (typeof https.certDays === "number" && https.certDays <= 7) {
    return {
      level: "red",
      text: `证书还剩 ${https.certDays} 天，即将到期`,
      hint: "请到面板点击「续签证书」；续期使用的 /.well-known 路径不要删除",
    };
  }
  if (password && plainOpen) {
    return {
      level: "red",
      text: "密码可通过 http:// 明文提交",
      hint: "等同明文传输密码；请到面板启用「强制 HTTPS」（80 跳转 443）",
    };
  }
  if (ip && plainOpen) {
    return {
      level: "yellow",
      text: "http:// 打开时内容不加密",
      hint: "虽然仅白名单 IP 可访问，仍建议在面板启用「强制 HTTPS」",
    };
  }
  if (!password && !ip && plainOpen) {
    return {
      level: "yellow",
      text: "http:// 和 https:// 均可访问",
      hint: "建议在面板启用「强制 HTTPS」，将 http:// 自动跳转到 https://",
    };
  }
  if (typeof https.certDays === "number" && https.certDays <= 30) {
    return {
      level: "yellow",
      text: `证书还剩 ${https.certDays} 天`,
      hint: "请注意；面板通常会自动续期，续期失败会显示红灯",
    };
  }
  return null;
}

// ---------- 漂移字段 → 人话 ----------

const DRIFT_TEXT: Record<string, string> = {
  conf: "云端未找到这条隧道的站点配置",
  password: "密码开关不一致",
  ip: "IP 白名单开关不一致",
  allowIps: "白名单中的 IP 不一致",
  htpasswd: "服务器上还没有密码文件",
};

function driftText(fields: string[]): string {
  const list = fields.map((f) => DRIFT_TEXT[f] ?? f).join("、");
  return list || "云端与设置不一致";
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
  /** 点门禁小牌：打开「访问保护」弹层改密码 / 白名单 */
  onEdit?: (tunnel: TunnelView) => void;
}

export function ProtectionStatus({ tunnel, onOpenNginx, onEdit }: Props) {
  const [state, setState] = useState<CheckState | null>(null);
  const [driftHidden, setDriftHidden] = useState(false);
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
    chipLabel = `${summary.label} · 核对失败`;
  } else {
    chipTone = "warn";
    chipLabel = `${summary.label} · 待核对`;
  }
  const chipTitle =
    !password && !ip
      ? onEdit
        ? "当前无访问保护；点击可添加访问保护"
        : "这条隧道当前无访问保护"
      : checkTs === null
        ? "尚未核对云端，请点击「检查云端」"
        : `上次核对：${timeText(checkTs)}`;

  const result = state?.phase === "done" ? cloudCheck(tunnel, state.actual) : null;
  const showDrift = checkStatus === "drift" && !driftHidden;

  const chipClass = `chip${onEdit ? " chip-btn" : ""} ${
    chipTone === "danger" ? "danger" : chipTone
  }`;

  return (
    <div>
      <div className="protect-row">
        {onEdit ? (
          <button
            type="button"
            className={chipClass}
            title={chipTitle}
            onClick={() => onEdit(tunnel)}
          >
            {chipLabel}
          </button>
        ) : (
          <span className={chipClass} title={chipTitle}>
            {chipLabel}
          </span>
        )}
        <button
          className="link-btn"
          disabled={state?.phase === "loading"}
          onClick={() => void run()}
        >
          {state?.phase === "loading" ? "检查中…" : "检查云端"}
        </button>
        {state?.phase === "done" && (
          <span className="protect-result" style={{ color: "var(--text-3)" }}>
            上次检查 {timeText(state.at)}：
          </span>
        )}
        {result && (
          <span className={`protect-result ${result.tone}`} title={result.hint}>
            {result.tone === "ok" ? "✓ " : "⚠ "}
            {result.text}
          </span>
        )}
        {result?.htpasswdMissing && (
          <span
            className="protect-result warn"
            title="密码可能无法使用，请在云端配置中重新生成"
          >
            ⚠ 服务器上还没有密码文件
          </span>
        )}
        {state?.phase === "error" && (
          <span className="protect-result warn">⚠ 检查失败：{state.message}</span>
        )}
      </div>

      {showDrift && (
        <div className="drift-bar">
          <span
            title={`${driftText(check?.driftFields ?? [])}；重新粘贴一次云端配置即可修复`}
          >
            云端与设置不一致
          </span>
          {onOpenNginx && (
            <button className="link-btn" onClick={() => onOpenNginx(tunnel)}>
              打开云端配置
            </button>
          )}
          <button className="link-btn" onClick={() => setDriftHidden(true)}>
            暂不处理
          </button>
        </div>
      )}

      {checkStatus === "error" && (
        <div className="drift-bar">
          <span title={check?.error ?? "原因未知"}>云端核对失败</span>
          <button className="link-btn" onClick={() => void run()}>
            再检查一次
          </button>
          <button className="link-btn" onClick={() => setDriftHidden(true)}>
            暂不处理
          </button>
        </div>
      )}

      {light && !httpsDismissed && (
        <div
          className={light.level === "red" ? "error-line" : "warn-line"}
          title={light.hint}
        >
          {light.level === "red" ? "红灯： " : "注意： "}
          {light.text}
          {onOpenNginx && (
            <>
              {" "}
              <button className="link-btn" onClick={() => onOpenNginx(tunnel)}>
                查看解决方法
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
                暂不处理
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

    </div>
  );
}
