// 「访问记录」弹窗：点隧道卡片的「访问记录」打开，关掉即卸载。
// 读这条隧道最近的被挡次数和来访样本（只在本机显示）。
// 从旧卡片内联小框（AuditBox）改成弹窗，显示规则不变。

import { useEffect, useRef, useState } from "react";

import { runAccessAudit, type AccessAudit } from "../api";
import { useModalA11y } from "./useModalA11y";

type AuditState =
  | { phase: "loading" }
  | { phase: "done"; data: AccessAudit }
  | { phase: "error"; message: string };

function AuditView({ data }: { data: AccessAudit }) {
  const blocked = data.count401 + data.count403;
  // 防弹：后端任何一段缺字段都不能把整个界面带崩（曾因此黑屏）。
  const topIps = data.topIps ?? [];
  const warnings = data.warnings ?? [];
  const entries = data.entries ?? [];
  const top = topIps[0];
  return (
    <>
      {!data.available ? (
        <p className="audit-summary">
          未找到访问日志（也可能是日志仅 root 可读），此步骤跳过。
        </p>
      ) : blocked === 0 ? (
        <p className="audit-summary">
          最近 {data.window} 行日志中没有被拦截的访问，暂未发现异常尝试。
        </p>
      ) : (
        <p className="audit-summary">
          最近 {data.window} 行日志中，密码错误被拦截 {data.count401} 次，白名单拦截{" "}
          {data.count403} 次。
          {top ? `次数最多的是 ${top.ip}（${top.count} 次）。` : ""}
        </p>
      )}
      {topIps.length > 1 && (
        <p className="audit-note">
          拦截次数最多的 IP：
          {topIps
            .slice(0, 5)
            .map((item) => `${item.ip}（${item.count} 次）`)
            .join("、")}
        </p>
      )}
      {data.truncated && <p className="audit-note">记录过多，仅查看最近的一部分。</p>}
      {data.path && <p className="audit-note mono">日志文件：{data.path}</p>}
      {warnings.map((w, index) => (
        <p className="audit-note" key={index}>
          ⚠ {w}
        </p>
      ))}
      {entries.length > 0 && (
        <div className="audit-entries">
          {entries.slice(-10).map((e, index) => (
            <span key={index}>
              {e.raw ??
                [e.time, e.ip, e.method, e.path, e.status].filter(Boolean).join(" ")}
            </span>
          ))}
        </div>
      )}
    </>
  );
}

export function AuditModal({
  tunnelId,
  tunnelName,
  onClose,
}: {
  tunnelId: string;
  tunnelName: string;
  onClose(): void;
}) {
  const [state, setState] = useState<AuditState>({ phase: "loading" });
  const [reloadKey, setReloadKey] = useState(0);
  const dialogRef = useRef<HTMLDivElement | null>(null);

  useModalA11y(dialogRef, onClose);

  useEffect(() => {
    let alive = true;
    setState({ phase: "loading" });
    runAccessAudit(tunnelId)
      .then((data) => {
        if (alive) setState({ phase: "done", data });
      })
      .catch((e) => {
        if (alive) setState({ phase: "error", message: String(e) });
      });
    return () => {
      alive = false;
    };
  }, [tunnelId, reloadKey]);

  return (
    <div className="modal-backdrop">
      <div className="modal" role="dialog" aria-modal="true" ref={dialogRef}>
        <div className="modal-head">
          <h2 className="modal-title">访问记录 · {tunnelName}</h2>
          <p className="modal-sub">
            次数为近似值；记录仅在本机显示，不会上传到任何位置。
          </p>
        </div>
        <div className="modal-body">
          {state.phase === "loading" && <p className="audit-note">读取访问记录中…</p>}
          {state.phase === "error" && (
            <p className="audit-summary">⚠ 无法读取访问记录：{state.message}</p>
          )}
          {state.phase === "done" && <AuditView data={state.data} />}
        </div>
        <div className="modal-foot">
          <button
            className="btn btn-ghost"
            title="重新从云端读取访问日志"
            onClick={() => setReloadKey((k) => k + 1)}
          >
            重新读取
          </button>
          <span className="grow" />
          <button className="btn btn-primary" onClick={onClose}>
            关闭
          </button>
        </div>
      </div>
    </div>
  );
}
