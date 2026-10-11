// 访问保护状态（只在「访问保护」页用）：
// 上半行显示「当前设置」，点「检查云端」再去服务器上读 nginx 实际生效的保护。

import { useEffect, useState } from "react";

import { checkTunnelAccess, type AccessCheck, type TunnelView } from "../api";

type CheckState =
  | { phase: "loading" }
  | { phase: "done"; check: AccessCheck }
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

export function ProtectionStatus({ tunnel }: { tunnel: TunnelView }) {
  const [state, setState] = useState<CheckState | null>(null);
  const summary = protectionSummary(tunnel);
  const { password, ip } = accessFlags(tunnel.access);
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
      setState({ phase: "done", check: await checkTunnelAccess(tunnel.id) });
    } catch (e) {
      setState({ phase: "error", message: String(e) });
    }
  };

  const result = state?.phase === "done" ? cloudCheck(tunnel, state.check) : null;
  // 绿色只在「检查云端」确认对得上之后；之前一律黄色「待核对」
  const verified = Boolean(result && result.tone === "ok");
  const chipTone = verified && summary.tone === "ok" ? "ok" : "warn";
  const chipLabel = verified ? summary.label : `${summary.label} · 待核对`;

  return (
    <div className="protect-row">
      <span className={`chip ${chipTone}`}>{chipLabel}</span>
      <button
        className="link-btn"
        disabled={state?.phase === "loading"}
        onClick={() => void run()}
      >
        {state?.phase === "loading" ? "检查中…" : "检查云端"}
      </button>
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
  );
}
