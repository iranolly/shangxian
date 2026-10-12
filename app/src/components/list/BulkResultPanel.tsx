// 批量操作结果面板（D4 §3.3 BulkResultPanel）：只列失败项，每行可单独重试。

import { useId, useRef } from "react";

import { useModalA11y } from "../useModalA11y";

export interface BulkFailureItem {
  id: string;
  name: string;
  error: string;
}

interface Props {
  result: {
    ok: number;
    failures: BulkFailureItem[];
    /** 批量删除成功项的撤销令牌（面板里也能一次性撤销） */
    tokens?: { token: string; name: string }[];
  };
  onClose(): void;
  onRetry(ids: string[]): void;
  onUndoDeleted?(): void;
}

export function BulkResultPanel({ result, onClose, onRetry, onUndoDeleted }: Props) {
  const dialogRef = useRef<HTMLDivElement | null>(null);
  const titleId = useId();
  useModalA11y(dialogRef, onClose);
  const fail = result.failures.length;
  const tokens = result.tokens ?? [];

  return (
    <div className="modal-backdrop">
      <div className="modal" role="dialog" aria-modal="true" aria-labelledby={titleId} ref={dialogRef}>
        <div className="modal-head">
          <h2 className="modal-title" id={titleId}>
            有 {fail} 条没成功
          </h2>
          <p className="modal-sub">
            {result.ok > 0 ? `另外 ${result.ok} 条已经处理好了。` : "这些条都没有处理成功。"}
          </p>
        </div>
        <div className="modal-body">
          <ul className="bulk-result-list">
            {result.failures.map((item) => (
              <li key={item.id}>
                <span className="bulk-result-text">
                  「{item.name}」：{item.error}
                </span>
                <button className="link-btn" onClick={() => onRetry([item.id])}>
                  重试
                </button>
              </li>
            ))}
          </ul>
        </div>
        <div className="modal-foot">
          <button className="btn btn-ghost" onClick={onClose}>
            关闭
          </button>
          <span className="grow" />
          {tokens.length > 0 && onUndoDeleted ? (
            <button
              className="btn btn-ghost"
              title={`把刚才删掉的 ${tokens.length} 条恢复回来`}
              aria-label={`撤销删除，恢复 ${tokens.length} 条`}
              onClick={onUndoDeleted}
            >
              撤销删除
            </button>
          ) : null}
          <button
            className="btn btn-primary"
            onClick={() => onRetry(result.failures.map((item) => item.id))}
          >
            全部重试
          </button>
        </div>
      </div>
    </div>
  );
}
