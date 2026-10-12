// 导入弹层：
// - 「配置文件」页签：走新的 import_config（merge / replace / 先检查 dry-run）；
// - 「旧脚本」页签：沿用原来的扫描旧启动脚本（importServices）。
//
// 规则：先检查（dry-run）永远不写盘；整体替换（replace）前后端会自动备份，
// 前端再加一道确认，避免手滑把现有配置全换掉。

import { useEffect, useRef, useState } from "react";

import {
  importConfig,
  importServices,
  scanImports,
  type ImportCounts,
  type ImportSuggestion,
  type ImportSummary,
} from "../api";
import { errorText } from "../store/commands";
import { toast } from "../store/toastStore";
import { ConfirmModal } from "./ConfirmModal";
import { useModalA11y } from "./useModalA11y";

interface Props {
  onClose(): void;
  onDone(): void;
}

type Tab = "config" | "script";

function countsText(c: ImportCounts): string {
  return `服务器 ${c.servers} · 服务 ${c.services} · 隧道 ${c.tunnels}`;
}

export function ImportModal({ onClose, onDone }: Props) {
  const dialogRef = useRef<HTMLDivElement | null>(null);
  useModalA11y(dialogRef, onClose);

  const [tab, setTab] = useState<Tab>("config");

  // ---------- 配置文件 ----------
  const [fileName, setFileName] = useState("");
  const [doc, setDoc] = useState("");
  const [mode, setMode] = useState<"merge" | "replace">("merge");
  const [summary, setSummary] = useState<ImportSummary | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [confirmReplace, setConfirmReplace] = useState(false);

  const readFile = async (file: File | null) => {
    if (!file) return;
    setError("");
    setSummary(null);
    try {
      const text = await file.text();
      setDoc(text);
      setFileName(file.name);
    } catch (e) {
      setError(`读不出这个文件：${errorText(e)}`);
    }
  };

  const dryRun = async () => {
    if (!doc.trim()) {
      setError("先选一个配置文件，或者把内容粘贴到下面");
      return;
    }
    setBusy(true);
    setError("");
    try {
      const result = await importConfig({ doc, mode, dryRun: true });
      setSummary(result);
    } catch (e) {
      setError(errorText(e));
      setSummary(null);
    } finally {
      setBusy(false);
    }
  };

  const doImport = async () => {
    if (!doc.trim()) {
      setError("先选一个配置文件，或者把内容粘贴到下面");
      return;
    }
    setBusy(true);
    setError("");
    try {
      const result = await importConfig({ doc, mode, dryRun: false });
      toast.success("设置已从文件导入", {
        detail: `新增：${countsText(result.added)}；更新：${countsText(result.updated)}`,
      });
      onDone();
      onClose();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  };

  const startImport = () => {
    if (mode === "replace") setConfirmReplace(true);
    else void doImport();
  };

  // ---------- 旧脚本 ----------
  const [items, setItems] = useState<ImportSuggestion[] | null>(null);
  const [checked, setChecked] = useState<boolean[]>([]);
  const [scriptBusy, setScriptBusy] = useState(false);
  const [scriptError, setScriptError] = useState("");

  useEffect(() => {
    if (tab !== "script" || items !== null) return;
    scanImports()
      .then((list) => {
        setItems(list);
        setChecked(list.map(() => true));
      })
      .catch((e) => setScriptError(errorText(e)));
  }, [tab, items]);

  const doScriptImport = async () => {
    if (!items) return;
    const selected = items.filter((_, i) => checked[i]);
    if (selected.length === 0) return;
    setScriptBusy(true);
    try {
      const added = await importServices(selected);
      if (added === 0) {
        setScriptError("这些服务已经在列表里了，不用重复导入。");
        setScriptBusy(false);
        return;
      }
      onDone();
    } catch (e) {
      setScriptError(errorText(e));
      setScriptBusy(false);
    }
  };

  return (
    <div className="modal-backdrop">
      <div className="modal" role="dialog" aria-modal="true" ref={dialogRef} style={{ width: 640 }}>
        <div className="modal-head">
          <h2 className="modal-title">导入</h2>
          <p className="modal-sub">从文件恢复设置，或扫描旧启动脚本。</p>
          <div className="seg" style={{ marginTop: 10 }} role="group" aria-label="导入方式">
            <button
              className={tab === "config" ? "active" : ""}
              onClick={() => setTab("config")}
            >
              配置文件
            </button>
            <button
              className={tab === "script" ? "active" : ""}
              onClick={() => setTab("script")}
            >
              旧脚本
            </button>
          </div>
        </div>
        <div className="modal-body">
          {tab === "config" ? (
            <>
              <div className="field">
                <label className="field-label">选一个配置文件</label>
                <input
                  className="input import-file"
                  type="file"
                  accept=".json,application/json"
                  onChange={(e) => void readFile(e.target.files?.[0] ?? null)}
                />
                {fileName && <p className="field-hint hint">已选：{fileName}</p>}
              </div>

              <div className="field">
                <label className="field-label">或者把内容粘贴到这里</label>
                <textarea
                  className="input"
                  rows={4}
                  value={doc}
                  onChange={(e) => {
                    setDoc(e.target.value);
                    setSummary(null);
                  }}
                  placeholder="把导出文件的内容整个粘进来也行"
                />
              </div>

              <div className="import-mode">
                <label
                  className="switch"
                  title="按 id 对上的只改文件里出现的字段，其它保持不动"
                >
                  <input
                    type="radio"
                    name="import-mode"
                    checked={mode === "merge"}
                    onChange={() => {
                      setMode("merge");
                      setSummary(null);
                    }}
                  />
                  合并（推荐）：只改文件里提到的
                </label>
                <label
                  className="switch"
                  style={{ marginTop: 6 }}
                  title="会先自动备份现在的配置"
                >
                  <input
                    type="radio"
                    name="import-mode"
                    checked={mode === "replace"}
                    onChange={() => {
                      setMode("replace");
                      setSummary(null);
                    }}
                  />
                  整体替换：文件里有什么就是什么
                </label>
              </div>

              {summary && (
                <div className="import-summary" style={{ marginTop: 12 }}>
                  {summary.dryRun ? "检查结果（还什么都没改）：" : "导入结果："}
                  <br />
                  新增：<strong>{countsText(summary.added)}</strong>
                  <br />
                  更新：<strong>{countsText(summary.updated)}</strong>
                  <br />
                  没变化：<strong>{countsText(summary.unchanged)}</strong>
                  {summary.warnings.map((w, i) => (
                    <div key={i}>⚠ {w}</div>
                  ))}
                  {summary.backupPath && (
                    <div>备份：{summary.backupPath}</div>
                  )}
                </div>
              )}

              {error && <div className="error-line">{error}</div>}
            </>
          ) : items === null ? (
            <p className="field-hint hint">正在扫描旧的启动脚本…</p>
          ) : items.length === 0 ? (
            <p className="field-hint hint">
              没找到旧脚本，可以直接加本机服务。
            </p>
          ) : (
            <>
              <div className="option-list">
                {items.map((it, i) => (
                  <label
                    key={i}
                    className={`option ${checked[i] ? "selected" : ""}`}
                  >
                    <input
                      type="checkbox"
                      checked={checked[i]}
                      onChange={() =>
                        setChecked((c) => c.map((v, j) => (j === i ? !v : v)))
                      }
                    />
                    <div className="option-text">
                      <div>{it.name}</div>
                      <div className="option-hint hint">
                        本机端口 {it.localPort} · 云端端口 {it.remotePort} ·{" "}
                        {it.serverUser}@{it.serverHost}
                      </div>
                      <div className="option-hint hint">来源：{it.source}</div>
                    </div>
                  </label>
                ))}
              </div>
              <p
                className="field-hint hint"
                title="停掉旧的启动脚本，再到「发布隧道」页点「开始发布」完成接管。"
              >
                导入后隧道是暂停状态，确认后到「发布隧道」页启动。
              </p>
              {scriptError && <div className="error-line">{scriptError}</div>}
            </>
          )}
        </div>
        <div className="modal-foot">
          <button className="btn btn-ghost" onClick={onClose}>
            取消
          </button>
          <span className="grow" />
          {tab === "config" ? (
            <>
              <button
                className="btn btn-ghost"
                disabled={busy || !doc.trim()}
                onClick={() => void dryRun()}
              >
                {busy ? "检查中…" : "先检查一下"}
              </button>
              <button
                className="btn btn-primary"
                disabled={busy || !doc.trim()}
                onClick={startImport}
              >
                {busy ? "导入中…" : mode === "replace" ? "整体替换" : "开始导入"}
              </button>
            </>
          ) : (
            <button
              className="btn btn-primary"
              disabled={scriptBusy || !items || items.length === 0}
              onClick={() => void doScriptImport()}
            >
              {scriptBusy ? "导入中…" : "导入选中"}
            </button>
          )}
        </div>
      </div>

      {confirmReplace && (
        <ConfirmModal
          title="要整体替换现在的配置吗？"
          body="文件里没有的东西会没了（会先自动备份）。"
          confirmLabel="整体替换"
          danger
          onConfirm={() => {
            setConfirmReplace(false);
            void doImport();
          }}
          onClose={() => setConfirmReplace(false)}
        />
      )}
    </div>
  );
}
