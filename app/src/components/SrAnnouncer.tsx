// 全局屏幕阅读器播报区（D4 §6.2 ⑲）。
// 只播报「掉线 / 恢复 / 批量操作完成 / 体检失败」这类关键变化，不播报 uptime 倒计时；3 秒节流，避免刷屏。

import { useEffect, useRef, useSyncExternalStore } from "react";

import type { TunnelStatus } from "../copy/status";
import { getState, subscribe } from "../store/snapshotStore";
import { getVisibleToasts, subscribeToasts } from "../store/toastStore";
import { useLastDoctorReport } from "./DoctorModal";

const THROTTLE_MS = 3000;

// 播报队列放模块级：掉线/恢复、批量完成、体检失败共用同一队列，3 秒一批。
const queue: string[] = [];
let timer: number | null = null;
let lastText = "";
let current = "";
const listeners = new Set<() => void>();

function show(message: string): void {
  if (message === lastText) return;
  lastText = message;
  current = message;
  for (const fn of listeners) fn();
}

function flush(): void {
  timer = null;
  if (queue.length === 0) return;
  show(queue.splice(0, 3).join("；"));
  if (queue.length > 0) timer = window.setTimeout(flush, THROTTLE_MS);
}

/** 加一条播报：第一条立刻播，后面的按 3 秒一批。 */
function announce(message: string): void {
  queue.push(message);
  if (timer !== null) return;
  const first = queue.shift();
  if (first) show(first);
  timer = window.setTimeout(flush, THROTTLE_MS);
}

function subscribeSr(fn: () => void): () => void {
  listeners.add(fn);
  return () => {
    listeners.delete(fn);
  };
}

/** BulkBar 的完成 toast：全部成功 / 部分失败 / 批量删除（key 带 bulk-）。 */
const BULK_DONE_RE = /条已处理，全部成功$|条中有 \d+ 条失败/;

export function SrAnnouncer() {
  const text = useSyncExternalStore(subscribeSr, () => current, () => current);
  const doctorReport = useLastDoctorReport();
  const announcedReportAt = useRef(0);

  // 掉线 / 恢复：盯每条隧道的状态变化
  useEffect(() => {
    const statuses = new Map<string, TunnelStatus>();
    for (const tunnel of getState().tunnels) statuses.set(tunnel.id, tunnel.status);

    const unsubscribe = subscribe(() => {
      const state = getState();
      for (const tunnel of state.tunnels) {
        const before = statuses.get(tunnel.id);
        statuses.set(tunnel.id, tunnel.status);
        if (before === undefined || before === tunnel.status) continue;
        const wasBad = before === "disconnected" || before === "error";
        const isBad = tunnel.status === "disconnected" || tunnel.status === "error";
        if (!wasBad && isBad) announce(`「${tunnel.name}」已断开`);
        else if (wasBad && tunnel.status === "running") announce(`「${tunnel.name}」已恢复`);
      }
      const alive = new Set(state.tunnels.map((t) => t.id));
      for (const id of [...statuses.keys()]) {
        if (!alive.has(id)) statuses.delete(id);
      }
    });

    return unsubscribe;
  }, []);

  // 批量操作完成：BulkBar 只发 toast，这里照单播报
  useEffect(() => {
    const seen = new Set(getVisibleToasts().map((t) => t.id));
    return subscribeToasts(() => {
      for (const t of getVisibleToasts()) {
        if (seen.has(t.id)) continue;
        seen.add(t.id);
        if (t.key.startsWith("bulk-") || BULK_DONE_RE.test(t.text)) {
          announce(`批量操作完成：${t.text}`);
        }
      }
    });
  }, []);

  // 体检失败：doctorStore 里最近一次报告有没通过的项就播报；同一份只播一次
  useEffect(() => {
    if (!doctorReport || doctorReport.summary.fail <= 0) return;
    if (announcedReportAt.current === doctorReport.checkedAt) return;
    announcedReportAt.current = doctorReport.checkedAt;
    announce(`体检未通过，有 ${doctorReport.summary.fail} 项未通过`);
  }, [doctorReport]);

  return (
    <div className="sr-only" role="status" aria-live="polite">
      {text}
    </div>
  );
}
