// 导出诊断包弹层（D4 §3.7 / D2 §12）。
// 只保留一个「深度检查」选项；包内容对用户讲清楚「放什么、不放什么」。
// 成功后 toast 带路径 + 「打开文件夹」；导出走 build_diag，固定落 <dataDir>/diag/。

import { useRef, useState } from "react";

import { revealItemInDir } from "@tauri-apps/plugin-opener";

import { buildDiag } from "../api";
import { T } from "../copy/toasts";
import { toast } from "../store/toastStore";
import { useModalA11y } from "./useModalA11y";

interface Props {
  onClose(): void;
}

function errorText(error: unknown): string {
  const text = error instanceof Error ? error.message : String(error);
  return text.trim() || "不知道出了什么问题";
}

export function DiagExportModal({ onClose }: Props) {
  const dialogRef = useRef<HTMLDivElement | null>(null);
  useModalA11y(dialogRef, onClose);
  const [deep, setDeep] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  const run = async () => {
    setBusy(true);
    setError("");
    try {
      const result = await buildDiag({ deep });
      toast.show({
        kind: "success",
        text: T.diagExported(result.path),
        detail: result.truncated ? "日志太多，包里的日志被缩短了" : undefined,
        action: {
          label: "打开文件夹",
          run: () => {
            void revealItemInDir(result.path).catch(() => {
              toast.error("打不开那个文件夹，手动去数据目录里找吧");
            });
          },
        },
      });
      onClose();
    } catch (e) {
      setError(`诊断包没做出来：${errorText(e)}`);
      toast.error(`诊断包没做出来：${errorText(e)}`);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="modal-backdrop">
      <div className="modal diag-modal" role="dialog" aria-modal="true" ref={dialogRef}>
        <div className="modal-head">
          <h2 className="modal-title">导出诊断包</h2>
          <p className="modal-sub">
            诊断包是一份“现场记录”，发给帮我排查的人就能定位问题。
          </p>
        </div>
        <div className="modal-body">
          <ul className="diag-list">
            <li>
              <span className="strong">里面会放：</span>
              应用版本、系统信息、脱敏后的设置、隧道和服务的状态、最近的日志、体检结果。
            </li>
            <li>
              <span className="strong">不会放：</span>
              登录密码、密码加密结果、私钥内容、控制令牌、启动参数原文。
            </li>
          </ul>
          <div className="field" style={{ marginTop: 14, marginBottom: 0 }}>
            <label className="switch">
              <input
                type="checkbox"
                checked={deep}
                disabled={busy}
                onChange={(e) => setDeep(e.target.checked)}
              />
              深度检查
            </label>
            <p className="field-hint">
              会连服务器和公网，最多 1 分钟；快速模式只查本机。
            </p>
          </div>
          {error && <div className="error-line settings-error">{error}</div>}
        </div>
        <div className="modal-foot">
          <button className="btn btn-ghost" disabled={busy} onClick={onClose}>
            取消
          </button>
          <span className="grow" />
          <button className="btn btn-primary" disabled={busy} onClick={() => void run()}>
            {busy ? "正在打包…" : "导出诊断包"}
          </button>
        </div>
      </div>
    </div>
  );
}
