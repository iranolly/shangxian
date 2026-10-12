// 体检弹层（D4 §3.6 / D2 §14）。
// 9 步固定顺序；第一处失败自动展开「怎么办」；全过绿条。
// 结果缓存在模块级 doctorStore（会话内），供 AboutCard 的 SSH 行复用。

import { useEffect, useRef, useState, useSyncExternalStore } from "react";

import { openDataDir, runDoctor, type DoctorReport } from "../api";
import { DOCTOR_STATUS_META, type DoctorStatus } from "../copy/status";
import { copyWithToast } from "../copy/clipboard";
import { T } from "../copy/toasts";
import { toast } from "../store/toastStore";
import { DiagExportModal } from "./DiagExportModal";
import { useModalA11y } from "./useModalA11y";

/** D2 §14.2 固定 9 步标题（体检中占位用，顺序不得改） */
const FIXED_STEP_TITLES = [
  "程序状态",
  "文件夹",
  "SSH 程序",
  "本机端口",
  "云端服务器",
  "发布隧道",
  "云端配置",
  "公网访问",
  "域名解析",
];

const ADVICE_FALLBACK =
  "照着上面的结果检查一下；不确定就点「导出诊断包」发给懂行的人。";

// ---------- 会话内的体检结果缓存（doctorStore） ----------

let lastReport: DoctorReport | null = null;
const reportListeners = new Set<() => void>();

function publishReport(report: DoctorReport): void {
  lastReport = report;
  for (const fn of reportListeners) fn();
}

export function getLastDoctorReport(): DoctorReport | null {
  return lastReport;
}

function subscribeReport(fn: () => void): () => void {
  reportListeners.add(fn);
  return () => {
    reportListeners.delete(fn);
  };
}

/** 供 AboutCard 等复用最近一次体检结果（没跑过返回 null）。 */
export function useLastDoctorReport(): DoctorReport | null {
  return useSyncExternalStore(subscribeReport, getLastDoctorReport, getLastDoctorReport);
}

// ---------- 汇总条 ----------

interface SummaryView {
  tone: "ok" | "warn" | "error";
  title: string;
  text: string;
}

/** 兼容旧后端的状态词：ok → pass，skipped → skip（D0 第 1 条）。 */
function statusMeta(status: string) {
  const normalized = status === "ok" ? "pass" : status === "skipped" ? "skip" : status;
  return DOCTOR_STATUS_META[normalized as DoctorStatus] ?? DOCTOR_STATUS_META.skip;
}

function summaryView(report: DoctorReport): SummaryView {
  const { fail, warn, pass, skip } = report.summary;
  if (fail > 0) {
    return {
      tone: "error",
      title: `有 ${fail} 项没通过`,
      text: "先看第一处失败，照着“怎么办”做。",
    };
  }
  if (warn > 0) {
    return {
      tone: "warn",
      title: `有 ${warn} 项要留意`,
      text: "不是坏事，但最好照着“怎么办”看一眼。",
    };
  }
  if (pass > 0 && skip > 0) {
    return {
      tone: "warn",
      title: `有 ${skip} 项跳过了`,
      text: "想查得更全，切「深度检查」再跑一次。",
    };
  }
  if (pass > 0) {
    return { tone: "ok", title: "体检全过了", text: `${pass} 项都正常。` };
  }
  return {
    tone: "warn",
    title: "体检没跑出结果",
    text: "等一下再试；还不行就点「导出诊断包」发给懂行的人。",
  };
}

interface Props {
  /** 打开就自动开跑（默认 true）；传 false 只展示上次结果 */
  autoRun?: boolean;
  onClose(): void;
}

