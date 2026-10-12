// 卡片上的访问保护徽标（D4 §3.3 ProtectionBadge / D2 §6.4 展示规则）。
// 只显示本地设置 + 最近一次核对结果；点整枚徽标去「访问保护」页。

import { protectionSummary, accessFlags } from "../ProtectionStatus";
import { useTunnel } from "../../store/useStore";
import { formatDay, olderThanHours } from "./time";

interface Props {
  tunnelId: string;
  /** 点击后去「访问保护」页（外壳没给导航能力时不打按钮） */
  onOpenProtection?: (tunnelId: string) => void;
}

export function ProtectionBadge({ tunnelId, onOpenProtection }: Props) {
  const tunnel = useTunnel(tunnelId);
  if (!tunnel) return null;

  const summary = protectionSummary(tunnel);
  const check = tunnel.protectionCheck;
  const { password, ip } = accessFlags(tunnel.access);
  const noProtection = !password && !ip;
  const stale = !check || check.status === "unknown" || olderThanHours(check.ts, 24);

  let tone: "ok" | "warn" | "danger" = "warn";
  let text = `无访问保护`;
  if (noProtection) {
    tone = "danger";
    text = `无访问保护`;
  } else if (!stale && check?.status === "ok") {
    tone = "ok";
    text = `✓ ${summary.label}`;
  } else if (check?.status === "error") {
    tone = "warn";
    text = `${summary.label} · 核不上`;
  } else {
    tone = "warn";
    text = `${summary.label} · 待核对`;
  }

  const title = noProtection
    ? "这条隧道谁都能进；想设门禁去「访问保护」页"
    : check?.error
      ? `云端核不上：${check.error}`
      : check?.ts
        ? `上次核对：${formatDay(check.ts)}`
        : "还没核对过云端，点「检查云端」才算数";

  const className = `chip chip-btn ${tone}`;
  if (onOpenProtection) {
    return (
      <button type="button" className={className} title={title} onClick={() => onOpenProtection(tunnelId)}>
        {text}
      </button>
    );
  }
  return (
    <span className={className} title={title}>
      {text}
    </span>
  );
}
