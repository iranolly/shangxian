// 重试倒计时 / 掉线提示（D4 §3.3 RetryLine）。
// 倒计时以「收到快照的本地时刻」为基准：每个轮询周期后端会重给 retryInSecs，误差 ≤1 个周期。

import { useEffect, useState } from "react";

import { actions } from "../../store/actions";
import { usePending } from "../../store/pendingStore";
import { useTunnel } from "../../store/useStore";
import { formatRetryIn } from "./time";

interface Props {
  tunnelId: string;
}

export function RetryLine({ tunnelId }: Props) {
  const tunnel = useTunnel(tunnelId);
  const pending = usePending(`tunnel:${tunnelId}`);
  const retryInSecs = tunnel?.retryInSecs ?? null;
  const [baseline, setBaseline] = useState<{ secs: number; at: number } | null>(null);
  const [, setTick] = useState(0);

  // 快照给了新的倒计时：重新记基准时刻
  useEffect(() => {
    if (retryInSecs == null) {
      setBaseline(null);
      return;
    }
    setBaseline({ secs: retryInSecs, at: Date.now() });
  }, [retryInSecs]);

  useEffect(() => {
    if (!baseline) return;
    const timer = window.setInterval(() => setTick((n) => n + 1), 1000);
    return () => window.clearInterval(timer);
  }, [baseline]);

  if (!tunnel) return null;
  // 正在发布 / 已暂停 / 出错：都不显示倒计时（出错由修复带负责）
  if (tunnel.status === "running" || tunnel.status === "paused" || tunnel.status === "error") {
    return null;
  }

  const retryState = tunnel.retryState ?? (tunnel.enabled ? "active" : "stopped");
  if (retryState === "active" && baseline) {
    const elapsed = Math.floor((Date.now() - baseline.at) / 1000);
    const remaining = Math.max(0, baseline.secs - elapsed);
    return (
      <span className="retry-line">
        {remaining > 0
          ? `第 ${(tunnel.attempt ?? 0) + 1} 次重试，${formatRetryIn(remaining)}`
          : "正在重试…"}
      </span>
    );
  }

  if (tunnel.status === "disconnected") {
    return (
      <span className="retry-line">
        {retryState === "stopped" ? "重试停了，点「立刻重连」再试" : "掉线了，正在自动重试"}
        <button
          className="link-btn"
          disabled={pending}
          aria-busy={pending}
          onClick={() => actions.tunnelRestart(tunnelId)}
        >
          立刻重连
        </button>
      </span>
    );
  }

  return null;
}
