// 导入现有脚本：扫描旧脚本 → 预览 → 确认导入（登记的隧道先保持「已暂停」）。

import { useEffect, useState } from "react";

import { importServices, scanImports, type ImportSuggestion } from "../api";

interface Props {
  onClose: () => void;
  onDone: () => void;
}

export function ImportModal({ onClose, onDone }: Props) {
  const [items, setItems] = useState<ImportSuggestion[] | null>(null);
  const [checked, setChecked] = useState<boolean[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => {
    scanImports()
      .then((list) => {
        setItems(list);
        setChecked(list.map(() => true));
      })
      .catch((e) => setError(String(e)));
  }, []);

  const doImport = async () => {
    if (!items) return;
    const selected = items.filter((_, i) => checked[i]);
    if (selected.length === 0) return;
    setBusy(true);
    try {
      const added = await importServices(selected);
      if (added === 0) {
        setError("这些服务已经在列表里了，不用重复导入。");
        setBusy(false);
        return;
      }
      onDone();
    } catch (e) {
      setError(String(e));
      setBusy(false);
    }
  };

  return (
    <div className="modal-backdrop">
      <div className="modal" role="dialog" aria-modal="true">
        <div className="modal-head">
          <h2 className="modal-title">导入现有脚本</h2>
          <p className="modal-sub">
            从旧的启动脚本里读出配置：会登记一台本机服务，并建一条「已暂停」的隧道。
            启动方式不会自动接管，需要到「本机服务」页补上。
          </p>
        </div>
        <div className="modal-body">
          {items === null ? (
            <p className="field-hint">正在扫描…</p>
          ) : items.length === 0 ? (
            <p className="field-hint">
              没有找到可以导入的旧脚本。你可以直接用「+ 添加本机服务」。
            </p>
          ) : (
            <>
              <div className="option-list">
                {items.map((it, i) => (
                  <div
                    key={i}
                    className={`option ${checked[i] ? "selected" : ""}`}
                    onClick={() =>
                      setChecked((c) => c.map((v, j) => (j === i ? !v : v)))
                    }
                  >
                    <input type="checkbox" checked={checked[i]} readOnly />
                    <div className="option-text">
                      <div>{it.name}</div>
                      <div className="option-hint">
                        本机端口 {it.localPort} · 云端端口 {it.remotePort} ·{" "}
                        {it.serverUser}@{it.serverHost}
                      </div>
                      <div className="option-hint">来源：{it.source}</div>
                    </div>
                  </div>
                ))}
              </div>
              <p className="field-hint">
                导入后隧道是「已暂停」。确认新工具跑稳后，停掉旧的启动脚本，
                再到「发布隧道」页点「开始发布」完成接管。
              </p>
            </>
          )}
          {error && <div className="error-line">{error}</div>}
        </div>
        <div className="modal-foot">
          <button className="btn btn-ghost" onClick={onClose}>
            取消
          </button>
          <span className="grow" />
          <button
            className="btn btn-primary"
            disabled={busy || !items || items.length === 0}
            onClick={doImport}
          >
            {busy ? "导入中…" : "导入选中"}
          </button>
        </div>
      </div>
    </div>
  );
}
