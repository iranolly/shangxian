// 「已连 2 小时 13 分 · 今日重连 3 次」（D4 §3.3 UptimeLine）。
// 只在正在发布时显示；每 20 秒刷新一次文本（分钟粒度，不逐秒重渲染）。

import { useEffect, useState } from "react";

import { useTunnel } from "../../store/useStore";
import { formatUptime } from "./time";

interface Props {
  tunnelId: string;
}

export function UptimeLine({ tunnelId }: Props) {
  const tunnel = useTunnel(tunnelId);
  const [, forceTick] = useState(0);

  useEffect(() => {
    const timer = window.setInterval(() => forceTick((n) => n + 1), 20_000);
    return () => window.clearInterval(timer);
  }, []);

  if (!tunnel || tunnel.status !== "running") return null;

  const uptime =
    tunnel.connectedSince && tunnel.connectedSince > 0
      ? Math.floor((Date.now() - tunnel.connectedSince) / 1000)
      : tunnel.uptimeSecs ?? 0;
  if (!tunnel.connectedSince && !tunnel.uptimeSecs) return null;

  const reconnects = tunnel.reconnectsToday ?? 0;
  const text =
    reconnects > 0
      ? `已连接 ${formatUptime(uptime)} · 今日重连 ${reconnects} 次`
      : `已连接 ${formatUptime(uptime)}`;

  return <span className="uptime-line">{text}</span>;
}
