// 日志行分类纯函数（D4 §3.2）。
// 「人话行」= 事件行（origin === "events"），或正文含中文且不含 keepalive / debug / trace / 心跳 这类机器噪声。
// 其余分「错误行」（level 为 error/warn 或正文命中错误词）和「技术行」（tech）：一行只归一类。

import type { LogEntry } from "../api";

export type LogFilter = "all" | "human" | "error";

const TECH_NOISE_RE = /keepalive|debug|trace|心跳/i;
const CJK_RE = /[\u4e00-\u9fff]/;
const ERROR_RE =
  /(\berror\b|\bfatal\b|failed|failure|refused|denied|timeout|timed out|失败|错误|拒绝|超时|断开|异常)/i;

/**
 * 分类规则（D4 §3.2）：
 * 1. 事件行永远算「人话」——通知中心 / 事件日志本来就是给人看的；
 * 2. 其余错误行（level 或正文命中错误词）算「error」；
 * 3. 含中文且不含机器噪声的算「human」；
 * 4. 剩下的是「tech」。
 */
export function classifyLine(e: LogEntry): "human" | "tech" | "error" {
  if (e.origin === "events") return "human";
  if (e.level === "error" || e.level === "warn" || ERROR_RE.test(e.raw)) return "error";
  if (CJK_RE.test(e.raw) && !TECH_NOISE_RE.test(e.raw)) return "human";
  return "tech";
}