export function DoctorModal({ autoRun, onClose }: Props) {
  const dialogRef = useRef<HTMLDivElement | null>(null);
  useModalA11y(dialogRef, onClose);

  const [mode, setMode] = useState<"quick" | "deep">(
    () => getLastDoctorReport()?.mode ?? "quick",
  );
  const [running, setRunning] = useState(false);
  const [elapsed, setElapsed] = useState(0);
  const [report, setReport] = useState<DoctorReport | null>(() => getLastDoctorReport());
  const [error, setError] = useState("");
  const [openFix, setOpenFix] = useState<string | null>(null);
  const [diagOpen, setDiagOpen] = useState(false);
  const startedRef = useRef(false);

  const run = async (nextMode: "quick" | "deep") => {
    setRunning(true);
    setError("");
    setElapsed(0);
    const startedAt = Date.now();
    try {
      const next = await runDoctor({ mode: nextMode });
      setReport(next);
      publishReport(next);
      const firstIssue = next.steps.find((s) => s.status === "fail" || s.status === "warn");
      setOpenFix(firstIssue?.id ?? null);
      if (next.summary.fail > 0) {
        toast.show({ kind: "warn", text: T.doctorDone(next.summary.pass, next.summary.fail) });
      } else if (next.summary.warn > 0) {
        toast.show({
          kind: "info",
          text: `体检完成：${next.summary.pass} 项通过，${next.summary.warn} 项要留意`,
        });
      } else if (next.summary.pass > 0) {
        toast.show({ kind: "success", text: T.doctorDone(next.summary.pass, 0) });
      }
    } catch (e) {
      setError(String(e) || "体检没跑起来");
    } finally {
      setRunning(false);
      setElapsed(Math.max(1, Math.round((Date.now() - startedAt) / 1000)));
    }
  };

  useEffect(() => {
    if (autoRun === false || startedRef.current) return;
    startedRef.current = true;
    void run("quick");
  }, []);

  useEffect(() => {
    if (!running) return;
    const startedAt = Date.now();
    const timer = window.setInterval(() => {
      setElapsed(Math.floor((Date.now() - startedAt) / 1000));
    }, 1000);
    return () => window.clearInterval(timer);
  }, [running]);

  const switchMode = (next: "quick" | "deep") => {
    if (next === mode) return;
    setMode(next);
    void run(next);
  };

  /** fix.label → 本地能做的动作；映射不到就只显示文字，不做死按钮。 */
  const fixAction = (label: string): (() => void) | undefined => {
    if (label.includes("目录") || label.includes("文件夹")) {
      return () => {
        void openDataDir().catch(() => toast.error("打不开数据目录，手动去文件夹里看看吧"));
      };
    }
    if (label.includes("诊断包")) return () => setDiagOpen(true);
    if (
      label.includes("体检") ||
      label.includes("再检") ||
      label.includes("重试") ||
      label.includes("再试")
    ) {
      return () => void run(mode);
    }
    if (label.includes("复制")) {
      return () => {
        void copyWithToast(JSON.stringify(report, null, 2), "已复制这段操作说明");
      };
    }
    if (label.includes("关闭")) return onClose;
    return undefined;
  };

  const copyReport = async () => {
    if (!report) return;
    await copyWithToast(JSON.stringify(report, null, 2), T.doctorCopied());
  };

  const summary = report ? summaryView(report) : null;

  return (
    <>
      <div className="modal-backdrop">
        <div className="modal doctor-modal" role="dialog" aria-modal="true" ref={dialogRef}>
        <div className="modal-head">
          <h2 className="modal-title">体检</h2>
          <p className="modal-sub">
            一键检查：应用、网络、服务器、隧道、证书，哪里不对告诉你怎么办。
          </p>
        </div>
        <div className="modal-body">
          <div className="doctor-modes">
            <div className="seg" role="group" aria-label="检查深度">
              <button
                className={`seg-btn ${mode === "quick" ? "active" : ""}`}
                aria-pressed={mode === "quick"}
                disabled={running}
                onClick={() => switchMode("quick")}
              >
                快速检查
              </button>
              <button
                className={`seg-btn ${mode === "deep" ? "active" : ""}`}
                aria-pressed={mode === "deep"}
                disabled={running}
                onClick={() => switchMode("deep")}
              >
                深度检查
              </button>
            </div>
            <span className="field-hint">深度检查会连服务器和公网，最多 1 分钟。</span>
          </div>

          {error && <div className="error-line settings-error">{error}</div>}

          {summary && (
            <div className={`doctor-summary ${summary.tone}`} role="status">
              <strong>{summary.title}</strong>
              <span>{summary.text}</span>
            </div>
          )}

          {running && (
            <div className="doctor-progress-line" role="status">
              体检中… 已用 {elapsed} 秒
            </div>
          )}

          {report ? (
            <div className="doctor-steps">
              {report.steps.map((step) => {
                const meta = statusMeta(step.status);
                const canFix =
                  (step.status === "fail" || step.status === "warn") &&
                  Boolean(step.advice || step.fix);
                const expanded = openFix === step.id;
                const action = step.fix ? fixAction(step.fix.label) : undefined;
                return (
                  <div className={`doctor-step ${step.status}`} key={step.id}>
                    <div className="doctor-step-line">
                      <span className={`doctor-step-icon ${meta.tone}`} aria-hidden="true">
                        {meta.icon}
                      </span>
                      <span className="doctor-step-title">{step.title}</span>
                      <span className={`doctor-step-status ${meta.tone}`}>{meta.word}</span>
                      {step.durationMs !== undefined && (
                        <span className="doctor-step-time">
                          {(step.durationMs / 1000).toFixed(1)}s
                        </span>
                      )}
                    </div>
                    <p className="doctor-step-detail">{step.detail}</p>
                    {canFix && (
                      <div className="doctor-fix-toggle">
                        <button
                          className="link-btn"
                          aria-expanded={expanded}
                          onClick={() => setOpenFix(expanded ? null : step.id)}
                        >
                          {expanded ? "收起怎么办" : "怎么办"}
                        </button>
                      </div>
                    )}
                    {canFix && expanded && (
                      <div className="doctor-fix">
                        {step.advice && <p className="doctor-fix-text">{step.advice}</p>}
                        {step.fix && <p className="doctor-fix-text">{step.fix.text}</p>}
                        {!step.advice && !step.fix && (
                          <p className="doctor-fix-text">{ADVICE_FALLBACK}</p>
                        )}
                        {step.fix && action && (
                          <button className="btn btn-sm" onClick={action}>
                            {step.fix.label}
                          </button>
                        )}
                      </div>
                    )}
                  </div>
                );
              })}
            </div>
          ) : running ? (
            <div className="doctor-steps">
              {FIXED_STEP_TITLES.map((title) => (
                <div className="doctor-step pending" key={title}>
                  <div className="doctor-step-line">
                    <span className="doctor-step-icon muted" aria-hidden="true">
                      …
                    </span>
                    <span className="doctor-step-title">{title}</span>
                    <span className="doctor-step-status muted">正在检查…</span>
                  </div>
                </div>
              ))}
            </div>
          ) : (
            <p className="doctor-empty">还没跑过体检。</p>
          )}
        </div>
        <div className="modal-foot">
          <button className="link-btn" disabled={!report} onClick={() => void copyReport()}>
            复制体检结果
          </button>
          <button className="link-btn" onClick={() => setDiagOpen(true)}>
            导出诊断包
          </button>
          <span className="grow" />
          <button className="btn btn-ghost" onClick={onClose}>
            关闭
          </button>
          <button className="btn btn-primary" disabled={running} onClick={() => void run(mode)}>
            {running ? "体检中…" : "再体检一次"}
          </button>
        </div>
        </div>
      </div>
      {diagOpen && <DiagExportModal onClose={() => setDiagOpen(false)} />}
    </>
  );
}
