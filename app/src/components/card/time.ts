// 卡片上的时间 / 时长文案（D4 §3.2 lib/format.ts 的等价实现，只保留卡片用得到的几个）。
// lib/ 不在本波（E2）的文件范围里，所以放在卡片目录下。

/** <60 → "刚刚"；<1 小时 → "X 分钟"；<1 天 → "X 小时 Y 分"（Y=0 省略）；否则 "X 天 Y 小时" */
export function formatUptime(secs: number): string {
  const total = Math.max(0, Math.floor(secs));
  if (total < 60) return "刚刚";
  if (total < 3600) return `${Math.floor(total / 60)} 分钟`;
  if (total < 86400) {
    const hours = Math.floor(total / 3600);
    const mins = Math.floor((total % 3600) / 60);
    return mins === 0 ? `${hours} 小时` : `${hours} 小时 ${mins} 分`;
  }
  const days = Math.floor(total / 86400);
  const hours = Math.floor((total % 86400) / 3600);
  return hours === 0 ? `${days} 天` : `${days} 天 ${hours} 小时`;
}

/** <60 → "X 秒后"；≥60 → "X 分钟后"（向上取整） */
export function formatRetryIn(secs: number): string {
  const total = Math.max(0, Math.ceil(secs));
  if (total < 60) return `${total} 秒后`;
  return `${Math.ceil(total / 60)} 分钟后`;
}

function clock(at: number): string {
  const d = new Date(at);
  const hh = String(d.getHours()).padStart(2, "0");
  const mm = String(d.getMinutes()).padStart(2, "0");
  return `${hh}:${mm}`;
}

function startOfDay(at: number): number {
  const d = new Date(at);
  d.setHours(0, 0, 0, 0);
  return d.getTime();
}

/** "今天 15:30" / "昨天 15:30" / "10 月 12 日 15:30" */
export function formatDay(at: number): string {
  if (!Number.isFinite(at) || at <= 0) return "尚无记录";
  const today = startOfDay(Date.now());
  const day = startOfDay(at);
  if (day === today) return `今天 ${clock(at)}`;
  if (day === today - 86_400_000) return `昨天 ${clock(at)}`;
  const d = new Date(at);
  return `${d.getMonth() + 1} 月 ${d.getDate()} 日 ${clock(at)}`;
}

/** 距今是否超过 24 小时（保护徽标「待核对」判断用） */
export function olderThanHours(at: number | null | undefined, hours: number): boolean {
  if (!at || !Number.isFinite(at) || at <= 0) return true;
  return Date.now() - at > hours * 3_600_000;
}
